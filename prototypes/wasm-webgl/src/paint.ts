import '@babylonjs/loaders/glTF';
import { ArcRotateCamera } from '@babylonjs/core/Cameras/arcRotateCamera';
import { Engine } from '@babylonjs/core/Engines/engine';
import { HemisphericLight } from '@babylonjs/core/Lights/hemisphericLight';
import { ImportMeshAsync } from '@babylonjs/core/Loading/sceneLoader';
import { Color3, Color4 } from '@babylonjs/core/Maths/math.color';
import { Vector3 } from '@babylonjs/core/Maths/math.vector';
import { PBRMaterial } from '@babylonjs/core/Materials/PBR/pbrMaterial';
import { DynamicTexture } from '@babylonjs/core/Materials/Textures/dynamicTexture';
import { Scene } from '@babylonjs/core/scene';
import { PaintDocument, layoutPoint, type PaintAction, type PaintLayout } from './paint-core';
import './style.css';

type FishId = 'coral' | 'stream';
type Tool = 'stroke' | 'fill' | 'erase' | 'pick';
declare global { interface Window {
  paintProbe?: { status: string; templateId?: string; layoutHash?: string;
    actionCount?: number; sampledColor?: string; texturePixel?: number[]; error?: string };
  paintResult?: () => ReturnType<PaintDocument['result']>;
} }

function required<T extends Element>(selector: string): T {
  const element = document.querySelector<T>(selector);
  if (!element) throw new Error(`Missing ${selector}`);
  return element;
}

const sheet = required<HTMLCanvasElement>('#paint-sheet');
const model = required<HTMLCanvasElement>('#paint-model');
const speciesInput = required<HTMLSelectElement>('#paint-species');
const toolInput = required<HTMLSelectElement>('#paint-tool');
const colorInput = required<HTMLInputElement>('#paint-color');
const sizeInput = required<HTMLInputElement>('#paint-size');
const status = required<HTMLParagraphElement>('#paint-status');
const undoButton = required<HTMLButtonElement>('#paint-undo');
const redoButton = required<HTMLButtonElement>('#paint-redo');
const clearButton = required<HTMLButtonElement>('#paint-clear');
const downloadButton = required<HTMLButtonElement>('#paint-download');
const sheetContext = sheet.getContext('2d')!;
const engine = new Engine(model, true);
const scene = new Scene(engine);
scene.clearColor = new Color4(0.04, 0.2, 0.28, 1);
const camera = new ArcRotateCamera('paint-preview', Math.PI / 2, Math.PI / 2, 6.3, Vector3.Zero(), scene);
scene.activeCamera = camera;
new HemisphericLight('paint-light', new Vector3(0, 1, 0), scene).intensity = 1.7;
let previewActive = true;
engine.runRenderLoop(() => { if (previewActive) scene.render(); });
window.addEventListener('message', event => {
  if (event.origin === location.origin && event.source === window.parent &&
      event.data?.type === 'ldw-preview' && typeof event.data.active === 'boolean')
    previewActive = event.data.active;
});
window.addEventListener('resize', () => engine.resize());

let documentState: PaintDocument | undefined;
let roots: Awaited<ReturnType<typeof ImportMeshAsync>>['meshes'] = [];
let texture: DynamicTexture | undefined;
let currentPointer: number | undefined;
let points: [number, number][] = [];
let pointerTool: 'stroke' | 'erase' = 'stroke';
let pointerColor = '#e9463a';
let pointerSize = 16;
let generation = 0;

function coordinate(event: PointerEvent): [number, number] {
  const rect = sheet.getBoundingClientRect();
  return [(event.clientX - rect.left) / rect.width * 512,
    (event.clientY - rect.top) / rect.height * 512];
}

function paintSheet(): void {
  const doc = documentState;
  if (!doc) return;
  sheetContext.setTransform(1, 0, 0, 1, 0, 0);
  sheetContext.fillStyle = '#fff';
  sheetContext.fillRect(0, 0, 1024, 1024);
  sheetContext.drawImage(doc.layer, 0, 0);
  sheetContext.save();
  sheetContext.scale(2, 2);
  if (points.length) {
    sheetContext.save();
    sheetContext.clip(doc.outline);
    sheetContext.lineCap = sheetContext.lineJoin = 'round';
    sheetContext.strokeStyle = pointerTool === 'erase' ? '#fff' : pointerColor;
    sheetContext.fillStyle = sheetContext.strokeStyle;
    sheetContext.lineWidth = pointerSize;
    sheetContext.beginPath();
    sheetContext.moveTo(...points[0]);
    for (const point of points.slice(1)) sheetContext.lineTo(...point);
    if (points.every(([x, y]) => Math.hypot(x - points[0][0], y - points[0][1]) < 0.1)) {
      sheetContext.arc(points[0][0], points[0][1], pointerSize / 2, 0, Math.PI * 2);
      sheetContext.fill();
    } else sheetContext.stroke();
    sheetContext.restore();
  }
  sheetContext.strokeStyle = '#172a33';
  sheetContext.lineWidth = 2.5;
  sheetContext.stroke(doc.outline);
  const [ex, ey] = layoutPoint(doc.layout, doc.layout.eye[0], doc.layout.eye[1]);
  const rx = doc.layout.eye[2] / (doc.layout.bounds[1] - doc.layout.bounds[0]) * 512;
  const ry = doc.layout.eye[2] / (doc.layout.bounds[3] - doc.layout.bounds[2]) * 512;
  sheetContext.fillStyle = '#fff';
  sheetContext.strokeStyle = '#172a33';
  sheetContext.beginPath();
  sheetContext.ellipse(ex, ey, rx, ry, 0, 0, Math.PI * 2);
  sheetContext.fill();
  sheetContext.stroke();
  sheetContext.fillStyle = '#172a33';
  sheetContext.beginPath();
  sheetContext.ellipse(ex, ey, rx * .48, ry * .48, 0, 0, Math.PI * 2);
  sheetContext.fill();
  sheetContext.restore();
  undoButton.disabled = !doc.undoable;
  redoButton.disabled = !doc.redoable;
  if (window.paintProbe?.status === 'PASS') window.paintProbe.actionCount = doc.actionCount;
}

function updateModel(): void {
  if (!documentState || !texture) return;
  const context = texture.getContext();
  context.drawImage(documentState.textureCanvas(), 0, 0);
  texture.update(false);
  const pixels = documentState.textureCanvas().getContext('2d')!.getImageData(256, 256, 1, 1).data;
  if (window.paintProbe?.status === 'PASS') window.paintProbe.texturePixel = Array.from(pixels);
}

function commit(action: PaintAction): void {
  documentState?.add(action);
  paintSheet();
  updateModel();
}

async function load(species: FishId): Promise<void> {
  const thisGeneration = ++generation;
  currentPointer = undefined;
  points = [];
  status.textContent = 'Загрузка шаблона и модели…';
  window.paintProbe = { status: 'LOADING' };
  try {
    const response = await fetch(`/fish/${species}.layout.json`);
    if (!response.ok) throw new Error(`Layout HTTP ${response.status}`);
    const layout = await response.json() as PaintLayout;
    if (layout.templateId !== species || !/^[a-f0-9]{64}$/.test(layout.contentHash))
      throw new Error('Invalid layout identity');
    const imported = await ImportMeshAsync(`/fish/${species}.glb`, scene);
    if (thisGeneration !== generation) { imported.meshes.forEach(mesh => mesh.dispose()); return; }
    roots.forEach(mesh => mesh.dispose(false, true));
    texture?.dispose();
    roots = imported.meshes;
    documentState = new PaintDocument(layout);
    window.paintResult = () => documentState!.result();
    const paintMaterials = new Set<PBRMaterial>();
    for (const mesh of roots) if (mesh.material instanceof PBRMaterial && mesh.material.name === 'paint')
      paintMaterials.add(mesh.material);
    if (paintMaterials.size !== 1) throw new Error('Expected one paint material');
    texture = new DynamicTexture('current-paint', { width: 512, height: 512 }, scene, false);
    for (const material of paintMaterials) {
      material.albedoColor = Color3.White();
      material.albedoTexture = texture;
    }
    if (engine.webGLVersion !== 2) throw new Error('WebGL 2 required');
    window.paintProbe = { status: 'PASS', templateId: species, layoutHash: layout.contentHash, actionCount: 0 };
    paintSheet();
    updateModel();
    status.textContent = `Готово: ${species}. Рисунок остаётся на этом устройстве до закрытия страницы.`;
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    if (thisGeneration === generation) {
      window.paintProbe = { status: 'FAIL', error: message };
      status.textContent = `Ошибка: ${message}`;
    }
  }
}

sheet.addEventListener('pointerdown', event => {
  if (!documentState) return;
  if (currentPointer !== undefined) {
    points = [];
    currentPointer = undefined;
    paintSheet();
    return;
  }
  if (event.pointerType === 'mouse' && event.button !== 0) return;
  const [x, y] = coordinate(event);
  const tool = toolInput.value as Tool;
  if (tool === 'fill') { commit({ kind: 'fill', x, y, color: colorInput.value }); return; }
  if (tool === 'pick') {
    const pixel = documentState.textureCanvas().getContext('2d')!
      .getImageData(Math.max(0, Math.min(511, Math.floor(x))), Math.max(0, Math.min(511, Math.floor(y))), 1, 1).data;
    colorInput.value = '#' + [...pixel].slice(0, 3).map(value => value.toString(16).padStart(2, '0')).join('');
    if (window.paintProbe) window.paintProbe.sampledColor = colorInput.value;
    return;
  }
  currentPointer = event.pointerId;
  pointerTool = tool;
  pointerColor = colorInput.value;
  pointerSize = Number(sizeInput.value);
  points = [[x, y]];
  sheet.setPointerCapture(event.pointerId);
  paintSheet();
});
sheet.addEventListener('pointermove', event => {
  if (currentPointer !== event.pointerId) return;
  points.push(coordinate(event));
  paintSheet();
});
sheet.addEventListener('pointerup', event => {
  if (currentPointer !== event.pointerId) return;
  points.push(coordinate(event));
  const action: PaintAction = { kind: pointerTool, points, size: pointerSize, color: pointerColor };
  currentPointer = undefined;
  points = [];
  commit(action);
});
sheet.addEventListener('pointercancel', event => {
  if (currentPointer === event.pointerId) { currentPointer = undefined; points = []; paintSheet(); }
});
undoButton.addEventListener('click', () => { documentState?.undo(); paintSheet(); updateModel(); });
redoButton.addEventListener('click', () => { documentState?.redo(); paintSheet(); updateModel(); });
clearButton.addEventListener('click', () => commit({ kind: 'clear' }));
downloadButton.addEventListener('click', async () => {
  if (!documentState) return;
  const result = await documentState.result();
  const url = URL.createObjectURL(result.image);
  const link = document.createElement('a');
  link.href = url;
  link.download = `${result.templateId}-v${result.templateVersion}-paint.png`;
  link.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
});
speciesInput.addEventListener('change', () => void load(speciesInput.value as FishId));
window.addEventListener('keydown', event => {
  if (!(event.ctrlKey || event.metaKey) || event.key.toLowerCase() !== 'z') return;
  event.preventDefault();
  if (event.shiftKey) documentState?.redo(); else documentState?.undo();
  paintSheet(); updateModel();
});
void load(speciesInput.value as FishId);
