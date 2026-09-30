import type { ScenePositions, SceneSnapshot, WorldDefinition } from '@ldw/contracts';
import { BabylonRendererAdapter } from '@ldw/renderer-babylon';
import worldData from '../../../content/underwater/world.json';
import { PaintDocument, type PaintLayout } from './paint-core';
import { listDrafts, type PaintDraft } from './paint-drafts';
import { PreviewSwimWorld } from './swim-path';
import './style.css';

const canvas = document.querySelector<HTMLCanvasElement>('#swim-scene')!;
const draftSelect = document.querySelector<HTMLSelectElement>('#swim-draft')!;
const reloadButton = document.querySelector<HTMLButtonElement>('#swim-reload')!;
const status = document.querySelector<HTMLParagraphElement>('#swim-status')!;
const world = worldData as WorldDefinition;
let paintUrl: string | undefined;
let generation = 0;
let swimTimer: number | undefined;
let simulationTick = 0;

const adapter = new BabylonRendererAdapter(canvas, '/fish/', () => paintUrl ?? '', 50);
adapter.setWorld(world);

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
  const seed = [...draft.id].reduce((value, char) =>
    (Math.imul(value, 31) + char.charCodeAt(0)) | 0, 0x4d595df4);
  const preview = new PreviewSwimWorld(seed);
  const snapshot: SceneSnapshot = { schemaVersion: 1, sceneEpoch: current, revision: 0,
    simulationTick: 0, worldId: world.id, worldVersion: world.version,
    entities: preview.positions().map((fish, index) => ({ id: fish.id,
      definitionId: index === 0 ? (draft.templateId === 'coral' ? 'coral-fish' : 'stream-fish') :
        index === 1 ? 'stream-fish' : 'coral-fish',
      definitionVersion: 1, position: { x: fish.x, y: fish.y },
      ...(index === 0 ? { paintBlobId: draft.id } : {}) })) };
  adapter.applySnapshot(snapshot);
  if (previousUrl) window.setTimeout(() => URL.revokeObjectURL(previousUrl), 1000);
  const move = () => {
    const positions = preview.step();
    simulationTick++;
    const frame: ScenePositions = { type: 'positions', schemaVersion: 1,
      sceneId: 'local-preview', sceneEpoch: current, revision: 0,
      simulationTick,
      positions: positions.map(fish => ({ id: fish.id,
        position: { x: fish.x, y: fish.y }, depth: fish.depth,
        heading: fish.heading, headingDepth: fish.headingDepth })) };
    adapter.applyPositions(frame);
  };
  move();
  swimTimer = window.setInterval(move, 50);
  status.textContent = 'Раскрашенная рыбка плавает среди двух соседей. Они выбирают разные цели и темп, иногда исследуют поверхность или дно и реагируют на сближение. Это локальный просмотр; в общем мире решения принимает сервер.';
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
