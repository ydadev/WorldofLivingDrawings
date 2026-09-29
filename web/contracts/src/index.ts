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
export interface InteractionDefinition {
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
}

export interface InteractionRequested {
  type: 'interaction_requested';
  commandId: string;
  interactionId: string;
  point: Point2;
}
export interface RealtimeSnapshot extends SceneSnapshot {
  type: 'snapshot';
  sceneId: string;
  simulationVersion: number;
  serverTime: number;
  activeActions: unknown[];
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
}
export interface RealtimeDelta extends SceneDelta {
  type: 'delta';
  sceneId: string;
  event: InteractionRequested;
}
export interface RealtimeCommand {
  type: 'command';
  commandId: string;
  sessionId: string;
  sceneId: string;
  sceneEpoch: number;
  interactionId: string;
  point: Point2;
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
