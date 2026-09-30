import type { Point2, SceneDelta, SceneDeltaV2, ScenePositions, SceneSnapshot, SceneSnapshotV2, WorldDefinition } from '@ldw/contracts';

/** A view of server state. Implementations never decide an interaction outcome. */
export interface RendererAdapter {
  setWorld(world: WorldDefinition): void;
  applySnapshot(snapshot: SceneSnapshot | SceneSnapshotV2): void;
  applyDelta(delta: SceneDelta | SceneDeltaV2): void;
  applyPositions(frame: ScenePositions): void;
  pickWorldPoint(clientX: number, clientY: number): Point2 | null;
  dispose(): void;
}

export type RendererFactory = (canvas: HTMLCanvasElement) => RendererAdapter;
