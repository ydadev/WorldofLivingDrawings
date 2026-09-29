import { createHash } from 'node:crypto';
import { mkdirSync, writeFileSync, copyFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { deflateSync } from 'node:zlib';
import { bodyDepth, paintUV, species } from './species.mjs';

const root = path.dirname(fileURLToPath(import.meta.url));
const output = path.join(root, 'generated');
const publicOutput = path.resolve(root, '../wasm-webgl/public/fish');
mkdirSync(output, { recursive: true });
mkdirSync(publicOutput, { recursive: true });
const sha256 = data => createHash('sha256').update(data).digest('hex');
const round = x => Math.round(x * 1e6) / 1e6;

function meshData() {
  return { positions: [], normals: [], uvs: [], indices: [] };
}

function vertex(mesh, position, normal, uv = [0, 0]) {
  const index = mesh.positions.length / 3;
  mesh.positions.push(...position.map(round));
  mesh.normals.push(...normal.map(round));
  mesh.uvs.push(...uv.map(round));
  return index;
}

function triangle(mesh, a, b, c) {
  mesh.indices.push(a, b, c);
}

function normalized(x, y, z) {
  const length = Math.hypot(x, y, z);
  return [x / length, y / length, z / length];
}

function addBody(mesh, fish) {
  const segments = 24;
  for (let i = 0; i < fish.body.length; i++) {
    const [x, height, depth] = fish.body[i];
    const before = fish.body[Math.max(0, i - 1)];
    const after = fish.body[Math.min(fish.body.length - 1, i + 1)];
    const dh = (after[1] - before[1]) / (after[0] - before[0]);
    const dd = (after[2] - before[2]) / (after[0] - before[0]);
    for (let j = 0; j < segments; j++) {
      const angle = 2 * Math.PI * j / segments;
      const y = height * Math.cos(angle), z = depth * Math.sin(angle);
      const normal = normalized(
        -(y * y * dh / height ** 3 + z * z * dd / depth ** 3),
        y / height ** 2,
        z / depth ** 2,
      );
      vertex(mesh, [x, y, z], normal, paintUV(fish, x, y));
    }
  }
  for (let i = 0; i < fish.body.length - 1; i++) {
    for (let j = 0; j < segments; j++) {
      const next = (j + 1) % segments;
      const a = i * segments + j, b = (i + 1) * segments + j;
      const c = (i + 1) * segments + next, d = i * segments + next;
      triangle(mesh, a, c, b);
      triangle(mesh, a, d, c);
    }
  }
  for (const [end, normalX] of [[0, -1], [fish.body.length - 1, 1]]) {
    const [x] = fish.body[end];
    const center = vertex(mesh, [x, 0, 0], [normalX, 0, 0], paintUV(fish, x, 0));
    for (let j = 0; j < segments; j++) {
      const a = end * segments + j, b = end * segments + (j + 1) % segments;
      if (normalX > 0) triangle(mesh, center, b, a);
      else triangle(mesh, center, a, b);
    }
  }
}

function addFin(mesh, fish, fin) {
  let points = fin.points;
  const cross = (points[1][0] - points[0][0]) * (points[2][1] - points[0][1]) -
    (points[1][1] - points[0][1]) * (points[2][0] - points[0][0]);
  if (cross < 0) points = [points[0], points[2], points[1]];
  const front = points.map(([x, y]) => vertex(mesh, [x, y, fin.depth], [0, 0, 1], paintUV(fish, x, y)));
  const back = points.map(([x, y]) => vertex(mesh, [x, y, -fin.depth], [0, 0, -1], paintUV(fish, x, y)));
  triangle(mesh, ...front);
  triangle(mesh, back[0], back[2], back[1]);
  for (let i = 0; i < 3; i++) {
    const j = (i + 1) % 3;
    const [x0, y0] = points[i], [x1, y1] = points[j];
    const [nx, ny] = normalized(y1 - y0, x0 - x1, 0);
    const a = vertex(mesh, [x0, y0, fin.depth], [nx, ny, 0], paintUV(fish, x0, y0));
    const b = vertex(mesh, [x0, y0, -fin.depth], [nx, ny, 0], paintUV(fish, x0, y0));
    const c = vertex(mesh, [x1, y1, -fin.depth], [nx, ny, 0], paintUV(fish, x1, y1));
    const d = vertex(mesh, [x1, y1, fin.depth], [nx, ny, 0], paintUV(fish, x1, y1));
    triangle(mesh, a, b, c);
    triangle(mesh, a, c, d);
  }
}

function addEye(mesh, fish, radius, zOffset) {
  const [x, y] = fish.eye;
  for (const side of [-1, 1]) {
    const z = side * (bodyDepth(fish, x, y) + zOffset);
    const normal = [0, 0, side];
    const center = vertex(mesh, [x, y, z], normal);
    const ring = [];
    for (let j = 0; j < 16; j++) {
      const angle = 2 * Math.PI * j / 16;
      ring.push(vertex(mesh, [x + radius * Math.cos(angle), y + radius * Math.sin(angle), z], normal));
    }
    for (let j = 0; j < 16; j++) {
      const a = ring[j], b = ring[(j + 1) % 16];
      if (side > 0) triangle(mesh, center, a, b);
      else triangle(mesh, center, b, a);
    }
  }
}

function floatBuffer(numbers) {
  const data = Buffer.alloc(numbers.length * 4);
  numbers.forEach((value, i) => data.writeFloatLE(value, i * 4));
  return data;
}

function indexBuffer(numbers) {
  const data = Buffer.alloc(numbers.length * 2);
  numbers.forEach((value, i) => data.writeUInt16LE(value, i * 2));
  return data;
}

function whitePixelPNG() {
  function crc32(bytes) {
    let crc = 0xffffffff;
    for (const byte of bytes) {
      crc ^= byte;
      for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ (crc & 1 ? 0xedb88320 : 0);
    }
    return (crc ^ 0xffffffff) >>> 0;
  }
  function chunk(type, body) {
    const name = Buffer.from(type);
    const result = Buffer.alloc(12 + body.length);
    result.writeUInt32BE(body.length, 0);
    name.copy(result, 4);
    body.copy(result, 8);
    result.writeUInt32BE(crc32(Buffer.concat([name, body])), 8 + body.length);
    return result;
  }
  const header = Buffer.alloc(13);
  header.writeUInt32BE(1, 0);
  header.writeUInt32BE(1, 4);
  header[8] = 8;
  header[9] = 6;
  return Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    chunk('IHDR', header),
    chunk('IDAT', deflateSync(Buffer.from([0, 255, 255, 255, 255]))),
    chunk('IEND', Buffer.alloc(0)),
  ]);
}

function glb(fish, meshes, layoutHash) {
  const bufferViews = [], accessors = [], parts = [];
  let byteOffset = 0;
  function appendBytes(data, target) {
    const bufferView = bufferViews.length;
    bufferViews.push({ buffer: 0, byteOffset, byteLength: data.length, ...(target ? { target } : {}) });
    parts.push(data);
    byteOffset += data.length;
    const pad = (4 - byteOffset % 4) % 4;
    if (pad) { parts.push(Buffer.alloc(pad)); byteOffset += pad; }
    return bufferView;
  }
  function accessor(numbers, componentType, type, target, bounds = false) {
    const data = componentType === 5126 ? floatBuffer(numbers) : indexBuffer(numbers);
    const bufferView = appendBytes(data, target);
    const count = numbers.length / ({ SCALAR: 1, VEC2: 2, VEC3: 3 }[type]);
    const value = { bufferView, componentType, count, type };
    if (bounds) {
      value.min = [0, 1, 2].map(axis => Math.min(...numbers.filter((_, i) => i % 3 === axis)));
      value.max = [0, 1, 2].map(axis => Math.max(...numbers.filter((_, i) => i % 3 === axis)));
    }
    accessors.push(value);
    return accessors.length - 1;
  }
  const primitives = meshes.map((mesh, material) => ({
    attributes: {
      POSITION: accessor(mesh.positions, 5126, 'VEC3', 34962, true),
      NORMAL: accessor(mesh.normals, 5126, 'VEC3', 34962),
      ...(material === 0 ? { TEXCOORD_0: accessor(mesh.uvs, 5126, 'VEC2', 34962) } : {}),
    },
    indices: accessor(mesh.indices, 5123, 'SCALAR', 34963),
    material,
    mode: 4,
  }));
  const neutralImageView = appendBytes(whitePixelPNG());
  const json = {
    asset: { version: '2.0', generator: 'World of Living Drawings RISK-02' },
    scene: 0,
    scenes: [{ nodes: [0] }],
    nodes: [{ name: fish.id, mesh: 0 }],
    meshes: [{ name: fish.id, primitives }],
    materials: [
      { name: 'paint', pbrMetallicRoughness: { baseColorFactor: [1, 1, 1, 1], baseColorTexture: { index: 0, texCoord: 0 }, metallicFactor: 0, roughnessFactor: 0.9 } },
      { name: 'eye-white', pbrMetallicRoughness: { baseColorFactor: [1, 1, 1, 1], metallicFactor: 0, roughnessFactor: 0.9 } },
      { name: 'eye-black', pbrMetallicRoughness: { baseColorFactor: [0.015, 0.02, 0.04, 1], metallicFactor: 0, roughnessFactor: 0.9 } },
    ],
    images: [{ bufferView: neutralImageView, mimeType: 'image/png' }],
    textures: [{ source: 0, sampler: 0 }],
    samplers: [{ magFilter: 9729, minFilter: 9729, wrapS: 33071, wrapT: 33071 }],
    buffers: [{ byteLength: byteOffset }], bufferViews, accessors,
    extras: { templateId: fish.id, templateVersion: fish.templateVersion, layoutHash, paintMaterial: 'paint', uvSet: 0 },
  };
  const jsonBytes = Buffer.from(JSON.stringify(json));
  const jsonPad = (4 - jsonBytes.length % 4) % 4;
  const jsonChunk = Buffer.concat([jsonBytes, Buffer.alloc(jsonPad, 0x20)]);
  const binaryChunk = Buffer.concat(parts);
  const result = Buffer.alloc(12 + 8 + jsonChunk.length + 8 + binaryChunk.length);
  result.writeUInt32LE(0x46546c67, 0);
  result.writeUInt32LE(2, 4);
  result.writeUInt32LE(result.length, 8);
  result.writeUInt32LE(jsonChunk.length, 12);
  result.writeUInt32LE(0x4e4f534a, 16);
  jsonChunk.copy(result, 20);
  const binaryHead = 20 + jsonChunk.length;
  result.writeUInt32LE(binaryChunk.length, binaryHead);
  result.writeUInt32LE(0x004e4942, binaryHead + 4);
  binaryChunk.copy(result, binaryHead + 8);
  return result;
}

function point(fish, x, y) {
  const [u, v] = paintUV(fish, x, y);
  return [round(u * 512), round(v * 512)];
}

function printableSVG(fish, layoutHash) {
  const top = fish.body.map(([x, y]) => point(fish, x, y));
  const bottom = fish.body.toReversed().map(([x, y]) => point(fish, x, -y));
  const bodyPath = [...top, ...bottom].map(([x, y], i) => `${i ? 'L' : 'M'}${x},${y}`).join(' ') + ' Z';
  const fins = fish.fins.map(fin => `<polygon points="${fin.points.map(([x, y]) => point(fish, x, y).join(',')).join(' ')}"/>`).join('\n      ');
  const [eyeX, eyeY] = point(fish, ...fish.eye.slice(0, 2));
  const eyeRadiusX = round(fish.eye[2] / (fish.bounds[1] - fish.bounds[0]) * 512);
  const eyeRadiusY = round(fish.eye[2] / (fish.bounds[3] - fish.bounds[2]) * 512);
  return `<?xml version="1.0" encoding="UTF-8"?>\n` +
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512" width="170mm" height="170mm" role="img" aria-label="${fish.title}">\n` +
    `  <title>${fish.title} — боковая раскраска</title>\n` +
    `  <desc>Шаблон ${fish.id} v${fish.templateVersion}, SHA-256 раскладки ${layoutHash}. Только одна сторона для раскрашивания.</desc>\n` +
    `  <rect width="512" height="512" fill="white"/>\n` +
    `  <g fill="white" stroke="#172a33" stroke-width="2.5" stroke-linejoin="round">\n` +
    `    <path d="${bodyPath}"/>\n      ${fins}\n  </g>\n` +
    `  <ellipse cx="${eyeX}" cy="${eyeY}" rx="${eyeRadiusX}" ry="${eyeRadiusY}" fill="white" stroke="#172a33" stroke-width="2"/>\n` +
    `  <ellipse cx="${eyeX}" cy="${eyeY}" rx="${round(eyeRadiusX * 0.48)}" ry="${round(eyeRadiusY * 0.48)}" fill="#172a33"/>\n` +
    `  <text x="256" y="490" text-anchor="middle" fill="#172a33" font-family="sans-serif" font-size="12">${fish.title} · ${fish.id} v${fish.templateVersion}</text>\n` +
    `</svg>\n`;
}

const manifest = { schemaVersion: 1, projection: 'orthographic-side', textureSize: [512, 512], species: [] };
for (const fish of species) {
  const layoutBase = { templateId: fish.id, templateVersion: fish.templateVersion, projection: 'orthographic-side', textureSize: [512, 512], bounds: fish.bounds, body: fish.body, fins: fish.fins, eye: fish.eye, paintMask: { include: ['body', 'fins'], exclude: ['eye'] }, paintMaterial: 'paint', uvSet: 0, sideMapping: 'same-xy-on-both-sides', edgeMapping: 'project-xy', colorSpace: 'sRGB' };
  const layoutHash = sha256(JSON.stringify(layoutBase));
  const layout = { ...layoutBase, contentHash: layoutHash };
  const paint = meshData(), white = meshData(), black = meshData();
  addBody(paint, fish);
  fish.fins.forEach(fin => addFin(paint, fish, fin));
  addEye(white, fish, fish.eye[2], 0.012);
  addEye(black, fish, fish.eye[2] * 0.48, 0.017);
  const model = glb(fish, [paint, white, black], layoutHash);
  const svg = printableSVG(fish, layoutHash);
  writeFileSync(path.join(output, `${fish.id}.glb`), model);
  writeFileSync(path.join(output, `${fish.id}.svg`), svg);
  writeFileSync(path.join(output, `${fish.id}.layout.json`), JSON.stringify(layout, null, 2) + '\n');
  copyFileSync(path.join(output, `${fish.id}.glb`), path.join(publicOutput, `${fish.id}.glb`));
  copyFileSync(path.join(output, `${fish.id}.svg`), path.join(publicOutput, `${fish.id}.svg`));
  copyFileSync(path.join(output, `${fish.id}.layout.json`), path.join(publicOutput, `${fish.id}.layout.json`));
  manifest.species.push({ id: fish.id, templateVersion: fish.templateVersion, layoutHash, modelSha256: sha256(model), templateSha256: sha256(svg), paintVertexCount: paint.positions.length / 3 });
}
writeFileSync(path.join(output, 'manifest.json'), JSON.stringify(manifest, null, 2) + '\n');
console.log(`Generated ${species.length} fish models and printable templates.`);
