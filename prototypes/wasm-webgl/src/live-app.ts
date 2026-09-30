import { BabylonRendererAdapter } from '@ldw/renderer-babylon';
import { WorldInteractionUi } from '@ldw/world-ui';
import type { Point2, WorldDefinition } from '@ldw/contracts';
import worldData from '../../../content/underwater/world.json';
import './style.css';

const world = worldData as WorldDefinition;
const role = document.body.dataset.role;
const form = document.querySelector<HTMLFormElement>('#access-form')!;
const accessStatus = document.querySelector<HTMLElement>('#access-status')!;
const shell = document.querySelector<HTMLElement>('#world-shell')!;
const sessionLabel = document.querySelector<HTMLElement>('#session-id')!;
let ui: WorldInteractionUi | null = null;
let csrf = '';
let sessionId = '';
let interactive = true;

interface PaintResult {
  templateId: string;
  templateVersion: number;
  layoutHash: string;
  sourceKind: 'browser' | 'paper';
  colorSpace: 'sRGB';
  image: Blob;
}
interface SceneInfo {
  scene_id: string;
  scene_epoch: number;
  world_id: string;
  world_version: number;
  server_time_ms: number;
}
interface Publication {
  paint: PaintResult;
  point: Point2;
  scene: SceneInfo;
  intentId?: string;
}
let readyPaint: PaintResult | null = null;
let preparedScene: SceneInfo | null = null;
let publication: Publication | null = null;
let publishing = false;
let editorKind: 'paint' | 'capture' | null = null;

const storedSession = new URL(location.href).searchParams.get('session');
if (storedSession) {
  const input = form.elements.namedItem('sessionId');
  if (input instanceof HTMLInputElement) input.value = storedSession;
}

async function request<T>(path: string, method: 'GET' | 'POST', body?: unknown, token?: string): Promise<T> {
  const response = await fetch(path, {
    method, credentials: 'same-origin',
    headers: { ...(body ? { 'Content-Type': 'application/json' } : {}),
      ...(token ? { 'x-csrf-token': token } : {}) },
    body: body ? JSON.stringify(body) : undefined,
  });
  const result = response.status === 204 ? {} : await response.json();
  if (!response.ok) throw new Error(typeof result?.error === 'string' ? result.error : `HTTP_${response.status}`);
  return result as T;
}

const sessionPath = (): string => `/api/sessions/${encodeURIComponent(sessionId)}`;
const publishStatus = (): HTMLElement => document.querySelector<HTMLElement>('#publish-status')!;
const editorShell = (): HTMLElement => document.querySelector<HTMLElement>('#editor-shell')!;
const editorFrame = (): HTMLIFrameElement => document.querySelector<HTMLIFrameElement>('#editor-frame')!;

async function sceneInfo(): Promise<SceneInfo> {
  const scene = await request<SceneInfo>(`${sessionPath()}/scene`, 'GET');
  if (scene.world_id !== world.id || scene.world_version !== world.version)
    throw new Error('UNSUPPORTED_WORLD');
  return scene;
}

function controlsForPublication(): void {
  if (role === 'viewer') return;
  document.querySelector<HTMLButtonElement>('#fish-place')!.hidden = !readyPaint ||
    !editorShell().hidden || !!publication || publishing;
  document.querySelector<HTMLButtonElement>('#publish-edit')!.hidden = !readyPaint ||
    !editorShell().hidden || !!publication || publishing;
  document.querySelector<HTMLButtonElement>('#publish-retry')!.hidden = !publication || publishing;
}

function setPreview(active: boolean): void {
  editorFrame().contentWindow?.postMessage({ type: 'ldw-preview', active }, location.origin);
}

function openEditor(kind: 'paint' | 'capture'): void {
  if (publication || publishing) {
    publishStatus().textContent = 'Сначала завершите или повторите сохранение текущей рыбки.';
    return;
  }
  readyPaint = null;
  preparedScene = null;
  ui?.close();
  const frame = editorFrame();
  if (editorKind !== kind) frame.src = kind === 'paint' ? '/paint.html' : '/capture.html';
  editorKind = kind;
  editorShell().hidden = false;
  setPreview(true);
  publishStatus().textContent = kind === 'paint'
    ? 'Раскрасьте рыбку и проверьте объёмный вид, затем нажмите «Оживить».':
      'Выберите фотографию листа и проверьте распознанную рыбку, затем нажмите «Оживить».';
  controlsForPublication();
}

function showWorld(): void {
  editorShell().hidden = true;
  setPreview(false);
  ui?.open();
  controlsForPublication();
}

async function preparePaint(): Promise<void> {
  if (publishing || publication || !editorKind) return;
  const button = document.querySelector<HTMLButtonElement>('#editor-pick')!;
  button.disabled = true;
  try {
    const editor = editorFrame().contentWindow;
    const result = editorKind === 'paint' ? await editor?.paintResult?.() : editor?.captureResult?.();
    if (!result) throw new Error('NO_PAINT_RESULT');
    if (!['coral', 'stream'].includes(result.templateId) ||
        !Number.isInteger(result.templateVersion) || !/^[a-f0-9]{64}$/.test(result.layoutHash) ||
        result.sourceKind !== (editorKind === 'paint' ? 'browser' : 'paper') ||
        result.colorSpace !== 'sRGB' || result.image.type !== 'image/png' ||
        !result.image.size || result.image.size > 2 * 1024 * 1024)
      throw new Error('INVALID_PAINT_RESULT');
    const scene = await sceneInfo();
    readyPaint = result;
    preparedScene = scene;
    showWorld();
    publishStatus().textContent = 'Рисунок готов. Нажмите «Выбрать место рыбки» и укажите точку в воде.';
  } catch (error) {
    const code = error instanceof Error ? error.message : 'UNKNOWN_ERROR';
    publishStatus().textContent = code === 'NO_PAINT_RESULT'
      ? 'Сначала закончите раскраску или обработку фотографии.' : `Не удалось подготовить рыбку: ${code}`;
  } finally {
    button.disabled = false;
  }
}

async function putPaint(intentId: string, paint: PaintResult): Promise<void> {
  const response = await fetch(`${sessionPath()}/upload-intents/${encodeURIComponent(intentId)}/paint`, {
    method: 'PUT', credentials: 'same-origin',
    headers: { 'Content-Type': 'image/png', 'x-csrf-token': csrf }, body: paint.image,
  });
  const result = await response.json();
  if (!response.ok) throw new Error(result?.error ?? `HTTP_${response.status}`);
}

async function finalize(intentId: string): Promise<{ fishId: string; paintBlobId: string }> {
  const scene = await sceneInfo();
  return request(`${sessionPath()}/upload-intents/${encodeURIComponent(intentId)}/finalize`,
    'POST', { expiresAt: scene.server_time_ms + 30_000 }, csrf);
}

async function savePublication(): Promise<void> {
  const attempt = publication;
  if (!attempt || publishing) return;
  publishing = true;
  controlsForPublication();
  publishStatus().textContent = 'Сохраняем рисунок и добавляем рыбку в мир…';
  try {
    if (!attempt.intentId) {
      const scene = await sceneInfo();
      if (scene.scene_id !== attempt.scene.scene_id || scene.scene_epoch !== attempt.scene.scene_epoch)
        throw new Error('STALE_SCENE');
      const created = await request<{ intentId: string }>(`${sessionPath()}/upload-intents`, 'POST', {
        sceneEpoch: scene.scene_epoch,
        definitionId: attempt.paint.templateId === 'coral' ? 'coral-fish' : 'stream-fish',
        templateId: attempt.paint.templateId,
        templateVersion: attempt.paint.templateVersion,
        layoutHash: attempt.paint.layoutHash,
        sourceKind: attempt.paint.sourceKind,
        colorSpace: attempt.paint.colorSpace,
        position: attempt.point,
      }, csrf);
      attempt.intentId = created.intentId;
    } else {
      // A lost response can mean that finalization already committed. Replaying
      // this intent returns its original fish instead of creating another.
      try {
        await finalize(attempt.intentId);
        completePublication();
        return;
      } catch (error) {
        if (!(error instanceof Error) || error.message !== 'UPLOAD_CONFLICT') throw error;
      }
    }
    await putPaint(attempt.intentId, attempt.paint);
    await finalize(attempt.intentId);
    completePublication();
  } catch (error) {
    const code = error instanceof Error ? error.message : 'UNKNOWN_ERROR';
    if (['STALE_SCENE', 'UPLOAD_INTENT_EXPIRED', 'SCENE_FULL', 'PAINT_TOO_LARGE',
      'INVALID_PAINT_IMAGE', 'INVALID_PAINT_RESULT', 'ACCESS_DENIED'].includes(code)) {
      publication = null;
      if (code === 'STALE_SCENE' || code === 'UPLOAD_INTENT_EXPIRED') {
        try { preparedScene = await sceneInfo(); } catch { preparedScene = null; }
      }
    }
    const message: Record<string, string> = {
      STALE_SCENE: 'Мир изменился. Рисунок сохранён на этом экране; выберите место заново.',
      UPLOAD_INTENT_EXPIRED: 'Время загрузки истекло. Рисунок остался здесь; выберите место заново.',
      SCENE_FULL: 'В этом мире уже 100 рыбок. Рисунок остался в редакторе.',
      SIMULATED_SESSION_LIMIT: 'Уже работают три мира. Рисунок остался в редакторе; повторите, когда освободится место.',
      UPLOAD_INTENT_LIMIT: 'Слишком много незавершённых загрузок. Подождите и повторите.',
      PAINT_TOO_LARGE: 'PNG больше 2 МиБ. Вернитесь к рисунку и сохраните меньший файл.',
      INVALID_PAINT_IMAGE: 'PNG не прошёл проверку. Рисунок остался в редакторе.',
      ACCESS_DENIED: 'Доступ истёк. Подключитесь снова; рисунок остаётся на этом экране.',
    };
    publishStatus().textContent = message[code] ?? `Не удалось сохранить рыбку: ${code}. Попробуйте ещё раз.`;
  } finally {
    publishing = false;
    controlsForPublication();
  }
}

function completePublication(): void {
  publishStatus().textContent = 'Рыбка сохранена. Она появится в мире после подтверждения сервером.';
  readyPaint = null;
  preparedScene = null;
  publication = null;
  editorKind = null;
  editorFrame().src = 'about:blank';
}

function placeFish(point: Point2): void {
  if (!readyPaint || publication || publishing) return;
  if (!preparedScene) {
    publishStatus().textContent = 'Обновляем мир. Нажмите «Выбрать место рыбки» ещё раз.';
    void sceneInfo().then(scene => { preparedScene = scene; }).catch(() => {
      publishStatus().textContent = 'Не удалось получить состояние мира. Попробуйте ещё раз.';
    });
    return;
  }
  publication = { paint: readyPaint, scene: preparedScene, point };
  controlsForPublication();
  void savePublication();
}

function setupPublication(): void {
  if (role === 'viewer') return;
  document.querySelector<HTMLButtonElement>('#paint-open')!.addEventListener('click', () => openEditor('paint'));
  document.querySelector<HTMLButtonElement>('#capture-open')!.addEventListener('click', () => openEditor('capture'));
  document.querySelector<HTMLButtonElement>('#editor-pick')!.addEventListener('click', () => void preparePaint());
  document.querySelector<HTMLButtonElement>('#editor-close')!.addEventListener('click', showWorld);
  document.querySelector<HTMLButtonElement>('#publish-edit')!.addEventListener('click', () => {
    if (editorKind) openEditor(editorKind);
  });
  document.querySelector<HTMLButtonElement>('#publish-retry')!.addEventListener('click', () => void savePublication());
  editorFrame().addEventListener('load', () => setPreview(!editorShell().hidden));
  controlsForPublication();
}

function mount(): void {
  ui?.dispose();
  shell.hidden = false;
  form.hidden = true;
  sessionLabel.textContent = sessionId;
  const controller = role === 'controller';
  document.querySelector<HTMLElement>('#action-panel')!.hidden = !interactive;
  ui = new WorldInteractionUi({
    world, sessionId, csrf, origin: location.origin, interactive,
    onDemand: controller,
    canCancelActions: role === 'owner',
    elements: {
      canvas: document.querySelector<HTMLCanvasElement>('#world-canvas')!,
      stage: document.querySelector<HTMLElement>('#world-stage')!,
      crosshair: document.querySelector<HTMLElement>('#world-crosshair')!,
      status: document.querySelector<HTMLElement>('#interaction-status')!,
      feed: document.querySelector<HTMLButtonElement>('#action-feed')!,
      boat: document.querySelector<HTMLButtonElement>('#action-boat')!,
      cancel: document.querySelector<HTMLButtonElement>('#action-cancel')!,
      toggle: document.querySelector<HTMLButtonElement>('#view-toggle') ?? undefined,
      placeFish: document.querySelector<HTMLButtonElement>('#fish-place') ?? undefined,
      activeActions: document.querySelector<HTMLElement>('#active-actions') ?? undefined,
      actionChoices: document.querySelector<HTMLElement>('#action-panel') ?? undefined,
    },
    onPlaceFish: role === 'viewer' ? undefined : placeFish,
    rendererFactory: canvas => {
      const renderer = new BabylonRendererAdapter(canvas, '/fish/',
        id => `/api/sessions/${encodeURIComponent(sessionId)}/paint/${encodeURIComponent(id)}`);
      if (controller) renderer.setRenderScale(.5);
      return renderer;
    },
  });
  setupPublication();
}

form.addEventListener('submit', async event => {
  event.preventDefault();
  const submit = form.querySelector<HTMLButtonElement>('button[type=submit]')!;
  submit.disabled = true;
  accessStatus.textContent = 'Подключаемся…';
  const values = new FormData(form);
  try {
    if (role === 'viewer') {
      sessionId = String(values.get('sessionId') ?? '').trim();
      const claim = await request<{ claim_id: string; code: string; csrf: string }>(
        `/api/sessions/${encodeURIComponent(sessionId)}/viewer-claims`, 'POST');
      accessStatus.textContent = `Код экрана: ${claim.code}. Попросите владельца разрешить подключение. Код действует 5 минут.`;
      let activated = false;
      for (let attempt = 0; attempt < 150; attempt++) {
        const result = await request<{ role?: string; csrf?: string; error?: string }>(
          `/api/sessions/${encodeURIComponent(sessionId)}/viewer-claims/${encodeURIComponent(claim.claim_id)}/activate`,
          'POST', undefined, claim.csrf);
        if (result.role && result.csrf) {
          csrf = result.csrf;
          interactive = result.role === 'viewer_interact';
          activated = true;
          break;
        }
        if (result.error !== 'VIEWER_PENDING') throw new Error(result.error ?? 'ACTIVATION_FAILED');
        await new Promise(resolve => setTimeout(resolve, 2000));
      }
      if (!activated) throw new Error('CLAIM_EXPIRED');
    } else if (role === 'controller') {
      sessionId = String(values.get('sessionId') ?? '').trim();
      const pin = String(values.get('pin') ?? '').trim();
      let clientKey: string | null = null;
      try { clientKey = localStorage.getItem('ldw-client-key'); } catch { /* private browsing */ }
      if (!clientKey) {
        clientKey = crypto.randomUUID();
        try { localStorage.setItem('ldw-client-key', clientKey); } catch { /* use this session */ }
      }
      const paired = await request<{ csrf: string }>(`/api/sessions/${encodeURIComponent(sessionId)}/pair`,
        'POST', { client_key: clientKey, pin });
      csrf = paired.csrf;
      interactive = true;
    } else {
      const login = String(values.get('login') ?? '').trim();
      const password = String(values.get('password') ?? '');
      const grant = await request<{ csrf: string }>('/api/login', 'POST', { login, password });
      csrf = grant.csrf;
      interactive = true;
      sessionId = String(values.get('sessionId') ?? '').trim();
      if (!sessionId) {
        const created = await request<{ session_id: string }>('/api/sessions', 'POST', undefined, csrf);
        sessionId = created.session_id;
      }
    }
    await sceneInfo();
    const passwordField = form.elements.namedItem('password');
    if (passwordField instanceof HTMLInputElement) passwordField.value = '';
    accessStatus.textContent = '';
    mount();
  } catch (error) {
    const code = error instanceof Error ? error.message : 'UNKNOWN_ERROR';
    accessStatus.textContent = ({ ACCESS_DENIED: 'Проверьте доступ или код подключения.',
      RATE_LIMITED: 'Слишком много попыток. Подождите минуту.',
      OWNER_APPROVAL_REQUIRED: 'Ожидается разрешение владельца.' } as Record<string, string>)[code]
      ?? `Не удалось подключиться: ${code}`;
  } finally {
    submit.disabled = false;
  }
});

document.querySelector<HTMLFormElement>('#approve-viewer-form')?.addEventListener('submit', async event => {
  event.preventDefault();
  const approval = document.querySelector<HTMLElement>('#viewer-approval-status')!;
  const values = new FormData(event.currentTarget as HTMLFormElement);
  try {
    await request(`/api/sessions/${encodeURIComponent(sessionId)}/viewer-claims/approve`,
      'POST', { code: String(values.get('code') ?? ''), interact: values.has('interact') }, csrf);
    approval.textContent = values.has('interact')
      ? 'Экрану разрешены просмотр и действия.' : 'Экрану разрешён просмотр.';
  } catch (error) {
    approval.textContent = `Не удалось подключить экран: ${error instanceof Error ? error.message : 'UNKNOWN_ERROR'}`;
  }
});

document.querySelector<HTMLButtonElement>('#invite')?.addEventListener('click', async () => {
  const invitation = document.querySelector<HTMLElement>('#invitation')!;
  try {
    const result = await request<{ pin: string; expires_in_seconds: number }>(
      `/api/sessions/${encodeURIComponent(sessionId)}/invitation`, 'POST', undefined, csrf);
    invitation.textContent = `ID сессии: ${sessionId}. Код подключения: ${result.pin}. Действует ${result.expires_in_seconds / 60} минут.`;
  } catch (error) {
    invitation.textContent = `Не удалось создать код: ${error instanceof Error ? error.message : 'UNKNOWN_ERROR'}`;
  }
});
