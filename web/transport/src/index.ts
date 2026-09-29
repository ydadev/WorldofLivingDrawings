import type { Point2, RealtimeAck, RealtimeCommand, RealtimeDelta, RealtimeSnapshot, ScenePositions } from '@ldw/contracts';

export type ConnectionState = 'offline' | 'connecting' | 'syncing' | 'ready';
export interface RealtimeCallbacks {
  onState(state: ConnectionState): void;
  onSnapshot(snapshot: RealtimeSnapshot): void;
  onDelta(delta: RealtimeDelta): void;
  onPositions(frame: ScenePositions): void;
  /** null means the original action was never confirmed and is now too old to retry. */
  onCommandResult(commandId: string, result: RealtimeAck | null): void;
}
export interface RealtimeOptions extends RealtimeCallbacks {
  sessionId: string;
  csrf: string;
  origin: string;
  createSocket?: (url: string) => WebSocket;
  now?: () => number;
  random?: () => number;
}

/** Cookie credentials stay in the browser cookie jar; no token appears in a URL or localStorage. */
export class SceneConnection {
  private socket: WebSocket | null = null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private snapshotTimer: ReturnType<typeof setTimeout> | null = null;
  private started = false;
  private retry = 0;
  private sceneId: string | null = null;
  private sceneEpoch = 0;
  private revision = 0;
  private simulationTick = 0;
  private serverOffsetMs = 0;
  private pending = new Map<string, RealtimeCommand>();
  private readonly createSocket: (url: string) => WebSocket;
  private readonly now: () => number;
  private readonly random: () => number;
  private readonly options: RealtimeOptions;
  private state: ConnectionState = 'offline';
  private readonly onOnline = (): void => { if (this.state === 'offline') this.reconnectNow(); };
  private readonly onVisible = (): void => {
    if (typeof document !== 'undefined' && document.visibilityState === 'visible' &&
        this.state === 'offline') this.reconnectNow();
  };

  constructor(options: RealtimeOptions) {
    this.options = options;
    this.createSocket = options.createSocket ?? (url => new WebSocket(url));
    this.now = options.now ?? Date.now;
    this.random = options.random ?? Math.random;
  }

  get connectionState(): ConnectionState { return this.state; }
  get currentRevision(): number { return this.revision; }

  start(): void {
    if (this.started) return;
    this.started = true;
    if (typeof window !== 'undefined') window.addEventListener('online', this.onOnline);
    if (typeof document !== 'undefined') document.addEventListener('visibilitychange', this.onVisible);
    this.open();
  }

  stop(): void {
    this.started = false;
    if (typeof window !== 'undefined') window.removeEventListener('online', this.onOnline);
    if (typeof document !== 'undefined') document.removeEventListener('visibilitychange', this.onVisible);
    this.clearReconnect();
    this.clearSnapshotTimeout();
    const socket = this.socket;
    this.socket = null;
    socket?.close();
    this.changeState('offline');
  }

  /** Browser online/visibility events may call this to retry without waiting for backoff. */
  reconnectNow(): void {
    if (!this.started) return;
    this.clearReconnect();
    this.clearSnapshotTimeout();
    const socket = this.socket;
    this.socket = null;
    socket?.close();
    this.open();
  }

  sendInteraction(interactionId: string, point: Point2): string | null {
    if (this.state !== 'ready' || !this.socket || !this.sceneId ||
        !Number.isFinite(point.x) || !Number.isFinite(point.y)) return null;
    const command: RealtimeCommand = {
      type: 'command', commandId: crypto.randomUUID(), sessionId: this.options.sessionId,
      sceneId: this.sceneId, sceneEpoch: this.sceneEpoch,
      interactionId, point, expiresAt: this.now() + this.serverOffsetMs + 10_000,
    };
    this.pending.set(command.commandId, command);
    try { this.socket.send(JSON.stringify(command)); }
    catch { this.reconnectNow(); }
    return command.commandId;
  }

  private open(): void {
    if (!this.started) return;
    const url = new URL(`/api/sessions/${encodeURIComponent(this.options.sessionId)}/ws`, this.options.origin);
    url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
    this.changeState('connecting');
    const socket = this.createSocket(url.toString());
    this.socket = socket;
    socket.onopen = () => {
      if (this.socket !== socket) return;
      this.changeState('syncing');
      socket.send(JSON.stringify({ type: 'hello', csrf: this.options.csrf }));
      this.snapshotTimer = setTimeout(() => this.reconnectNow(), 10_000);
    };
    socket.onmessage = event => {
      if (this.socket !== socket || typeof event.data !== 'string') return;
      let message: unknown;
      try { message = JSON.parse(event.data); } catch { this.reconnectNow(); return; }
      this.receive(message);
    };
    socket.onclose = () => {
      if (this.socket !== socket) return;
      this.socket = null;
      this.clearSnapshotTimeout();
      this.changeState('offline');
      this.scheduleReconnect();
    };
    socket.onerror = () => { if (this.socket === socket) socket.close(); };
  }

  private receive(message: unknown): void {
    if (!message || typeof message !== 'object') return;
    const value = message as Record<string, unknown>;
    if (value.type === 'snapshot') {
      if (value.schemaVersion !== 1 || typeof value.sceneId !== 'string' ||
          !Number.isSafeInteger(value.sceneEpoch) || !Number.isSafeInteger(value.revision) ||
          typeof value.serverTime !== 'number') { this.reconnectNow(); return; }
      const snapshot = value as unknown as RealtimeSnapshot;
      this.clearSnapshotTimeout();
      this.sceneId = snapshot.sceneId;
      this.sceneEpoch = snapshot.sceneEpoch;
      this.revision = snapshot.revision;
      this.simulationTick = snapshot.simulationTick;
      this.serverOffsetMs = snapshot.serverTime - this.now();
      this.retry = 0;
      this.options.onSnapshot(snapshot);
      this.changeState('ready');
      for (const commandId of this.pending.keys()) {
        this.socket?.send(JSON.stringify({ type: 'status', commandId }));
      }
    } else if (value.type === 'delta') {
      if (this.state !== 'ready' || value.schemaVersion !== 1 || value.sceneId !== this.sceneId ||
          value.sceneEpoch !== this.sceneEpoch || !Number.isSafeInteger(value.revision)) {
        this.reconnectNow(); return;
      }
      const delta = value as unknown as RealtimeDelta;
      if (delta.revision <= this.revision) return;
      if (delta.revision !== this.revision + 1) { this.reconnectNow(); return; }
      this.revision = delta.revision;
      this.options.onDelta(delta);
    } else if (value.type === 'positions') {
      if (this.state !== 'ready' || value.schemaVersion !== 1 || value.sceneId !== this.sceneId ||
          value.sceneEpoch !== this.sceneEpoch || typeof value.revision !== 'number' ||
          !Number.isSafeInteger(value.revision) || value.revision > this.revision ||
          typeof value.simulationTick !== 'number' || !Number.isSafeInteger(value.simulationTick) ||
          value.simulationTick <= this.simulationTick || !Array.isArray(value.positions) ||
          value.positions.length > 100 || !value.positions.every(item =>
            item && typeof item.id === 'string' && item.position && item.heading &&
            Number.isFinite(item.position.x) && Number.isFinite(item.position.y) &&
            Number.isFinite(item.heading.x) && Number.isFinite(item.heading.y)) ||
          (value.actionPositions !== undefined && (!Array.isArray(value.actionPositions) ||
            value.actionPositions.length > 1 || !value.actionPositions.every(item =>
              item && typeof item.id === 'string' && item.position &&
              Number.isFinite(item.position.x) && Number.isFinite(item.position.y))))) return;
      this.simulationTick = value.simulationTick;
      this.options.onPositions(value as unknown as ScenePositions);
    } else if (value.type === 'ack') {
      const commandId = value.commandId;
      if (typeof commandId !== 'string' || !this.pending.has(commandId)) return;
      this.pending.delete(commandId);
      this.options.onCommandResult(commandId, value as unknown as RealtimeAck);
    } else if (value.type === 'status' && value.known === false) {
      const commandId = value.commandId;
      if (typeof commandId !== 'string') return;
      const command = this.pending.get(commandId);
      if (!command) return;
      if (command.expiresAt > this.now() + this.serverOffsetMs && this.socket) {
        this.socket.send(JSON.stringify(command));
      } else {
        this.pending.delete(commandId);
        this.options.onCommandResult(commandId, null);
      }
    } else if (value.type === 'error' && value.code === 'ACCESS_DENIED') {
      this.stop();
    }
  }

  private scheduleReconnect(): void {
    if (!this.started) return;
    const delays = [1000, 2000, 4000, 8000, 15000];
    const base = delays[Math.min(this.retry++, delays.length - 1)];
    const delay = base * (0.85 + this.random() * 0.3);
    this.reconnectTimer = setTimeout(() => { this.reconnectTimer = null; this.open(); }, delay);
  }

  private clearReconnect(): void {
    if (this.reconnectTimer !== null) clearTimeout(this.reconnectTimer);
    this.reconnectTimer = null;
  }

  private clearSnapshotTimeout(): void {
    if (this.snapshotTimer !== null) clearTimeout(this.snapshotTimer);
    this.snapshotTimer = null;
  }

  private changeState(state: ConnectionState): void {
    if (this.state === state) return;
    this.state = state;
    this.options.onState(state);
  }
}
