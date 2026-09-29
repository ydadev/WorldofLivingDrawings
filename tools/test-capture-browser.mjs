import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, writeFileSync } from 'node:fs';
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
    try { if ((await fetch('http://127.0.0.1:4173/capture.html')).ok) { ready = true; break; } }
    catch { /* Starting. */ }
    await new Promise(resolve => setTimeout(resolve, 250));
  }
  if (!ready) throw new Error(`Vite preview failed: ${stderr}`);
  browser = await chromium.launch({ executablePath: executable, headless: true,
    args: ['--enable-unsafe-swiftshader', '--use-gl=angle', '--use-angle=swiftshader'] });
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors = [];
  const uploads = [];
  page.on('pageerror', error => errors.push(error.message));
  page.on('request', request => { if (request.method() !== 'GET') uploads.push(`${request.method()} ${request.url()}`); });
  await page.goto('http://127.0.0.1:4173/capture.html', { waitUntil: 'networkidle' });
  const makeSynthetic = async (fish, options = {}) => Buffer.from(await page.evaluate(async ({ fish, options }) => {
    const sheet = new Image();
    sheet.src = `/fish/${fish}.paper.svg`;
    await sheet.decode();
    const canvas = document.createElement('canvas');
    canvas.width = 1200; canvas.height = 1700;
    const context = canvas.getContext('2d');
    context.fillStyle = '#cfcac0';
    context.fillRect(0, 0, canvas.width, canvas.height);
    context.drawImage(sheet, 90, 90, 1020, 1442.57);
    if (options.mixed) {
      const other = new Image();
      other.src = `/fish/${fish === 'coral' ? 'stream' : 'coral'}.paper.svg`;
      await other.decode();
      context.save();
      context.beginPath(); context.rect(0, 850, 1200, 850); context.clip();
      context.drawImage(other, 90, 90, 1020, 1442.57);
      context.restore();
    }
    if (options.colored) {
      context.fillStyle = '#ee4131';
      context.fillRect(90 + 1020 * 90 / 210, 90 + 1442.57 * 130 / 297, 1020 * 30 / 210, 1442.57 * 34 / 297);
    }
    if (options.cropMarker) {
      context.fillStyle = '#fff';
      context.fillRect(875, 170, 210, 205);
    }
    if (options.rotate) {
      const rotated = document.createElement('canvas');
      rotated.width = canvas.height; rotated.height = canvas.width;
      const rotatedContext = rotated.getContext('2d');
      rotatedContext.translate(rotated.width, 0);
      rotatedContext.rotate(Math.PI / 2);
      rotatedContext.drawImage(canvas, 0, 0);
      return rotated.toDataURL('image/png').split(',')[1];
    }
    if (options.perspective) {
      const original = context.getImageData(0, 0, canvas.width, canvas.height).data;
      const warped = document.createElement('canvas');
      warped.width = canvas.width; warped.height = canvas.height;
      const warpedContext = warped.getContext('2d');
      const image = warpedContext.createImageData(warped.width, warped.height);
      image.data.fill(220);
      for (let pixelY = 0; pixelY < warped.height; pixelY++) {
        for (let pixelX = 0; pixelX < warped.width; pixelX++) {
          const X = pixelX, Y = pixelY;
          const a = 1040 - X * .10, b = 85 - X * .04, c = X - 30;
          const d = 35 - Y * .10, e = 1570 - Y * .04, f = Y - 45;
          const determinant = a * e - b * d;
          const u = (c * e - b * f) / determinant;
          const v = (a * f - c * d) / determinant;
          if (u < 0 || v < 0 || u >= 1 || v >= 1) continue;
          const sourceX = Math.floor(u * canvas.width), sourceY = Math.floor(v * canvas.height);
          const from = (sourceY * canvas.width + sourceX) * 4;
          const to = (pixelY * warped.width + pixelX) * 4;
          for (let channel = 0; channel < 4; channel++) image.data[to + channel] = original[from + channel];
        }
      }
      warpedContext.putImageData(image, 0, 0);
      return warped.toDataURL('image/jpeg', .88).split(',')[1];
    }
    return canvas.toDataURL('image/png').split(',')[1];
  }, { fish, options }), 'base64');
  const upload = async (buffer, expected, mimeType = 'image/png') => {
    await page.locator('#capture-file').setInputFiles({ name: mimeType === 'image/jpeg' ? 'test.jpg' : 'test.png', mimeType, buffer });
    await page.waitForFunction(() => window.captureProbe?.status === 'PASS' || window.captureProbe?.status === 'FAIL', null, { timeout: 30000 });
    const probe = await page.evaluate(() => window.captureProbe);
    if (probe.status !== expected) throw new Error(`Expected ${expected}, got ${JSON.stringify(probe)}`);
    return probe;
  };
  const coral = await upload(await makeSynthetic('coral', { colored: true }), 'PASS');
  if (coral.templateId !== 'coral') throw new Error(`Wrong species: ${JSON.stringify(coral)}`);
  await page.waitForFunction(() => (window.captureProbe?.frames ?? 0) >= 20);
  const rendered = await page.evaluate(() => window.captureProbe);
  if (!rendered.modelPixel || rendered.modelPixel[0] < 100 || rendered.modelPixel[0] < rendered.modelPixel[1] * 1.3)
    throw new Error(`Photo color was not shown on the 3D fish: ${JSON.stringify(rendered)}`);
  const exported = await page.evaluate(() => {
    const result = window.captureResult();
    return { species: result?.templateId, version: result?.templateVersion,
      hash: result?.layoutHash, format: result?.markerFormatVersion,
      source: result?.sourceKind, type: result?.image.type, size: result?.image.size };
  });
  if (exported.species !== 'coral' || exported.version !== 1 || exported.format !== 1 ||
      exported.source !== 'paper' || exported.type !== 'image/png' ||
      !/^[a-f0-9]{64}$/.test(exported.hash) || exported.size < 1000)
    throw new Error(`Invalid paper PaintResult: ${JSON.stringify(exported)}`);
  mkdirSync(path.join(root, '.local'), { recursive: true });
  await page.screenshot({ path: path.join(root, '.local/risk03-coral.png') });
  const stream = await upload(await makeSynthetic('stream'), 'PASS');
  if (stream.templateId !== 'stream') throw new Error(`Wrong species: ${JSON.stringify(stream)}`);
  await page.waitForFunction(() => (window.captureProbe?.frames ?? 0) >= 20);
  await page.screenshot({ path: path.join(root, '.local/risk03-stream.png') });
  const perspectiveInput = await makeSynthetic('coral', { colored: true, perspective: true });
  writeFileSync(path.join(root, '.local/risk03-input-perspective.jpg'), perspectiveInput);
  const perspective = await upload(perspectiveInput, 'PASS', 'image/jpeg');
  if (perspective.templateId !== 'coral' || perspective.centerPixel?.[0] < 150 ||
      perspective.centerPixel?.[0] < perspective.centerPixel?.[1] * 1.4)
    throw new Error(`Perspective JPEG did not preserve colored center: ${JSON.stringify(perspective)}`);
  await page.waitForFunction(() => (window.captureProbe?.frames ?? 0) >= 20);
  await page.screenshot({ path: path.join(root, '.local/risk03-perspective.png') });
  const rotated = await upload(await makeSynthetic('stream', { rotate: true }), 'PASS');
  if (rotated.templateId !== 'stream') throw new Error(`Rotated sheet identity failed: ${JSON.stringify(rotated)}`);
  const mixed = await upload(await makeSynthetic('coral', { mixed: true }), 'FAIL');
  if (mixed.reason !== 'TEMPLATE_MISMATCH') throw new Error(`Mixed sheet was not rejected: ${JSON.stringify(mixed)}`);
  const cropped = await upload(await makeSynthetic('coral', { cropMarker: true }), 'FAIL');
  if (cropped.reason !== 'MARKERS_MISSING') throw new Error(`Missing marker was not rejected: ${JSON.stringify(cropped)}`);
  const blank = Buffer.from(await page.evaluate(() => {
    const canvas = document.createElement('canvas');
    canvas.width = 1200; canvas.height = 1700;
    canvas.getContext('2d').fillRect(0, 0, 1200, 1700);
    return canvas.toDataURL('image/png').split(',')[1];
  }), 'base64');
  const rejected = await upload(blank, 'FAIL');
  if (rejected.reason !== 'MARKERS_MISSING') throw new Error(`Blank page was not rejected: ${JSON.stringify(rejected)}`);
  await page.locator('#capture-file').setInputFiles({ name: 'camera.heic', mimeType: 'image/heic', buffer: Buffer.from('unsupported') });
  const unsupported = await page.evaluate(() => window.captureProbe);
  if (unsupported?.status !== 'FAIL' || unsupported.reason !== 'UNSUPPORTED_FORMAT')
    throw new Error(`HEIC fallback reason missing: ${JSON.stringify(unsupported)}`);
  if (errors.length) throw new Error(`Browser errors: ${errors.join(' | ')}`);
  if (uploads.length) throw new Error(`Photo path made a network mutation: ${uploads.join(' | ')}`);
  await page.goto('http://127.0.0.1:4173/fish/coral.paper.svg');
  const dimensions = await page.evaluate(() => ({ width: document.documentElement.getAttribute('width'),
    height: document.documentElement.getAttribute('height') }));
  if (dimensions.width !== '210mm' || dimensions.height !== '297mm')
    throw new Error(`A4 sheet dimensions wrong: ${JSON.stringify(dimensions)}`);
  console.log('RISK-03 Chrome synthetic: A4/QR, two species, rotated/perspective JPEG, PaintResult/model, negatives: PASS');
} finally {
  await browser?.close();
  server.kill();
}
