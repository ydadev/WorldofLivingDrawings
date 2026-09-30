export interface PaintLayout {
  templateId: string;
  templateVersion: number;
  contentHash: string;
  bounds: [number, number, number, number];
  body: [number, number, number][];
  fins: { id: string; points: [number, number][] }[];
  eye: [number, number, number];
}

export type PaintAction =
  | { kind: 'stroke' | 'erase'; points: [number, number][]; size: number; color: string }
  | { kind: 'fill'; x: number; y: number; color: string }
  | { kind: 'clear' };

const WORK_SIZE = 1024;
const TEXTURE_SIZE = 512;
const MAX_HISTORY_ACTIONS = 50;
const MAX_HISTORY_BYTES = 32 * 1024 * 1024;

function actionBytes(action: PaintAction): number {
  return action.kind === 'stroke' || action.kind === 'erase'
    ? 256 + action.points.length * 64 : 256;
}

export function layoutPoint(layout: PaintLayout, x: number, y: number): [number, number] {
  const [left, right, bottom, top] = layout.bounds;
  return [(x - left) / (right - left) * TEXTURE_SIZE,
    (top - y) / (top - bottom) * TEXTURE_SIZE];
}

export function silhouette(layout: PaintLayout): Path2D {
  const path = new Path2D();
  const top = layout.body.map(([x, y]) => layoutPoint(layout, x, y));
  const bottom = layout.body.slice().reverse().map(([x, y]) => layoutPoint(layout, x, -y));
  [...top, ...bottom].forEach(([x, y], i) => i ? path.lineTo(x, y) : path.moveTo(x, y));
  path.closePath();
  for (const fin of layout.fins) {
    fin.points.forEach(([x, y], i) => {
      const [u, v] = layoutPoint(layout, x, y);
      if (i) path.lineTo(u, v); else path.moveTo(u, v);
    });
    path.closePath();
  }
  return path;
}

export class PaintDocument {
  readonly layer = document.createElement('canvas');
  readonly mask = document.createElement('canvas');
  private readonly base = document.createElement('canvas');
  readonly layout: PaintLayout;
  readonly outline: Path2D;
  private actions: PaintAction[] = [];
  private cursor = 0;

  constructor(layout: PaintLayout) {
    this.layout = layout;
    this.layer.width = this.layer.height = WORK_SIZE;
    this.mask.width = this.mask.height = WORK_SIZE;
    this.base.width = this.base.height = WORK_SIZE;
    this.outline = silhouette(layout);
    const m = this.mask.getContext('2d', { willReadFrequently: true })!;
    m.scale(2, 2);
    m.fillStyle = '#fff';
    m.fill(this.outline);
    const [ex, ey] = layoutPoint(layout, layout.eye[0], layout.eye[1]);
    const rx = layout.eye[2] / (layout.bounds[1] - layout.bounds[0]) * TEXTURE_SIZE;
    const ry = layout.eye[2] / (layout.bounds[3] - layout.bounds[2]) * TEXTURE_SIZE;
    m.globalCompositeOperation = 'destination-out';
    m.beginPath();
    m.ellipse(ex, ey, rx, ry, 0, 0, Math.PI * 2);
    m.fill();
    if (m.getImageData(512, 512, 1, 1).data[3] === 0) throw new Error('Paint mask is empty at body center');
  }

  get undoable(): boolean { return this.cursor > 0; }
  get redoable(): boolean { return this.cursor < this.actions.length; }
  get actionCount(): number { return this.cursor; }

  add(action: PaintAction): void {
    this.actions.length = this.cursor;
    this.actions.push(action);
    this.cursor++;
    this.render();
    while (this.actions.length > MAX_HISTORY_ACTIONS ||
      this.actions.reduce((total, item) => total + actionBytes(item), 0) > MAX_HISTORY_BYTES)
      this.foldOldest();
  }

  private foldOldest(): void {
    const oldest = this.actions[0];
    const remaining = this.actions.slice(1);
    const nextCursor = this.cursor - 1;
    this.actions = [oldest];
    this.cursor = 1;
    this.render();
    const context = this.base.getContext('2d')!;
    context.clearRect(0, 0, WORK_SIZE, WORK_SIZE);
    context.drawImage(this.layer, 0, 0);
    this.actions = remaining;
    this.cursor = nextCursor;
    this.render();
  }

  undo(): void { if (this.undoable) { this.cursor--; this.render(); } }
  redo(): void { if (this.redoable) { this.cursor++; this.render(); } }

  async restoreLayer(image: Blob): Promise<void> {
    if (image.type !== 'image/png' || image.size > 64 * 1024 * 1024)
      throw new Error('Неверный формат черновика');
    const bitmap = await createImageBitmap(image);
    try {
      if (bitmap.width !== WORK_SIZE || bitmap.height !== WORK_SIZE)
        throw new Error('Неверный размер черновика');
      const context = this.base.getContext('2d')!;
      context.clearRect(0, 0, WORK_SIZE, WORK_SIZE);
      context.save();
      try {
        context.drawImage(bitmap, 0, 0);
        context.globalCompositeOperation = 'destination-in';
        context.drawImage(this.mask, 0, 0);
      } finally { context.restore(); }
      this.actions = [];
      this.cursor = 0;
      this.render();
    } finally {
      bitmap.close();
    }
  }

  async draftLayer(): Promise<Blob> {
    return new Promise((resolve, reject) => this.layer.toBlob(
      blob => blob ? resolve(blob) : reject(new Error('Не удалось сохранить слой')), 'image/png'));
  }

  private fill(x: number, y: number, color: string): void {
    const px = Math.floor(x * 2), py = Math.floor(y * 2);
    if (px < 0 || py < 0 || px >= WORK_SIZE || py >= WORK_SIZE) return;
    const context = this.layer.getContext('2d', { willReadFrequently: true })!;
    const image = context.getImageData(0, 0, WORK_SIZE, WORK_SIZE);
    const pixels = image.data;
    const mask = this.mask.getContext('2d', { willReadFrequently: true })!
      .getImageData(0, 0, WORK_SIZE, WORK_SIZE).data;
    const start = py * WORK_SIZE + px;
    if (mask[start * 4 + 3] < 128) return;
    const target = Array.from(pixels.slice(start * 4, start * 4 + 4));
    const sample = document.createElement('canvas');
    sample.width = sample.height = 1;
    const sampleContext = sample.getContext('2d')!;
    sampleContext.fillStyle = color;
    sampleContext.fillRect(0, 0, 1, 1);
    const replacement = sampleContext.getImageData(0, 0, 1, 1).data;
    if (target.every((value, i) => Math.abs(value - replacement[i]) < 8)) return;
    const seen = new Uint8Array(WORK_SIZE * WORK_SIZE);
    const queue = new Int32Array(WORK_SIZE * WORK_SIZE);
    let head = 0, tail = 0;
    queue[tail++] = start;
    seen[start] = 1;
    while (head < tail) {
      const index = queue[head++], offset = index * 4;
      if (mask[offset + 3] < 128 || target.some((value, i) => Math.abs(value - pixels[offset + i]) > 12)) continue;
      for (let i = 0; i < 4; i++) pixels[offset + i] = replacement[i];
      const column = index % WORK_SIZE;
      const neighbors = [index - WORK_SIZE, index + WORK_SIZE];
      if (column > 0) neighbors.push(index - 1);
      if (column < WORK_SIZE - 1) neighbors.push(index + 1);
      for (const next of neighbors) {
        if (next >= 0 && next < seen.length && !seen[next]) {
          seen[next] = 1;
          queue[tail++] = next;
        }
      }
    }
    context.putImageData(image, 0, 0);
  }

  render(): void {
    const context = this.layer.getContext('2d', { willReadFrequently: true })!;
    context.resetTransform();
    context.clearRect(0, 0, WORK_SIZE, WORK_SIZE);
    context.drawImage(this.base, 0, 0);
    for (const action of this.actions.slice(0, this.cursor)) {
      if (action.kind === 'clear') {
        context.clearRect(0, 0, WORK_SIZE, WORK_SIZE);
      } else if (action.kind === 'fill') {
        this.fill(action.x, action.y, action.color);
      } else if (action.points.length) {
        context.save();
        context.scale(2, 2);
        context.globalCompositeOperation = action.kind === 'erase' ? 'destination-out' : 'source-over';
        context.lineWidth = action.size;
        context.lineCap = context.lineJoin = 'round';
        context.strokeStyle = action.color;
        context.fillStyle = action.color;
        const [firstX, firstY] = action.points[0];
        if (action.points.every(([x, y]) => Math.hypot(x - firstX, y - firstY) < 0.1)) {
          context.beginPath();
          context.arc(firstX, firstY, action.size / 2, 0, Math.PI * 2);
          context.fill();
        } else {
          context.beginPath();
          context.moveTo(firstX, firstY);
          for (const [x, y] of action.points.slice(1)) context.lineTo(x, y);
          context.stroke();
        }
        context.restore();
        context.save();
        context.globalCompositeOperation = 'destination-in';
        context.drawImage(this.mask, 0, 0);
        context.restore();
      }
    }
  }

  textureCanvas(): HTMLCanvasElement {
    const canvas = document.createElement('canvas');
    canvas.width = canvas.height = TEXTURE_SIZE;
    const context = canvas.getContext('2d')!;
    context.fillStyle = '#fff';
    context.fillRect(0, 0, TEXTURE_SIZE, TEXTURE_SIZE);
    context.drawImage(this.layer, 0, 0, TEXTURE_SIZE, TEXTURE_SIZE);
    return canvas;
  }

  async result(): Promise<{ templateId: string; templateVersion: number; layoutHash: string;
    sourceKind: 'browser'; colorSpace: 'sRGB'; image: Blob }> {
    const image = await new Promise<Blob>((resolve, reject) => this.textureCanvas()
      .toBlob(blob => blob ? resolve(blob) : reject(new Error('PNG export failed')), 'image/png'));
    return { templateId: this.layout.templateId, templateVersion: this.layout.templateVersion,
      layoutHash: this.layout.contentHash, sourceKind: 'browser', colorSpace: 'sRGB', image };
  }
}
