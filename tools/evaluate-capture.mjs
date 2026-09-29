import { spawn } from 'node:child_process';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from '../prototypes/wasm-webgl/node_modules/playwright-core/index.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const datasetRoot = path.join(root, '.local/capture-dataset');
const manifestPath = path.join(datasetRoot, 'manifest.json');
if (!existsSync(manifestPath)) {
  console.error('Create .local/capture-dataset/manifest.json using docs/CAPTURE-VALIDATION.md; no real photos were evaluated.');
  process.exit(2);
}
const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
if (!Array.isArray(manifest.valid) || !Array.isArray(manifest.negative))
  throw new Error('Manifest must contain valid and negative arrays');
const executable = process.env.LDW_CHROME_PATH || [
  'C:/Program Files/Google/Chrome/Application/chrome.exe', '/usr/bin/google-chrome', '/usr/bin/chromium',
].find(existsSync);
if (!executable) throw new Error('Set LDW_CHROME_PATH to an installed Chrome executable');
const web = path.join(root, 'prototypes/wasm-webgl');
const server = spawn(process.execPath, [path.join(web, 'node_modules/vite/bin/vite.js'),
  'preview', '--host', '127.0.0.1', '--port', '4173', '--strictPort'], { cwd: web, stdio: ['ignore', 'pipe', 'pipe'] });
let browser;
try {
  let ready = false;
  for (let i = 0; i < 60; i++) {
    if (server.exitCode !== null) throw new Error('Preview server exited');
    try { if ((await fetch('http://127.0.0.1:4173/capture.html')).ok) { ready = true; break; } }
    catch { /* Starting. */ }
    await new Promise(resolve => setTimeout(resolve, 250));
  }
  if (!ready) throw new Error('Preview server did not start');
  browser = await chromium.launch({ executablePath: executable, headless: true,
    args: ['--enable-unsafe-swiftshader', '--use-gl=angle', '--use-angle=swiftshader'] });
  const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
  const mutations = [];
  page.on('request', request => { if (request.method() !== 'GET') mutations.push(request.method()); });
  await page.goto('http://127.0.0.1:4173/capture.html', { waitUntil: 'networkidle' });
  const outcomes = { coral: { total: 0, correct: 0, wrong: 0, rejected: 0 },
    stream: { total: 0, correct: 0, wrong: 0, rejected: 0 },
    negative: { total: 0, accepted: 0, rejected: 0 }, reasons: {} };
  const evaluate = async item => {
    const file = path.resolve(datasetRoot, item.file);
    const relative = path.relative(datasetRoot, file);
    if (relative.startsWith('..') || path.isAbsolute(relative) || !existsSync(file))
      throw new Error('Dataset entry is outside the private capture-dataset directory or is missing');
    await page.locator('#capture-file').setInputFiles(file);
    await page.waitForFunction(() => ['PASS', 'FAIL'].includes(window.captureProbe?.status), null, { timeout: 45000 });
    const probe = await page.evaluate(() => window.captureProbe);
    if (probe.status === 'FAIL') outcomes.reasons[probe.reason] = (outcomes.reasons[probe.reason] ?? 0) + 1;
    return probe;
  };
  for (const item of manifest.valid) {
    if (!['coral', 'stream'].includes(item.templateId) || typeof item.file !== 'string')
      throw new Error('Each valid entry needs file and templateId coral/stream');
    const counters = outcomes[item.templateId];
    counters.total++;
    const probe = await evaluate(item);
    if (probe.status === 'FAIL') counters.rejected++;
    else if (probe.templateId === item.templateId) counters.correct++;
    else counters.wrong++;
  }
  for (const item of manifest.negative) {
    if (typeof item.file !== 'string') throw new Error('Each negative entry needs file');
    outcomes.negative.total++;
    const probe = await evaluate(item);
    if (probe.status === 'PASS') outcomes.negative.accepted++;
    else outcomes.negative.rejected++;
  }
  if (mutations.length) throw new Error('Capture sent a non-GET network request');
  const gate = Object.values({ coral: outcomes.coral, stream: outcomes.stream })
    .every(item => item.total >= 100 && item.correct / item.total >= .95 && item.wrong === 0) &&
    outcomes.negative.total > 0 && outcomes.negative.accepted === 0;
  const report = { evaluatedAtUtc: new Date().toISOString(),
    sampleType: 'real-printed-photo', markerFormatVersion: 1, outcomes,
    criteria: { minValidPerTemplate: 100, minRecognition: .95, maxWrongIdentity: 0,
      maxNegativeAccepted: 0 }, result: gate ? 'PASS' : 'NOT_MET' };
  writeFileSync(path.join(root, '.local/capture-evaluation.json'), JSON.stringify(report, null, 2) + '\n');
  console.log(JSON.stringify(report, null, 2));
  if (!gate) process.exitCode = 1;
} finally {
  await browser?.close();
  server.kill();
}
