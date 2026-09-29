import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { existsSync, mkdirSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from '../prototypes/wasm-webgl/node_modules/playwright-core/index.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const web = path.join(root, 'prototypes/wasm-webgl');
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
      simulationTick: 1, upsert: [{ id: 'fish-1', definitionId: 'coral-fish',
        definitionVersion: 1, position: { x: 2, y: -1 } }], remove: [] });
    const mesh = adapter.scene.getMeshByName('fish-1');
    return { gapRejected, wrongWorldRejected, inserted: mesh?.position.asArray() };
  });
  assert(result.gapRejected && result.wrongWorldRejected);
  assert.deepEqual(result.inserted, [2, -1, 0]);
  await desktop.waitForTimeout(100);
  mkdirSync(path.join(root, '.local'), { recursive: true });
  await desktop.screenshot({ path: path.join(root, '.local/core01-renderer.png') });
  const removed = await desktop.evaluate(() => {
    const adapter = window.coreAdapter;
    adapter.applyDelta({ schemaVersion: 1, sceneEpoch: 1, revision: 2,
      simulationTick: 2, upsert: [], remove: ['fish-1'] });
    return !adapter.scene.getMeshByName('fish-1');
  });
  assert(removed);
  if (errors.length) throw new Error(`Browser errors: ${errors.join(' | ')}`);
  console.log('CORE-01 Chrome: WebGL2, fixed view, same point on desktop/mobile, snapshot/delta/gap: PASS');
} finally {
  await browser?.close();
  server.kill();
}
