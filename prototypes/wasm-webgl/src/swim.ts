import { Color3 } from '@babylonjs/core/Maths/math.color';
import { Vector3 } from '@babylonjs/core/Maths/math.vector';
import { StandardMaterial } from '@babylonjs/core/Materials/standardMaterial';
import { MeshBuilder } from '@babylonjs/core/Meshes/meshBuilder';
import type { ScenePositions, SceneSnapshot, WorldDefinition } from '@ldw/contracts';
import { BabylonRendererAdapter } from '@ldw/renderer-babylon';
import worldData from '../../../content/underwater/world.json';
import { PaintDocument, type PaintLayout } from './paint-core';
import { listDrafts, type PaintDraft } from './paint-drafts';
import './style.css';

const canvas = document.querySelector<HTMLCanvasElement>('#swim-scene')!;
const draftSelect = document.querySelector<HTMLSelectElement>('#swim-draft')!;
const reloadButton = document.querySelector<HTMLButtonElement>('#swim-reload')!;
const status = document.querySelector<HTMLParagraphElement>('#swim-status')!;
const world = worldData as WorldDefinition;
const entityId = 'local-preview-fish';
let paintUrl: string | undefined;
let generation = 0;
let swimTimer: number | undefined;
let simulationTick = 0;
let swimStart = 0;

const adapter = new BabylonRendererAdapter(canvas, '/fish/', () => paintUrl ?? '');
adapter.setWorld(world);

// A quiet aquarium backdrop, kept behind the actual world renderer's fish.
function material(name: string, color: Color3): StandardMaterial {
  const result = new StandardMaterial(name, adapter.scene);
  result.diffuseColor = color;
  result.emissiveColor = color;
  result.disableLighting = true;
  return result;
}
const sand = MeshBuilder.CreatePlane('sea-floor', { width: 16, height: 1.45 }, adapter.scene);
sand.position.set(0, -3.8, 1.3);
sand.material = material('sand', new Color3(.13, .38, .43));
const reefMaterial = material('reef', new Color3(.19, .53, .52));
for (const [x, height] of [[-6.8, 1.3], [-5.9, .9], [5.7, 1.1], [6.7, 1.5]]) {
  const reef = MeshBuilder.CreateSphere(`reef-${x}`, { diameter: 1, segments: 8 }, adapter.scene);
  reef.scaling.set(.35, height, .2);
  reef.position.set(x, -3.25 + height * .3, 1.1);
  reef.material = reefMaterial;
}
const bubbleMaterial = material('bubbles', new Color3(.32, .65, .72));
for (let i = 0; i < 10; i++) {
  const bubble = MeshBuilder.CreateSphere(`bubble-${i}`, { diameter: .05 + (i % 3) * .035 }, adapter.scene);
  bubble.position = new Vector3(-7 + i * 1.55, -2.5 + (i % 5) * 1.25, 1.2);
  bubble.material = bubbleMaterial;
}

function stopSwim(): void {
  if (swimTimer !== undefined) window.clearInterval(swimTimer);
  swimTimer = undefined;
}

async function showDraft(draft: PaintDraft): Promise<void> {
  const current = ++generation;
  stopSwim();
  status.textContent = 'Готовим модель и раскраску…';
  if (draft.templateId !== 'coral' && draft.templateId !== 'stream')
    throw new Error('Неизвестный вид рыбки');
  const response = await fetch(`/fish/${draft.templateId}.layout.json`);
  if (!response.ok) throw new Error('Не удалось загрузить шаблон рыбки');
  const layout = await response.json() as PaintLayout;
  if (layout.templateId !== draft.templateId || layout.templateVersion !== draft.templateVersion ||
      layout.contentHash !== draft.layoutHash)
    throw new Error('Версия шаблона изменилась. Открой черновик в редакторе.');
  const documentState = new PaintDocument(layout);
  await documentState.restoreLayer(draft.image);
  const result = await documentState.result();
  if (current !== generation) return;
  const nextUrl = URL.createObjectURL(result.image);
  const previousUrl = paintUrl;
  paintUrl = nextUrl;
  simulationTick = 0;
  swimStart = performance.now();
  const snapshot: SceneSnapshot = { schemaVersion: 1, sceneEpoch: current, revision: 0,
    simulationTick: 0, worldId: world.id, worldVersion: world.version,
    entities: [{ id: entityId,
      definitionId: draft.templateId === 'coral' ? 'coral-fish' : 'stream-fish',
      definitionVersion: 1, position: { x: -6.5, y: 0 }, paintBlobId: draft.id }] };
  adapter.applySnapshot(snapshot);
  if (previousUrl) window.setTimeout(() => URL.revokeObjectURL(previousUrl), 1000);
  const move = () => {
    const seconds = (performance.now() - swimStart) / 1000;
    const lap = (seconds % 16) / 16;
    const frame: ScenePositions = { type: 'positions', schemaVersion: 1,
      sceneId: 'local-preview', sceneEpoch: current, revision: 0,
      simulationTick: ++simulationTick,
      positions: [{ id: entityId,
        position: { x: -6.5 + 13 * lap, y: .45 * Math.sin(seconds * 1.2) },
        heading: { x: 1, y: 0 } }] };
    adapter.applyPositions(frame);
  };
  move();
  swimTimer = window.setInterval(move, 500);
  status.textContent = 'Рыбка плавает в аквариуме. Раскраска взята из черновика этого браузера.';
}

async function refreshDrafts(): Promise<void> {
  const selected = draftSelect.value;
  const drafts = await listDrafts();
  draftSelect.replaceChildren(new Option('Выбери черновик', ''));
  for (const draft of drafts) {
    draftSelect.add(new Option(`${draft.templateId === 'coral' ? 'Круглая' : 'Быстрая'} · ${new Date(draft.modifiedAt).toLocaleString()}`, draft.id));
  }
  if (!drafts.length) {
    stopSwim();
    status.textContent = 'Черновиков нет. Сохрани рыбку в редакторе и вернись сюда.';
    return;
  }
  const draft = drafts.find(item => item.id === selected) ?? drafts[0];
  draftSelect.value = draft.id;
  await showDraft(draft);
}

function report(error: unknown): void {
  stopSwim();
  status.textContent = error instanceof Error ? error.message : 'Не удалось показать рыбку';
}
reloadButton.addEventListener('click', () => { void refreshDrafts().catch(report); });
draftSelect.addEventListener('change', () => {
  void listDrafts().then(drafts => {
    const draft = drafts.find(item => item.id === draftSelect.value);
    if (draft) return showDraft(draft);
  }).catch(report);
});
window.addEventListener('pagehide', () => { stopSwim(); if (paintUrl) URL.revokeObjectURL(paintUrl); adapter.dispose(); });
void refreshDrafts().catch(report);
