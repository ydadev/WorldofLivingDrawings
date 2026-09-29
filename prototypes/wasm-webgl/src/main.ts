import { ArcRotateCamera } from '@babylonjs/core/Cameras/arcRotateCamera';
import { Engine } from '@babylonjs/core/Engines/engine';
import { HemisphericLight } from '@babylonjs/core/Lights/hemisphericLight';
import { MeshBuilder } from '@babylonjs/core/Meshes/meshBuilder';
import { StandardMaterial } from '@babylonjs/core/Materials/standardMaterial';
import { Scene } from '@babylonjs/core/scene';
import { Vector3 } from '@babylonjs/core/Maths/math.vector';
import { Color3 } from '@babylonjs/core/Maths/math.color';
import './style.css';

interface ProbeExports {
  advance_phase(phase: number, delta: number): number;
}

declare global {
  interface Window {
    risk01?: { status: 'PASS' | 'FAIL'; webglVersion?: number; wasmPhase?: number; worker?: string; evalBlocked?: boolean; frames?: number; centerPixel?: number[]; error?: string };
  }
}

const status = document.querySelector<HTMLParagraphElement>('#status');
const canvas = document.querySelector<HTMLCanvasElement>('#scene');
if (!status || !canvas) throw new Error('Prototype markup is incomplete');

async function main(): Promise<void> {
  const worker = new Worker(new URL('./worker.ts', import.meta.url), { type: 'module' });
  const workerResult = await new Promise<string>((resolve, reject) => {
    const timeout = window.setTimeout(() => reject(new Error('Worker timed out')), 3000);
    worker.onmessage = (event: MessageEvent<string>) => {
      window.clearTimeout(timeout);
      resolve(event.data);
    };
    worker.onerror = () => {
      window.clearTimeout(timeout);
      reject(new Error('Worker failed'));
    };
    worker.postMessage('ready?');
  });
  worker.terminate();
  if (workerResult !== 'ready') throw new Error('Worker response mismatch');

  const response = await fetch('/sim.wasm');
  if (!response.ok) throw new Error(`WebAssembly HTTP ${response.status}`);
  const bytes = await response.arrayBuffer();
  const instance = await WebAssembly.instantiate(bytes);
  const { advance_phase } = instance.instance.exports as unknown as ProbeExports;
  const wasmPhase = advance_phase(0.75, 0.5);
  if (Math.abs(wasmPhase - 0.25) > 0.000001) throw new Error('WebAssembly result mismatch');

  let evalBlocked = false;
  try {
    Function('return 4')();
  } catch {
    evalBlocked = true;
  }
  if (!evalBlocked) throw new Error('CSP permitted JavaScript code generation');

  const engine = new Engine(canvas, true);
  if (engine.webGLVersion !== 2) {
    engine.dispose();
    throw new Error(`WebGL ${engine.webGLVersion} detected; WebGL 2 required`);
  }
  const scene = new Scene(engine);
  const camera = new ArcRotateCamera('camera', Math.PI / 2, Math.PI / 2.4, 5, Vector3.Zero(), scene);
  scene.activeCamera = camera;
  camera.attachControl(canvas, true);
  new HemisphericLight('light', new Vector3(0, 1, 0), scene);
  const probe = MeshBuilder.CreateSphere('probe', { diameter: 1, segments: 16 }, scene);
  const material = new StandardMaterial('probe-color', scene);
  material.emissiveColor = new Color3(1, 0.42, 0.08);
  probe.material = material;
  let phase = wasmPhase;
  let frames = 0;
  engine.runRenderLoop(() => {
    phase = advance_phase(phase, 0.002);
    probe.position.x = Math.sin(phase * Math.PI * 2) * 0.2;
    scene.render();
    if (window.risk01?.status === 'PASS') {
      window.risk01.frames = ++frames;
      const gl = canvas!.getContext('webgl2');
      if (gl) {
        const pixel = new Uint8Array(4);
        gl.readPixels(canvas!.width >> 1, canvas!.height >> 1, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, pixel);
        window.risk01.centerPixel = Array.from(pixel);
      }
    }
  });
  window.addEventListener('resize', () => engine.resize());
  window.risk01 = { status: 'PASS', webglVersion: engine.webGLVersion, wasmPhase, worker: workerResult, evalBlocked };
  status!.textContent = `PASS: WebAssembly, Worker и WebGL ${engine.webGLVersion}`;
}

main().catch((error: unknown) => {
  const message = error instanceof Error ? error.message : String(error);
  window.risk01 = { status: 'FAIL', error: message };
  status.textContent = `FAIL: ${message}`;
});
