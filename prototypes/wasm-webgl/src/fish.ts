import '@babylonjs/loaders/glTF';
import { ArcRotateCamera } from '@babylonjs/core/Cameras/arcRotateCamera';
import { Engine } from '@babylonjs/core/Engines/engine';
import { HemisphericLight } from '@babylonjs/core/Lights/hemisphericLight';
import { ImportMeshAsync } from '@babylonjs/core/Loading/sceneLoader';
import { Color3, Color4 } from '@babylonjs/core/Maths/math.color';
import { Vector3 } from '@babylonjs/core/Maths/math.vector';
import { PBRMaterial } from '@babylonjs/core/Materials/PBR/pbrMaterial';
import { DynamicTexture } from '@babylonjs/core/Materials/Textures/dynamicTexture';
import { Scene } from '@babylonjs/core/scene';
import './style.css';

type FishId = 'coral' | 'stream';
type Side = 'near' | 'far' | 'edge';
interface FishProbe {
  status: 'LOADING' | 'PASS' | 'FAIL';
  species: FishId;
  side: Side;
  webglVersion?: number;
  paintMaterialCount?: number;
  protectedEyeMaterialCount?: number;
  frames?: number;
  leftPixel?: number[];
  rightPixel?: number[];
  error?: string;
}
declare global { interface Window { fishProbe?: FishProbe } }

const canvas = document.querySelector<HTMLCanvasElement>('#fish-scene');
const status = document.querySelector<HTMLParagraphElement>('#fish-status');
const template = document.querySelector<HTMLImageElement>('#fish-template');
const download = document.querySelector<HTMLAnchorElement>('#print-template');
if (!canvas || !status || !template || !download) throw new Error('Fish QA markup is incomplete');

const engine = new Engine(canvas, true);
const scene = new Scene(engine);
scene.clearColor = new Color4(0.04, 0.20, 0.28, 1);
const camera = new ArcRotateCamera('fixed-qa-view', Math.PI / 2, Math.PI / 2, 6.3, Vector3.Zero(), scene);
scene.activeCamera = camera;
new HemisphericLight('light', new Vector3(0, 1, 0), scene).intensity = 1.7;
let frameCount = 0;
engine.runRenderLoop(() => {
  scene.render();
  if (window.fishProbe?.status === 'PASS') {
    window.fishProbe.frames = ++frameCount;
    const gl = canvas!.getContext('webgl2');
    if (gl) {
      const left = new Uint8Array(4), right = new Uint8Array(4);
      gl.readPixels(Math.round(canvas!.width * 0.39), canvas!.height >> 1, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, left);
      gl.readPixels(Math.round(canvas!.width * 0.62), canvas!.height >> 1, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, right);
      window.fishProbe.leftPixel = Array.from(left);
      window.fishProbe.rightPixel = Array.from(right);
    }
  }
});
window.addEventListener('resize', () => engine.resize());

let species: FishId = 'coral';
let side: Side = 'near';
let loadedRoots: Awaited<ReturnType<typeof ImportMeshAsync>>['meshes'] = [];
let paintTexture: DynamicTexture | undefined;
function setCameraSide(): void {
  camera.alpha = side === 'far' ? -Math.PI / 2 : Math.PI / 2;
  camera.beta = side === 'edge' ? 0.75 : Math.PI / 2;
}
function sideLabel(): string {
  return side === 'near' ? 'первая сторона' : side === 'far' ? 'обратная сторона' : 'спина и торцы';
}

function makePaintTexture(): DynamicTexture {
  const texture = new DynamicTexture('one-side-paint', { width: 512, height: 512 }, scene, false);
  const context = texture.getContext();
  context.fillStyle = '#ffe6aa';
  context.fillRect(0, 0, 512, 512);
  context.fillStyle = '#f04432';
  context.fillRect(0, 0, 190, 512);
  context.fillStyle = '#ffd44a';
  context.fillRect(190, 0, 145, 512);
  context.fillStyle = '#3152e8';
  context.fillRect(335, 0, 177, 512);
  context.fillStyle = '#102644';
  context.fillRect(232, 0, 28, 512);
  texture.update(false);
  return texture;
}

async function loadFish(): Promise<void> {
  window.fishProbe = { status: 'LOADING', species, side };
  status!.textContent = 'Загрузка модели…';
  loadedRoots.forEach(mesh => mesh.dispose(false, true));
  loadedRoots = [];
  paintTexture?.dispose();
  paintTexture = undefined;
  try {
    const imported = await ImportMeshAsync(`/fish/${species}.glb`, scene);
    loadedRoots = imported.meshes;
    const paintMaterials = new Set<PBRMaterial>();
    const eyeMaterials = new Set<PBRMaterial>();
    for (const mesh of imported.meshes) {
      const material = mesh.material;
      if (material instanceof PBRMaterial && material.name === 'paint') paintMaterials.add(material);
      if (material instanceof PBRMaterial && material.name === 'eye') eyeMaterials.add(material);
    }
    if (paintMaterials.size !== 1 || eyeMaterials.size !== 1)
      throw new Error(`Paint/eye materials: ${paintMaterials.size}/${eyeMaterials.size}`);
    paintTexture = makePaintTexture();
    for (const material of paintMaterials) {
      material.albedoColor = Color3.White();
      material.albedoTexture = paintTexture;
    }
    setCameraSide();
    template!.src = `/fish/${species}.svg`;
    download!.href = `/fish/${species}.svg`;
    const result: FishProbe = { status: 'PASS', species, side, webglVersion: engine.webGLVersion,
      paintMaterialCount: paintMaterials.size, protectedEyeMaterialCount: eyeMaterials.size, frames: 0 };
    if (engine.webGLVersion !== 2) throw new Error('WebGL 2 required');
    frameCount = 0;
    window.fishProbe = result;
    status!.textContent = `PASS: ${species}, ${sideLabel()}`;
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    window.fishProbe = { status: 'FAIL', species, side, error: message };
    status!.textContent = `FAIL: ${message}`;
  }
}

for (const button of document.querySelectorAll<HTMLButtonElement>('[data-fish]')) {
  button.addEventListener('click', () => {
    species = button.dataset.fish as FishId;
    void loadFish();
  });
}
for (const button of document.querySelectorAll<HTMLButtonElement>('[data-side]')) {
  button.addEventListener('click', () => {
    side = button.dataset.side as Side;
    setCameraSide();
    frameCount = 0;
    if (window.fishProbe?.status === 'PASS') {
      window.fishProbe.side = side;
      window.fishProbe.frames = 0;
    }
    status!.textContent = `PASS: ${species}, ${sideLabel()}`;
  });
}
void loadFish();
