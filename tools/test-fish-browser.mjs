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
  cwd: web, stdio: ['ignore', 'pipe', 'pipe'],
});
let stderr = '';
server.stderr.on('data', chunk => { stderr += chunk.toString(); });
let browser;
try {
  let ready = false;
  for (let i = 0; i < 60; i++) {
    if (server.exitCode !== null) throw new Error(`Vite exited: ${stderr}`);
    try {
      const response = await fetch('http://127.0.0.1:4173/fish.html');
      if (response.ok) { ready = true; break; }
    } catch { /* Preview is still starting. */ }
    await new Promise(resolve => setTimeout(resolve, 250));
  }
  if (!ready) throw new Error(`Vite preview failed: ${stderr}`);
  browser = await chromium.launch({
    executablePath: executable, headless: true,
    args: ['--enable-unsafe-swiftshader', '--use-gl=angle', '--use-angle=swiftshader'],
  });
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const pageErrors = [];
  page.on('pageerror', error => pageErrors.push(error.message));
  const response = await page.goto('http://127.0.0.1:4173/fish.html', { waitUntil: 'networkidle' });
  if (!response?.ok()) throw new Error(`Fish page HTTP ${response?.status()}`);
  const csp = response.headers()['content-security-policy'];
  if (!csp?.includes("'wasm-unsafe-eval'") || csp.includes("'unsafe-eval'")) throw new Error(`Unexpected CSP: ${csp}`);
  mkdirSync(path.join(root, '.local'), { recursive: true });
  for (const fish of ['coral', 'stream']) {
    if (fish === 'stream') await page.locator('[data-fish="stream"]').click();
    for (const side of ['near', 'far', 'edge']) {
      if (side !== 'near' || fish === 'stream') await page.locator(`[data-side="${side}"]`).click();
      await page.waitForFunction(({ fish, side }) =>
        (window.fishProbe?.status === 'FAIL') ||
        (window.fishProbe?.species === fish && window.fishProbe?.side === side && (window.fishProbe?.frames ?? 0) >= 20),
      { fish, side }, { timeout: 30000 });
      const result = await page.evaluate(() => window.fishProbe);
      if (result?.status !== 'PASS' || result.species !== fish || result.side !== side ||
          result.webglVersion !== 2 || result.paintMaterialCount !== 1 || result.protectedEyeMaterialCount !== 1)
        throw new Error(`Fish browser probe failed: ${JSON.stringify(result)}`);
      const isRed = pixel => pixel?.[0] > 100 && pixel[0] > pixel[1] * 1.7 && pixel[0] > pixel[2] * 1.5;
      const isBlue = pixel => pixel?.[2] > 100 && pixel[2] > pixel[0] * 1.4 && pixel[2] > pixel[1] * 1.4;
      if (side === 'near' && (!isRed(result.leftPixel) || !isBlue(result.rightPixel)))
        throw new Error(`${fish}: front head/tail paint direction is wrong: ${JSON.stringify(result)}`);
      if (side === 'far' && (!isBlue(result.leftPixel) || !isRed(result.rightPixel)))
        throw new Error(`${fish}: back side is not mirrored correctly: ${JSON.stringify(result)}`);
      const imageReady = await page.locator('#fish-template').evaluate(image => image.complete && image.naturalWidth > 0);
      if (!imageReady) throw new Error(`${fish}: printable template image not loaded`);
      if (pageErrors.length) throw new Error(`Browser runtime errors: ${pageErrors.join(' | ')}`);
      await page.screenshot({ path: path.join(root, '.local', `risk02-${fish}-${side}.png`) });
      console.log(`${fish}/${side}: WebGL 2, ${result.frames} frames, paint/eye materials, pixels ${JSON.stringify(result.leftPixel)} ${JSON.stringify(result.rightPixel)}: PASS`);
    }
  }
} finally {
  await browser?.close();
  server.kill();
}
