export type SourceKind = 'paper' | 'browser';
export type InteractionEffect = 'attraction' | 'threat';

export interface Point2 { x: number; y: number }
export interface PaintResult {
  schemaVersion: 1;
  templateId: string;
  templateVersion: number;
  layoutHash: string;
  sourceKind: SourceKind;
  colorSpace: 'sRGB';
  width: 512;
  height: 512;
  /** Reference to a validated, re-encoded PNG blob; no raw photo or data URL. */
  blobId: string;
}
export interface EntityDefinition {
  schemaVersion: 1;
  id: string;
  version: number;
  modelAssetId: string;
  paintTemplateId: string;
  paintTemplateVersion: number;
  capabilities: string[];
}
export interface InteractionDefinitionV1 {
  schemaVersion: 1;
  id: string;
  version: number;
  effect: InteractionEffect;
  allowedZoneId: string;
  requiredCapability: string;
  radius: number;
  durationTicks: number;
  maxActive: number;
  cooldownTicks: number;
  priority: number;
}
export type BehaviorStep =
  | { primitive: 'find-candidates'; maxCandidates: number }
  | { primitive: 'reserve'; maxPerSource: number }
  | { primitive: 'move-to'; depth: number }
  | { primitive: 'consume'; radius: number; depthTolerance: number }
  | { primitive: 'flee'; holdTicks: number; releaseRadiusFactor: number; escapeDepth: number }
  | { primitive: 'timeout' }
  | { primitive: 'cleanup' };
export interface InteractionDefinitionV2 extends Omit<InteractionDefinitionV1, 'schemaVersion'> {
  schemaVersion: 2;
  /** Public action name; additional definitions require it. */
  label?: string;
  /** Bounded, acyclic primitive chain; the server validates supported order before activation. */
  behavior: BehaviorStep[];
}
export type InteractionDefinition = InteractionDefinitionV1 | InteractionDefinitionV2;
export interface InteractionIntent {
  schemaVersion: 1;
  commandId: string;
  sceneEpoch: number;
  interactionId: string;
  point: Point2;
}
export interface WorldDefinition {
  schemaVersion: 1;
  id: string;
  version: number;
  view: { kind: 'orthographic-side'; width: number; height: number; fixed: true };
  zones: { id: string; bounds: [number, number, number, number] }[];
  entityDefinitions: string[];
  interactions: string[];
}
export interface SceneEntity {
  id: string;
  definitionId: string;
  definitionVersion: number;
  position: Point2;
  paintBlobId?: string;
}
export interface SceneSnapshot {
  schemaVersion: 1;
  sceneEpoch: number;
  revision: number;
  simulationTick: number;
  worldId: string;
  worldVersion: number;
  entities: SceneEntity[];
  activeActions?: ActiveAction[];
}

export interface InteractionRequested {
  type: 'interaction_requested';
  commandId: string;
  interactionId: string;
  point: Point2;
  targetActionId?: string;
}
export interface EntityPublished {
  type: 'entity_published';
  entity: SceneEntity;
}
export interface FeedAction {
  id: string;
  interactionId: 'feed';
  point: Point2;
  remaining: number;
  expiresAtTick: number;
}
export interface BoatAction {
  id: string;
  interactionId: 'boat';
  point: Point2;
  position: Point2;
  entry: Point2;
  exit: Point2;
  expiresAtTick: number;
}
export type ActiveAction = FeedAction | BoatAction;
export interface InteractionState {
  type: 'interaction_state';
  activeActions: ActiveAction[];
  appliedCommandIds: string[];
  simulationTick: number;
}
export interface RealtimeSnapshot extends SceneSnapshot {
  type: 'snapshot';
  sceneId: string;
  simulationVersion: number;
  serverTime: number;
  activeActions: ActiveAction[];
  pendingInteractions: InteractionRequested[];
  resources: Record<string, unknown>;
  reservations: unknown[];
}
export interface SceneDelta {
  schemaVersion: 1;
  sceneEpoch: number;
  revision: number;
  simulationTick: number;
  upsert: SceneEntity[];
  remove: string[];
  event?: InteractionRequested | EntityPublished | InteractionState;
}
export interface RealtimeDelta extends SceneDelta {
  type: 'delta';
  sceneId: string;
  event: InteractionRequested | EntityPublished | InteractionState;
}
export interface ActionCatalogEntry {
  id: string;
  effect: InteractionEffect;
  label: string;
  allowedZoneId: string;
}
export type ActiveActionV2 =
  | (Omit<FeedAction, 'interactionId'> & { interactionId: string; effect: 'attraction' })
  | (Omit<BoatAction, 'interactionId'> & { interactionId: string; effect: 'threat' });
export interface InteractionStateV2 {
  type: 'interaction_state';
  activeActions: ActiveActionV2[];
  appliedCommandIds: string[];
  simulationTick: number;
}
export type AnyActiveAction = ActiveAction | ActiveActionV2;
/** A scene with actions supplied by its checkpointed package registry. */
export type RealtimeSnapshotV2 = Omit<RealtimeSnapshot, 'schemaVersion' | 'activeActions'> & {
  schemaVersion: 2;
  actionCatalog: ActionCatalogEntry[];
  activeActions: ActiveActionV2[];
};
export type RealtimeDeltaV2 = Omit<RealtimeDelta, 'schemaVersion' | 'event'> & {
  schemaVersion: 2;
  event: InteractionRequested | EntityPublished | InteractionStateV2;
};
export type SceneSnapshotV2 = Omit<SceneSnapshot, 'schemaVersion' | 'activeActions'> &
  Pick<RealtimeSnapshotV2, 'schemaVersion' | 'actionCatalog' | 'activeActions'>;
export type SceneDeltaV2 = Omit<SceneDelta, 'schemaVersion' | 'event'> &
  Pick<RealtimeDeltaV2, 'schemaVersion' | 'event'>;
export type AnyRealtimeSnapshot = RealtimeSnapshot | RealtimeSnapshotV2;
export type AnyRealtimeDelta = RealtimeDelta | RealtimeDeltaV2;
export interface ScenePositions {
  type: 'positions';
  schemaVersion: 1;
  sceneId: string;
  sceneEpoch: number;
  revision: number;
  simulationTick: number;
  positions: { id: string; position: Point2; heading: Point2;
    depth?: number; headingDepth?: number }[];
  actionPositions?: { id: string; position: Point2 }[];
}
export interface RealtimeCommand {
  type: 'command';
  commandId: string;
  sessionId: string;
  sceneId: string;
  sceneEpoch: number;
  interactionId: string;
  point: Point2;
  targetActionId?: string;
  expiresAt: number;
}
export interface RealtimeAck {
  type: 'ack';
  commandId: string;
  accepted: boolean;
  code: string;
  sceneId: string;
  sceneEpoch: number;
  revision: number;
}
export interface AssetManifest {
  schemaVersion: 1;
  packageId: string;
  packageVersion: number;
  assets: { id: string; path: string; sha256: string; mediaType: string; license: string }[];
}
