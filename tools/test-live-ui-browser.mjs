import { spawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from '../prototypes/wasm-webgl/node_modules/playwright-core/index.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const web = path.join(root, 'prototypes/wasm-webgl');
const executable = process.env.LDW_CHROME_PATH || [
  'C:/Program Files/Google/Chrome/Application/chrome.exe', '/usr/bin/google-chrome', '/usr/bin/chromium',
].find(existsSync);
if (!executable) throw new Error('Set LDW_CHROME_PATH to Chrome');
const port = 4188;
const origin = `http://127.0.0.1:${port}`;
const sessionId = '00000000-0000-4000-8000-000000000001';
const server = spawn(process.execPath, [path.join(web, 'live-server.mjs')],
  { cwd: web, env: { ...process.env, LDW_LIVE_PORT: String(port) }, stdio: ['ignore', 'pipe', 'pipe'] });
let stderr = '';
server.stderr.on('data', chunk => { stderr += chunk.toString(); });
let browser;

function assert(condition, detail) { if (!condition) throw new Error(detail); }
async function point(page, u, v, touch = false) {
  const canvas = page.locator('#world-canvas');
  await canvas.scrollIntoViewIfNeeded();
  const box = await canvas.boundingBox();
  const inset = await canvas.evaluate(element => ({ left: element.clientLeft, top: element.clientTop,
    width: element.clientWidth, height: element.clientHeight }));
  const x = box.x + inset.left + inset.width * u;
  const y = box.y + inset.top + inset.height * v;
  if (touch) await page.touchscreen.tap(x, y);
  else await canvas.click({ position: { x: inset.left + inset.width * u,
    y: inset.top + inset.height * v } });
}
async function probe() { return (await (await fetch(`${origin}/probe`)).json()); }
async function waitCommands(count) {
  for (let i = 0; i < 60; i++) {
    const value = await probe();
    if (value.commands.length >= count) return value;
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  throw new Error(`Expected ${count} commands, got ${JSON.stringify(await probe())}`);
}

try {
  for (let i = 0; i < 60; i++) {
    if (server.exitCode !== null) throw new Error(`Live server exited: ${stderr}`);
    try { if ((await fetch(`${origin}/health`)).ok) break; } catch { /* wait */ }
    await new Promise(resolve => setTimeout(resolve, 150));
  }
  browser = await chromium.launch({ executablePath: executable, headless: true,
    args: ['--enable-unsafe-swiftshader', '--use-gl=angle', '--use-angle=swiftshader'] });
  const desktop = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const mobileContext = await browser.newContext({ viewport: { width: 390, height: 844 },
    isMobile: true, hasTouch: true, deviceScaleFactor: 2 });
  const mobile = await mobileContext.newPage();
  const viewerContext = await browser.newContext({ viewport: { width: 1280, height: 720 } });
  const viewer = await viewerContext.newPage();
  const errors = [];
  for (const page of [desktop, mobile, viewer]) page.on('pageerror', error => errors.push(error.message));
  await desktop.goto(`${origin}/world.html`);
  await desktop.locator('[name=login]').fill('fixture-owner');
  await desktop.locator('[name=password]').fill('fixture-password');
  await desktop.locator('[name=sessionId]').fill(sessionId);
  await desktop.locator('#access-form button[type=submit]').click();
  await desktop.locator('#action-feed').waitFor({ state: 'visible' });
  await desktop.waitForFunction(() => !document.querySelector('#action-feed').disabled);
  await desktop.locator('#invite').click();
  await desktop.waitForFunction(() => document.querySelector('#invitation')?.textContent.includes('123456'));
  assert((await desktop.locator('#invitation').textContent()).includes('123456'),
    'Owner should see pairing PIN');

  await viewer.goto(`${origin}/viewer.html?session=${sessionId}`);
  await viewer.locator('#access-form button[type=submit]').click();
  await viewer.waitForFunction(() => document.querySelector('#access-status')?.textContent.includes('87654321'));
  await desktop.locator('#approve-viewer-form [name=code]').fill('87654321');
  await desktop.locator('#approve-viewer-form button[type=submit]').click();
  await desktop.waitForFunction(() => document.querySelector('#viewer-approval-status')?.textContent.includes('разрешён просмотр'));
  await viewer.locator('#world-stage').waitFor({ state: 'visible', timeout: 15000 });
  await viewer.waitForFunction(() => document.querySelector('#interaction-status')?.textContent.includes('Режим просмотра'));
  assert(await viewer.locator('#action-panel').isHidden(), 'Read-only Viewer should not show action controls');

  await mobile.goto(`${origin}/controller.html?session=${sessionId}`);
  await mobile.locator('[name=pin]').fill('123456');
  await mobile.locator('#access-form button[type=submit]').click();
  await mobile.locator('#view-toggle').waitFor({ state: 'visible' });
  assert(await mobile.locator('#world-stage').isHidden(), 'Controller should open view on demand');
  await mobile.locator('#view-toggle').click();
  await mobile.waitForFunction(() => !document.querySelector('#action-boat').disabled);

  await desktop.locator('#action-feed').click();
  await point(desktop, .5, .5);
  let state = await waitCommands(1);
  assert(state.commands[0].interactionId === 'feed' &&
    Math.abs(state.commands[0].point.x) < .05 && Math.abs(state.commands[0].point.y) < .05,
  `Desktop feed point wrong: ${JSON.stringify(state.commands[0])}`);
  await desktop.waitForFunction(() => !document.querySelector('#action-feed').disabled);

  await mobile.locator('#action-boat').click();
  await point(mobile, .25, .75, true);
  state = await waitCommands(2);
  assert(state.commands[1].interactionId === 'boat' &&
    Math.abs(state.commands[1].point.x + 4) < .1 &&
    Math.abs(state.commands[1].point.y + 2.25) < .1,
  `Controller boat point wrong: ${JSON.stringify(state.commands[1])}`);
  await mobile.waitForFunction(() => !document.querySelector('#action-boat').disabled);

  await desktop.locator('#action-feed').click();
  await point(desktop, .25, .75);
  state = await waitCommands(3);
  assert(Math.hypot(state.commands[1].point.x - state.commands[2].point.x,
    state.commands[1].point.y - state.commands[2].point.y) < .04,
  `Screens disagree on world coordinates: ${JSON.stringify(state.commands)}`);
  await desktop.waitForFunction(() => !document.querySelector('#action-feed').disabled);

  await desktop.locator('#action-feed').click();
  await point(desktop, .99, .5);
  assert((await probe()).commands.length === 3, 'Tap outside water sent a command');
  await desktop.keyboard.press('Escape');
  await point(desktop, .5, .5);
  assert((await probe()).commands.length === 3, 'Escape failed to cancel');
  await desktop.locator('#action-feed').click();
  const dragBox = await desktop.locator('#world-canvas').boundingBox();
  await desktop.mouse.move(dragBox.x + dragBox.width / 2, dragBox.y + dragBox.height / 2);
  await desktop.mouse.down();
  await desktop.mouse.move(dragBox.x + dragBox.width / 2 + 30, dragBox.y + dragBox.height / 2);
  await desktop.mouse.up();
  assert((await probe()).commands.length === 3, 'Drag sent a command');
  await desktop.keyboard.press('Escape');

  await desktop.locator('#action-boat').click();
  await desktop.keyboard.press('ArrowRight');
  await desktop.keyboard.press('Enter');
  state = await waitCommands(4);
  assert(Math.abs(state.commands[3].point.x - .25) < .01 &&
    Math.abs(state.commands[3].point.y) < .01,
  `Keyboard cursor wrong: ${JSON.stringify(state.commands[3])}`);
  await desktop.waitForFunction(() => !document.querySelector('#action-boat').disabled);

  await desktop.locator('#action-boat').click();
  await point(desktop, .5, .1);
  state = await waitCommands(5);
  assert(!state.commands[4].accepted, 'Blocked boat route should be rejected');
  await desktop.waitForFunction(() => document.querySelector('#interaction-status')
    .textContent.includes('лодка не сможет пройти'));

  const interactiveContext = await browser.newContext({ viewport: { width: 1280, height: 720 } });
  const interactiveViewer = await interactiveContext.newPage();
  interactiveViewer.on('pageerror', error => errors.push(error.message));
  await interactiveViewer.goto(`${origin}/viewer.html?session=${sessionId}`);
  await interactiveViewer.locator('#access-form button[type=submit]').click();
  await interactiveViewer.waitForFunction(() => document.querySelector('#access-status')?.textContent.includes('87654321'));
  await desktop.locator('#approve-viewer-form [name=interact]').check();
  await desktop.locator('#approve-viewer-form button[type=submit]').click();
  await desktop.waitForFunction(() => document.querySelector('#viewer-approval-status')?.textContent.includes('просмотр и действия'));
  await interactiveViewer.waitForFunction(() => !document.querySelector('#action-feed').disabled,
    null, { timeout: 15000 });
  await interactiveViewer.locator('#action-feed').click();
  await point(interactiveViewer, .5, .5);
  state = await waitCommands(6);
  assert(state.commands[5].interactionId === 'feed' && state.commands[5].accepted,
    'Interactive Viewer should send an accepted command');

  await mobile.locator('#view-toggle').click();
  assert(await mobile.locator('#world-stage').isHidden(), 'Controller close should hide view');
  await mobile.locator('#view-toggle').click();
  await mobile.waitForFunction(() => !document.querySelector('#action-feed').disabled);
  await fetch(`${origin}/drop`);
  await Promise.all([desktop.waitForFunction(() => document.querySelector('#action-feed').disabled),
    mobile.waitForFunction(() => document.querySelector('#action-feed').disabled),
    interactiveViewer.waitForFunction(() => document.querySelector('#action-feed').disabled)]);
  await Promise.all([desktop.waitForFunction(() => !document.querySelector('#action-feed').disabled),
    mobile.waitForFunction(() => !document.querySelector('#action-feed').disabled),
    interactiveViewer.waitForFunction(() => !document.querySelector('#action-feed').disabled),
    viewer.waitForFunction(() => document.querySelector('#interaction-status')?.textContent.includes('Режим просмотра'))]);
  assert(errors.length === 0, `Browser errors: ${errors.join('; ')}`);
  console.log('CORE-04 live UI: Owner + Viewer + Controller, coordinates, keyboard, rejection and reconnect: PASS');
} finally {
  await browser?.close();
  server.kill();
}
