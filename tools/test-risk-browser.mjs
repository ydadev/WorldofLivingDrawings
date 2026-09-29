import { spawn } from 'node:child_process';
import { existsSync, mkdirSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from '../prototypes/wasm-webgl/node_modules/playwright-core/index.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const web = path.join(root, 'prototypes/wasm-webgl');
const executable = process.env.LDW_CHROME_PATH || [
  'C:/Program Files/Google/Chrome/Application/chrome.exe',
  '/usr/bin/google-chrome',
  '/usr/bin/chromium',
].find(existsSync);
if (!executable) throw new Error('Set LDW_CHROME_PATH to an installed Chrome executable');

const vite = path.join(web, 'node_modules/vite/bin/vite.js');
const server = spawn(process.execPath, [vite, 'preview', '--host', '127.0.0.1', '--port', '4173', '--strictPort'], {
  cwd: web,
  stdio: ['ignore', 'pipe', 'pipe'],
});
let stderr = '';
server.stderr.on('data', chunk => { stderr += chunk.toString(); });
let browser;
try {
  let ready = false;
  for (let i = 0; i < 60; i++) {
    if (server.exitCode !== null) throw new Error(`Vite exited: ${stderr}`);
    try {
      const response = await fetch('http://127.0.0.1:4173/');
      if (response.ok) { ready = true; break; }
    } catch { /* Preview is still starting. */ }
    await new Promise(resolve => setTimeout(resolve, 250));
  }
  if (!ready) throw new Error(`Vite preview failed to start: ${stderr}`);

  browser = await chromium.launch({
    executablePath: executable,
    headless: true,
    args: ['--enable-unsafe-swiftshader', '--use-gl=angle', '--use-angle=swiftshader'],
  });
  const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
  const pageErrors = [];
  page.on('pageerror', error => pageErrors.push(error.message));
  const response = await page.goto('http://127.0.0.1:4173/', { waitUntil: 'networkidle' });
  if (!response?.ok()) throw new Error(`Page HTTP ${response?.status()}`);
  const csp = response.headers()['content-security-policy'];
  if (!csp?.includes("'wasm-unsafe-eval'") || !csp.includes("worker-src 'self'") ||
      csp.includes("'unsafe-eval'") || csp.includes("'unsafe-inline'"))
    throw new Error(`Unexpected CSP: ${csp}`);
  await page.waitForFunction(() => window.risk01?.status, null, { timeout: 15000 });
  const result = await page.evaluate(() => window.risk01);
  if (result?.status !== 'PASS' || result.webglVersion !== 2 || result.worker !== 'ready' || result.wasmPhase !== 0.25 || result.evalBlocked !== true)
    throw new Error(`Browser probe failed: ${JSON.stringify(result)}`);
  await page.waitForFunction(() => (window.risk01?.frames ?? 0) >= 5, null, { timeout: 15000 });
  if (pageErrors.length) throw new Error(`Browser runtime errors: ${pageErrors.join(' | ')}`);
  const rendered = await page.evaluate(() => window.risk01);
  const pixel = rendered?.centerPixel;
  if (!pixel || pixel[0] < 180 || pixel[1] < 100 || pixel[0] - pixel[2] < 40 || pixel[3] !== 255)
    throw new Error(`Expected rendered sphere at canvas center, got ${JSON.stringify(pixel)}`);
  const artifactDir = path.join(root, '.local');
  mkdirSync(artifactDir, { recursive: true });
  await page.screenshot({ path: path.join(artifactDir, 'risk01-chrome.png') });
  console.log(`RISK-01 Chrome PASS: ${JSON.stringify(rendered)}; JS eval blocked; CSP active`);
} finally {
  await browser?.close();
  server.kill();
}
