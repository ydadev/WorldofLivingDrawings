import '@babylonjs/core/Culling/ray';
import { Camera } from '@babylonjs/core/Cameras/camera';
import { FreeCamera } from '@babylonjs/core/Cameras/freeCamera';
import { Engine } from '@babylonjs/core/Engines/engine';
import { HemisphericLight } from '@babylonjs/core/Lights/hemisphericLight';
import { Color3, Color4 } from '@babylonjs/core/Maths/math.color';
import { Matrix, Vector3 } from '@babylonjs/core/Maths/math.vector';
import { Plane } from '@babylonjs/core/Maths/math.plane';
import { StandardMaterial } from '@babylonjs/core/Materials/standardMaterial';
import { AbstractMesh } from '@babylonjs/core/Meshes/abstractMesh';
import { MeshBuilder } from '@babylonjs/core/Meshes/meshBuilder';
import { Scene } from '@babylonjs/core/scene';
import type { Point2, SceneDelta, SceneEntity, ScenePositions, SceneSnapshot, WorldDefinition } from '@ldw/contracts';
import type { RendererAdapter } from '@ldw/renderer';

/** Minimal WebGL 2 renderer for the empty synchronized scene in CORE-01. */
export class BabylonRendererAdapter implements RendererAdapter {
  readonly engine: Engine;
  readonly scene: Scene;
  private readonly camera: FreeCamera;
  private readonly markers = new Map<string, AbstractMesh>();
  private readonly movement = new Map<string, { from: Point2; to: Point2; started: number }>();
  private readonly interactionPlane = Plane.FromPositionAndNormal(Vector3.Zero(), new Vector3(0, 0, 1));
  private readonly resizeObserver: ResizeObserver;
  private world?: WorldDefinition;
  private sceneEpoch = 0;
  private revision = 0;
  private simulationTick = 0;

  constructor(private readonly canvas: HTMLCanvasElement) {
    this.engine = new Engine(canvas, true);
    this.scene = new Scene(this.engine);
    this.scene.clearColor = new Color4(.03, .15, .23, 1);
    this.camera = new FreeCamera('fixed-side-camera', new Vector3(0, 0, -10), this.scene);
    this.camera.mode = Camera.ORTHOGRAPHIC_CAMERA;
    this.camera.setTarget(Vector3.Zero());
    this.scene.activeCamera = this.camera;
    new HemisphericLight('ambient', new Vector3(0, 1, -1), this.scene).intensity = 1.2;
    this.engine.runRenderLoop(() => { this.interpolate(); this.scene.render(); });
    this.resizeObserver = new ResizeObserver(() => this.engine.resize());
    this.resizeObserver.observe(canvas);
    requestAnimationFrame(() => this.engine.resize());
  }

  setWorld(world: WorldDefinition): void {
    if (world.schemaVersion !== 1 || !world.view.fixed || world.view.kind !== 'orthographic-side')
      throw new Error('UNSUPPORTED_WORLD_VIEW');
    this.world = world;
    const { width, height } = world.view;
    this.camera.orthoLeft = -width / 2;
    this.camera.orthoRight = width / 2;
    this.camera.orthoTop = height / 2;
    this.camera.orthoBottom = -height / 2;
  }

  applySnapshot(snapshot: SceneSnapshot): void {
    if (!this.world || snapshot.schemaVersion !== 1 || snapshot.worldId !== this.world.id ||
        snapshot.worldVersion !== this.world.version) throw new Error('WORLD_VERSION_MISMATCH');
    for (const id of this.markers.keys()) this.removeMarker(id);
    this.movement.clear();
    this.sceneEpoch = snapshot.sceneEpoch;
    this.revision = snapshot.revision;
    this.simulationTick = snapshot.simulationTick;
    for (const entity of snapshot.entities) this.upsert(entity);
  }

  applyDelta(delta: SceneDelta): void {
    if (delta.schemaVersion !== 1 || delta.sceneEpoch !== this.sceneEpoch ||
        delta.revision !== this.revision + 1) throw new Error('REVISION_GAP');
    for (const id of delta.remove) this.removeMarker(id);
    for (const entity of delta.upsert) this.upsert(entity);
    this.revision = delta.revision;
  }

  applyPositions(frame: ScenePositions): void {
    if (frame.sceneEpoch !== this.sceneEpoch || frame.revision > this.revision ||
        frame.simulationTick <= this.simulationTick) return;
    const now = performance.now();
    this.interpolate(now);
    this.simulationTick = frame.simulationTick;
    for (const item of frame.positions) {
      const marker = this.markers.get(item.id);
      if (!marker) continue;
      this.movement.set(item.id, {
        from: { x: marker.position.x, y: marker.position.y },
        to: item.position,
        started: now,
      });
    }
  }

  pickWorldPoint(clientX: number, clientY: number): Point2 | null {
    if (!this.world) return null;
    const rect = this.canvas.getBoundingClientRect();
    const x = clientX - rect.left - this.canvas.clientLeft;
    const y = clientY - rect.top - this.canvas.clientTop;
    if (x < 0 || y < 0 || x > this.canvas.clientWidth || y > this.canvas.clientHeight) return null;
    const ray = this.scene.createPickingRay(x, y, Matrix.Identity(), this.camera, false);
    const distance = ray.intersectsPlane(this.interactionPlane);
    if (distance === null) return null;
    const point = ray.origin.add(ray.direction.scale(distance));
    return Number.isFinite(point.x) && Number.isFinite(point.y) ? { x: point.x, y: point.y } : null;
  }

  dispose(): void {
    this.resizeObserver.disconnect();
    this.scene.dispose();
    this.engine.dispose();
  }

  private upsert(entity: SceneEntity): void {
    this.movement.delete(entity.id);
    let marker = this.markers.get(entity.id);
    if (!marker) {
      marker = MeshBuilder.CreateSphere(entity.id, { diameter: .45 }, this.scene);
      const material = new StandardMaterial(`material-${entity.id}`, this.scene);
      material.diffuseColor = new Color3(.9, .72, .28);
      marker.material = material;
      this.markers.set(entity.id, marker);
    }
    marker.position.set(entity.position.x, entity.position.y, 0);
  }

  private removeMarker(id: string): void {
    this.movement.delete(id);
    const marker = this.markers.get(id);
    marker?.material?.dispose();
    marker?.dispose();
    this.markers.delete(id);
  }

  private interpolate(now = performance.now()): void {
    for (const [id, move] of this.movement) {
      const marker = this.markers.get(id);
      if (!marker) { this.movement.delete(id); continue; }
      const progress = Math.min(1, (now - move.started) / 500);
      marker.position.x = move.from.x + (move.to.x - move.from.x) * progress;
      marker.position.y = move.from.y + (move.to.y - move.from.y) * progress;
      if (progress === 1) this.movement.delete(id);
    }
  }
}
