import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync, realpathSync, statSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import Ajv2020 from 'ajv/dist/2020.js';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const contentRoot = path.join(root, 'content/underwater');
const readJson = relative => JSON.parse(readFileSync(path.join(root, relative), 'utf8'));
const schema = readJson('schemas/v1/definitions.schema.json');
const ajv = new Ajv2020({ strict: true, allErrors: true });
ajv.addSchema(schema);
const validate = name => ajv.getSchema(`${schema.$id}#/$defs/${name}`);
const valid = (name, value) => {
  const run = validate(name);
  assert(run, `Missing schema ${name}`);
  assert(run(value), `${name}: ${ajv.errorsText(run.errors)}`);
};
const invalid = (name, value) => assert(!validate(name)(value), `${name} should reject ${JSON.stringify(value)}`);

const world = readJson('content/underwater/world.json');
const entities = readJson('content/underwater/entities.json');
const interactions = readJson('content/underwater/interactions.json');
const manifest = readJson('content/underwater/manifest.json');
valid('worldDefinition', world);
for (const entity of entities) valid('entityDefinition', entity);
for (const interaction of interactions) valid('interactionDefinition', interaction);
valid('assetManifest', manifest);
assert.equal(new Set(entities.map(item => item.id)).size, entities.length);
assert.equal(new Set(interactions.map(item => item.id)).size, interactions.length);
assert.equal(new Set(manifest.assets.map(item => item.id)).size, manifest.assets.length);
assert.deepEqual(world.entityDefinitions, entities.map(item => item.id));
assert.deepEqual(world.interactions, interactions.map(item => item.id));
const assetsById = new Map(manifest.assets.map(asset => [asset.id, asset]));
const checkAsset = asset => {
  const absolute = realpathSync(path.join(contentRoot, asset.path));
  assert(absolute.startsWith(realpathSync(contentRoot) + path.sep), `Asset leaves package: ${asset.path}`);
  assert(statSync(absolute).isFile(), `Missing file: ${asset.path}`);
  const digest = createHash('sha256').update(readFileSync(absolute)).digest('hex');
  assert.equal(asset.sha256, digest, `Asset digest: ${asset.path}`);
};
for (const asset of manifest.assets) checkAsset(asset);
assert.throws(() => checkAsset({ ...manifest.assets[0], path: '../../docs/STATUS.md' }), /leaves package/);
for (const entity of entities) {
  assert(world.entityDefinitions.includes(entity.id));
  assert(assetsById.has(entity.modelAssetId));
  const layoutAsset = assetsById.get(`${entity.paintTemplateId}-layout`);
  assert(layoutAsset, `Missing PaintLayout: ${entity.id}`);
  const layout = JSON.parse(readFileSync(path.join(contentRoot, layoutAsset.path), 'utf8'));
  assert.equal(layout.templateVersion, entity.paintTemplateVersion);
  assert.equal(layout.templateId, entity.paintTemplateId);
  assert.match(layout.contentHash, /^[a-f0-9]{64}$/);
}
for (const interaction of interactions) {
  assert(world.zones.some(zone => zone.id === interaction.allowedZoneId));
  assert(entities.some(entity => entity.capabilities.includes(interaction.requiredCapability)));
}
const layout = readJson('prototypes/fish-assets/generated/coral.layout.json');
const paint = { schemaVersion: 1, templateId: 'coral', templateVersion: 1,
  layoutHash: layout.contentHash, sourceKind: 'browser', colorSpace: 'sRGB',
  width: 512, height: 512, blobId: 'paint-001' };
valid('paintResult', paint);
valid('paintResult', { ...paint, sourceKind: 'paper' });
invalid('paintResult', { ...paint, sourceKind: 'camera' });
invalid('paintResult', { ...paint, schemaVersion: 2 });
invalid('paintResult', { ...paint, rawPhoto: 'not-allowed' });
invalid('assetManifest', { ...manifest, assets: [{ ...manifest.assets[0], sha256: 'broken' }] });
const snapshot = { schemaVersion: 1, sceneEpoch: 1, revision: 0, simulationTick: 0,
  worldId: world.id, worldVersion: world.version, entities: [] };
valid('sceneSnapshot', snapshot);
const requested = { type: 'interaction_requested', commandId: '00000000-0000-4000-8000-000000000001',
  interactionId: 'feed', point: { x: 2, y: -1 } };
valid('sceneSnapshot', { ...snapshot, type: 'snapshot', sceneId: 'scene-uuid', simulationVersion: 1,
  serverTime: 1, activeActions: [], pendingInteractions: [requested], resources: {}, reservations: [] });
valid('sceneDelta', { schemaVersion: 1, sceneEpoch: 1, revision: 1,
  simulationTick: 1, upsert: [], remove: [] });
valid('sceneDelta', { type: 'delta', sceneId: 'scene-uuid', schemaVersion: 1,
  sceneEpoch: 1, revision: 1, simulationTick: 0, upsert: [], remove: [], event: requested });
invalid('sceneDelta', { schemaVersion: 1, sceneEpoch: 1, revision: 0,
  simulationTick: 1, upsert: [], remove: [] });
const positions = { type: 'positions', schemaVersion: 1, sceneId: 'scene-uuid',
  sceneEpoch: 1, revision: 0, simulationTick: 10,
  positions: [{ id: 'fish-00000000000000000000000000000001',
    position: { x: 1, y: -1 }, heading: { x: 1, y: 0 } }] };
valid('scenePositions', positions);
invalid('scenePositions', { ...positions, simulationTick: -1 });
invalid('scenePositions', { ...positions, positions: [{ ...positions.positions[0], id: '1' }] });
valid('interactionIntent', { schemaVersion: 1, commandId: 'command-1', sceneEpoch: 1,
  interactionId: 'feed', point: { x: 2, y: -1 } });
invalid('interactionIntent', { schemaVersion: 1, commandId: 'command-1', sceneEpoch: 1,
  interactionId: 'feed', point: { x: 2, y: -1 }, participantIds: ['fish-1'] });
console.log('CORE-01 contracts, versions, references and self-contained asset hashes: PASS');
