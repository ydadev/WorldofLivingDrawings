import type { ActionCatalogEntry, AnyActiveAction, AnyRealtimeDelta, AnyRealtimeSnapshot, InteractionEffect, Point2, RealtimeAck, WorldDefinition } from '@ldw/contracts';
import type { RendererAdapter, RendererFactory } from '@ldw/renderer';
import { SceneConnection, type ConnectionState } from '@ldw/transport';

type CancelAction = 'cancel_feed' | 'cancel_boat';
type Selection = string;

const BASE_ACTIONS: ActionCatalogEntry[] = [
  { id: 'feed', effect: 'attraction', label: 'Корм', allowedZoneId: 'water' },
  { id: 'boat', effect: 'threat', label: 'Подводная лодка', allowedZoneId: 'water' },
];

export interface WorldUiElements {
  canvas: HTMLCanvasElement;
  stage: HTMLElement;
  feed: HTMLButtonElement;
  boat: HTMLButtonElement;
  cancel: HTMLButtonElement;
  status: HTMLElement;
  crosshair: HTMLElement;
  toggle?: HTMLButtonElement;
  placeFish?: HTMLButtonElement;
  activeActions?: HTMLElement;
  actionChoices?: HTMLElement;
}

export interface WorldUiOptions {
  elements: WorldUiElements;
  world: WorldDefinition;
  sessionId: string;
  csrf: string;
  origin: string;
  interactive: boolean;
  /** Controller opens the view only when requested and releases its GPU resources on close. */
  onDemand?: boolean;
  rendererFactory: RendererFactory;
  onPlaceFish?: (point: Point2) => void;
  canCancelActions?: boolean;
  createSocket?: (url: string) => WebSocket;
}

const REASONS: Record<string, string> = {
  READ_ONLY: 'На этом экране доступен только просмотр.',
  OUTSIDE_WATER: 'Выберите точку внутри воды.',
  INVALID_BOAT_ROUTE: 'Здесь лодка не сможет пройти. Выберите другой уровень воды.',
  BOAT_LIMIT: 'Одна лодка уже плывёт. Дождитесь её выхода.',
  BOAT_SCENE_COOLDOWN: 'Подождите перед запуском следующей лодки.',
  FEED_LIMIT: 'В сцене уже три источника корма.',
  FEED_COOLDOWN: 'Подождите перед следующим кормлением.',
  FEED_SCENE_COOLDOWN: 'Подождите перед следующим кормлением.',
  STALE_SCENE: 'Мир изменился. Дождитесь обновления сцены.',
  SCENE_NOT_RUNNING: 'Сцена сейчас остановлена.',
  SIMULATED_SESSION_LIMIT: 'Уже работают три мира. Повторите, когда один из них остановится.',
  EXPIRED_COMMAND: 'Команда устарела. Выберите точку ещё раз.',
  OWNER_REQUIRED: 'Отменять активные события может только владелец мира.',
  ACTION_NOT_ACTIVE: 'Это событие уже завершилось. Сцена обновлена.',
  INVALID_ACTION_TARGET: 'Не удалось определить событие для отмены.',
};

export class WorldInteractionUi {
  private readonly connection: SceneConnection;
  private renderer: RendererAdapter | null = null;
  private selected: Selection | null = null;
  private pending: { id: string; action: string; targetActionId?: string;
    accepted: boolean; absentInSnapshot: boolean } | null = null;
  private activeActions: AnyActiveAction[] = [];
  private catalog = new Map(BASE_ACTIONS.map(action => [action.id, action]));
  private actionButtons = new Map<string, HTMLButtonElement>();
  private extraActionButtons: HTMLButtonElement[] = [];
  private pointer: { id: number; x: number; y: number; at: number } | null = null;
  private keyboardPoint: Point2 = { x: 0, y: 0 };
  private state: ConnectionState = 'offline';
  private opened = false;
  private disposed = false;

  constructor(private readonly options: WorldUiOptions) {
    const { elements } = options;
    if (options.world.view.kind !== 'orthographic-side' || !options.world.view.fixed)
      throw new Error('UNSUPPORTED_WORLD_VIEW');
    elements.canvas.tabIndex = 0;
    elements.feed.addEventListener('click', this.onFeed);
    elements.boat.addEventListener('click', this.onBoat);
    elements.cancel.addEventListener('click', this.onCancel);
    elements.placeFish?.addEventListener('click', this.onPlaceFish);
    elements.canvas.addEventListener('pointerdown', this.onPointerDown);
    elements.canvas.addEventListener('pointerup', this.onPointerUp);
    elements.canvas.addEventListener('pointercancel', this.onPointerCancel);
    elements.canvas.addEventListener('keydown', this.onKeyDown);
    elements.toggle?.addEventListener('click', this.onToggle);
    document.addEventListener('visibilitychange', this.onVisibility);
    this.connection = new SceneConnection({
      sessionId: options.sessionId, csrf: options.csrf, origin: options.origin,
      createSocket: options.createSocket,
      onState: state => this.onState(state),
      onSnapshot: snapshot => this.onSnapshot(snapshot),
      onDelta: delta => this.onDelta(delta),
      onPositions: frame => this.renderer?.applyPositions(frame),
      onCommandResult: (id, result) => this.onCommandResult(id, result),
    });
    this.renderActionChoices();
    if (options.onDemand) {
      elements.stage.hidden = true;
      elements.toggle?.setAttribute('aria-expanded', 'false');
      this.setStatus('Откройте вид мира, чтобы выбрать действие.');
    } else {
      this.open();
    }
    this.updateControls();
  }

  open(): void {
    if (this.disposed || this.opened) return;
    this.opened = true;
    const { elements, rendererFactory, world } = this.options;
    elements.stage.hidden = false;
    elements.toggle?.setAttribute('aria-expanded', 'true');
    try {
      this.renderer = rendererFactory(elements.canvas);
      this.renderer.setWorld(world);
    } catch {
      this.renderer?.dispose();
      this.renderer = null;
      this.opened = false;
      elements.stage.hidden = true;
      elements.toggle?.setAttribute('aria-expanded', 'false');
      this.setStatus('Этот браузер не смог открыть WebGL-сцену.');
      this.updateControls();
      return;
    }
    this.connection.start();
    this.setStatus('Подключаемся к миру…');
    this.updateControls();
  }

  close(): void {
    if (!this.opened) return;
    this.opened = false;
    this.cancelSelection();
    this.connection.stop();
    this.renderer?.dispose();
    this.renderer = null;
    this.options.elements.stage.hidden = true;
    this.options.elements.toggle?.setAttribute('aria-expanded', 'false');
    this.setStatus('Вид мира закрыт.');
    this.updateControls();
  }

  dispose(): void {
    if (this.disposed) return;
    this.close();
    this.disposed = true;
    const { elements } = this.options;
    elements.feed.removeEventListener('click', this.onFeed);
    elements.boat.removeEventListener('click', this.onBoat);
    for (const button of this.extraActionButtons) button.remove();
    this.extraActionButtons = [];
    elements.cancel.removeEventListener('click', this.onCancel);
    elements.placeFish?.removeEventListener('click', this.onPlaceFish);
    elements.canvas.removeEventListener('pointerdown', this.onPointerDown);
    elements.canvas.removeEventListener('pointerup', this.onPointerUp);
    elements.canvas.removeEventListener('pointercancel', this.onPointerCancel);
    elements.canvas.removeEventListener('keydown', this.onKeyDown);
    elements.toggle?.removeEventListener('click', this.onToggle);
    document.removeEventListener('visibilitychange', this.onVisibility);
  }

  private readonly onFeed = (): void => this.choose(this.options.elements.feed.dataset.interactionId ?? 'feed');
  private readonly onBoat = (): void => this.choose(this.options.elements.boat.dataset.interactionId ?? 'boat');
  private readonly onPlaceFish = (): void => this.choose('fish');
  private readonly onCancel = (): void => this.cancelSelection();
  private readonly onToggle = (): void => { if (this.opened) this.close(); else this.open(); };
  private readonly onVisibility = (): void => {
    if (this.options.onDemand && document.visibilityState === 'hidden') this.close();
  };

  private choose(action: Selection): void {
    if (!this.canInteract()) return;
    if (action === 'fish' && !this.options.onPlaceFish) return;
    if (action !== 'fish' && !this.catalog.has(action)) return;
    this.selected = action;
    this.pointer = null;
    this.keyboardPoint = { x: 0, y: 0 };
    this.showCrosshair();
    const label = action === 'fish' ? 'рыбки' : `«${this.catalog.get(action)?.label}»`;
    this.setStatus(`Укажите место для ${label} в воде. Стрелки и Enter тоже работают.`);
    this.updateControls();
  }

  private cancelSelection(): void {
    if (!this.selected) return;
    this.selected = null;
    this.pointer = null;
    this.options.elements.crosshair.hidden = true;
    this.setStatus('Выбор действия отменён.');
    this.updateControls();
  }

  private readonly onPointerDown = (event: PointerEvent): void => {
    if (!this.selected || !this.canInteract() || (event.pointerType === 'mouse' && event.button !== 0)) return;
    if (this.pointer) { this.pointer = null; return; }
    this.pointer = { id: event.pointerId, x: event.clientX, y: event.clientY, at: performance.now() };
    this.options.elements.canvas.setPointerCapture(event.pointerId);
  };
  private readonly onPointerCancel = (): void => { this.pointer = null; };
  private readonly onPointerUp = (event: PointerEvent): void => {
    const start = this.pointer;
    this.pointer = null;
    if (!start || start.id !== event.pointerId || !this.selected || !this.canInteract()) return;
    if (Math.hypot(event.clientX - start.x, event.clientY - start.y) > 8 ||
        performance.now() - start.at > 600) return;
    const point = this.renderer?.pickWorldPoint(event.clientX, event.clientY);
    if (point) this.send(point);
  };
  private readonly onKeyDown = (event: KeyboardEvent): void => {
    if (event.key === 'Escape') { this.cancelSelection(); return; }
    if (!this.selected || !this.canInteract()) return;
    const step = event.shiftKey ? .5 : .25;
    if (event.key.startsWith('Arrow')) {
      event.preventDefault();
      if (event.key === 'ArrowLeft') this.keyboardPoint.x -= step;
      if (event.key === 'ArrowRight') this.keyboardPoint.x += step;
      if (event.key === 'ArrowUp') this.keyboardPoint.y += step;
      if (event.key === 'ArrowDown') this.keyboardPoint.y -= step;
      const zoneId = this.selected === 'fish' ? 'water' :
        this.catalog.get(this.selected)?.allowedZoneId;
      const bounds = this.options.world.zones.find(item => item.id === zoneId)?.bounds;
      if (bounds) {
        this.keyboardPoint.x = Math.max(bounds[0], Math.min(bounds[1], this.keyboardPoint.x));
        this.keyboardPoint.y = Math.max(bounds[2], Math.min(bounds[3], this.keyboardPoint.y));
      }
      this.showCrosshair();
    } else if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      this.send(this.keyboardPoint);
    }
  };

  private send(point: Point2): void {
    if (!this.selected || !this.canInteract()) return;
    const zoneId = this.selected === 'fish' ? 'water' :
      this.catalog.get(this.selected)?.allowedZoneId;
    const zone = this.options.world.zones.find(item => item.id === zoneId);
    if (!zone || point.x < zone.bounds[0] || point.x > zone.bounds[1] ||
        point.y < zone.bounds[2] || point.y > zone.bounds[3]) {
      this.setStatus('Выберите точку внутри воды.');
      return;
    }
    const action = this.selected;
    this.selected = null;
    this.options.elements.crosshair.hidden = true;
    if (action === 'fish') {
      this.options.onPlaceFish?.(point);
      this.updateControls();
      return;
    }
    const id = this.connection.sendInteraction(action, point);
    if (!id) {
      this.setStatus('Связь прервалась. Дождитесь подключения.');
    } else {
      this.pending = { id, action, accepted: false, absentInSnapshot: false };
      this.setStatus('Отправлено. Ждём решение сервера…');
    }
    this.updateControls();
  }

  private actionEffect(action: AnyActiveAction): InteractionEffect {
    return 'effect' in action ? action.effect :
      (action.interactionId === 'feed' ? 'attraction' : 'threat');
  }

  private cancelActiveAction(action: AnyActiveAction): void {
    if (!this.options.canCancelActions || !this.canInteract()) return;
    const kind: CancelAction = this.actionEffect(action) === 'attraction' ? 'cancel_feed' : 'cancel_boat';
    const id = this.connection.sendInteraction(kind, action.point, action.id);
    if (!id) {
      this.setStatus('Связь прервалась. Дождитесь подключения.');
      return;
    }
    this.pending = { id, action: kind, targetActionId: action.id,
      accepted: false, absentInSnapshot: false };
    this.setStatus('Отмена отправлена. Ждём решение сервера…');
    this.updateControls();
  }

  private renderActiveActions(): void {
    const container = this.options.elements.activeActions;
    if (!container || !this.options.canCancelActions) return;
    container.replaceChildren();
    if (!this.activeActions.length) {
      container.textContent = 'Активных событий нет.';
      return;
    }
    for (const action of this.activeActions) {
      const row = document.createElement('div');
      row.className = 'active-action';
      const label = document.createElement('span');
      const effect = this.actionEffect(action);
      const actionLabel = this.catalog.get(action.interactionId)?.label ?? action.interactionId;
      label.textContent = effect === 'attraction' && 'remaining' in action
        ? `${actionLabel}: осталось ${action.remaining} порций` : `${actionLabel} плывёт`;
      const button = document.createElement('button');
      button.type = 'button';
      button.dataset.actionId = action.id;
      button.textContent = effect === 'attraction' ? 'Убрать корм' : 'Убрать лодку';
      button.disabled = !this.canInteract();
      button.addEventListener('click', () => this.cancelActiveAction(action));
      row.append(label, button);
      container.append(row);
    }
  }

  private onState(state: ConnectionState): void {
    this.state = state;
    if (state === 'offline') this.setStatus('Связь прервалась. Подключаемся снова…');
    if (state === 'connecting' || state === 'syncing') this.setStatus('Подключаемся к миру…');
    if (state === 'ready' && !this.pending) this.setStatus(this.options.interactive
      ? 'Выберите действие и место в воде.' : 'Режим просмотра. Действия недоступны.');
    this.updateControls();
  }

  private onSnapshot(snapshot: AnyRealtimeSnapshot): void {
    this.catalog = new Map((snapshot.schemaVersion === 2 ? snapshot.actionCatalog : BASE_ACTIONS)
      .map(action => [action.id, action]));
    if (this.selected && this.selected !== 'fish' && !this.catalog.has(this.selected))
      this.cancelSelection();
    this.renderActionChoices();
    this.renderer?.applySnapshot(snapshot);
    this.activeActions = snapshot.activeActions;
    this.renderActiveActions();
    if (this.pending) {
      const id = this.pending.id.replace(/-/g, '');
      if (this.pending.targetActionId &&
          !snapshot.activeActions.some(action => action.id === this.pending?.targetActionId)) {
        this.pending = null;
        this.setStatus('Событие отменено.');
      } else if (!this.pending.targetActionId &&
          snapshot.activeActions.some(action => action.id.endsWith(id))) {
        this.pending = null;
        this.setStatus('Действие началось.');
      } else if (snapshot.pendingInteractions.some(item => item.commandId === this.pending?.id)) {
        this.pending.absentInSnapshot = false;
        this.setStatus('Команда принята; событие ещё готовится.');
      } else {
        this.pending.absentInSnapshot = true;
      }
    }
  }

  private onDelta(delta: AnyRealtimeDelta): void {
    this.renderer?.applyDelta(delta);
    if (delta.event.type !== 'interaction_state') return;
    this.activeActions = delta.event.activeActions;
    this.renderActiveActions();
    if (this.pending && delta.event.appliedCommandIds.includes(this.pending.id)) {
      const effect = this.catalog.get(this.pending.action)?.effect;
      this.setStatus(this.pending.targetActionId ? 'Событие отменено.' :
        effect === 'attraction' ? 'Корм появился в мире.' : 'Лодка появилась в мире.');
      this.pending = null;
      this.updateControls();
    }
  }

  private onCommandResult(id: string, result: RealtimeAck | null): void {
    if (this.pending?.id !== id) return;
    if (!result) {
      this.pending = null;
      this.setStatus('Подтверждение не получено. Обновите сцену перед повтором.');
    } else if (!result.accepted) {
      this.pending = null;
      this.setStatus(REASONS[result.code] ?? `Сервер отклонил действие: ${result.code}`);
    } else {
      if (this.pending.absentInSnapshot) {
        this.pending = null;
        this.setStatus('Действие подтверждено. Сцена синхронизирована.');
      } else {
        this.pending.accepted = true;
        this.setStatus(this.pending.targetActionId
          ? 'Отмена принята сервером. Ждём обновления сцены…'
          : 'Принято сервером. Ждём появления события…');
      }
    }
    this.updateControls();
  }

  private canInteract(): boolean {
    return this.opened && this.state === 'ready' && this.options.interactive && !this.pending;
  }

  private renderActionChoices(): void {
    for (const button of this.extraActionButtons) button.remove();
    this.extraActionButtons = [];
    this.actionButtons.clear();
    const actions = [...this.catalog.values()];
    const attraction = actions.find(action => action.effect === 'attraction');
    const threat = actions.find(action => action.effect === 'threat');
    const { feed, boat, cancel } = this.options.elements;
    for (const [button, entry] of [[feed, attraction], [boat, threat]] as const) {
      button.dataset.interactionId = entry?.id ?? '';
      button.textContent = entry?.label ?? '';
      if (entry) this.actionButtons.set(entry.id, button);
    }
    const panel = this.options.elements.actionChoices ?? feed.parentElement;
    for (const action of actions) {
      if (action.id === attraction?.id || action.id === threat?.id || !panel) continue;
      const button = document.createElement('button');
      button.type = 'button';
      button.dataset.interactionId = action.id;
      button.textContent = action.label;
      button.addEventListener('click', () => this.choose(action.id));
      if (cancel.parentElement === panel) panel.insertBefore(button, cancel);
      else panel.append(button);
      this.extraActionButtons.push(button);
      this.actionButtons.set(action.id, button);
    }
    this.updateControls();
  }

  private updateControls(): void {
    const { feed, boat, cancel, crosshair, placeFish } = this.options.elements;
    for (const button of this.actionButtons.values()) {
      button.disabled = !this.canInteract();
      button.setAttribute('aria-pressed', String(this.selected === button.dataset.interactionId));
    }
    if (placeFish) {
      placeFish.disabled = !this.canInteract();
      placeFish.setAttribute('aria-pressed', String(this.selected === 'fish'));
    }
    cancel.disabled = !this.selected;
    this.options.elements.activeActions?.querySelectorAll('button').forEach(button => {
      button.disabled = !this.canInteract();
    });
    feed.hidden = !feed.dataset.interactionId;
    boat.hidden = !boat.dataset.interactionId;
    if (!this.selected) crosshair.hidden = true;
  }

  private showCrosshair(): void {
    const { crosshair, canvas } = this.options.elements;
    const { width, height } = this.options.world.view;
    crosshair.style.left = `${(this.keyboardPoint.x / width + .5) * 100}%`;
    crosshair.style.top = `${(.5 - this.keyboardPoint.y / height) * 100}%`;
    crosshair.hidden = false;
    canvas.focus();
  }

  private setStatus(message: string): void { this.options.elements.status.textContent = message; }
}
