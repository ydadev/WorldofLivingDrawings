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
import { DraftError, deleteDraft, listDrafts, loadDraft, saveDraft, type PaintDraft } from './paint-drafts';
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
const draftSaveButton = required<HTMLButtonElement>('#draft-save');
const draftList = required<HTMLSelectElement>('#draft-list');
const draftOpenButton = required<HTMLButtonElement>('#draft-open');
const draftCopyButton = required<HTMLButtonElement>('#draft-copy');
const draftDeleteButton = required<HTMLButtonElement>('#draft-delete');
const draftStatus = required<HTMLParagraphElement>('#draft-status');
const draftPreview = required<HTMLDivElement>('#draft-preview');
const draftPreviewImage = required<HTMLImageElement>('#draft-preview-image');
const draftPreviewDownload = required<HTMLButtonElement>('#draft-preview-download');
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
let draftId: string | undefined;
let draftRevision = 0;
let draftEnabled = false;
let editNumber = 0;
let savedNumber = 0;
let saveTimer: number | undefined;
let saving: Promise<boolean> | undefined;
let previewUrl: string | undefined;
let previewDraft: PaintDraft | undefined;

function draftMessage(message: string, state: string): void {
  draftStatus.textContent = message;
  draftStatus.dataset.state = state;
}

function hideDraftPreview(): void {
  if (previewUrl) URL.revokeObjectURL(previewUrl);
  previewUrl = undefined;
  previewDraft = undefined;
  draftPreview.hidden = true;
  draftPreviewImage.removeAttribute('src');
}

async function refreshDrafts(selected = draftId): Promise<void> {
  try {
    const drafts = await listDrafts();
    draftList.replaceChildren();
    draftList.add(new Option('Выбери черновик', ''));
    for (const draft of drafts) {
      const label = `${draft.templateId} · ${new Date(draft.modifiedAt).toLocaleString()} · v${draft.revision}`;
      const option = new Option(label, draft.id);
      option.dataset.revision = String(draft.revision);
      draftList.add(option);
    }
    if (selected && drafts.some(draft => draft.id === selected)) draftList.value = selected;
    draftOpenButton.disabled = draftDeleteButton.disabled = !draftList.value;
  } catch (error) {
    draftMessage(error instanceof Error ? error.message : 'Черновики недоступны', 'error');
  }
}

function changed(): void {
  if (!draftEnabled) return;
  editNumber++;
  draftMessage('Сохраняем…', 'saving');
  if (saveTimer !== undefined) clearTimeout(saveTimer);
  saveTimer = window.setTimeout(() => { saveTimer = undefined; void flushDraft(); }, 350);
}

async function writeDraft(): Promise<boolean> {
  const doc = documentState;
  if (!doc || !draftEnabled || !draftId) return true;
  const snapshotNumber = editNumber;
  draftMessage('Сохраняем…', 'saving');
  try {
    const image = await doc.draftLayer();
    const saved = await saveDraft({ id: draftId, editorVersion: 1,
      templateId: doc.layout.templateId, templateVersion: doc.layout.templateVersion,
      layoutHash: doc.layout.contentHash, modelId: `fish/${doc.layout.templateId}.glb`, image }, draftRevision);
    draftRevision = saved.revision;
    savedNumber = snapshotNumber;
    if (savedNumber === editNumber) draftMessage('Сохранено. Черновик хранится на этом устройстве.', 'saved');
    await refreshDrafts(saved.id);
    return true;
  } catch (error) {
    const code = error instanceof DraftError ? error.code : 'unavailable';
    draftMessage(`${error instanceof Error ? error.message : 'Не удалось сохранить'}. Рисунок остаётся в памяти; скачай PNG или освободи место.`, code);
    draftCopyButton.hidden = code !== 'conflict';
    return false;
  }
}

async function flushDraft(): Promise<boolean> {
  if (saveTimer !== undefined) { clearTimeout(saveTimer); saveTimer = undefined; }
  if (!draftEnabled || savedNumber === editNumber) return true;
  if (saving) {
    const previous = saving;
    const okay = await previous;
    if (saving === previous) saving = undefined;
    return okay ? flushDraft() : false;
  }
  const current = writeDraft();
  saving = current;
  const okay = await current;
  if (saving === current) saving = undefined;
  if (okay && savedNumber !== editNumber) return flushDraft();
  return okay;
}

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
  changed();
}

async function load(species: FishId, draft?: PaintDraft): Promise<void> {
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
    if (draft) await documentState.restoreLayer(draft.image);
    draftId = draft?.id;
    draftRevision = draft?.revision ?? 0;
    draftEnabled = !!draft;
    editNumber = savedNumber = 0;
    draftCopyButton.hidden = true;
    hideDraftPreview();
    draftMessage(draft ? 'Сохранено. Черновик хранится на этом устройстве.' : 'Рисунок пока не сохранён.', draft ? 'saved' : 'unsaved');
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
    status.textContent = `Готово: ${species}. ${draft ? 'Локальный черновик открыт.' : 'Рисунок остаётся в памяти до сохранения черновика.'}`;
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
undoButton.addEventListener('click', () => { if (!documentState?.undoable) return; documentState.undo(); paintSheet(); updateModel(); changed(); });
redoButton.addEventListener('click', () => { if (!documentState?.redoable) return; documentState.redo(); paintSheet(); updateModel(); changed(); });
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
draftSaveButton.addEventListener('click', () => {
  if (!documentState) return;
  if (!draftEnabled) {
    draftId = crypto.randomUUID();
    draftRevision = 0;
    draftEnabled = true;
    editNumber++;
  }
  void flushDraft();
});
draftList.addEventListener('change', () => {
  draftOpenButton.disabled = draftDeleteButton.disabled = !draftList.value;
});
draftOpenButton.addEventListener('click', async () => {
  if (!draftList.value || !(await flushDraft())) return;
  try {
    const draft = await loadDraft(draftList.value);
    if (draft.templateId !== 'coral' && draft.templateId !== 'stream') throw new DraftError('invalid', 'Неизвестная модель черновика');
    const response = await fetch(`/fish/${draft.templateId}.layout.json`);
    if (!response.ok) throw new Error('Шаблон недоступен');
    const layout = await response.json() as PaintLayout;
    if (draft.templateVersion !== layout.templateVersion || draft.layoutHash !== layout.contentHash ||
        draft.modelId !== `fish/${draft.templateId}.glb`) {
      hideDraftPreview();
      previewUrl = URL.createObjectURL(draft.image);
      previewDraft = draft;
      draftPreviewImage.src = previewUrl;
      draftPreview.hidden = false;
      draftMessage('Версия шаблона не совпадает. Доступен просмотр и скачивание PNG.', 'incompatible');
      return;
    }
    speciesInput.value = draft.templateId;
    await load(draft.templateId, draft);
  } catch (error) {
    draftMessage(error instanceof Error ? error.message : 'Не удалось открыть черновик', 'error');
  }
});
draftCopyButton.addEventListener('click', () => {
  if (!documentState) return;
  draftId = crypto.randomUUID();
  draftRevision = 0;
  draftEnabled = true;
  editNumber++;
  draftCopyButton.hidden = true;
  void flushDraft();
});
draftDeleteButton.addEventListener('click', async () => {
  if (draftList.value === draftId && !(await flushDraft())) return;
  const id = draftList.value;
  const revision = Number(draftList.selectedOptions[0]?.dataset.revision);
  if (!id || !revision) return;
  try {
    await deleteDraft(id, revision);
    if (draftId === id) {
      if (saveTimer !== undefined) clearTimeout(saveTimer);
      draftEnabled = false;
      draftId = undefined;
      draftRevision = 0;
      draftMessage('Черновик удалён. Текущий рисунок остался в памяти.', 'unsaved');
    }
    hideDraftPreview();
    await refreshDrafts();
  } catch (error) {
    draftMessage(error instanceof Error ? error.message : 'Не удалось удалить черновик', 'error');
  }
});
draftPreviewDownload.addEventListener('click', () => {
  if (!previewDraft || !previewUrl) return;
  const link = document.createElement('a');
  link.href = previewUrl;
  link.download = `${previewDraft.templateId}-v${previewDraft.templateVersion}-draft.png`;
  link.click();
});
speciesInput.addEventListener('change', async () => {
  const previous = documentState?.layout.templateId as FishId | undefined;
  const next = speciesInput.value as FishId;
  if (!(await flushDraft())) { if (previous) speciesInput.value = previous; return; }
  await load(next);
  await refreshDrafts();
});
window.addEventListener('keydown', event => {
  if (!(event.ctrlKey || event.metaKey) || event.key.toLowerCase() !== 'z') return;
  event.preventDefault();
  if (event.shiftKey) { if (!documentState?.redoable) return; documentState.redo(); }
  else { if (!documentState?.undoable) return; documentState.undo(); }
  paintSheet(); updateModel();
  changed();
});
void load(speciesInput.value as FishId);
void refreshDrafts();
