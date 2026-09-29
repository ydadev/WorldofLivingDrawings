import { BabylonRendererAdapter } from '@ldw/renderer-babylon';
import { WorldInteractionUi } from '@ldw/world-ui';
import type { WorldDefinition } from '@ldw/contracts';
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
    elements: {
      canvas: document.querySelector<HTMLCanvasElement>('#world-canvas')!,
      stage: document.querySelector<HTMLElement>('#world-stage')!,
      crosshair: document.querySelector<HTMLElement>('#world-crosshair')!,
      status: document.querySelector<HTMLElement>('#interaction-status')!,
      feed: document.querySelector<HTMLButtonElement>('#action-feed')!,
      boat: document.querySelector<HTMLButtonElement>('#action-boat')!,
      cancel: document.querySelector<HTMLButtonElement>('#action-cancel')!,
      toggle: document.querySelector<HTMLButtonElement>('#view-toggle') ?? undefined,
    },
    rendererFactory: canvas => {
      const renderer = new BabylonRendererAdapter(canvas, '/fish/',
        id => `/api/sessions/${encodeURIComponent(sessionId)}/paint/${encodeURIComponent(id)}`);
      if (controller) renderer.setRenderScale(.5);
      return renderer;
    },
  });
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
    const scene = await request<{ world_id: string; world_version: number }>(
      `/api/sessions/${encodeURIComponent(sessionId)}/scene`, 'GET');
    if (scene.world_id !== world.id || scene.world_version !== world.version)
      throw new Error('UNSUPPORTED_WORLD');
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
