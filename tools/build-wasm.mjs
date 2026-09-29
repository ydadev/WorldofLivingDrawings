import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const manifest = path.join(root, 'prototypes/wasm-core/Cargo.toml');
const target = 'wasm32v1-none';
execFileSync('cargo', ['build', '--locked', '--release', '--target', target, '--manifest-path', manifest], {
  cwd: root,
  stdio: 'inherit',
});
const output = path.join(root, 'prototypes/wasm-webgl/public');
mkdirSync(output, { recursive: true });
copyFileSync(
  path.join(root, 'prototypes/wasm-core/target', target, 'release/ldw_wasm_probe.wasm'),
  path.join(output, 'sim.wasm'),
);
console.log('WebAssembly probe copied to prototype public directory.');
