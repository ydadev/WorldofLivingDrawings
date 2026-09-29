import type { Point2, SceneDelta, ScenePositions, SceneSnapshot, WorldDefinition } from '@ldw/contracts';

/** A view of server state. Implementations never decide an interaction outcome. */
export interface RendererAdapter {
  setWorld(world: WorldDefinition): void;
  applySnapshot(snapshot: SceneSnapshot): void;
  applyDelta(delta: SceneDelta): void;
  applyPositions(frame: ScenePositions): void;
  pickWorldPoint(clientX: number, clientY: number): Point2 | null;
  dispose(): void;
}

export type RendererFactory = (canvas: HTMLCanvasElement) => RendererAdapter;
