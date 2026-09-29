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
import type { PaintLayout } from './paint-core';
import './style.css';

interface CapturePass {
  status: 'PASS';
  pixels: ArrayBuffer;
  width: 512;
  height: 512;
  templateId: string;
  templateVersion: number;
  layoutHash: string;
  markerFormatVersion: number;
  sourceKind: 'paper';
  colorSpace: 'sRGB';
  markers: Record<string, [number, number]>;
}
interface CaptureFail { status: 'FAIL'; reason: string }
type CaptureResponse = CapturePass | CaptureFail;
interface PaperResult extends Omit<CapturePass, 'pixels' | 'width' | 'height' | 'markers' | 'status'> {
  image: Blob;
}
declare global { interface Window {
  captureProbe?: { status: string; templateId?: string; reason?: string; centerPixel?: number[];
    frames?: number; modelPixel?: number[] };
  captureResult?: () => PaperResult | undefined;
} }

function required<T extends Element>(selector: string): T {
  const element = document.querySelector<T>(selector);
  if (!element) throw new Error(`Missing ${selector}`);
  return element;
}
const fileInput = required<HTMLInputElement>('#capture-file');
const status = required<HTMLParagraphElement>('#capture-status');
const resultCanvas = required<HTMLCanvasElement>('#capture-result');
const modelCanvas = required<HTMLCanvasElement>('#capture-model');
const download = required<HTMLButtonElement>('#capture-download');
const resultContext = resultCanvas.getContext('2d', { willReadFrequently: true })!;
const engine = new Engine(modelCanvas, true);
const scene = new Scene(engine);
scene.clearColor = new Color4(0.04, .2, .28, 1);
const camera = new ArcRotateCamera('capture-preview', Math.PI / 2, Math.PI / 2, 6.3, Vector3.Zero(), scene);
scene.activeCamera = camera;
new HemisphericLight('capture-light', new Vector3(0, 1, 0), scene).intensity = 1.7;
let previewActive = true;
engine.runRenderLoop(() => {
  if (!previewActive) return;
  scene.render();
  if (window.captureProbe?.status === 'PASS') {
    window.captureProbe.frames = (window.captureProbe.frames ?? 0) + 1;
    const gl = modelCanvas.getContext('webgl2');
    if (gl) {
      const pixel = new Uint8Array(4);
      gl.readPixels(modelCanvas.width >> 1, modelCanvas.height >> 1, 1, 1,
        gl.RGBA, gl.UNSIGNED_BYTE, pixel);
      window.captureProbe.modelPixel = Array.from(pixel);
    }
  }
});
window.addEventListener('message', event => {
  if (event.origin === location.origin && event.source === window.parent &&
      event.data?.type === 'ldw-preview' && typeof event.data.active === 'boolean')
    previewActive = event.data.active;
});
window.addEventListener('resize', () => engine.resize());
let roots: Awaited<ReturnType<typeof ImportMeshAsync>>['meshes'] = [];
let texture: DynamicTexture | undefined;
let result: PaperResult | undefined;

const messages: Record<string, string> = {
  IMAGE_SIZE: 'Снимок слишком маленький или большой. Сфотографируй лист целиком в разрешении до 16 Мп.',
  MARKERS_MISSING: 'Не найдены все четыре QR-маркера. Сфотографируй весь лист при хорошем свете.',
  MARKER_FORMAT: 'Метка листа не распознана. Используй шаблон этого проекта.',
  TEMPLATE_MISMATCH: 'Версия листа не совпадает с установленным шаблоном. Скачай и распечатай новый лист.',
  DUPLICATE_MARKER: 'Метки листа неразличимы. Сделай снимок целого листа ещё раз.',
  INVALID_GEOMETRY: 'Лист снят под слишком большим углом. Сфотографируй его почти сверху.',
  IMAGE_BOUNDS: 'Часть рисунка оказалась за границей снимка. Сфотографируй лист целиком.',
};

async function process(file: File): Promise<void> {
  result = undefined;
  download.disabled = true;
  resultContext.fillStyle = '#fff';
  resultContext.fillRect(0, 0, 512, 512);
  roots.forEach(mesh => mesh.dispose(false, true));
  roots = [];
  texture?.dispose();
  texture = undefined;
  window.captureProbe = { status: 'LOADING' };
  status.textContent = 'Обработка фото на этом устройстве…';
  if (!['image/jpeg', 'image/png'].includes(file.type) || file.size > 16_000_000) {
    status.textContent = 'Выбери JPEG или PNG не больше 16 МБ. HEIC пока не поддерживается: пересними в JPEG.';
    window.captureProbe = { status: 'FAIL', reason: 'UNSUPPORTED_FORMAT' };
    return;
  }
  fileInput.disabled = true;
  let worker: Worker | undefined;
  try {
    const [format, ...layouts] = await Promise.all([
      fetch('/fish/paper-format.json').then(response => response.json()),
      ...['coral', 'stream'].map(id => fetch(`/fish/${id}.layout.json`).then(response => response.json() as Promise<PaintLayout>)),
    ]);
    const bitmap = await createImageBitmap(file);
    if (bitmap.width * bitmap.height > 16_000_000 || bitmap.width < 600 || bitmap.height < 600) {
      bitmap.close();
      throw new Error('IMAGE_SIZE');
    }
    const photo = document.createElement('canvas');
    photo.width = bitmap.width;
    photo.height = bitmap.height;
    const context = photo.getContext('2d', { willReadFrequently: true })!;
    context.drawImage(bitmap, 0, 0);
    bitmap.close();
    const image = context.getImageData(0, 0, photo.width, photo.height);
    photo.width = photo.height = 0;
    worker = new Worker(new URL('./capture-worker.ts', import.meta.url), { type: 'module' });
    const response = await new Promise<CaptureResponse>((resolve, reject) => {
      const timeout = setTimeout(() => reject(new Error('TIMEOUT')), 30_000);
      worker!.onmessage = (event: MessageEvent<CaptureResponse>) => { clearTimeout(timeout); resolve(event.data); };
      worker!.onerror = event => { clearTimeout(timeout); reject(new Error(event.message)); };
      worker!.postMessage({ pixels: image.data.buffer, width: image.width, height: image.height,
        layouts, format }, [image.data.buffer]);
    });
    if (response.status === 'FAIL') throw new Error(response.reason);
    resultContext.putImageData(new ImageData(new Uint8ClampedArray(response.pixels), 512, 512), 0, 0);
    const imageBlob = await new Promise<Blob>((resolve, reject) => resultCanvas.toBlob(blob =>
      blob ? resolve(blob) : reject(new Error('PNG_EXPORT')), 'image/png'));
    const { pixels: _pixels, width: _width, height: _height, markers: _markers, status: _status, ...metadata } = response;
    result = { ...metadata, image: imageBlob };
    await showModel(response.templateId);
    download.disabled = false;
    status.textContent = `Распознан шаблон ${response.templateId} v${response.templateVersion}. Проверь рисунок перед добавлением.`;
    const centerPixel = Array.from(resultContext.getImageData(256, 256, 1, 1).data);
    window.captureProbe = { status: 'PASS', templateId: response.templateId, centerPixel, frames: 0 };
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error);
    window.captureProbe = { status: 'FAIL', reason };
    status.textContent = messages[reason] ?? `Не удалось обработать фото: ${reason}`;
  } finally {
    worker?.terminate();
    fileInput.disabled = false;
    fileInput.value = '';
  }
}

async function showModel(species: string): Promise<void> {
  const imported = await ImportMeshAsync(`/fish/${species}.glb`, scene);
  roots.forEach(mesh => mesh.dispose(false, true));
  texture?.dispose();
  roots = imported.meshes;
  const paints = new Set<PBRMaterial>();
  for (const mesh of roots) if (mesh.material instanceof PBRMaterial && mesh.material.name === 'paint') paints.add(mesh.material);
  if (paints.size !== 1) throw new Error('MODEL_MATERIAL');
  texture = new DynamicTexture('paper-paint', { width: 512, height: 512 }, scene, false);
  texture.getContext().drawImage(resultCanvas, 0, 0);
  texture.update(false);
  for (const material of paints) { material.albedoColor = Color3.White(); material.albedoTexture = texture; }
}

fileInput.addEventListener('change', () => { const file = fileInput.files?.[0]; if (file) void process(file); });
download.addEventListener('click', () => {
  if (!result) return;
  const url = URL.createObjectURL(result.image);
  const link = document.createElement('a');
  link.href = url;
  link.download = `${result.templateId}-v${result.templateVersion}-paper.png`;
  link.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
});
window.captureResult = () => result;
