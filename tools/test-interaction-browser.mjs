import { spawn } from 'node:child_process';
import { existsSync, mkdirSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from '../prototypes/wasm-webgl/node_modules/playwright-core/index.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const web = path.join(root, 'prototypes/wasm-webgl');
const executable = process.env.LDW_CHROME_PATH || [
  'C:/Program Files/Google/Chrome/Application/chrome.exe', '/usr/bin/google-chrome', '/usr/bin/chromium',
].find(existsSync);
if (!executable) throw new Error('Set LDW_CHROME_PATH to an installed Chrome executable');
const server = spawn(process.execPath, [path.join(web, 'interaction-server.mjs')],
  { cwd: web, env: { ...process.env, LDW_INTERACTION_PORT: '4187' }, stdio: ['ignore', 'pipe', 'pipe'] });
let stderr = '';
let stdout = '';
server.stderr.on('data', chunk => { stderr += chunk.toString(); });
server.stdout.on('data', chunk => { stdout += chunk.toString(); });
let browser;
try {
  let ready = false;
  let lastError = '';
  for (let i = 0; i < 60; i++) {
    if (server.exitCode !== null) throw new Error(`Interaction server exited: ${stderr}`);
    try { if ((await fetch('http://127.0.0.1:4187/health')).ok) { ready = true; break; } }
    catch (error) { lastError = `${String(error)} ${String(error?.cause)}`; }
    await new Promise(resolve => setTimeout(resolve, 250));
  }
  if (!ready) throw new Error(`Interaction server not ready: stdout=${stdout} stderr=${stderr} lastError=${lastError} pid=${server.pid}`);
  browser = await chromium.launch({ executablePath: executable, headless: true,
    args: ['--enable-unsafe-swiftshader', '--use-gl=angle', '--use-angle=swiftshader'] });
  const desktop = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const mobileContext = await browser.newContext({ viewport: { width: 390, height: 844 }, isMobile: true,
    hasTouch: true, deviceScaleFactor: 2 });
  const mobile = await mobileContext.newPage();
  const errors = [];
  for (const page of [desktop, mobile]) page.on('pageerror', error => errors.push(error.message));
  await Promise.all([desktop.goto('http://127.0.0.1:4187/interaction.html'),
    mobile.goto('http://127.0.0.1:4187/interaction.html')]);
  for (const page of [desktop, mobile]) {
    await page.waitForFunction(() => window.interactionProbe?.connected && window.interactionProbe?.frames >= 20,
      null, { timeout: 30000 });
    const probe = await page.evaluate(() => window.interactionProbe);
    if (probe.webglVersion !== 2 || probe.revision !== 0) throw new Error(`Initial scene not ready: ${JSON.stringify(probe)}`);
  }
  const point = async (page, u, v, touch = false) => {
    const canvas = page.locator('#interaction-scene');
    await canvas.scrollIntoViewIfNeeded();
    const box = await canvas.boundingBox();
    const content = await canvas.evaluate(element => ({ left: element.clientLeft, top: element.clientTop,
      width: element.clientWidth, height: element.clientHeight }));
    const localX = content.left + content.width * u, localY = content.top + content.height * v;
    const x = box.x + localX, y = box.y + localY;
    if (touch) await page.touchscreen.tap(x, y);
    else await canvas.click({ position: { x: localX, y: localY } });
  };
  const waitRevision = async revision => {
    for (const page of [desktop, mobile]) {
      try { await page.waitForFunction(value => window.interactionProbe?.revision === value, revision, { timeout: 15000 }); }
      catch (error) {
        const states = await Promise.all([desktop.evaluate(() => ({ ...window.interactionProbe,
          status: document.querySelector('#interaction-status').textContent,
          pressed: document.querySelector('#action-feed').getAttribute('aria-pressed') })),
          mobile.evaluate(() => window.interactionProbe)]);
        throw new Error(`Revision ${revision} missing: ${JSON.stringify(states)} / pageErrors=${errors} / ${error}`);
      }
    }
  };
  const assertSame = async revision => {
    const a = await desktop.evaluate(() => window.interactionProbe);
    const b = await mobile.evaluate(() => window.interactionProbe);
    if (a.revision !== revision || b.revision !== revision ||
        JSON.stringify(a.events) !== JSON.stringify(b.events))
      throw new Error(`Screens disagree at revision ${revision}: ${JSON.stringify(a)} / ${JSON.stringify(b)}`);
    return [a, b];
  };
  await desktop.locator('#action-feed').click();
  await point(desktop, .5, .5);
  await waitRevision(1);
  let [a, b] = await assertSame(1);
  if (a.events[0].action !== 'feed' || Math.abs(a.events[0].x) > .05 || Math.abs(a.events[0].y) > .05 ||
      a.acknowledgments.length !== 1 || b.acknowledgments.length !== 0)
    throw new Error(`Desktop center feed wrong: ${JSON.stringify(a)} canvas=${JSON.stringify(await desktop.locator('#interaction-scene').boundingBox())}`);
  await mobile.locator('#action-boat').click();
  await point(mobile, .25, .75, true);
  await waitRevision(2);
  [a, b] = await assertSame(2);
  if (a.events[1].action !== 'boat' || b.acknowledgments.length !== 1 ||
      Math.abs(a.events[1].x + 4) > .08 || Math.abs(a.events[1].y + 2.25) > .08)
    throw new Error(`Mobile point mapping wrong: ${JSON.stringify(b)}`);
  await desktop.locator('#action-feed').click();
  await point(desktop, .25, .75);
  await waitRevision(3);
  [a, b] = await assertSame(3);
  const mobilePoint = b.sent[0], desktopPoint = a.sent[1];
  if (Math.hypot(mobilePoint.x - desktopPoint.x, mobilePoint.y - desktopPoint.y) > .03)
    throw new Error(`Same visible point maps differently: ${JSON.stringify({ mobilePoint, desktopPoint })}`);
  await desktop.locator('#action-feed').click();
  await desktop.locator('h1').click();
  if ((await desktop.evaluate(() => window.interactionProbe.sent.length)) !== 2)
    throw new Error('Click outside the world sent a command');
  await point(desktop, .99, .5);
  if ((await desktop.evaluate(() => window.interactionProbe.sent.length)) !== 2)
    throw new Error('Click outside the water zone sent a command');
  await desktop.keyboard.press('Escape');
  await point(desktop, .5, .5);
  if ((await desktop.evaluate(() => window.interactionProbe.sent.length)) !== 2)
    throw new Error('Escape did not cancel placement');
  await desktop.locator('#action-feed').click();
  const canvas = desktop.locator('#interaction-scene');
  await canvas.scrollIntoViewIfNeeded();
  const box = await canvas.boundingBox();
  const center = { x: box.x + box.width / 2, y: box.y + box.height / 2 };
  await desktop.mouse.move(center.x, center.y);
  await desktop.mouse.down();
  await new Promise(resolve => setTimeout(resolve, 650));
  await desktop.mouse.up();
  await desktop.mouse.down();
  await desktop.mouse.move(center.x + 30, center.y, { steps: 3 });
  await desktop.mouse.up();
  if ((await desktop.evaluate(() => window.interactionProbe.sent.length)) !== 2)
    throw new Error('Long press or drag sent a placement command');
  await desktop.evaluate(() => {
    const last = window.interactionProbe.sent[1];
    window.interactionSocket.send(JSON.stringify({ kind: 'intent', sceneEpoch: 1, ...last }));
  });
  await desktop.waitForFunction(() => window.interactionProbe.acknowledgments.length === 3);
  if ((await desktop.evaluate(() => window.interactionProbe.revision)) !== 3)
    throw new Error('Duplicate command created another event');
  await desktop.evaluate(() => {
    const last = window.interactionProbe.sent[1];
    window.interactionSocket.send(JSON.stringify({ kind: 'intent', sceneEpoch: 1,
      ...last, x: last.x + 1 }));
  });
  await desktop.waitForFunction(() => window.interactionProbe.rejections.includes('COMMAND_CONFLICT'));
  await desktop.evaluate(() => window.interactionSocket.send(JSON.stringify({ kind: 'intent',
    commandId: crypto.randomUUID(), sceneEpoch: 0, action: 'feed', x: 0, y: 0 })));
  await desktop.waitForFunction(() => window.interactionProbe.rejections.includes('STALE_EPOCH'));
  await desktop.evaluate(() => window.interactionSocket.send(JSON.stringify({ kind: 'intent',
    commandId: crypto.randomUUID(), sceneEpoch: 1, action: 'feed', x: 100, y: 0 })));
  await desktop.waitForFunction(() => window.interactionProbe.rejections.includes('OUTSIDE_WATER'));
  const late = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await late.goto('http://127.0.0.1:4187/interaction.html');
  await late.waitForFunction(() => window.interactionProbe?.connected && window.interactionProbe?.revision === 3);
  const lateEvents = await late.evaluate(() => window.interactionProbe.events);
  if (JSON.stringify(lateEvents) !== JSON.stringify(a.events)) throw new Error('Late viewer snapshot disagrees');
  if (errors.length) throw new Error(`Browser errors: ${errors.join(' | ')}`);
  mkdirSync(path.join(root, '.local'), { recursive: true });
  await desktop.screenshot({ path: path.join(root, '.local/risk05-desktop.png') });
  await mobile.screenshot({ path: path.join(root, '.local/risk05-mobile.png') });
  console.log('RISK-05 Chrome: PC/touch point mapping, shared revisions, one-shot commands, dedup/reject/snapshot: PASS');
} finally {
  await browser?.close();
  server.kill();
}
