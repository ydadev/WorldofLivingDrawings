import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { validateBytes } from 'gltf-validator';
import { paintUV, species } from '../fish-assets/species.mjs';

const assets = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../fish-assets/generated');
const manifest = JSON.parse(readFileSync(path.join(assets, 'manifest.json')));
const digest = bytes => createHash('sha256').update(bytes).digest('hex');
function check(condition, message) { if (!condition) throw new Error(message); }

for (const fish of species) {
  const bytes = readFileSync(path.join(assets, `${fish.id}.glb`));
  const report = await validateBytes(new Uint8Array(bytes), { uri: `${fish.id}.glb` });
  check(report.issues.numErrors === 0 && report.issues.numWarnings === 0 && report.issues.numInfos === 0,
    `${fish.id}: glTF validation issues: ${JSON.stringify(report.issues.messages)}`);
  check(bytes.readUInt32LE(0) === 0x46546c67 && bytes.readUInt32LE(4) === 2 && bytes.readUInt32LE(8) === bytes.length,
    `${fish.id}: invalid GLB header`);
  const jsonLength = bytes.readUInt32LE(12);
  const gltf = JSON.parse(bytes.subarray(20, 20 + jsonLength).toString('utf8'));
  const binaryStart = 20 + jsonLength + 8;
  check(gltf.buffers.length === 1 && !gltf.buffers[0].uri && !gltf.images[0].uri,
    `${fish.id}: GLB must be self-contained`);
  const paint = gltf.meshes[0].primitives[0];
  check(gltf.materials[paint.material].name === 'paint' &&
    gltf.materials[paint.material].pbrMetallicRoughness.baseColorTexture.texCoord === 0,
    `${fish.id}: paint binding is missing`);
  check(gltf.meshes[0].primitives.length === 2 &&
    !gltf.meshes[0].primitives[1].attributes.TEXCOORD_0 &&
    gltf.materials[gltf.meshes[0].primitives[1].material].name === 'eye',
    `${fish.id}: eyes must use protected, unpainted materials`);
  const tailNode = gltf.nodes.find(node => node.name === 'tail-pivot');
  check(tailNode && tailNode.mesh === 1 && tailNode.translation?.length === 3 &&
    gltf.nodes[0].children.includes(gltf.nodes.indexOf(tailNode)) &&
    gltf.meshes[1]?.primitives.length === 1 && gltf.meshes[1].primitives[0].material === paint.material,
    `${fish.id}: animated painted tail is missing`);
  const layout = JSON.parse(readFileSync(path.join(assets, `${fish.id}.layout.json`)));
  const { contentHash, ...source } = layout;
  check(contentHash === digest(JSON.stringify(source)) && contentHash === gltf.extras.layoutHash,
    `${fish.id}: model and template versions differ`);
  const svg = readFileSync(path.join(assets, `${fish.id}.svg`));
  check(svg.includes(contentHash) && svg.includes(`v${fish.templateVersion}`),
    `${fish.id}: printable template version differs`);
  const entry = manifest.species.find(item => item.id === fish.id);
  check(entry?.modelSha256 === digest(bytes) && entry.templateSha256 === digest(svg),
    `${fish.id}: manifest digest differs`);

  function floats(index, dimensions) {
    const accessor = gltf.accessors[index], view = gltf.bufferViews[accessor.bufferView];
    check(accessor.componentType === 5126, `${fish.id}: expected float attribute`);
    const values = [];
    for (let i = 0; i < accessor.count; i++) {
      const position = [];
      for (let n = 0; n < dimensions; n++) {
        position.push(bytes.readFloatLE(binaryStart + view.byteOffset + i * dimensions * 4 + n * 4));
      }
      values.push(position);
    }
    return values;
  }
  const positions = floats(paint.attributes.POSITION, 3);
  const normals = floats(paint.attributes.NORMAL, 3);
  const colors = floats(paint.attributes.COLOR_0, 3);
  const uvs = floats(paint.attributes.TEXCOORD_0, 2);
  check(positions.length === uvs.length && positions.length === normals.length &&
    positions.length === colors.length && colors.every(rgb => rgb.every(value => value >= .57 && value <= 1)),
    `${fish.id}: paint attributes have different lengths`);
  const sidePairs = new Map();
  let edges = 0, front = 0, back = 0;
  for (let i = 0; i < positions.length; i++) {
    const [x, y, z] = positions[i];
    const [expectedU, expectedV] = paintUV(fish, x, y);
    const [u, v] = uvs[i];
    check(Number.isFinite(u) && Number.isFinite(v) && u >= 0 && u <= 1 && v >= 0 && v <= 1 &&
      Math.abs(u - expectedU) < 0.00001 && Math.abs(v - expectedV) < 0.00001,
    `${fish.id}: paint UV does not follow the side layout at vertex ${i}`);
    if (Math.abs(normals[i][2]) < 0.001 && (Math.abs(normals[i][0]) > 0.1 || Math.abs(normals[i][1]) > 0.1)) edges++;
    if (z > 0.001) front++;
    if (z < -0.001) back++;
    if (Math.abs(z) > 0.001) {
      const key = [x, y, Math.abs(z), u, v].map(value => value.toFixed(5)).join(':');
      const pair = sidePairs.get(key) ?? { front: false, back: false };
      pair[z > 0 ? 'front' : 'back'] = true;
      sidePairs.set(key, pair);
    }
  }
  const matched = [...sidePairs.values()].filter(pair => pair.front && pair.back).length;
  check(front > 80 && back > 80 && matched > 80 && edges >= 24,
    `${fish.id}: matching painted sides or colored edges are incomplete`);
  const tail = gltf.meshes[1].primitives[0];
  const tailPositions = floats(tail.attributes.POSITION, 3);
  const tailColors = floats(tail.attributes.COLOR_0, 3);
  const tailUVs = floats(tail.attributes.TEXCOORD_0, 2);
  check(tailPositions.length === tailUVs.length && tailPositions.length === tailColors.length &&
    tailPositions.length >= 24,
    `${fish.id}: tail paint geometry is incomplete`);
  for (let i = 0; i < tailPositions.length; i++) {
    const [x, y] = tailPositions[i];
    const expected = paintUV(fish, x + tailNode.translation[0], y);
    check(tailUVs[i].every((value, axis) => Math.abs(value - expected[axis]) < .00001),
      `${fish.id}: animated tail lost paint alignment at vertex ${i}`);
  }
  const eyeColors = floats(gltf.meshes[0].primitives[1].attributes.COLOR_0, 3);
  check(eyeColors.some(rgb => rgb.every(value => value > .99)) &&
    eyeColors.some(rgb => rgb.every(value => value < .05)),
    `${fish.id}: white eye and dark pupil must remain unpainted`);
  console.log(`${fish.id}: glTF valid; ${positions.length} paint vertices; ${matched} mirrored UV pairs; ${edges} edge vertices; template ${contentHash.slice(0, 12)}`);
}
console.log('RISK-02 geometry and template validation: PASS');
