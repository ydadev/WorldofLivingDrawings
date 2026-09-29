import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { existsSync, mkdirSync } from 'node:fs';
import { createRequire } from 'node:module';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from '../prototypes/wasm-webgl/node_modules/playwright-core/index.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const web = path.join(root, 'prototypes/wasm-webgl');
const { PNG } = createRequire(import.meta.url)(path.join(web, 'node_modules/pngjs'));
const chrome = process.env.LDW_CHROME_PATH || [
  'C:/Program Files/Google/Chrome/Application/chrome.exe', '/usr/bin/google-chrome', '/usr/bin/chromium',
].find(existsSync);
if (!chrome) throw new Error('Set LDW_CHROME_PATH to installed Chrome');
const server = spawn(process.execPath, [path.join(web, 'node_modules/vite/bin/vite.js'),
  'preview', '--host', '127.0.0.1', '--port', '4188', '--strictPort'],
  { cwd: web, stdio: ['ignore', 'pipe', 'pipe'] });
let stderr = '';
server.stderr.on('data', chunk => { stderr += chunk.toString(); });
let browser;
try {
  let ready = false;
  for (let i = 0; i < 60; i++) {
    if (server.exitCode !== null) throw new Error(`Preview exited: ${stderr}`);
    try { if ((await fetch('http://127.0.0.1:4188/core.html')).ok) { ready = true; break; } }
    catch { /* wait */ }
    await new Promise(resolve => setTimeout(resolve, 250));
  }
  if (!ready) throw new Error(`Preview not ready: ${stderr}`);
  browser = await chromium.launch({ executablePath: chrome, headless: true,
    args: ['--enable-unsafe-swiftshader', '--use-gl=angle', '--use-angle=swiftshader'] });
  const desktop = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const mobile = await browser.newPage({ viewport: { width: 390, height: 844 } });
  const errors = [];
  const consoleErrors = [];
  let coralRequests = 0;
  desktop.on('request', request => { if (request.url().endsWith('/fish/coral.glb')) coralRequests++; });
  desktop.on('console', message => { if (message.type() === 'error') consoleErrors.push(message.text()); });
  for (const page of [desktop, mobile]) page.on('pageerror', error => errors.push(error.message));
  await Promise.all([desktop.goto('http://127.0.0.1:4188/core.html'),
    mobile.goto('http://127.0.0.1:4188/core.html')]);
  for (const page of [desktop, mobile]) {
    await page.waitForFunction(() => window.coreProbe?.ready, null, { timeout: 30000 });
    assert.equal(await page.evaluate(() => window.coreProbe.webglVersion), 2);
  }
  const point = async (page, u, v) => {
    const canvas = page.locator('#core-scene');
    await canvas.scrollIntoViewIfNeeded();
    const box = await canvas.boundingBox();
    const padding = await canvas.evaluate(element => ({ x: element.clientLeft, y: element.clientTop,
      width: element.clientWidth, height: element.clientHeight }));
    return page.evaluate(({ x, y }) => window.coreAdapter.pickWorldPoint(x, y),
      { x: box.x + padding.x + padding.width * u, y: box.y + padding.y + padding.height * v });
  };
  const desktopPoint = await point(desktop, .25, .75);
  const mobilePoint = await point(mobile, .25, .75);
  assert(desktopPoint && mobilePoint);
  assert(Math.hypot(desktopPoint.x - mobilePoint.x, desktopPoint.y - mobilePoint.y) < .03,
    `Renderer adapter point mapping differs: ${JSON.stringify({ desktopPoint, mobilePoint })}`);
  assert(Math.abs(desktopPoint.x + 4) < .08 && Math.abs(desktopPoint.y + 2.25) < .08);
  const result = await desktop.evaluate(() => {
    const adapter = window.coreAdapter;
    let gapRejected = false, wrongWorldRejected = false;
    try { adapter.applyDelta({ schemaVersion: 1, sceneEpoch: 1, revision: 2,
      simulationTick: 1, upsert: [], remove: [] }); } catch { gapRejected = true; }
    try { adapter.applySnapshot({ schemaVersion: 1, sceneEpoch: 1, revision: 0,
      simulationTick: 0, worldId: 'other', worldVersion: 1, entities: [] }); }
    catch { wrongWorldRejected = true; }
    adapter.applyDelta({ schemaVersion: 1, sceneEpoch: 1, revision: 1,
      simulationTick: 1, upsert: [
        { id: 'fish-1', definitionId: 'coral-fish',
          definitionVersion: 1, position: { x: 2, y: -1 }, paintBlobId: 'red' },
        { id: 'fish-2', definitionId: 'coral-fish',
          definitionVersion: 1, position: { x: -2, y: 1 }, paintBlobId: 'blue' },
      ], remove: [] });
    const mesh = adapter.scene.getTransformNodeByName('fish-1');
    return { gapRejected, wrongWorldRejected, inserted: mesh?.position.asArray() };
  });
  assert(result.gapRejected && result.wrongWorldRejected);
  assert.deepEqual(result.inserted, [2, -1, 0]);
  try {
    await desktop.waitForFunction(() => ['fish-1', 'fish-2'].every(id =>
      window.coreAdapter.scene.meshes.some(mesh => mesh.name.startsWith(`${id}/`) &&
        mesh.material?.name.endsWith('/paint') && mesh.material.albedoTexture?.isReady())),
      null, { timeout: 10000 });
  } catch (error) {
    const meshes = await desktop.evaluate(() => window.coreAdapter.scene.meshes.map(mesh =>
      ({ name: mesh.name, material: mesh.material?.name, visible: mesh.isVisible,
        texture: mesh.material?.albedoTexture?.url, textureReady: mesh.material?.albedoTexture?.isReady() })));
    throw new Error(`GLB instances missing: ${JSON.stringify({ meshes, coralRequests, consoleErrors, errors })}`, { cause: error });
  }
  const models = await desktop.evaluate(() => {
    const meshes = window.coreAdapter.scene.meshes;
    const paint = id => meshes.find(mesh => mesh.name.startsWith(`${id}/`) &&
      mesh.material?.name.endsWith('/paint'))?.material;
    const eye = id => meshes.find(mesh => mesh.name.startsWith(`${id}/`) &&
      mesh.material?.name === 'eye-white')?.material;
    return { distinctPaintMaterials: paint('fish-1') !== paint('fish-2'),
      distinctPaintTextures: paint('fish-1')?.albedoTexture !== paint('fish-2')?.albedoTexture,
      sharedEyeMaterial: !!eye('fish-1') && eye('fish-1') === eye('fish-2'),
      texturesReady: !!paint('fish-1')?.albedoTexture?.isReady() && !!paint('fish-2')?.albedoTexture?.isReady(),
      firstVisible: meshes.some(mesh => mesh.name.startsWith('fish-1/') && mesh.isVisible),
      secondVisible: meshes.some(mesh => mesh.name.startsWith('fish-2/') && mesh.isVisible),
      activeMeshes: window.coreAdapter.scene.getActiveMeshes().length };
  });
  const { activeMeshes, ...modelChecks } = models;
  assert(activeMeshes >= 2, `GLB meshes were culled: ${activeMeshes}`);
  assert.deepEqual(modelChecks, { distinctPaintMaterials: true, distinctPaintTextures: true,
    sharedEyeMaterial: true, texturesReady: true, firstVisible: true, secondVisible: true });
  assert.equal(coralRequests, 1, 'one model download serves two Entity instances');
  await desktop.evaluate(() => window.coreAdapter.applyPositions({
    type: 'positions', schemaVersion: 1, sceneId: 'scene-1', sceneEpoch: 1,
    revision: 1, simulationTick: 10,
    positions: [{ id: 'fish-1', position: { x: 3, y: -1 }, heading: { x: 1, y: 0 } }],
  }));
  await desktop.waitForFunction(() => Math.abs(window.coreAdapter.scene.getTransformNodeByName('fish-1')?.position.x - 3) < .01,
    null, { timeout: 3000 });
  const afterStale = await desktop.evaluate(() => {
    const adapter = window.coreAdapter;
    adapter.applyPositions({ type: 'positions', schemaVersion: 1, sceneId: 'scene-1',
      sceneEpoch: 1, revision: 1, simulationTick: 9,
      positions: [{ id: 'fish-1', position: { x: -3, y: -1 }, heading: { x: -1, y: 0 } }] });
    return adapter.scene.getTransformNodeByName('fish-1').position.x;
  });
  assert(Math.abs(afterStale - 3) < .01, 'stale position frame must not move the mesh');
  mkdirSync(path.join(root, '.local'), { recursive: true });
  const screenshot = PNG.sync.read(await desktop.screenshot({ path: path.join(root, '.local/core01-renderer.png') }));
  let redPixels = 0, bluePixels = 0;
  for (let offset = 0; offset < screenshot.data.length; offset += 4) {
    const [red, green, blue] = screenshot.data.subarray(offset, offset + 3);
    if (red > 150 && green < 130 && blue < 130) redPixels++;
    if (blue > 180 && red < 100 && green > 70 && green < 160) bluePixels++;
  }
  assert(redPixels > 1000 && bluePixels > 1000,
    `Painted fish are not visible in screenshot: ${JSON.stringify({ redPixels, bluePixels })}`);
  const removed = await desktop.evaluate(() => {
    const adapter = window.coreAdapter;
    adapter.applyDelta({ schemaVersion: 1, sceneEpoch: 1, revision: 2,
      simulationTick: 2, upsert: [], remove: ['fish-1', 'fish-2'] });
    return !adapter.scene.getTransformNodeByName('fish-1') && !adapter.scene.getTransformNodeByName('fish-2') &&
      !adapter.scene.meshes.some(mesh => /^fish-[12]\//.test(mesh.name)) &&
      !adapter.scene.materials.some(material => /^fish-[12]\/paint$/.test(material.name));
  });
  assert(removed);
  const budget = await desktop.evaluate(() => {
    const adapter = window.coreAdapter;
    const canvas = document.querySelector('#core-scene');
    canvas.style.width = '3840px';
    canvas.style.height = '2160px';
    adapter.setRenderScale(1);
    const standard = [adapter.engine.getRenderWidth(), adapter.engine.getRenderHeight()];
    adapter.setRenderScale(.75);
    const reduced = [adapter.engine.getRenderWidth(), adapter.engine.getRenderHeight()];
    canvas.style.removeProperty('width');
    canvas.style.removeProperty('height');
    adapter.setRenderScale(1);
    return { standard, reduced };
  });
  assert(budget.standard[0] <= 1280 && budget.standard[1] <= 720, `LOW cap: ${JSON.stringify(budget)}`);
  assert(budget.reduced[0] <= 960 && budget.reduced[1] <= 540, `LOW scale: ${JSON.stringify(budget)}`);
  await desktop.evaluate(() => {
    const entities = Array.from({ length: 100 }, (_, index) => ({
      id: `fish-load-${index}`, definitionId: index % 2 ? 'coral-fish' : 'stream-fish',
      definitionVersion: 1,
      paintBlobId: `load-${index}`,
      position: { x: -6.7 + (index % 10) * 1.45, y: -3.4 + Math.floor(index / 10) * .74 },
    }));
    window.coreAdapter.applySnapshot({ schemaVersion: 1, sceneEpoch: 2, revision: 0,
      simulationTick: 0, worldId: 'underwater', worldVersion: 1, entities });
    window.coreAdapter.setRenderScale(.75);
  });
  await desktop.waitForFunction(() => window.coreAdapter.scene.meshes.filter(mesh =>
    mesh.name.startsWith('fish-load-') && mesh.material?.name.endsWith('/paint') &&
    mesh.material.albedoTexture?.isReady()).length === 100,
  null, { timeout: 30000 });
  const profile = await desktop.evaluate(async () => {
    const scene = window.coreAdapter.scene;
    const samples = [];
    await new Promise(resolve => setTimeout(resolve, 3000));
    let last = performance.now();
    const observer = scene.onAfterRenderObservable.add(() => {
      const now = performance.now();
      samples.push(now - last);
      last = now;
    });
    await new Promise(resolve => setTimeout(resolve, 5000));
    scene.onAfterRenderObservable.remove(observer);
    const sorted = samples.slice(5).sort((a, b) => a - b);
    return { fish: scene.meshes.filter(mesh => mesh.name.startsWith('fish-load-') &&
      mesh.material?.name.endsWith('/paint')).length,
      meshes: scene.meshes.length, materials: scene.materials.length,
      frames: sorted.length, p95FrameMs: sorted[Math.floor(sorted.length * .95)] ?? null,
      width: window.coreAdapter.engine.getRenderWidth(),
      height: window.coreAdapter.engine.getRenderHeight() };
  });
  assert.equal(profile.fish, 100);
  assert(profile.frames > 10, `No sustained frames: ${JSON.stringify(profile)}`);
  console.log(`Short software-GPU 100-fish LOW probe: ${JSON.stringify(profile)}`);
  if (errors.length) throw new Error(`Browser errors: ${errors.join(' | ')}`);
  console.log('CORE-04 Chrome: fixed view, snapshot/delta, two painted GLB instances, interpolation and cleanup: PASS');
} finally {
  await browser?.close();
  server.kill();
}
