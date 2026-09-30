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
        mesh.material?.name.endsWith('/paint') && mesh.material.getActiveTextures()?.[0]?.isReady())),
      null, { timeout: 10000 });
  } catch (error) {
    const meshes = await desktop.evaluate(() => window.coreAdapter.scene.meshes.map(mesh =>
      ({ name: mesh.name, material: mesh.material?.name, visible: mesh.isVisible,
        texture: mesh.material?.getActiveTextures()?.[0]?.url,
        textureReady: mesh.material?.getActiveTextures()?.[0]?.isReady() })));
    throw new Error(`GLB instances missing: ${JSON.stringify({ meshes, coralRequests, consoleErrors, errors })}`, { cause: error });
  }
  const models = await desktop.evaluate(() => {
    const meshes = window.coreAdapter.scene.meshes;
    const paint = id => meshes.find(mesh => mesh.name.startsWith(`${id}/`) &&
      mesh.material?.name.endsWith('/paint'))?.material;
    const paintTexture = id => paint(id)?.getActiveTextures()?.[0];
    const eye = id => meshes.find(mesh => mesh.name.startsWith(`${id}/`) &&
      mesh.material?.name === 'eye')?.material;
    return { distinctPaintMaterials: paint('fish-1') !== paint('fish-2'),
      distinctPaintTextures: paintTexture('fish-1') !== paintTexture('fish-2'),
      sharedEyeMaterial: !!eye('fish-1') && eye('fish-1') === eye('fish-2'),
      texturesReady: !!paintTexture('fish-1')?.isReady() && !!paintTexture('fish-2')?.isReady(),
      lightweightPaint: paint('fish-1')?.getClassName() === 'ShaderMaterial',
      firstVisible: meshes.some(mesh => mesh.name.startsWith('fish-1/') && mesh.isVisible),
      secondVisible: meshes.some(mesh => mesh.name.startsWith('fish-2/') && mesh.isVisible),
      activeMeshes: window.coreAdapter.scene.getActiveMeshes().length };
  });
  const { activeMeshes, ...modelChecks } = models;
  assert(activeMeshes >= 2, `GLB meshes were culled: ${activeMeshes}`);
  assert.deepEqual(modelChecks, { distinctPaintMaterials: true, distinctPaintTextures: true,
    sharedEyeMaterial: true, texturesReady: true, lightweightPaint: true,
    firstVisible: true, secondVisible: true });
  assert.equal(coralRequests, 1, 'one model download serves two Entity instances');
  const initialPose = await desktop.evaluate(async () => {
    const adapter = window.coreAdapter;
    adapter.applyPositions({ type: 'positions', schemaVersion: 1,
      sceneId: 'scene-1', sceneEpoch: 1, revision: 1, simulationTick: 9,
      positions: [{ id: 'fish-1', position: { x: 2, y: -1 }, depth: -1.2,
        heading: { x: 1, y: 0 }, headingDepth: 0 }],
    });
    await new Promise(resolve => setTimeout(resolve, 100));
    const eye = adapter.scene.meshes.find(mesh => mesh.name.startsWith('fish-1/') &&
      mesh.material?.name === 'eye');
    const tail = adapter.scene.meshes.find(mesh => mesh.name.startsWith('fish-1/') &&
      mesh.name.includes('tail-pivot') && mesh.material?.name.endsWith('/paint'));
    return { depth: adapter.scene.getTransformNodeByName('fish-1').position.z,
      eyeX: eye?.getBoundingInfo().boundingBox.centerWorld.x,
      tailX: tail?.getBoundingInfo().boundingBox.centerWorld.x };
  });
  assert(Math.abs(initialPose.depth + 1.2) < .01 && initialPose.eyeX > initialPose.tailX,
    `Initial depth correction must keep the nose forward: ${JSON.stringify(initialPose)}`);
  await desktop.evaluate(() => window.coreAdapter.applyPositions({
    type: 'positions', schemaVersion: 1, sceneId: 'scene-1', sceneEpoch: 1,
    revision: 1, simulationTick: 10,
    positions: [{ id: 'fish-1', position: { x: 3, y: -1 }, depth: 1.2,
      heading: { x: .92, y: 0 }, headingDepth: .38 }],
  }));
  await desktop.waitForFunction(() => {
    const marker = window.coreAdapter.scene.getTransformNodeByName('fish-1');
    return Math.abs(marker?.position.x - 3) < .01 && Math.abs(marker?.position.z - 1.2) < .01;
  },
    null, { timeout: 3000 });
  const depthVisual = await desktop.evaluate(async () => {
    const scene = window.coreAdapter.scene;
    const marker = scene.getTransformNodeByName('fish-1');
    const tail = scene.getNodeByName('fish-1/tail-pivot');
    const first = tail?.rotation.y;
    await new Promise(resolve => setTimeout(resolve, 120));
    const middle = tail?.rotation.y;
    await new Promise(resolve => setTimeout(resolve, 120));
    const yaw = marker.rotation.y;
    const eyeMesh = scene.meshes.find(mesh => mesh.name.startsWith('fish-1/') && mesh.material?.name === 'eye');
    const tailMesh = scene.meshes.find(mesh => mesh.name.startsWith('fish-1/') &&
      mesh.name.includes('tail-pivot') && mesh.material?.name.endsWith('/paint'));
    const eyeCenter = eyeMesh?.getBoundingInfo().boundingBox.centerWorld;
    const tailCenter = tailMesh?.getBoundingInfo().boundingBox.centerWorld;
    const noseVector = eyeCenter && tailCenter ? eyeCenter.subtract(tailCenter) : null;
    const noseDotMotion = noseVector &&
      (noseVector.x + noseVector.z * 2.4) /
      (Math.hypot(noseVector.x, noseVector.z) * Math.hypot(1, 2.4));
    return { depth: marker.position.z, scale: marker.scaling.x, yaw, noseDotMotion,
      tailPresent: !!tail, tailMoved: Math.max(Math.abs(middle - first),
        Math.abs(tail?.rotation.y - middle)) > .02,
      tailNames: scene.meshes.concat(scene.transformNodes).filter(node => node.name.includes('tail')).map(node => node.name) };
  });
  assert(Math.abs(depthVisual.depth - 1.2) < .01 && depthVisual.scale < .9 &&
    depthVisual.yaw < -.5 && depthVisual.noseDotMotion > .98 &&
    depthVisual.tailPresent && depthVisual.tailMoved,
    `Depth, turn and tail must animate: ${JSON.stringify(depthVisual)}`);
  const climbVisual = await desktop.evaluate(async () => {
    const adapter = window.coreAdapter;
    adapter.applyPositions({ type: 'positions', schemaVersion: 1,
      sceneId: 'scene-1', sceneEpoch: 1, revision: 1, simulationTick: 11,
      positions: [{ id: 'fish-1', position: { x: 3, y: -1 }, depth: 1.2,
        heading: { x: .8, y: .5 }, headingDepth: .3 }] });
    await new Promise(resolve => setTimeout(resolve, 300));
    const pitch = adapter.scene.getTransformNodeByName('fish-1').rotation.z;
    const eye = adapter.scene.meshes.find(mesh => mesh.name.startsWith('fish-1/') &&
      mesh.material?.name === 'eye');
    const tail = adapter.scene.meshes.find(mesh => mesh.name.startsWith('fish-1/') &&
      mesh.name.includes('tail-pivot') && mesh.material?.name.endsWith('/paint'));
    return { pitch, noseAboveTail: eye && tail &&
      eye.getBoundingInfo().boundingBox.centerWorld.y -
      tail.getBoundingInfo().boundingBox.centerWorld.y };
  });
  assert(climbVisual.pitch > .05 && climbVisual.noseAboveTail > .25,
    `Fish climbing in Y must raise its nose: ${JSON.stringify(climbVisual)}`);
  const reverseVisual = await desktop.evaluate(async () => {
    const adapter = window.coreAdapter;
    adapter.applyPositions({ type: 'positions', schemaVersion: 1,
      sceneId: 'scene-1', sceneEpoch: 1, revision: 1, simulationTick: 12,
      positions: [{ id: 'fish-1', position: { x: 3, y: -1 }, depth: 1.2,
        heading: { x: -1, y: 0 }, headingDepth: 0 }] });
    await new Promise(resolve => setTimeout(resolve, 1800));
    const eye = adapter.scene.meshes.find(mesh => mesh.name.startsWith('fish-1/') &&
      mesh.material?.name === 'eye');
    const tail = adapter.scene.meshes.find(mesh => mesh.name.startsWith('fish-1/') &&
      mesh.name.includes('tail-pivot') && mesh.material?.name.endsWith('/paint'));
    return { eyeX: eye?.getBoundingInfo().boundingBox.centerWorld.x,
      tailX: tail?.getBoundingInfo().boundingBox.centerWorld.x };
  });
  assert(reverseVisual.eyeX < reverseVisual.tailX,
    `Fish moving left must put its head before the tail: ${JSON.stringify(reverseVisual)}`);
  const afterStale = await desktop.evaluate(() => {
    const adapter = window.coreAdapter;
    adapter.applyPositions({ type: 'positions', schemaVersion: 1, sceneId: 'scene-1',
      sceneEpoch: 1, revision: 1, simulationTick: 9,
      positions: [{ id: 'fish-1', position: { x: -3, y: -1 }, heading: { x: -1, y: 0 } }] });
    return adapter.scene.getTransformNodeByName('fish-1').position.x;
  });
  assert(Math.abs(afterStale - 3) < .01, 'stale position frame must not move the mesh');
  const movingLeft = await desktop.evaluate(async () => {
    const adapter = window.coreAdapter;
    adapter.applyPositions({ type: 'positions', schemaVersion: 1,
      sceneId: 'scene-1', sceneEpoch: 1, revision: 1, simulationTick: 13,
      positions: [{ id: 'fish-1', position: { x: 2, y: -1 }, depth: 1.2,
        heading: { x: 1, y: 0 }, headingDepth: 0 }] });
    await new Promise(resolve => setTimeout(resolve, 120));
    const eye = adapter.scene.meshes.find(mesh => mesh.name.startsWith('fish-1/') &&
      mesh.material?.name === 'eye');
    const tail = adapter.scene.meshes.find(mesh => mesh.name.startsWith('fish-1/') &&
      mesh.name.includes('tail-pivot') && mesh.material?.name.endsWith('/paint'));
    return { eyeX: eye?.getBoundingInfo().boundingBox.centerWorld.x,
      tailX: tail?.getBoundingInfo().boundingBox.centerWorld.x,
      markerX: adapter.scene.getTransformNodeByName('fish-1')?.position.x };
  });
  assert(movingLeft.markerX < 3 && movingLeft.eyeX < movingLeft.tailX,
    `Fish must face its visible motion, even when heading arrives late: ${JSON.stringify(movingLeft)}`);
  const forwardLap = await desktop.evaluate(async () => {
    const adapter = window.coreAdapter;
    const marker = adapter.scene.getTransformNodeByName('fish-1');
    const eye = adapter.scene.meshes.find(mesh => mesh.name.startsWith('fish-1/') &&
      mesh.material?.name === 'eye');
    const tail = adapter.scene.meshes.find(mesh => mesh.name.startsWith('fish-1/') &&
      mesh.name.includes('tail-pivot') && mesh.material?.name.endsWith('/paint'));
    const targets = [
      { x: 1, depth: -1.2 }, // turn toward the glass
      { x: 3, depth: -1.2 }, // swim right along the glass
      { x: 4, depth: 0 }, // turn away at the right edge
      { x: 2, depth: 1.2 }, // swim left at the back
      { x: 0, depth: 0 }, // turn toward the glass at the left edge
    ];
    const checks = [];
    for (const [index, target] of targets.entries()) {
      const start = marker.position.clone();
      adapter.applyPositions({ type: 'positions', schemaVersion: 1,
        sceneId: 'scene-1', sceneEpoch: 1, revision: 1, simulationTick: 14 + index,
        positions: [{ id: 'fish-1', position: { x: target.x, y: -1 }, depth: target.depth,
          heading: { x: -1, y: 0 }, headingDepth: 0 }] });
      await new Promise(resolve => setTimeout(resolve, 550));
      const nose = eye.getBoundingInfo().boundingBox.centerWorld.subtract(
        tail.getBoundingInfo().boundingBox.centerWorld);
      const travel = { x: target.x - start.x, z: target.depth - start.z };
      checks.push((nose.x * travel.x + nose.z * travel.z) /
        (Math.hypot(nose.x, nose.z) * Math.hypot(travel.x, travel.z)));
    }
    return checks;
  });
  assert(forwardLap.every(dot => dot > .9),
    `Fish must lead with its nose throughout the depth lap: ${JSON.stringify(forwardLap)}`);
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
  const feed = await desktop.evaluate(() => {
    const adapter = window.coreAdapter;
    const id = 'feed-000000000000000000000000000000ab';
    const source = (remaining) => ({ id, interactionId: 'feed',
      point: { x: 0, y: 0 }, remaining, expiresAtTick: 300 });
    adapter.applyDelta({ schemaVersion: 1, sceneEpoch: 1, revision: 3,
      simulationTick: 20, upsert: [], remove: [], event: { type: 'interaction_state',
        activeActions: [source(10)], appliedCommandIds: ['command-1'], simulationTick: 20 } });
    const mesh = adapter.scene.getMeshByName(`${id}/source`);
    const startScale = mesh?.scaling.x;
    adapter.applyDelta({ schemaVersion: 1, sceneEpoch: 1, revision: 4,
      simulationTick: 40, upsert: [], remove: [], event: { type: 'interaction_state',
        activeActions: [source(9)], appliedCommandIds: [], simulationTick: 40 } });
    const afterEating = mesh?.scaling.x;
    adapter.applyDelta({ schemaVersion: 1, sceneEpoch: 1, revision: 5,
      simulationTick: 300, upsert: [], remove: [], event: { type: 'interaction_state',
        activeActions: [], appliedCommandIds: [], simulationTick: 300 } });
    const cleared = !adapter.scene.getMeshByName(`${id}/source`);
    adapter.applySnapshot({ schemaVersion: 1, sceneEpoch: 2, revision: 0,
      simulationTick: 40, worldId: 'underwater', worldVersion: 1,
      entities: [], activeActions: [source(9)] });
    const restored = !!adapter.scene.getMeshByName(`${id}/source`);
    adapter.applySnapshot({ schemaVersion: 1, sceneEpoch: 2, revision: 1,
      simulationTick: 301, worldId: 'underwater', worldVersion: 1,
      entities: [], activeActions: [] });
    return { startScale, afterEating, cleared, restored,
      clearedOnSnapshot: !adapter.scene.getMeshByName(`${id}/source`),
      materialCleared: !adapter.scene.materials.some(material => material.name === 'feed-source') };
  });
  assert(feed.startScale > feed.afterEating && feed.cleared && feed.restored &&
    feed.clearedOnSnapshot && feed.materialCleared,
    `Feed source must follow server state and reconnect snapshot: ${JSON.stringify(feed)}`);
  const boat = await desktop.evaluate(async () => {
    const adapter = window.coreAdapter;
    const id = 'boat-000000000000000000000000000000bc';
    const active = { id, interactionId: 'boat', point: { x: 1, y: 0 },
      position: { x: -7.05, y: 0 }, entry: { x: -7.05, y: 0 },
      exit: { x: 7.05, y: 0 }, expiresAtTick: 600 };
    adapter.applySnapshot({ schemaVersion: 1, sceneEpoch: 3, revision: 0,
      simulationTick: 0, worldId: 'underwater', worldVersion: 1,
      entities: [], activeActions: [active] });
    const root = adapter.scene.getTransformNodeByName(id);
    const created = !!root && !!adapter.scene.getMeshByName(`${id}/hull`);
    adapter.applyPositions({ type: 'positions', schemaVersion: 1, sceneId: 'scene-1',
      sceneEpoch: 3, revision: 0, simulationTick: 10, positions: [],
      actionPositions: [{ id, position: { x: -5.5, y: 0 } }] });
    await new Promise(resolve => setTimeout(resolve, 550));
    const moved = root?.position.x > -5.6;
    adapter.applyDelta({ schemaVersion: 1, sceneEpoch: 3, revision: 1,
      simulationTick: 150, upsert: [], remove: [], event: { type: 'interaction_state',
        activeActions: [], appliedCommandIds: [], simulationTick: 150 } });
    return { created, moved, removed: !adapter.scene.getTransformNodeByName(id),
      materialCleared: !adapter.scene.materials.some(material => material.name === 'boat') };
  });
  assert(boat.created && boat.moved && boat.removed && boat.materialCleared,
    `Boat must follow server frames and clean up after exit: ${JSON.stringify(boat)}`);
  const catalogVisual = await desktop.evaluate(() => {
    const adapter = window.coreAdapter;
    const feedId = 'feed-000000000000000000000000000000cd';
    const boatId = 'boat-000000000000000000000000000000de';
    const actionCatalog = [
      { id: 'feed', effect: 'attraction', label: 'Корм', allowedZoneId: 'water' },
      { id: 'boat', effect: 'threat', label: 'Лодка', allowedZoneId: 'water' },
      { id: 'feed-slow', effect: 'attraction', label: 'Медленный корм', allowedZoneId: 'water' },
      { id: 'boat-red', effect: 'threat', label: 'Красная лодка', allowedZoneId: 'water' },
    ];
    adapter.applySnapshot({ schemaVersion: 2, sceneEpoch: 4, revision: 0,
      simulationTick: 0, worldId: 'underwater', worldVersion: 1, entities: [], actionCatalog,
      activeActions: [
        { id: feedId, interactionId: 'feed-slow', effect: 'attraction',
          point: { x: 0, y: 0 }, remaining: 5, expiresAtTick: 100 },
        { id: boatId, interactionId: 'boat-red', effect: 'threat',
          point: { x: 2, y: 0 }, position: { x: -7, y: 0 },
          entry: { x: -7, y: 0 }, exit: { x: 7, y: 0 }, expiresAtTick: 200 },
      ] });
    const created = !!adapter.scene.getMeshByName(`${feedId}/source`) &&
      !!adapter.scene.getMeshByName(`${boatId}/hull`);
    adapter.applyDelta({ schemaVersion: 2, sceneEpoch: 4, revision: 1,
      simulationTick: 1, upsert: [], remove: [], event: { type: 'interaction_state',
        activeActions: [], appliedCommandIds: [], simulationTick: 1 } });
    return { created, removed: !adapter.scene.getMeshByName(`${feedId}/source`) &&
      !adapter.scene.getMeshByName(`${boatId}/hull`) };
  });
  assert(catalogVisual.created && catalogVisual.removed,
    `v2 actions must render by effect and clean up: ${JSON.stringify(catalogVisual)}`);
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
    mesh.material.getActiveTextures()?.[0]?.isReady()).length === 200,
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
    return { fish: new Set(scene.meshes.filter(mesh => mesh.name.startsWith('fish-load-') &&
      mesh.material?.name.endsWith('/paint')).map(mesh => mesh.name.split('/')[0])).size,
      meshes: scene.meshes.length, materials: scene.materials.length,
      frames: sorted.length, p95FrameMs: sorted[Math.floor(sorted.length * .95)] ?? null,
      width: window.coreAdapter.engine.getRenderWidth(),
      height: window.coreAdapter.engine.getRenderHeight() };
  });
  assert.equal(profile.fish, 100);
  assert(profile.frames > 10, `No sustained frames: ${JSON.stringify(profile)}`);
  console.log(`Short software-GPU 100-fish LOW probe: ${JSON.stringify(profile)}`);
  if (errors.length) throw new Error(`Browser errors: ${errors.join(' | ')}`);
  console.log('CORE-04 Chrome: fixed view, painted GLB, feed and boat state, interpolation and cleanup: PASS');
} finally {
  await browser?.close();
  server.kill();
}
