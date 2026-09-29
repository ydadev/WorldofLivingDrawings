import jsQR from 'jsqr';
import type { PaintLayout } from './paint-core';

interface PaperFormat {
  markerFormatVersion: number;
  imageRectMm: [number, number, number, number];
  markers: Record<string, [number, number]>;
}
interface CaptureRequest {
  pixels: ArrayBuffer;
  width: number;
  height: number;
  layouts: PaintLayout[];
  format: PaperFormat;
}
type Point = [number, number];

function fail(code: string): never { throw new Error(code); }

function detectionImage(source: Uint8ClampedArray, width: number, height: number) {
  const scale = Math.min(1, 1600 / Math.max(width, height));
  const smallWidth = Math.round(width * scale), smallHeight = Math.round(height * scale);
  const small = new Uint8ClampedArray(smallWidth * smallHeight * 4);
  for (let y = 0; y < smallHeight; y++) {
    const sourceY = Math.min(height - 1, Math.floor(y / scale));
    for (let x = 0; x < smallWidth; x++) {
      const sourceX = Math.min(width - 1, Math.floor(x / scale));
      const from = (sourceY * width + sourceX) * 4, to = (y * smallWidth + x) * 4;
      for (let k = 0; k < 4; k++) small[to + k] = source[from + k];
    }
  }
  return { small, smallWidth, smallHeight, scale };
}

function scanMarkers(source: Uint8ClampedArray, width: number, height: number,
  layouts: PaintLayout[], format: PaperFormat): { markers: Record<string, Point>; layout: PaintLayout } {
  const { small, smallWidth, smallHeight, scale } = detectionImage(source, width, height);
  const result: Record<string, Point> = {};
  let selected: PaintLayout | undefined;
  const regions = [
    [0, 0, .4, .35], [.6, 0, 1, .35], [0, .65, .4, 1], [.6, .65, 1, 1],
  ];
  for (const [left, top, right, bottom] of regions) {
    const x0 = Math.floor(left * smallWidth), y0 = Math.floor(top * smallHeight);
    const x1 = Math.ceil(right * smallWidth), y1 = Math.ceil(bottom * smallHeight);
    const cropWidth = x1 - x0, cropHeight = y1 - y0;
    const crop = new Uint8ClampedArray(cropWidth * cropHeight * 4);
    for (let y = 0; y < cropHeight; y++) {
      const from = ((y + y0) * smallWidth + x0) * 4;
      crop.set(small.subarray(from, from + cropWidth * 4), y * cropWidth * 4);
    }
    const qr = jsQR(crop, cropWidth, cropHeight, { inversionAttempts: 'dontInvert' });
    if (!qr) continue;
    const match = /^WLD(\d+)\|([a-z0-9-]+)\|(\d+)\|([a-f0-9]{20})\|(TL|TR|BL|BR)$/.exec(qr.data);
    if (!match) fail('MARKER_FORMAT');
    const recognized = layouts.find(layout => match[2] === layout.templateId &&
      Number(match[3]) === layout.templateVersion && match[4] === layout.contentHash.slice(0, 20));
    if (Number(match[1]) !== format.markerFormatVersion || !recognized ||
        (selected && selected !== recognized)) fail('TEMPLATE_MISMATCH');
    selected = recognized;
    const corner = match[5];
    if (result[corner]) fail('DUPLICATE_MARKER');
    const location = qr.location;
    const points = [location.topLeftCorner, location.topRightCorner,
      location.bottomLeftCorner, location.bottomRightCorner];
    result[corner] = [
      (points.reduce((sum, point) => sum + point.x, 0) / 4 + x0) / scale,
      (points.reduce((sum, point) => sum + point.y, 0) / 4 + y0) / scale,
    ];
  }
  if (Object.keys(result).length !== 4 || !selected) fail('MARKERS_MISSING');
  return { markers: result, layout: selected };
}

function homography(source: Record<string, Point>, target: Record<string, Point>): number[] {
  const rows: number[][] = [];
  for (const corner of ['TL', 'TR', 'BL', 'BR']) {
    const [x, y] = source[corner], [u, v] = target[corner];
    rows.push([x, y, 1, 0, 0, 0, -u * x, -u * y, u]);
    rows.push([0, 0, 0, x, y, 1, -v * x, -v * y, v]);
  }
  for (let col = 0; col < 8; col++) {
    let pivot = col;
    for (let row = col + 1; row < 8; row++)
      if (Math.abs(rows[row][col]) > Math.abs(rows[pivot][col])) pivot = row;
    if (Math.abs(rows[pivot][col]) < 1e-9) fail('INVALID_GEOMETRY');
    [rows[col], rows[pivot]] = [rows[pivot], rows[col]];
    const divisor = rows[col][col];
    for (let j = col; j < 9; j++) rows[col][j] /= divisor;
    for (let row = 0; row < 8; row++) {
      if (row === col) continue;
      const factor = rows[row][col];
      for (let j = col; j < 9; j++) rows[row][j] -= factor * rows[col][j];
    }
  }
  return rows.map(row => row[8]);
}

function project(h: number[], x: number, y: number): Point {
  const divisor = h[6] * x + h[7] * y + 1;
  if (!Number.isFinite(divisor) || Math.abs(divisor) < 1e-8) fail('INVALID_GEOMETRY');
  return [(h[0] * x + h[1] * y + h[2]) / divisor,
    (h[3] * x + h[4] * y + h[5]) / divisor];
}

function area(points: Point[]): number {
  let sum = 0;
  for (let i = 0; i < points.length; i++) {
    const [x, y] = points[i], [nextX, nextY] = points[(i + 1) % points.length];
    sum += x * nextY - nextX * y;
  }
  return Math.abs(sum) / 2;
}

function inTriangle(x: number, y: number, points: Point[]): boolean {
  const cross = (a: Point, b: Point) => (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0]);
  const signs = [cross(points[0], points[1]), cross(points[1], points[2]), cross(points[2], points[0])];
  return signs.every(value => value >= 0) || signs.every(value => value <= 0);
}

function inPaint(layout: PaintLayout, u: number, v: number): boolean {
  const [left, right, bottom, top] = layout.bounds;
  const x = left + u * (right - left), y = top - v * (top - bottom);
  const [eyeX, eyeY, eyeRadius] = layout.eye;
  if (((x - eyeX) / eyeRadius) ** 2 + ((y - eyeY) / eyeRadius) ** 2 < 1) return false;
  for (let i = 0; i < layout.body.length - 1; i++) {
    const [x0, h0] = layout.body[i], [x1, h1] = layout.body[i + 1];
    if (x < x0 || x > x1) continue;
    const height = h0 + (h1 - h0) * (x - x0) / (x1 - x0);
    if (Math.abs(y) <= height) return true;
  }
  return layout.fins.some(fin => inTriangle(x, y, fin.points));
}

function sample(source: Uint8ClampedArray, width: number, height: number, x: number, y: number,
  output: Uint8ClampedArray, offset: number): void {
  if (x < 0 || y < 0 || x >= width - 1 || y >= height - 1) fail('IMAGE_BOUNDS');
  const left = Math.floor(x), top = Math.floor(y), dx = x - left, dy = y - top;
  const a = (top * width + left) * 4, b = a + 4, c = a + width * 4, d = c + 4;
  for (let channel = 0; channel < 3; channel++) {
    output[offset + channel] = Math.round(
      source[a + channel] * (1 - dx) * (1 - dy) +
      source[b + channel] * dx * (1 - dy) +
      source[c + channel] * (1 - dx) * dy +
      source[d + channel] * dx * dy);
  }
  output[offset + 3] = 255;
}

const workerScope = self as unknown as { onmessage: ((event: MessageEvent<CaptureRequest>) => void) | null;
  postMessage: (message: unknown, transfer?: Transferable[]) => void };
workerScope.onmessage = (event: MessageEvent<CaptureRequest>) => {
  try {
    const { pixels, width, height, layouts, format } = event.data;
    if (width < 600 || height < 600 || width * height > 16_000_000 ||
        pixels.byteLength !== width * height * 4) fail('IMAGE_SIZE');
    const source = new Uint8ClampedArray(pixels);
    const { markers, layout } = scanMarkers(source, width, height, layouts, format);
    const quadrilateral = [markers.TL, markers.TR, markers.BR, markers.BL];
    if (area(quadrilateral) < width * height * .08) fail('INVALID_GEOMETRY');
    const h = homography(format.markers, markers);
    const [rectX, rectY, rectWidth, rectHeight] = format.imageRectMm;
    const output = new Uint8ClampedArray(512 * 512 * 4);
    output.fill(255);
    for (let y = 0; y < 512; y++) {
      for (let x = 0; x < 512; x++) {
        if (!inPaint(layout, (x + .5) / 512, (y + .5) / 512)) continue;
        const [px, py] = project(h, rectX + (x + .5) / 512 * rectWidth,
          rectY + (y + .5) / 512 * rectHeight);
        sample(source, width, height, px, py, output, (y * 512 + x) * 4);
      }
    }
    workerScope.postMessage({ status: 'PASS', pixels: output.buffer, width: 512, height: 512,
      templateId: layout.templateId, templateVersion: layout.templateVersion,
      layoutHash: layout.contentHash, markerFormatVersion: format.markerFormatVersion,
      sourceKind: 'paper', colorSpace: 'sRGB', markers }, [output.buffer]);
  } catch (error) {
    workerScope.postMessage({ status: 'FAIL', reason: error instanceof Error ? error.message : String(error) });
  }
};
