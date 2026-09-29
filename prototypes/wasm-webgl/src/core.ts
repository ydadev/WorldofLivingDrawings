import type { SceneSnapshot, WorldDefinition } from '@ldw/contracts';
import { BabylonRendererAdapter } from '@ldw/renderer-babylon';
import worldData from '../../../content/underwater/world.json';
import './style.css';

declare global {
  interface Window {
    coreAdapter?: BabylonRendererAdapter;
    coreProbe?: { webglVersion: number; ready: boolean };
  }
}

const paintUrl = (id: string): string => {
  const color = id === 'red' ? '#ed4333' : '#328bea';
  const paint = document.createElement('canvas');
  paint.width = paint.height = 512;
  const context = paint.getContext('2d')!;
  context.fillStyle = color;
  context.fillRect(0, 0, 512, 512);
  return paint.toDataURL('image/png');
};
const adapter = new BabylonRendererAdapter(document.querySelector<HTMLCanvasElement>('#core-scene')!, '/fish/', paintUrl);
const world = worldData as WorldDefinition;
adapter.setWorld(world);
const snapshot: SceneSnapshot = { schemaVersion: 1, sceneEpoch: 1, revision: 0,
  simulationTick: 0, worldId: world.id, worldVersion: world.version, entities: [] };
adapter.applySnapshot(snapshot);
window.coreAdapter = adapter;
window.coreProbe = { webglVersion: adapter.engine.webGLVersion, ready: true };
