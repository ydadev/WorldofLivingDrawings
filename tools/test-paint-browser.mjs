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
const server = spawn(process.execPath, [path.join(web, 'node_modules/vite/bin/vite.js'),
  'preview', '--host', '127.0.0.1', '--port', '4173', '--strictPort'], { cwd: web, stdio: ['ignore', 'pipe', 'pipe'] });
let stderr = '';
server.stderr.on('data', chunk => { stderr += chunk.toString(); });
let browser;
try {
  let ready = false;
  for (let i = 0; i < 60; i++) {
    if (server.exitCode !== null) throw new Error(`Vite exited: ${stderr}`);
    try { if ((await fetch('http://127.0.0.1:4173/paint.html')).ok) { ready = true; break; } }
    catch { /* Starting. */ }
    await new Promise(resolve => setTimeout(resolve, 250));
  }
  if (!ready) throw new Error(`Vite preview failed: ${stderr}`);
  browser = await chromium.launch({ executablePath: executable, headless: true,
    args: ['--enable-unsafe-swiftshader', '--use-gl=angle', '--use-angle=swiftshader'] });
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, hasTouch: true });
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.goto('http://127.0.0.1:4173/paint.html', { waitUntil: 'networkidle' });
  await page.waitForFunction(() => window.paintProbe?.status === 'PASS' || window.paintProbe?.status === 'FAIL', null, { timeout: 30000 });
  const assertReady = async fish => {
    const result = await page.evaluate(() => window.paintProbe);
    if (result?.status !== 'PASS' || result.templateId !== fish) throw new Error(`Paint load: ${JSON.stringify(result)}`);
  };
  await assertReady('coral');
  const canvas = page.locator('#paint-sheet');
  const clickAt = async (x, y) => {
    const box = await canvas.boundingBox();
    await page.mouse.click(box.x + box.width * x / 512, box.y + box.height * y / 512);
  };
  const colorAt = async (x, y) => page.evaluate(({ x, y }) =>
    [...document.querySelector('#paint-sheet').getContext('2d').getImageData(x * 2, y * 2, 1, 1).data], { x, y });
  const white = pixel => pixel[0] > 245 && pixel[1] > 245 && pixel[2] > 245;
  const red = pixel => pixel[0] > 170 && pixel[1] < 120 && pixel[2] < 120;
  const blue = pixel => pixel[2] > 160 && pixel[0] < 100;
  await clickAt(256, 256);
  if (!red(await colorAt(256, 256))) {
    mkdirSync(path.join(root, '.local'), { recursive: true });
    await page.screenshot({ path: path.join(root, '.local/risk04-debug.png') });
    const details = await page.evaluate(() => {
      const data = document.querySelector('#paint-sheet').getContext('2d').getImageData(0, 0, 1024, 1024).data;
      let redCount = 0, first = -1;
      for (let i = 0; i < data.length; i += 4) if (data[i] > 170 && data[i + 1] < 120 && data[i + 2] < 120) { redCount++; if (first < 0) first = i / 4; }
      return { redCount, first, canvas: document.querySelector('#paint-sheet').getBoundingClientRect().toJSON() };
    });
    throw new Error(`Brush did not color the fish: pixel ${await colorAt(256, 256)}, probe ${JSON.stringify(await page.evaluate(() => window.paintProbe))}, details ${JSON.stringify(details)}, errors ${errors}`);
  }
  await page.locator('#paint-undo').click();
  if (!white(await colorAt(256, 256))) throw new Error('Undo did not remove the stroke');
  await page.locator('#paint-redo').click();
  if (!red(await colorAt(256, 256))) throw new Error('Redo did not restore the stroke');
  await page.locator('#paint-tool').selectOption('erase');
  await clickAt(256, 256);
  if (!white(await colorAt(256, 256))) throw new Error('Eraser did not expose the base');
  await page.locator('#paint-tool').selectOption('fill');
  await page.locator('#paint-color').fill('#2456df');
  await clickAt(256, 256);
  if (!blue(await colorAt(256, 256))) throw new Error('Fill did not color the selected area');
  await page.locator('#paint-tool').selectOption('pick');
  await clickAt(256, 256);
  if ((await page.locator('#paint-color').inputValue()) !== '#2456df') throw new Error('Eyedropper failed');
  const result = await page.evaluate(async () => {
    const app = window.paintProbe;
    return { ...app, center: app?.texturePixel };
  });
  if (!blue(result.center)) throw new Error(`3D texture was not updated: ${JSON.stringify(result)}`);
  await page.locator('#paint-clear').click();
  if (!white(await colorAt(256, 256))) throw new Error('Clear failed');
  await page.locator('#paint-undo').click();
  if (!blue(await colorAt(256, 256))) throw new Error('Clear could not be undone');
  await page.locator('#paint-clear').click();
  await page.locator('#paint-tool').selectOption('fill');
  await page.locator('#paint-color').fill('#e9463a');
  await clickAt(256, 256);
  if (!red(await colorAt(180, 256)) || !white(await colorAt(5, 5)))
    throw new Error('Fill did not stay inside the fish silhouette');
  const eye = await page.evaluate(async () => {
    const layout = await (await fetch('/fish/coral.layout.json')).json();
    const [left, right, bottom, top] = layout.bounds;
    const x = Math.round((layout.eye[0] - left) / (right - left) * 1024);
    const y = Math.round((top - layout.eye[1]) / (top - bottom) * 1024);
    return [...document.querySelector('#paint-sheet').getContext('2d').getImageData(x, y, 1, 1).data];
  });
  if (red(eye)) throw new Error(`Protected eye was painted: ${eye}`);
  const exported = await page.evaluate(async () => {
    const value = await window.paintResult();
    const bytes = new Uint8Array(await value.image.arrayBuffer());
    const bitmap = await createImageBitmap(value.image);
    const canvas = document.createElement('canvas');
    canvas.width = canvas.height = 512;
    const context = canvas.getContext('2d');
    context.drawImage(bitmap, 0, 0);
    bitmap.close();
    const layout = await (await fetch('/fish/coral.layout.json')).json();
    const [left, right, bottom, top] = layout.bounds;
    const eyeX = Math.round((layout.eye[0] - left) / (right - left) * 512);
    const eyeY = Math.round((top - layout.eye[1]) / (top - bottom) * 512);
    return { templateId: value.templateId, templateVersion: value.templateVersion,
      layoutHash: value.layoutHash, sourceKind: value.sourceKind, colorSpace: value.colorSpace,
      mime: value.image.type, signature: [...bytes.slice(0, 8)],
      width: new DataView(bytes.buffer).getUint32(16), height: new DataView(bytes.buffer).getUint32(20),
      eyePixel: [...context.getImageData(eyeX, eyeY, 1, 1).data],
      outsidePixel: [...context.getImageData(5, 5, 1, 1).data] };
  });
  if (exported.templateId !== 'coral' || exported.templateVersion !== 1 ||
      !/^[a-f0-9]{64}$/.test(exported.layoutHash) || exported.sourceKind !== 'browser' ||
      exported.colorSpace !== 'sRGB' || exported.mime !== 'image/png' ||
      exported.width !== 512 || exported.height !== 512 ||
      exported.signature.join(',') !== '137,80,78,71,13,10,26,10')
    throw new Error(`Invalid PaintResult: ${JSON.stringify(exported)}`);
  if (!white(exported.eyePixel) || !white(exported.outsidePixel))
    throw new Error(`PaintResult mask did not protect eye and background: ${JSON.stringify(exported)}`);
  mkdirSync(path.join(root, '.local'), { recursive: true });
  await page.screenshot({ path: path.join(root, '.local/risk04-coral.png') });
  await page.locator('#paint-species').selectOption('stream');
  await page.waitForFunction(() => window.paintProbe?.templateId === 'stream' && window.paintProbe?.status === 'PASS');
  await assertReady('stream');
  await page.locator('#paint-tool').selectOption('stroke');
  await page.locator('#paint-color').fill('#e9463a');
  const box = await canvas.boundingBox();
  await page.touchscreen.tap(box.x + box.width / 2, box.y + box.height / 2);
  if (!red(await colorAt(256, 256))) throw new Error('Touch did not paint');
  if (errors.length) throw new Error(`Browser errors: ${errors.join(' | ')}`);
  await page.screenshot({ path: path.join(root, '.local/risk04-stream-touch.png') });
  console.log('RISK-04 paint: both fish, brush/touch, undo/redo, erase, fill, picker, clear and model texture: PASS');

  await page.reload({ waitUntil: 'networkidle' });
  await page.waitForFunction(() => window.paintProbe?.status === 'PASS');
  await clickAt(256, 256);
  await page.reload({ waitUntil: 'networkidle' });
  await page.waitForFunction(() => window.paintProbe?.status === 'PASS');
  if (!white(await colorAt(256, 256))) throw new Error('Editor saved before explicit opt-in');
  await clickAt(256, 256);
  await page.locator('#draft-save').click();
  await page.locator('#draft-status[data-state="saved"]').waitFor();
  const firstId = await page.locator('#draft-list').inputValue();
  if (!firstId) throw new Error('Saved draft missing from list');
  const stored = await page.evaluate(async () => {
    const db = await new Promise((resolve, reject) => {
      const open = indexedDB.open('ldw-paint-drafts', 1);
      open.onsuccess = () => resolve(open.result); open.onerror = () => reject(open.error);
    });
    const rows = await new Promise((resolve, reject) => {
      const get = db.transaction('drafts').objectStore('drafts').getAll();
      get.onsuccess = () => resolve(get.result); get.onerror = () => reject(get.error);
    });
    db.close();
    return rows.map(row => ({ keys: Object.keys(row).sort(), imageType: row.image.type, size: row.image.size }));
  });
  if (stored.length !== 1 || stored[0].imageType !== 'image/png' || stored[0].size === 0 ||
      stored[0].keys.some(key => /token|photo|stroke|stylus|undo/i.test(key)))
    throw new Error(`Draft stored forbidden or missing fields: ${JSON.stringify(stored)}`);
  await page.reload({ waitUntil: 'networkidle' });
  await page.waitForFunction(() => window.paintProbe?.status === 'PASS');
  await page.locator('#draft-list').selectOption(firstId);
  await page.locator('#draft-open').click();
  await page.waitForFunction(() => document.querySelector('#draft-status')?.dataset.state === 'saved');
  if (!red(await colorAt(256, 256)) || await page.locator('#paint-undo').isEnabled())
    throw new Error('Saved layer was not restored as the undo baseline');

  const second = await context.newPage();
  second.on('pageerror', error => errors.push(error.message));
  await second.goto('http://127.0.0.1:4173/paint.html', { waitUntil: 'networkidle' });
  await second.waitForFunction(() => window.paintProbe?.status === 'PASS');
  await second.locator('#draft-list').selectOption(firstId);
  await second.locator('#draft-open').click();
  await second.waitForFunction(() => document.querySelector('#draft-status')?.dataset.state === 'saved');
  await page.locator('#paint-tool').selectOption('fill');
  await page.locator('#paint-color').fill('#2456df');
  await clickAt(256, 256);
  await page.waitForFunction(() => document.querySelector('#draft-list option:checked')?.dataset.revision === '2');
  const secondCanvas = second.locator('#paint-sheet');
  const secondBox = await secondCanvas.boundingBox();
  await second.locator('#paint-tool').selectOption('fill');
  await second.locator('#paint-color').fill('#29c244');
  await second.mouse.click(secondBox.x + secondBox.width / 2, secondBox.y + secondBox.height / 2);
  await second.locator('#draft-status[data-state="conflict"]').waitFor();
  if (!await second.locator('#draft-copy').isVisible()) throw new Error('Conflict copy action missing');
  await second.locator('#draft-copy').click();
  await second.locator('#draft-status[data-state="saved"]').waitFor();
  const copyId = await second.locator('#draft-list').inputValue();
  if (!copyId || copyId === firstId) throw new Error('Conflict copy overwrote original');
  await page.reload({ waitUntil: 'networkidle' });
  await page.waitForFunction(() => window.paintProbe?.status === 'PASS');
  await page.locator('#draft-list').selectOption(firstId);
  await page.locator('#draft-open').click();
  await page.waitForFunction(() => document.querySelector('#draft-status')?.dataset.state === 'saved');
  if (!blue(await colorAt(256, 256))) throw new Error('Conflicting tab changed the original draft');

  const incompatibleId = await page.evaluate(async () => {
    const db = await new Promise((resolve, reject) => {
      const open = indexedDB.open('ldw-paint-drafts', 1);
      open.onsuccess = () => resolve(open.result); open.onerror = () => reject(open.error);
    });
    const original = await new Promise((resolve, reject) => {
      const get = db.transaction('drafts').objectStore('drafts').getAll();
      get.onsuccess = () => resolve(get.result[0]); get.onerror = () => reject(get.error);
    });
    const id = crypto.randomUUID();
    await new Promise((resolve, reject) => {
      const tx = db.transaction('drafts', 'readwrite');
      tx.objectStore('drafts').put({ ...original, id, templateVersion: 999 });
      tx.oncomplete = resolve; tx.onerror = () => reject(tx.error);
    });
    db.close();
    return id;
  });
  await page.reload({ waitUntil: 'networkidle' });
  await page.waitForFunction(() => window.paintProbe?.status === 'PASS');
  await page.locator('#draft-list').selectOption(incompatibleId);
  await page.locator('#draft-open').click();
  await page.locator('#draft-status[data-state="incompatible"]').waitFor();
  if (!await page.locator('#draft-preview-image').isVisible() ||
      !await page.locator('#draft-preview-download').isEnabled())
    throw new Error('Incompatible template cannot be previewed or downloaded');
  await page.locator('#draft-delete').click();
  await page.waitForFunction(id => ![...document.querySelector('#draft-list').options].some(option => option.value === id), incompatibleId);

  await page.evaluate(async () => {
    const db = await new Promise((resolve, reject) => {
      const open = indexedDB.open('ldw-paint-drafts', 1);
      open.onsuccess = () => resolve(open.result); open.onerror = () => reject(open.error);
    });
    const existing = await new Promise((resolve, reject) => {
      const get = db.transaction('drafts').objectStore('drafts').getAll();
      get.onsuccess = () => resolve(get.result); get.onerror = () => reject(get.error);
    });
    await new Promise((resolve, reject) => {
      const tx = db.transaction('drafts', 'readwrite');
      for (let i = existing.length; i < 10; i++) tx.objectStore('drafts').put({ ...existing[0], id: crypto.randomUUID() });
      tx.oncomplete = resolve; tx.onerror = () => reject(tx.error);
    });
    db.close();
  });
  await page.reload({ waitUntil: 'networkidle' });
  await page.waitForFunction(() => window.paintProbe?.status === 'PASS');
  await clickAt(256, 256);
  await page.locator('#draft-save').click();
  await page.locator('#draft-status[data-state="limit"]').waitFor();
  if (!red(await colorAt(256, 256))) throw new Error('Quota failure lost the in-memory drawing');
  const countAfterLimit = await page.locator('#draft-list option').count();
  if (countAfterLimit !== 11) throw new Error('Quota failure deleted an existing draft');
  await page.locator('#draft-list').selectOption(firstId);
  await page.locator('#draft-delete').click();
  await page.waitForFunction(id => ![...document.querySelector('#draft-list').options].some(option => option.value === id), firstId);
  await page.locator('#draft-save').click();
  await page.locator('#draft-status[data-state="saved"]').waitFor();
  if (!red(await colorAt(256, 256))) throw new Error('Saving after explicit delete lost the drawing');

  const unavailable = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  await unavailable.addInitScript(() => Object.defineProperty(window, 'indexedDB', { value: undefined }));
  const unavailablePage = await unavailable.newPage();
  unavailablePage.on('pageerror', error => errors.push(error.message));
  await unavailablePage.goto('http://127.0.0.1:4173/paint.html', { waitUntil: 'networkidle' });
  await unavailablePage.waitForFunction(() => window.paintProbe?.status === 'PASS');
  await unavailablePage.locator('#draft-save').click();
  await unavailablePage.locator('#draft-status[data-state="unavailable"]').waitFor();
  if (await unavailablePage.locator('#paint-download').isDisabled())
    throw new Error('Download unavailable when IndexedDB is blocked');
  await unavailable.close();
  if (errors.length) throw new Error(`Draft browser errors: ${errors.join(' | ')}`);
  console.log('MVP-02 drafts: opt-in, reload, conflict/copy, count limit and unavailable storage: PASS');
} finally {
  await browser?.close();
  server.kill();
}
