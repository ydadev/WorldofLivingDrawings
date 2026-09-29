import type { Point2, RealtimeAck, RealtimeDelta, RealtimeSnapshot, WorldDefinition } from '@ldw/contracts';
import type { RendererAdapter, RendererFactory } from '@ldw/renderer';
import { SceneConnection, type ConnectionState } from '@ldw/transport';

type Action = 'feed' | 'boat';

export interface WorldUiElements {
  canvas: HTMLCanvasElement;
  stage: HTMLElement;
  feed: HTMLButtonElement;
  boat: HTMLButtonElement;
  cancel: HTMLButtonElement;
  status: HTMLElement;
  crosshair: HTMLElement;
  toggle?: HTMLButtonElement;
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
  EXPIRED_COMMAND: 'Команда устарела. Выберите точку ещё раз.',
};

export class WorldInteractionUi {
  private readonly connection: SceneConnection;
  private renderer: RendererAdapter | null = null;
  private selected: Action | null = null;
  private pending: { id: string; action: Action; accepted: boolean; absentInSnapshot: boolean } | null = null;
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
    elements.cancel.removeEventListener('click', this.onCancel);
    elements.canvas.removeEventListener('pointerdown', this.onPointerDown);
    elements.canvas.removeEventListener('pointerup', this.onPointerUp);
    elements.canvas.removeEventListener('pointercancel', this.onPointerCancel);
    elements.canvas.removeEventListener('keydown', this.onKeyDown);
    elements.toggle?.removeEventListener('click', this.onToggle);
    document.removeEventListener('visibilitychange', this.onVisibility);
  }

  private readonly onFeed = (): void => this.choose('feed');
  private readonly onBoat = (): void => this.choose('boat');
  private readonly onCancel = (): void => this.cancelSelection();
  private readonly onToggle = (): void => { if (this.opened) this.close(); else this.open(); };
  private readonly onVisibility = (): void => {
    if (this.options.onDemand && document.visibilityState === 'hidden') this.close();
  };

  private choose(action: Action): void {
    if (!this.canInteract()) return;
    this.selected = action;
    this.pointer = null;
    this.keyboardPoint = { x: 0, y: 0 };
    this.showCrosshair();
    this.setStatus(`Укажите место для ${action === 'feed' ? 'корма' : 'лодки'} в воде. Стрелки и Enter тоже работают.`);
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
      const bounds = this.options.world.zones.find(item => item.id === 'water')?.bounds;
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
    const zone = this.options.world.zones.find(item => item.id === 'water');
    if (!zone || point.x < zone.bounds[0] || point.x > zone.bounds[1] ||
        point.y < zone.bounds[2] || point.y > zone.bounds[3]) {
      this.setStatus('Выберите точку внутри воды.');
      return;
    }
    const action = this.selected;
    this.selected = null;
    this.options.elements.crosshair.hidden = true;
    const id = this.connection.sendInteraction(action, point);
    if (!id) {
      this.setStatus('Связь прервалась. Дождитесь подключения.');
    } else {
      this.pending = { id, action, accepted: false, absentInSnapshot: false };
      this.setStatus('Отправлено. Ждём решение сервера…');
    }
    this.updateControls();
  }

  private onState(state: ConnectionState): void {
    this.state = state;
    if (state === 'offline') this.setStatus('Связь прервалась. Подключаемся снова…');
    if (state === 'connecting' || state === 'syncing') this.setStatus('Подключаемся к миру…');
    if (state === 'ready' && !this.pending) this.setStatus(this.options.interactive
      ? 'Выберите действие и место в воде.' : 'Режим просмотра. Действия недоступны.');
    this.updateControls();
  }

  private onSnapshot(snapshot: RealtimeSnapshot): void {
    this.renderer?.applySnapshot(snapshot);
    if (this.pending) {
      const id = this.pending.id.replace(/-/g, '');
      if (snapshot.activeActions.some(action => action.id.endsWith(id))) {
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

  private onDelta(delta: RealtimeDelta): void {
    this.renderer?.applyDelta(delta);
    if (delta.event.type !== 'interaction_state') return;
    if (this.pending && delta.event.appliedCommandIds.includes(this.pending.id)) {
      this.setStatus(this.pending.action === 'feed' ? 'Корм появился в мире.' : 'Лодка появилась в мире.');
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
        this.setStatus('Принято сервером. Ждём появления события…');
      }
    }
    this.updateControls();
  }

  private canInteract(): boolean {
    return this.opened && this.state === 'ready' && this.options.interactive && !this.pending;
  }

  private updateControls(): void {
    const { feed, boat, cancel, crosshair } = this.options.elements;
    feed.disabled = boat.disabled = !this.canInteract();
    cancel.disabled = !this.selected;
    feed.setAttribute('aria-pressed', String(this.selected === 'feed'));
    boat.setAttribute('aria-pressed', String(this.selected === 'boat'));
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
