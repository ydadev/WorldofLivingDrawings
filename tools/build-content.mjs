import { createHash } from 'node:crypto';
import { copyFileSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const source = path.join(root, 'prototypes/fish-assets/generated');
const destination = path.join(root, 'content/underwater/assets');
const generated = JSON.parse(readFileSync(path.join(source, 'manifest.json')));
mkdirSync(destination, { recursive: true });
const assets = [];
for (const fish of generated.species) {
  for (const [suffix, role, mediaType] of [
    ['.glb', 'model', 'model/gltf-binary'],
    ['.layout.json', 'layout', 'application/json'],
    ['.svg', 'template', 'image/svg+xml'],
    ['.paper.svg', 'paper', 'image/svg+xml'],
  ]) {
    const filename = `${fish.id}${suffix}`;
    const bytes = readFileSync(path.join(source, filename));
    copyFileSync(path.join(source, filename), path.join(destination, filename));
    assets.push({ id: `${fish.id}-${role}`, path: `assets/${filename}`,
      sha256: createHash('sha256').update(bytes).digest('hex'), mediaType,
      license: 'project-content' });
  }
}
const manifest = { schemaVersion: 1, packageId: 'underwater', packageVersion: 1, assets };
writeFileSync(path.join(root, 'content/underwater/manifest.json'), JSON.stringify(manifest, null, 2) + '\n');
console.log(`Underwater package: ${assets.length} assets with SHA-256`);
