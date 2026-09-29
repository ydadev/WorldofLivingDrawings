import '@babylonjs/core/Culling/ray';
import { Camera } from '@babylonjs/core/Cameras/camera';
import { FreeCamera } from '@babylonjs/core/Cameras/freeCamera';
import { Engine } from '@babylonjs/core/Engines/engine';
import { HemisphericLight } from '@babylonjs/core/Lights/hemisphericLight';
import { Color3, Color4 } from '@babylonjs/core/Maths/math.color';
import { Matrix, Vector3 } from '@babylonjs/core/Maths/math.vector';
import { Plane } from '@babylonjs/core/Maths/math.plane';
import { StandardMaterial } from '@babylonjs/core/Materials/standardMaterial';
import { MeshBuilder } from '@babylonjs/core/Meshes/meshBuilder';
import { AbstractMesh } from '@babylonjs/core/Meshes/abstractMesh';
import { Scene } from '@babylonjs/core/scene';
import './style.css';

type Action = 'feed' | 'boat';
interface WorldEvent { id: string; action: Action; x: number; y: number }
interface Probe {
  connected: boolean;
  sceneEpoch: number;
  revision: number;
  events: WorldEvent[];
  sent: { commandId: string; action: Action; x: number; y: number }[];
  acknowledgments: string[];
  rejections: string[];
  webglVersion: number;
  frames: number;
  pointerDowns: number;
  pointerUps: number;
  pointerCancels: number;
  lastGestureMs?: number;
  lastDelta?: number;
  lastPointerIds?: [number, number];
  sizing?: number[];
  lastPick?: { x: number; y: number } | null;
  error?: string;
}
declare global { interface Window { interactionProbe?: Probe; interactionSocket?: WebSocket } }

const canvas = document.querySelector<HTMLCanvasElement>('#interaction-scene')!;
const status = document.querySelector<HTMLParagraphElement>('#interaction-status')!;
const eventList = document.querySelector<HTMLParagraphElement>('#interaction-events')!;
const feedButton = document.querySelector<HTMLButtonElement>('#action-feed')!;
const boatButton = document.querySelector<HTMLButtonElement>('#action-boat')!;
const cancelButton = document.querySelector<HTMLButtonElement>('#action-cancel')!;
const engine = new Engine(canvas, true);
const scene = new Scene(engine);
scene.clearColor = new Color4(.03, .15, .23, 1);
const camera = new FreeCamera('fixed-side-camera', new Vector3(0, 0, -10), scene);
camera.setTarget(Vector3.Zero());
camera.mode = Camera.ORTHOGRAPHIC_CAMERA;
camera.orthoLeft = -8;
camera.orthoRight = 8;
camera.orthoTop = 4.5;
camera.orthoBottom = -4.5;
scene.activeCamera = camera;
new HemisphericLight('world-light', new Vector3(0, 1, 1), scene).intensity = 1.2;
const water = MeshBuilder.CreatePlane('water', { width: 16, height: 9 }, scene);
water.position.z = 1;
const waterMaterial = new StandardMaterial('water-blue', scene);
waterMaterial.diffuseColor = new Color3(.03, .32, .48);
waterMaterial.emissiveColor = new Color3(.03, .32, .48);
waterMaterial.disableLighting = true;
waterMaterial.backFaceCulling = false;
water.material = waterMaterial;
const edge = MeshBuilder.CreateLines('water-interaction-edge', { points: [
  new Vector3(-7.5, -4, .8), new Vector3(7.5, -4, .8),
  new Vector3(7.5, 4, .8), new Vector3(-7.5, 4, .8), new Vector3(-7.5, -4, .8),
] }, scene);
edge.color = new Color3(.28, .72, .78);
const eventMeshes = new Map<string, AbstractMesh>();
const probe: Probe = { connected: false, sceneEpoch: 0, revision: 0, events: [],
  sent: [], acknowledgments: [], rejections: [], webglVersion: engine.webGLVersion, frames: 0,
  pointerDowns: 0, pointerUps: 0, pointerCancels: 0 };
window.interactionProbe = probe;
engine.runRenderLoop(() => { scene.render(); probe.frames++; });
window.addEventListener('resize', () => engine.resize());
new ResizeObserver(() => engine.resize()).observe(canvas);
requestAnimationFrame(() => engine.resize());

let selected: Action | undefined;
let socket: WebSocket | undefined;
let pointerStart: { id: number; x: number; y: number; time: number } | undefined;
const interactionPlane = Plane.FromPositionAndNormal(Vector3.Zero(), new Vector3(0, 0, 1));

function updateControls(): void {
  feedButton.disabled = boatButton.disabled = !probe.connected;
  cancelButton.disabled = !selected;
  feedButton.setAttribute('aria-pressed', String(selected === 'feed'));
  boatButton.setAttribute('aria-pressed', String(selected === 'boat'));
}
function addMarker(event: WorldEvent): void {
  if (eventMeshes.has(event.id)) return;
  const marker = event.action === 'feed'
    ? MeshBuilder.CreateSphere(event.id, { diameter: .45 }, scene)
    : MeshBuilder.CreateBox(event.id, { width: 1.1, height: .4, depth: .25 }, scene);
  marker.position = new Vector3(event.x, event.y, 0);
  const material = new StandardMaterial(`material-${event.id}`, scene);
  material.diffuseColor = event.action === 'feed' ? new Color3(1, .78, .08) : new Color3(.92, .34, .24);
  marker.material = material;
  eventMeshes.set(event.id, marker);
}
function showEvents(): void {
  eventList.textContent = probe.events.map(event => `${event.id}: ${event.action} (${event.x.toFixed(2)}, ${event.y.toFixed(2)})`).join(' · ');
}
function worldPoint(event: PointerEvent): { x: number; y: number } | undefined {
  const rect = canvas.getBoundingClientRect();
  const px = event.clientX - rect.left - canvas.clientLeft;
  const py = event.clientY - rect.top - canvas.clientTop;
  probe.sizing = [canvas.width, canvas.height, canvas.clientWidth, canvas.clientHeight,
    engine.getRenderWidth(), engine.getRenderHeight(), px, py];
  if (px < 0 || py < 0 || px > canvas.clientWidth || py > canvas.clientHeight) return;
  const ray = scene.createPickingRay(px, py, Matrix.Identity(), camera, false);
  const distance = ray.intersectsPlane(interactionPlane);
  if (distance === null) return;
  const point = ray.origin.add(ray.direction.scale(distance));
  if (!Number.isFinite(point.x) || !Number.isFinite(point.y)) return;
  return { x: point.x, y: point.y };
}

function connect(): void {
  const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
  socket = new WebSocket(`${protocol}//${location.host}/ws`);
  window.interactionSocket = socket;
  socket.addEventListener('message', messageEvent => {
    let message: any;
    try { message = JSON.parse(messageEvent.data); }
    catch { probe.error = 'INVALID_SERVER_MESSAGE'; return; }
    if (message.kind === 'snapshot') {
      probe.sceneEpoch = message.sceneEpoch;
      probe.revision = message.revision;
      probe.events = message.events;
      for (const mesh of eventMeshes.values()) { mesh.material?.dispose(); mesh.dispose(); }
      eventMeshes.clear();
      probe.events.forEach(addMarker);
      probe.connected = true;
      status.textContent = 'Подключено. Выбери действие и точку внутри рамки.';
      showEvents(); updateControls();
    } else if (message.kind === 'event') {
      if (message.sceneEpoch !== probe.sceneEpoch || message.revision !== probe.revision + 1) {
        probe.error = 'REVISION_GAP';
        socket?.close(); return;
      }
      probe.revision = message.revision;
      probe.events.push(message.event);
      addMarker(message.event);
      showEvents();
      status.textContent = `${message.event.action === 'feed' ? 'Корм' : 'Лодка'}: событие подтверждено сервером.`;
    } else if (message.kind === 'ack') {
      probe.acknowledgments.push(message.commandId);
    } else if (message.kind === 'rejected') {
      probe.rejections.push(message.reason);
      status.textContent = `Сервер отклонил действие: ${message.reason}`;
    }
  });
  socket.addEventListener('close', () => {
    probe.connected = false;
    selected = undefined;
    updateControls();
    status.textContent = 'Соединение потеряно. Подключаемся…';
    setTimeout(connect, 1000);
  });
  socket.addEventListener('error', () => { probe.error = 'SOCKET_ERROR'; });
}

function choose(action: Action): void {
  if (!probe.connected) return;
  selected = action;
  status.textContent = `Укажи точку для ${action === 'feed' ? 'корма' : 'лодки'} внутри рамки.`;
  updateControls();
}
feedButton.addEventListener('click', () => choose('feed'));
boatButton.addEventListener('click', () => choose('boat'));
cancelButton.addEventListener('click', () => { selected = undefined; updateControls(); status.textContent = 'Выбор отменён.'; });
window.addEventListener('keydown', event => {
  if (event.key === 'Escape' && selected) { selected = undefined; updateControls(); status.textContent = 'Выбор отменён.'; }
});
canvas.addEventListener('pointerdown', event => {
  if (!selected || !probe.connected || (event.pointerType === 'mouse' && event.button !== 0)) return;
  if (pointerStart) {
    if (pointerStart.id !== event.pointerId) pointerStart = undefined;
    return;
  }
  probe.pointerDowns++;
  pointerStart = { id: event.pointerId, x: event.clientX, y: event.clientY, time: performance.now() };
  canvas.setPointerCapture(event.pointerId);
});
canvas.addEventListener('pointercancel', () => { probe.pointerCancels++; pointerStart = undefined; });
canvas.addEventListener('pointerup', event => {
  probe.pointerUps++;
  probe.lastPointerIds = [pointerStart?.id ?? -1, event.pointerId];
  if (!pointerStart || pointerStart.id !== event.pointerId || !selected || !probe.connected) return;
  const start = pointerStart;
  pointerStart = undefined;
  probe.lastDelta = Math.hypot(event.clientX - start.x, event.clientY - start.y);
  probe.lastGestureMs = performance.now() - start.time;
  if (probe.lastDelta > 8 || probe.lastGestureMs > 600) return;
  const point = worldPoint(event);
  probe.lastPick = point ?? null;
  if (!point) return;
  if (Math.abs(point.x) > 7.5 || Math.abs(point.y) > 4) {
    status.textContent = 'Выберите место внутри воды.';
    return;
  }
  const action = selected;
  selected = undefined;
  updateControls();
  const commandId = crypto.randomUUID();
  const intent = { kind: 'intent', commandId, sceneEpoch: probe.sceneEpoch,
    action, x: point.x, y: point.y };
  probe.sent.push({ commandId, action, x: point.x, y: point.y });
  socket?.send(JSON.stringify(intent));
  status.textContent = 'Ждём решение сервера…';
});

updateControls();
connect();
