import { copyFileSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import QRCode from 'qrcode';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const format = JSON.parse(readFileSync(path.join(root, 'fish-assets/paper-format.json'), 'utf8'));
const output = path.join(root, 'fish-assets/generated');
const publicOutput = path.join(root, 'wasm-webgl/public/fish');
copyFileSync(path.join(root, 'fish-assets/paper-format.json'), path.join(publicOutput, 'paper-format.json'));

function qrSymbol(text, x, y, size) {
  const qr = QRCode.create(text, { errorCorrectionLevel: 'M' });
  const modules = qr.modules.size;
  const cell = size / (modules + 8);
  let pathData = '';
  for (let row = 0; row < modules; row++) {
    for (let col = 0; col < modules; col++) {
      if (!qr.modules.get(row, col)) continue;
      const px = (x + (col + 4) * cell).toFixed(4);
      const py = (y + (row + 4) * cell).toFixed(4);
      pathData += `M${px} ${py}h${cell.toFixed(4)}v${cell.toFixed(4)}h-${cell.toFixed(4)}z`;
    }
  }
  return `<rect x="${x}" y="${y}" width="${size}" height="${size}" fill="white"/><path d="${pathData}" fill="black"/>`;
}

for (const id of ['coral', 'stream']) {
  const layout = JSON.parse(readFileSync(path.join(output, `${id}.layout.json`), 'utf8'));
  const original = readFileSync(path.join(output, `${id}.svg`), 'utf8');
  const inner = original.match(/<svg[^>]*>([\s\S]*?)<\/svg>/)?.[1];
  if (!inner) throw new Error(`Invalid SVG for ${id}`);
  const [imageX, imageY, imageWidth, imageHeight] = format.imageRectMm;
  const markers = Object.entries(format.markers).map(([corner, [cx, cy]]) => {
    const payload = `WLD${format.markerFormatVersion}|${id}|${layout.templateVersion}|${layout.contentHash.slice(0, 20)}|${corner}`;
    return qrSymbol(payload, cx - format.markerSizeMm / 2,
      cy - format.markerSizeMm / 2, format.markerSizeMm);
  }).join('\n  ');
  const svg = `<?xml version="1.0" encoding="UTF-8"?>\n` +
    `<svg xmlns="http://www.w3.org/2000/svg" width="210mm" height="297mm" viewBox="0 0 210 297">\n` +
    `<title>${id} — печатный лист v${layout.templateVersion}</title>\n` +
    `<rect width="210" height="297" fill="white"/>\n  ${markers}\n` +
    `<g transform="translate(${imageX} ${imageY}) scale(${(imageWidth / 512).toFixed(8)} ${(imageHeight / 512).toFixed(8)})">${inner}</g>\n` +
    `<text x="105" y="55" font-family="sans-serif" font-size="4" text-anchor="middle">Раскрась рыбку внутри контура. Не закрашивай четыре QR-маркера.</text>\n` +
    `<text x="105" y="286" font-family="sans-serif" font-size="3" text-anchor="middle">${id} · шаблон ${layout.templateVersion} · лист WLD${format.markerFormatVersion} · ${layout.contentHash.slice(0, 20)}</text>\n` +
    `</svg>\n`;
  const filename = `${id}.paper.svg`;
  writeFileSync(path.join(output, filename), svg);
  copyFileSync(path.join(output, filename), path.join(publicOutput, filename));
  console.log(`${id}: A4 printable sheet with four versioned QR markers`);
}
