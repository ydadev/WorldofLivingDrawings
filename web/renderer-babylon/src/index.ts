import '@babylonjs/loaders/glTF';
import '@babylonjs/core/Shaders/default.vertex';
import '@babylonjs/core/Shaders/default.fragment';
import type { AssetContainer, InstantiatedEntries } from '@babylonjs/core/assetContainer';
import '@babylonjs/core/Culling/ray';
import { Camera } from '@babylonjs/core/Cameras/camera';
import { FreeCamera } from '@babylonjs/core/Cameras/freeCamera';
import { Engine } from '@babylonjs/core/Engines/engine';
import { HemisphericLight } from '@babylonjs/core/Lights/hemisphericLight';
import { Color3, Color4 } from '@babylonjs/core/Maths/math.color';
import { Matrix, Vector3 } from '@babylonjs/core/Maths/math.vector';
import { Plane } from '@babylonjs/core/Maths/math.plane';
import { LoadAssetContainerAsync } from '@babylonjs/core/Loading/sceneLoader';
import { PBRMaterial } from '@babylonjs/core/Materials/PBR/pbrMaterial';
import { ShaderMaterial } from '@babylonjs/core/Materials/shaderMaterial';
import { StandardMaterial } from '@babylonjs/core/Materials/standardMaterial';
import { Texture } from '@babylonjs/core/Materials/Textures/texture';
import { AbstractMesh } from '@babylonjs/core/Meshes/abstractMesh';
import { MeshBuilder } from '@babylonjs/core/Meshes/meshBuilder';
import { TransformNode } from '@babylonjs/core/Meshes/transformNode';
import { Scene } from '@babylonjs/core/scene';
import type { AnyActiveAction, Point2, SceneDelta, SceneDeltaV2, SceneEntity, ScenePositions, SceneSnapshot, SceneSnapshotV2, WorldDefinition } from '@ldw/contracts';
import type { RendererAdapter } from '@ldw/renderer';
import { addAquarium } from './aquarium';
import { createFishPaintMaterial } from './fish-paint-material';

const modelByDefinition: Record<string, string> = {
  'coral-fish': 'coral.glb',
  'stream-fish': 'stream.glb',
};

const lengthByDefinition: Record<string, number> = {
  'coral-fish': 2.9,
  'stream-fish': 3.4,
};

type FishMotion = { yaw: number; targetYaw: number; pitch: number; targetPitch: number;
  phase: number; speed: number; targetSpeed: number; length: number; lastAnimatedAt: number;
  lastPositionAt: number; traveling: boolean; hasPositionFrame: boolean; tail?: TransformNode };

/** Fixed side-view renderer. Each Entity receives its own material and paint texture. */
export class BabylonRendererAdapter implements RendererAdapter {
  readonly engine: Engine;
  readonly scene: Scene;
  private readonly camera: FreeCamera;
  private readonly fallbackMaterial: StandardMaterial;
  private feedMaterial?: StandardMaterial;
  private boatMaterial?: StandardMaterial;
  private readonly markers = new Map<string, TransformNode>();
  private readonly feedMarkers = new Map<string, { root: TransformNode; source: AbstractMesh; point: Point2 }>();
  private readonly boatMarkers = new Map<string, TransformNode>();
  private readonly loadingMarkers = new Map<string, AbstractMesh>();
  private readonly modelCache = new Map<string, Promise<AssetContainer>>();
  private readonly modelEntries = new Map<string, InstantiatedEntries>();
  private readonly paintTextures = new Map<string, Texture>();
  private readonly paintMaterials = new Map<string, ShaderMaterial[]>();
  private readonly entityVersions = new Map<string, string>();
  private readonly movement = new Map<string, { from: Point2 & { depth: number };
    to: Point2 & { depth: number }; started: number }>();
  private readonly fishMotion = new Map<string, FishMotion>();
  private readonly boatMovement = new Map<string, { from: Point2; to: Point2; started: number }>();
  private readonly interactionPlane = Plane.FromPositionAndNormal(Vector3.Zero(), new Vector3(0, 0, 1));
  private readonly resizeObserver: ResizeObserver;
  private world?: WorldDefinition;
  private aquariumAdded = false;
  private sceneEpoch = 0;
  private revision = 0;
  private simulationTick = 0;
  private disposed = false;
  private renderScale = 1;

  constructor(private readonly canvas: HTMLCanvasElement,
    private readonly assetBaseUrl = '/content/underwater/assets/',
    private readonly paintUrl: (blobId: string) => string = id => `/api/paint/${encodeURIComponent(id)}`,
    private readonly fishFrameDurationMs = 500) {
    if (!Number.isFinite(fishFrameDurationMs) || fishFrameDurationMs < 50 ||
        fishFrameDurationMs > 1000) throw new Error('INVALID_FRAME_DURATION');
    this.engine = new Engine(canvas, true);
    this.scene = new Scene(this.engine);
    this.scene.clearColor = new Color4(.03, .15, .23, 1);
    this.camera = new FreeCamera('fixed-side-camera', new Vector3(0, 0, -10), this.scene);
    this.camera.mode = Camera.ORTHOGRAPHIC_CAMERA;
    this.camera.setTarget(Vector3.Zero());
    this.scene.activeCamera = this.camera;
    new HemisphericLight('ambient', new Vector3(0, 1, -1), this.scene).intensity = 1.2;
    this.fallbackMaterial = new StandardMaterial('fish-loading', this.scene);
    this.fallbackMaterial.diffuseColor = new Color3(.9, .72, .28);
    this.engine.runRenderLoop(() => {
      this.interpolate(); this.animateFish(); this.animateFeed(); this.scene.render();
    });
    this.resizeObserver = new ResizeObserver(() => this.resizeToBudget());
    this.resizeObserver.observe(canvas);
    requestAnimationFrame(() => this.resizeToBudget());
  }

  /** LOW starts at at most 1280×720 even when the panel has a 4K viewport. */
  setRenderScale(scale: number): void {
    if (!Number.isFinite(scale) || scale < .5 || scale > 1)
      throw new Error('INVALID_RENDER_SCALE');
    this.renderScale = scale;
    this.resizeToBudget();
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
    if (!this.aquariumAdded) {
      addAquarium(this.scene, width, height);
      this.aquariumAdded = true;
    }
  }

  applySnapshot(snapshot: SceneSnapshot | SceneSnapshotV2): void {
    if (!this.world || ![1, 2].includes(snapshot.schemaVersion) || snapshot.worldId !== this.world.id ||
        snapshot.worldVersion !== this.world.version) throw new Error('WORLD_VERSION_MISMATCH');
    for (const id of this.markers.keys()) this.removeMarker(id);
    this.movement.clear();
    this.sceneEpoch = snapshot.sceneEpoch;
    this.revision = snapshot.revision;
    this.simulationTick = snapshot.simulationTick;
    for (const entity of snapshot.entities) this.upsert(entity);
    this.syncActions(snapshot.activeActions ?? []);
  }

  applyDelta(delta: SceneDelta | SceneDeltaV2): void {
    if (![1, 2].includes(delta.schemaVersion) || delta.sceneEpoch !== this.sceneEpoch ||
        delta.revision !== this.revision + 1) throw new Error('REVISION_GAP');
    for (const id of delta.remove) this.removeMarker(id);
    for (const entity of delta.upsert) this.upsert(entity);
    if (delta.event?.type === 'interaction_state') this.syncActions(delta.event.activeActions);
    this.revision = delta.revision;
  }

  applyPositions(frame: ScenePositions): void {
    if (frame.sceneEpoch !== this.sceneEpoch || frame.revision > this.revision ||
        frame.simulationTick <= this.simulationTick) return;
    const now = performance.now();
    this.interpolate(now);
    const elapsedSeconds = Math.max(.05, Math.min(1,
      (frame.simulationTick - this.simulationTick) / 20));
    this.simulationTick = frame.simulationTick;
    for (const item of frame.positions) {
      const marker = this.markers.get(item.id);
      if (!marker) continue;
      if (!Number.isFinite(item.position.x) || !Number.isFinite(item.position.y) ||
          (item.depth !== undefined && (!Number.isFinite(item.depth) || Math.abs(item.depth) > 1.5))) continue;
      const depth = item.depth ?? marker.position.z;
      const visual = this.fishMotion.get(item.id);
      // A snapshot has no depth. Its first position frame supplies the real Z;
      // that initial correction is not swimming and must not point the nose at it.
      const firstFrame = visual && !visual.hasPositionFrame;
      if (firstFrame) marker.position.z = depth;
      const dx = item.position.x - marker.position.x;
      const dy = item.position.y - marker.position.y;
      const dz = depth - marker.position.z;
      this.movement.set(item.id, {
        from: { x: marker.position.x, y: marker.position.y, depth: marker.position.z },
        to: { ...item.position, depth },
        started: now,
      });
      if (visual) visual.hasPositionFrame = true;
      if (visual && Number.isFinite(item.heading.x) && Number.isFinite(item.heading.y)) {
        const headingDepth = item.headingDepth ?? 0;
        if (Number.isFinite(headingDepth) && Math.abs(headingDepth) <= 1) {
          // The glTF loader's handedness root turns the authored -X nose into
          // Babylon +X. Follow the visible interpolation segment; a delayed
          // heading frame must never make the fish slide tail-first. When the
          // position is unchanged, keep the server heading for the next turn.
          const moving = !firstFrame && Math.hypot(dx, dy, dz) > .001;
          const forwardX = moving ? dx : item.heading.x;
          const forwardY = moving ? dy : item.heading.y;
          const forwardZ = moving ? dz : headingDepth;
          const horizontal = Math.hypot(forwardX, forwardZ);
          if (horizontal > .01) visual.targetYaw = Math.atan2(-forwardZ, forwardX);
          visual.targetPitch = Math.max(-.32, Math.min(.32,
            Math.atan2(forwardY, Math.max(.3, horizontal)) * .28));
          visual.targetSpeed = moving ? Math.min(2.5,
            Math.hypot(dx, dy, dz) / elapsedSeconds) : 0;
          visual.lastPositionAt = now;
          visual.traveling = moving;
        }
      }
    }
    for (const item of frame.actionPositions ?? []) {
      const marker = this.boatMarkers.get(item.id);
      if (!marker || !Number.isFinite(item.position.x) || !Number.isFinite(item.position.y)) continue;
      this.boatMovement.set(item.id, {
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
    this.disposed = true;
    this.resizeObserver.disconnect();
    for (const id of [...this.markers.keys()]) this.removeMarker(id);
    for (const id of [...this.feedMarkers.keys()]) this.removeFeed(id);
    for (const id of [...this.boatMarkers.keys()]) this.removeBoat(id);
    this.scene.dispose();
    this.engine.dispose();
  }

  private upsert(entity: SceneEntity): void {
    const version = `${entity.definitionId}@${entity.definitionVersion}/${entity.paintBlobId ?? ''}`;
    if (this.entityVersions.get(entity.id) !== version && this.markers.has(entity.id))
      this.removeMarker(entity.id);
    this.movement.delete(entity.id);
    let marker = this.markers.get(entity.id);
    if (!marker) {
      marker = new TransformNode(entity.id, this.scene);
      const loading = MeshBuilder.CreateSphere(`${entity.id}/loading`, { diameter: .45 }, this.scene);
      loading.parent = marker;
      loading.material = this.fallbackMaterial;
      this.loadingMarkers.set(entity.id, loading);
      this.markers.set(entity.id, marker);
      this.fishMotion.set(entity.id, { yaw: 0, targetYaw: 0, pitch: 0,
        targetPitch: 0, speed: 0, targetSpeed: 0,
        length: lengthByDefinition[entity.definitionId] ?? 3,
        lastAnimatedAt: performance.now(), lastPositionAt: 0,
        traveling: false, hasPositionFrame: false,
        phase: [...entity.id].reduce((sum, char) => sum + char.charCodeAt(0), 0) * .31 });
      this.entityVersions.set(entity.id, version);
      if (entity.definitionVersion !== 1 || !modelByDefinition[entity.definitionId])
        throw new Error('UNSUPPORTED_ENTITY_DEFINITION');
      void this.loadModel(entity, marker).catch(error => {
        if (this.markers.get(entity.id) === marker)
          console.error(`Model load failed for ${entity.definitionId}`, error);
      });
    }
    marker.position.set(entity.position.x, entity.position.y, 0);
  }

  private removeMarker(id: string): void {
    this.movement.delete(id);
    this.entityVersions.delete(id);
    this.fishMotion.delete(id);
    this.paintTextures.get(id)?.dispose();
    this.paintTextures.delete(id);
    this.loadingMarkers.get(id)?.dispose();
    this.loadingMarkers.delete(id);
    this.modelEntries.get(id)?.dispose();
    this.modelEntries.delete(id);
    for (const material of this.paintMaterials.get(id) ?? []) material.dispose();
    this.paintMaterials.delete(id);
    const marker = this.markers.get(id);
    marker?.dispose();
    this.markers.delete(id);
  }

  private syncFeed(actions: AnyActiveAction[]): void {
    const active = new Set<string>();
    for (const action of actions) {
      const effect = 'effect' in action ? action.effect :
        (action.interactionId === 'feed' ? 'attraction' : 'threat');
      if (effect !== 'attraction' || !('remaining' in action) || !Number.isFinite(action.point.x) ||
          !Number.isFinite(action.point.y) || !Number.isInteger(action.remaining) ||
          action.remaining < 1 || action.remaining > 10 || active.has(action.id)) continue;
      active.add(action.id);
      let marker = this.feedMarkers.get(action.id);
      if (!marker) {
        const root = new TransformNode(action.id, this.scene);
        const source = MeshBuilder.CreateSphere(`${action.id}/source`,
          { diameter: .28, segments: 6 }, this.scene);
        source.parent = root;
        source.material = this.ensureFeedMaterial();
        marker = { root, source, point: action.point };
        this.feedMarkers.set(action.id, marker);
      }
      marker.point = action.point;
      marker.root.position.set(action.point.x, action.point.y, -.35);
      marker.source.scaling.setAll(.55 + action.remaining * .045);
    }
    for (const id of [...this.feedMarkers.keys()]) {
      if (!active.has(id)) this.removeFeed(id);
    }
  }

  private syncActions(actions: AnyActiveAction[]): void {
    this.syncFeed(actions);
    const active = new Set<string>();
    for (const action of actions) {
      const effect = 'effect' in action ? action.effect :
        (action.interactionId === 'boat' ? 'threat' : 'attraction');
      if (effect !== 'threat' || !('position' in action) || !Number.isFinite(action.position.x) ||
          !Number.isFinite(action.position.y) || active.has(action.id)) continue;
      active.add(action.id);
      let root = this.boatMarkers.get(action.id);
      if (!root) {
        root = new TransformNode(action.id, this.scene);
        const hull = MeshBuilder.CreateSphere(`${action.id}/hull`, { diameter: 1, segments: 10 }, this.scene);
        hull.scaling.set(1.25, .42, .55);
        hull.parent = root;
        hull.material = this.ensureBoatMaterial();
        const tower = MeshBuilder.CreateBox(`${action.id}/tower`,
          { width: .36, height: .32, depth: .4 }, this.scene);
        tower.position.y = .29;
        tower.parent = root;
        tower.material = this.ensureBoatMaterial();
        this.boatMarkers.set(action.id, root);
      }
      root.position.set(action.position.x, action.position.y, -.2);
      root.scaling.x = action.exit.x >= action.entry.x ? 1 : -1;
    }
    for (const id of [...this.boatMarkers.keys()]) if (!active.has(id)) this.removeBoat(id);
  }

  private removeBoat(id: string): void {
    this.boatMovement.delete(id);
    this.boatMarkers.get(id)?.dispose(false, true);
    this.boatMarkers.delete(id);
    if (this.boatMarkers.size === 0) {
      this.boatMaterial?.dispose();
      this.boatMaterial = undefined;
    }
  }

  private ensureBoatMaterial(): StandardMaterial {
    if (!this.boatMaterial) {
      this.boatMaterial = new StandardMaterial('boat', this.scene);
      this.boatMaterial.diffuseColor = new Color3(.25, .67, .77);
      this.boatMaterial.emissiveColor = new Color3(.08, .24, .3);
    }
    return this.boatMaterial;
  }

  private removeFeed(id: string): void {
    const marker = this.feedMarkers.get(id);
    marker?.source.dispose();
    marker?.root.dispose();
    this.feedMarkers.delete(id);
    if (this.feedMarkers.size === 0) {
      this.feedMaterial?.dispose();
      this.feedMaterial = undefined;
    }
  }

  private ensureFeedMaterial(): StandardMaterial {
    if (!this.feedMaterial) {
      const material = new StandardMaterial('feed-source', this.scene);
      material.diffuseColor = new Color3(1, .77, .27);
      material.emissiveColor = new Color3(.7, .42, .08);
      material.disableLighting = true;
      this.feedMaterial = material;
    }
    return this.feedMaterial;
  }

  private animateFeed(): void {
    const seconds = performance.now() / 1000;
    for (const [id, marker] of this.feedMarkers) {
      marker.root.position.y = marker.point.y + .035 * Math.sin(seconds * 2 + id.length);
    }
  }

  private async loadModel(entity: SceneEntity, marker: TransformNode): Promise<void> {
    const file = modelByDefinition[entity.definitionId];
    const url = new URL(file, new URL(this.assetBaseUrl, document.baseURI)).toString();
    let pending = this.modelCache.get(file);
    if (!pending) {
      pending = LoadAssetContainerAsync(url, this.scene);
      this.modelCache.set(file, pending);
    }
    const container = await pending;
    if (this.disposed || this.markers.get(entity.id) !== marker) return;
    for (const material of container.materials) {
      if (material instanceof PBRMaterial) material.unlit = true;
    }
    const entries = container.instantiateModelsToScene(name => `${entity.id}/${name}`, false,
      { doNotInstantiate: true });
    if (this.disposed || this.markers.get(entity.id) !== marker) { entries.dispose(); return; }
    for (const root of entries.rootNodes) root.parent = marker;
    this.modelEntries.set(entity.id, entries);
    const visual = this.fishMotion.get(entity.id);
    if (visual) for (const root of entries.rootNodes) {
      for (const node of root.getDescendants(false)) {
        if (node instanceof TransformNode && node.name.endsWith('/tail-pivot')) visual.tail = node;
      }
    }
    const paintedTexture = entity.paintBlobId ?
      new Texture(this.paintUrl(entity.paintBlobId), this.scene, false, true) : undefined;
    if (paintedTexture) this.paintTextures.set(entity.id, paintedTexture);
    let material: ShaderMaterial | undefined;
    for (const root of entries.rootNodes) {
      for (const node of root.getDescendants(false)) {
        if (!(node instanceof AbstractMesh) || !(node.material instanceof PBRMaterial) ||
            node.material.name !== 'paint') continue;
        const texture = paintedTexture ?? node.material.albedoTexture;
        if (!(texture instanceof Texture)) continue;
        material ??= createFishPaintMaterial(this.scene, `${entity.id}/paint`, texture);
        node.material = material;
      }
    }
    if (entity.paintBlobId && !material) throw new Error('PAINT_MESH_MISSING');
    if (material) this.paintMaterials.set(entity.id, [material]);
    this.loadingMarkers.get(entity.id)?.dispose();
    this.loadingMarkers.delete(entity.id);
  }

  private interpolate(now = performance.now()): void {
    for (const [id, move] of this.movement) {
      const marker = this.markers.get(id);
      if (!marker) { this.movement.delete(id); continue; }
      const progress = Math.min(1, (now - move.started) / this.fishFrameDurationMs);
      marker.position.x = move.from.x + (move.to.x - move.from.x) * progress;
      marker.position.y = move.from.y + (move.to.y - move.from.y) * progress;
      marker.position.z = move.from.depth + (move.to.depth - move.from.depth) * progress;
      marker.scaling.setAll(1 - marker.position.z * .16);
      if (progress === 1) this.movement.delete(id);
    }
    for (const [id, move] of this.boatMovement) {
      const marker = this.boatMarkers.get(id);
      if (!marker) { this.boatMovement.delete(id); continue; }
      const progress = Math.min(1, (now - move.started) / 500);
      marker.position.x = move.from.x + (move.to.x - move.from.x) * progress;
      marker.position.y = move.from.y + (move.to.y - move.from.y) * progress;
      if (progress === 1) this.boatMovement.delete(id);
    }
  }

  private animateFish(): void {
    const now = performance.now();
    for (const [id, visual] of this.fishMotion) {
      const marker = this.markers.get(id);
      if (!marker) continue;
      const dt = Math.max(0, Math.min(.05, (now - visual.lastAnimatedAt) / 1000));
      visual.lastAnimatedAt = now;
      const desiredSpeed = now - visual.lastPositionAt > 1200 ? 0 : visual.targetSpeed;
      visual.speed += (desiredSpeed - visual.speed) * Math.min(1, dt * 4);
      const speedRatio = Math.min(1, visual.speed / 1.8);
      visual.phase += dt * Math.PI * 2 * (.18 + .95 * visual.speed / visual.length);
      let remainingYaw = Math.atan2(Math.sin(visual.targetYaw - visual.yaw),
        Math.cos(visual.targetYaw - visual.yaw));
      if (visual.traveling && Math.abs(remainingYaw) > .48) {
        visual.yaw += remainingYaw - Math.sign(remainingYaw) * .48;
        remainingYaw = Math.sign(remainingYaw) * .48;
      }
      visual.yaw += Math.sign(remainingYaw) * Math.min(Math.abs(remainingYaw),
        dt * (visual.traveling ? 5.5 : 3));
      visual.pitch += (visual.targetPitch - visual.pitch) * Math.min(1, dt * 12);
      const stroke = Math.sin(visual.phase);
      marker.rotation.y = visual.yaw + stroke * .012;
      marker.rotation.z = visual.pitch;
      if (visual.tail) visual.tail.rotation.y = stroke * (.08 + .34 * speedRatio);
      for (const material of this.paintMaterials.get(id) ?? []) {
        material.setFloat('swimPhase', visual.phase);
        material.setFloat('swimStrength', .015 + .08 * speedRatio);
      }
    }
  }

  private resizeToBudget(): void {
    const width = this.canvas.clientWidth;
    const height = this.canvas.clientHeight;
    if (width <= 0 || height <= 0) return;
    const scaling = Math.max(1, width / (1280 * this.renderScale),
      height / (720 * this.renderScale));
    this.engine.setHardwareScalingLevel(scaling);
    this.engine.resize();
  }
}
