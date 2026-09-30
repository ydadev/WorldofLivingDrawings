import { spawn, execFileSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { readFileSync, existsSync, mkdtempSync } from 'node:fs';
import { rm, readFile, stat } from 'node:fs/promises';
import { createServer } from 'node:https';
import { request as httpRequest, get as httpGet } from 'node:http';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from '../prototypes/wasm-webgl/node_modules/playwright-core/index.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const dist = path.join(root, 'prototypes/wasm-webgl/dist');
const executable = process.env.LDW_CHROME_PATH || ['/usr/bin/google-chrome', '/usr/bin/chromium',
  'C:/Program Files/Google/Chrome/Application/chrome.exe'].find(existsSync);
if (!executable || !process.env.DATABASE_URL) throw new Error('Chrome and DATABASE_URL are required');
const origin = 'https://127.0.0.1:9443';
const fixturePassword = randomBytes(24).toString('base64url');
const temp = mkdtempSync(path.join(os.tmpdir(), 'ldw-ui-'));
const blobRoot = path.join(temp, 'blobs');
const keyPath = path.join(temp, 'key.pem');
const certPath = path.join(temp, 'cert.pem');
execFileSync('openssl', ['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', keyPath,
  '-out', certPath, '-subj', '/CN=127.0.0.1', '-addext', 'subjectAltName=IP:127.0.0.1',
  '-days', '1'], { stdio: 'ignore' });

const fixture = spawn('cargo', ['run', '-p', 'ldw-server', '--example', 'browser_fixture', '--locked'], {
  cwd: root, env: { ...process.env, LDW_UI_BASE_DATABASE_URL: process.env.DATABASE_URL,
    LDW_UI_FIXTURE_PASSWORD: fixturePassword, LDW_UI_PUBLIC_ORIGIN: origin,
    LDW_UI_BIND_ADDR: '127.0.0.1:4189', LDW_UI_BLOB_DIR: blobRoot },
  stdio: ['ignore', 'pipe', 'pipe'],
});
let fixtureError = '';
fixture.stderr.on('data', chunk => { fixtureError += chunk.toString(); });
const contentTypes = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8', '.json': 'application/json', '.wasm': 'application/wasm',
  '.glb': 'model/gltf-binary', '.png': 'image/png', '.svg': 'image/svg+xml' };
const csp = ["default-src 'none'", "script-src 'self' 'wasm-unsafe-eval'", "style-src 'self'",
  "img-src 'self' data: blob:", "connect-src 'self' wss://127.0.0.1:9443",
  "worker-src 'self'", "object-src 'none'", "base-uri 'none'", "frame-src 'self'",
  "frame-ancestors 'self'"].join('; ');

const proxy = createServer({ key: readFileSync(keyPath), cert: readFileSync(certPath) },
  async (request, response) => {
    response.setHeader('Content-Security-Policy', csp);
    response.setHeader('X-Content-Type-Options', 'nosniff');
    const pathname = new URL(request.url ?? '/', origin).pathname;
    if (pathname.startsWith('/api/') || pathname === '/health/ready') {
      const upstream = httpRequest({ hostname: '127.0.0.1', port: 4189, method: request.method,
        path: request.url, headers: { ...request.headers, host: '127.0.0.1:4189' } }, result => {
        response.writeHead(result.statusCode ?? 502, result.headers);
        result.pipe(response);
      });
      upstream.on('error', () => { if (!response.headersSent) response.writeHead(502).end(); });
      request.pipe(upstream);
      return;
    }
    if (request.method !== 'GET') { response.writeHead(405).end(); return; }
    const target = path.resolve(dist, `.${pathname === '/' ? '/world.html' : pathname}`);
    if (!target.startsWith(dist + path.sep)) { response.writeHead(404).end(); return; }
    try {
      if (!(await stat(target)).isFile()) throw new Error('not a file');
      response.writeHead(200, { 'Content-Type': contentTypes[path.extname(target)] ??
        'application/octet-stream' });
      response.end(await readFile(target));
    } catch { response.writeHead(404).end(); }
  });
proxy.on('upgrade', (request, socket, head) => {
  if (!new URL(request.url ?? '/', origin).pathname.startsWith('/api/sessions/')) {
    socket.destroy(); return;
  }
  const upstream = net.connect(4189, '127.0.0.1');
  upstream.on('connect', () => {
    const headers = { ...request.headers, host: '127.0.0.1:4189' };
    upstream.write(`${request.method} ${request.url} HTTP/1.1\r\n${Object.entries(headers)
      .map(([name, value]) => `${name}: ${value}`).join('\r\n')}\r\n\r\n`);
    if (head.length) upstream.write(head);
    socket.pipe(upstream).pipe(socket);
  });
  upstream.on('error', () => socket.destroy());
  socket.on('error', () => upstream.destroy());
});

let browser;
function assert(condition, message) { if (!condition) throw new Error(message); }
async function waitReady() {
  for (let i = 0; i < 240; i++) {
    if (fixture.exitCode !== null) throw new Error(`Rust fixture exited: ${fixtureError}`);
    try {
      const status = await new Promise((resolve, reject) => {
        httpGet('http://127.0.0.1:4189/health/ready', result => {
          result.resume(); resolve(result.statusCode);
        }).on('error', reject);
      });
      if (status === 200) return;
    } catch { /* compilation or migration still running */ }
    await new Promise(resolve => setTimeout(resolve, 500));
  }
  throw new Error(`Rust fixture did not become ready: ${fixtureError}`);
}
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
async function waitFrame(frames, predicate, timeoutMs = 15000) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    const found = frames.find(predicate);
    if (found) return found;
    await new Promise(resolve => setTimeout(resolve, 50));
  }
  throw new Error(`Expected WS frame missing; recent types: ${frames.slice(-12).map(frame => frame.type)}`);
}
function recordFrames(page, frames) {
  page.on('websocket', socket => socket.on('framereceived', frame => {
    try { frames.push(JSON.parse(frame.payload)); } catch { /* ignore non-JSON */ }
  }));
}
async function verifyRecoveredScene(sessionId, coralBlob, streamBlob) {
  const result = await new Promise((resolve, reject) => {
    const check = spawn('bash', [path.join(root, 'tools/test-live-scene-restore.sh'),
      temp, sessionId, coralBlob, streamBlob], {
      cwd: root, env: { ...process.env, LDW_RESTORE_OWNER_PASSWORD: fixturePassword },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = '';
    let stderr = '';
    check.stdout.on('data', chunk => { stdout += chunk.toString(); });
    check.stderr.on('data', chunk => { stderr += chunk.toString(); });
    check.on('error', reject);
    check.on('close', code => code === 0 ? resolve(stdout) : reject(new Error(
      `Live scene restore failed (${code}): ${stderr.slice(-3000)}`)));
  });
  console.log(result.trim());
}

try {
  await waitReady();
  await new Promise((resolve, reject) => proxy.listen(9443, '127.0.0.1', error =>
    error ? reject(error) : resolve()));
  browser = await chromium.launch({ executablePath: executable, headless: true,
    args: ['--enable-unsafe-swiftshader', '--use-gl=angle', '--use-angle=swiftshader',
      '--ignore-certificate-errors'] });
  const ownerContext = await browser.newContext({ ignoreHTTPSErrors: true,
    viewport: { width: 1440, height: 900 } });
  const mobileContext = await browser.newContext({ ignoreHTTPSErrors: true,
    viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true });
  const viewerContext = await browser.newContext({ ignoreHTTPSErrors: true,
    viewport: { width: 1280, height: 720 } });
  const owner = await ownerContext.newPage();
  const mobile = await mobileContext.newPage();
  const viewer = await viewerContext.newPage();
  const errors = [];
  for (const page of [owner, mobile, viewer]) page.on('pageerror', error => errors.push(error.message));
  const ownerFrames = [];
  const mobileFrames = [];
  recordFrames(owner, ownerFrames);
  recordFrames(mobile, mobileFrames);

  await owner.goto(`${origin}/world.html`);
  await owner.locator('[name=login]').fill('ui-fixture-owner');
  await owner.locator('[name=password]').fill(fixturePassword);
  await owner.locator('#access-form button[type=submit]').click();
  await owner.locator('#world-shell').waitFor({ state: 'visible', timeout: 20000 });
  await owner.waitForFunction(() => !document.querySelector('#action-feed').disabled,
    null, { timeout: 20000 });
  const sessionId = (await owner.locator('#session-id').textContent()).trim();
  assert(/^[0-9a-f-]{36}$/.test(sessionId), 'Real backend did not create a session');
  await owner.locator('#paint-open').click();
  const editor = owner.frameLocator('#editor-frame');
  await editor.locator('#paint-sheet').waitFor({ state: 'visible', timeout: 20000 });
  await owner.waitForFunction(() => document.querySelector('#editor-frame')?.contentWindow?.paintProbe?.status === 'PASS',
    null, { timeout: 20000 });
  await editor.locator('#paint-tool').selectOption('fill');
  await editor.locator('#paint-sheet').click({ position: { x: 240, y: 240 } });
  const pixelBeforeSave = await editor.locator('#paint-sheet').evaluate(canvas =>
    [...canvas.getContext('2d').getImageData(512, 512, 1, 1).data]);
  assert(pixelBeforeSave[0] > 170 && pixelBeforeSave[1] < 120 && pixelBeforeSave[2] < 120,
    `Digital paint was not visible before draft save: ${pixelBeforeSave}`);
  await editor.locator('#draft-save').click();
  await editor.locator('#draft-status[data-state="saved"]').waitFor({ timeout: 20000 });
  const draftId = await editor.locator('#draft-list').inputValue();
  assert(draftId, 'Saved digital draft was not listed');
  await owner.locator('#editor-frame').evaluate(frame => new Promise(resolve => {
    frame.addEventListener('load', () => resolve(), { once: true });
    frame.contentWindow.location.reload();
  }));
  await owner.waitForFunction(() => document.querySelector('#editor-frame')?.contentWindow?.paintProbe?.status === 'PASS',
    null, { timeout: 20000 });
  await editor.locator('#draft-list').selectOption(draftId);
  await editor.locator('#draft-open').click();
  await editor.locator('#draft-status[data-state="saved"]').waitFor({ timeout: 20000 });
  const pixelAfterRestore = await editor.locator('#paint-sheet').evaluate(canvas =>
    [...canvas.getContext('2d').getImageData(512, 512, 1, 1).data]);
  assert(pixelAfterRestore[0] > 170 && pixelAfterRestore[1] < 120 && pixelAfterRestore[2] < 120,
    `Digital paint was lost after draft restore: ${pixelAfterRestore}`);
  await owner.locator('#editor-pick').click();
  await owner.locator('#fish-place').waitFor({ state: 'visible', timeout: 20000 });
  await owner.waitForFunction(() => !document.querySelector('#fish-place').disabled,
    null, { timeout: 20000 });
  const ownerIntentRequests = [];
  owner.on('request', request => {
    if (request.method() === 'POST' && request.url().endsWith('/upload-intents'))
      ownerIntentRequests.push(request.postDataJSON());
  });
  let lostIntentResponse = false;
  await owner.route('**/upload-intents', async route => {
    if (lostIntentResponse) return route.continue();
    lostIntentResponse = true;
    const committed = await route.fetch();
    assert(committed.status() === 201, 'First intent was not committed before its response was lost');
    await route.abort('failed');
  });
  await owner.locator('#fish-place').click();
  await point(owner, .5, .5);
  await owner.locator('#publish-retry').waitFor({ state: 'visible', timeout: 20000 });
  await owner.locator('#publish-retry').click();
  await owner.waitForFunction(() => /Рыбка сохранена/.test(document.querySelector('#publish-status')?.textContent ?? ''),
    null, { timeout: 20000 });
  assert(lostIntentResponse && ownerIntentRequests.length === 2 &&
    /^[0-9a-f-]{36}$/.test(ownerIntentRequests[0].requestId) &&
    ownerIntentRequests[0].requestId === ownerIntentRequests[1].requestId,
  'Lost intent response must retry with the same requestId');
  const published = await waitFrame(ownerFrames, frame => frame.type === 'delta' &&
    frame.event?.type === 'entity_published', 20000);
  assert(published.event.entity.definitionId === 'coral-fish', 'Wrong fish definition published');
  assert(Math.abs(published.event.entity.position.x) < .05 &&
    Math.abs(published.event.entity.position.y) < .05, 'Wrong fish position published');
  const paintResponse = await ownerContext.request.get(`${origin}/api/sessions/${sessionId}/paint/${published.event.entity.paintBlobId}`);
  assert(paintResponse.status() === 200 && paintResponse.headers()['content-type'] === 'image/png',
    'Owner could not fetch published private PNG');
  await owner.locator('#invite').click();
  await owner.waitForFunction(() => /Код подключения: \d{6}/.test(document.querySelector('#invitation')?.textContent ?? ''));
  const invitation = await owner.locator('#invitation').textContent();
  const pin = invitation.match(/Код подключения: (\d{6})/)?.[1];
  assert(pin, 'Real backend did not issue Controller PIN');

  await viewer.goto(`${origin}/viewer.html?session=${sessionId}`);
  await viewer.locator('#access-form button[type=submit]').click();
  await viewer.waitForFunction(() => /\b\d{8}\b/.test(document.querySelector('#access-status')?.textContent ?? ''));
  const claimText = await viewer.locator('#access-status').textContent();
  const code = claimText.match(/\b\d{8}\b/)?.[0];
  assert(code, 'Real backend did not issue Viewer claim');
  await owner.locator('#approve-viewer-form [name=code]').fill(code);
  await owner.locator('#approve-viewer-form button[type=submit]').click();
  await owner.waitForFunction(() => document.querySelector('#viewer-approval-status')?.textContent.includes('разрешён просмотр'));
  await viewer.waitForFunction(() => document.querySelector('#interaction-status')?.textContent.includes('Режим просмотра'),
    null, { timeout: 20000 });
  assert(await viewer.locator('#action-panel').isHidden(), 'Read-only Viewer gained action buttons');

  await mobile.goto(`${origin}/controller.html?session=${sessionId}`);
  await mobile.locator('[name=pin]').fill(pin);
  await mobile.locator('#access-form button[type=submit]').click();
  await mobile.locator('#view-toggle').waitFor({ state: 'visible', timeout: 20000 });
  await mobile.locator('#view-toggle').click();
  await mobile.waitForFunction(() => !document.querySelector('#action-feed').disabled,
    null, { timeout: 20000 });

  const syntheticPaper = Buffer.from(await mobile.evaluate(async () => {
    const sheet = new Image();
    sheet.src = '/fish/stream.paper.svg';
    await sheet.decode();
    const canvas = document.createElement('canvas');
    canvas.width = 1200;
    canvas.height = 1700;
    const context = canvas.getContext('2d');
    context.fillStyle = '#cfcac0';
    context.fillRect(0, 0, canvas.width, canvas.height);
    context.drawImage(sheet, 90, 90, 1020, 1442.57);
    context.fillStyle = '#ee4131';
    context.fillRect(90 + 1020 * 90 / 210, 90 + 1442.57 * 130 / 297,
      1020 * 30 / 210, 1442.57 * 34 / 297);
    return canvas.toDataURL('image/png').split(',')[1];
  }), 'base64');
  const paperRequests = [];
  mobile.on('request', request => {
    if (request.method() !== 'GET') paperRequests.push(request);
  });
  await mobile.locator('#capture-open').click();
  const capture = mobile.frameLocator('#editor-frame');
  await mobile.waitForFunction(() => typeof document.querySelector('#editor-frame')?.contentWindow?.captureResult === 'function',
    null, { timeout: 20000 });
  await capture.locator('#capture-file').setInputFiles({
    name: 'test-paper.png', mimeType: 'image/png', buffer: syntheticPaper,
  });
  await mobile.waitForFunction(() => ['PASS', 'FAIL'].includes(
    document.querySelector('#editor-frame')?.contentWindow?.captureProbe?.status ?? ''),
  null, { timeout: 30000 });
  const paperProbe = await mobile.evaluate(() => document.querySelector('#editor-frame')?.contentWindow?.captureProbe);
  assert(paperProbe.status === 'PASS' && paperProbe.templateId === 'stream',
    `Controller paper capture failed: ${JSON.stringify(paperProbe)}`);
  assert(paperRequests.length === 0, 'Raw photo processing made a network mutation');
  await mobile.locator('#editor-pick').click();
  await mobile.locator('#fish-place').waitFor({ state: 'visible', timeout: 20000 });
  await mobile.waitForFunction(() => !document.querySelector('#fish-place').disabled,
    null, { timeout: 20000 });
  await mobile.locator('#fish-place').click();
  await point(mobile, .65, .45, true);
  await mobile.waitForFunction(() => /Рыбка сохранена/.test(document.querySelector('#publish-status')?.textContent ?? ''),
    null, { timeout: 20000 });
  const paperIntent = paperRequests.find(request => request.url().endsWith('/upload-intents'));
  assert(paperIntent?.postDataJSON()?.sourceKind === 'paper', 'Paper sourceKind was not sent');
  const paperPut = paperRequests.find(request => request.method() === 'PUT' && request.url().endsWith('/paint'));
  assert(paperPut?.headers()['content-type'] === 'image/png', 'Controller did not send normalized PNG');
  const paperPublished = await waitFrame(ownerFrames, frame => frame.type === 'delta' &&
    frame.event?.type === 'entity_published' && frame.event.entity.definitionId === 'stream-fish', 20000);
  assert(Math.abs(paperPublished.event.entity.position.x - 2.4) < .12 &&
    Math.abs(paperPublished.event.entity.position.y - .45) < .12,
  'Paper fish was published at the wrong point');
  const paperTexture = await mobileContext.request.get(`${origin}/api/sessions/${sessionId}/paint/${paperPublished.event.entity.paintBlobId}`);
  assert(paperTexture.status() === 200 && paperTexture.headers()['content-type'] === 'image/png',
    'Controller could not fetch published paper texture');

  await owner.locator('#action-feed').click();
  await point(owner, .5, .5);
  const feedRequested = await waitFrame(ownerFrames, frame => frame.type === 'delta' &&
    frame.event?.type === 'interaction_requested' && frame.event.interactionId === 'feed');
  assert(Math.abs(feedRequested.event.point.x) < .05 && Math.abs(feedRequested.event.point.y) < .05,
    'Real backend received wrong feed point');
  await waitFrame(mobileFrames, frame => frame.type === 'delta' &&
    frame.event?.type === 'interaction_state' &&
    frame.event.activeActions?.some(action => action.interactionId === 'feed'));
  await owner.waitForFunction(() => !document.querySelector('#action-feed').disabled,
    null, { timeout: 20000 });
  const feedCancel = owner.locator('#active-actions button[data-action-id^="feed-"]');
  await feedCancel.waitFor({ state: 'visible', timeout: 20000 });
  await feedCancel.click();
  const feedCancelRequested = await waitFrame(ownerFrames, frame => frame.type === 'delta' &&
    frame.event?.type === 'interaction_requested' && frame.event.interactionId === 'cancel_feed');
  assert(feedCancelRequested.event.targetActionId.startsWith('feed-'),
    'Owner did not name the active feed source');
  await waitFrame(mobileFrames, frame => frame.type === 'delta' &&
    frame.event?.type === 'interaction_state' &&
    frame.event.appliedCommandIds.includes(feedCancelRequested.event.commandId) &&
    !frame.event.activeActions.some(action => action.id === feedCancelRequested.event.targetActionId));
  assert(await mobile.locator('#active-actions').count() === 0,
    'Controller gained Owner-only cancellation controls');

  await mobile.locator('#action-boat').click();
  await point(mobile, .25, .75, true);
  const boatRequested = await waitFrame(ownerFrames, frame => frame.type === 'delta' &&
    frame.event?.type === 'interaction_requested' && frame.event.interactionId === 'boat');
  assert(Math.abs(boatRequested.event.point.x + 4) < .1 &&
    Math.abs(boatRequested.event.point.y + 2.25) < .1,
  'Real backend received wrong Controller boat point');
  await waitFrame(ownerFrames, frame => frame.type === 'delta' &&
    frame.event?.type === 'interaction_state' &&
    frame.event.activeActions?.some(action => action.interactionId === 'boat'));
  await waitFrame(ownerFrames, frame => frame.type === 'positions' &&
    frame.actionPositions?.some(action => action.id.startsWith('boat-')));
  const boatCancel = owner.locator('#active-actions button[data-action-id^="boat-"]');
  await boatCancel.waitFor({ state: 'visible', timeout: 20000 });
  await boatCancel.click();
  const boatCancelRequested = await waitFrame(ownerFrames, frame => frame.type === 'delta' &&
    frame.event?.type === 'interaction_requested' && frame.event.interactionId === 'cancel_boat');
  assert(boatCancelRequested.event.targetActionId.startsWith('boat-'),
    'Owner did not name the active boat');
  await waitFrame(mobileFrames, frame => frame.type === 'delta' &&
    frame.event?.type === 'interaction_state' &&
    frame.event.appliedCommandIds.includes(boatCancelRequested.event.commandId) &&
    !frame.event.activeActions.some(action => action.id === boatCancelRequested.event.targetActionId));
  await owner.waitForFunction(() => !document.querySelector('#active-actions button'),
    null, { timeout: 20000 });

  assert(errors.length === 0, `Browser errors: ${errors.join('; ')}`);
  await verifyRecoveredScene(sessionId, published.event.entity.paintBlobId,
    paperPublished.event.entity.paintBlobId);
  console.log('MVP-03 real backend UI: Owner, Viewer, Controller, fish publication, feed and boat with Owner cancellation: PASS');
} finally {
  await browser?.close();
  if (proxy.listening) {
    proxy.closeAllConnections();
    await new Promise(resolve => proxy.close(() => resolve()));
  }
  fixture.kill('SIGINT');
  await rm(temp, { recursive: true, force: true });
}
