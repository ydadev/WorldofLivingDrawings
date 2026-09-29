import { createServer } from 'node:http';
import { readFile, stat } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { WebSocketServer, WebSocket } from 'ws';

const root = path.dirname(fileURLToPath(import.meta.url));
const dist = path.join(root, 'dist');
const port = Number(process.env.LDW_INTERACTION_PORT ?? 4187);
if (!Number.isInteger(port) || port < 1024 || port > 65535) throw new Error('Invalid local probe port');
const origin = `http://127.0.0.1:${port}`;
const contentTypes = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8', '.json': 'application/json', '.wasm': 'application/wasm',
  '.glb': 'model/gltf-binary', '.svg': 'image/svg+xml', '.png': 'image/png' };
const csp = ["default-src 'none'", "script-src 'self' 'wasm-unsafe-eval'", "style-src 'self'",
  "img-src 'self' data: blob:", `connect-src 'self' ws://127.0.0.1:${port}`,
  "worker-src 'self'", "object-src 'none'", "base-uri 'none'", "frame-ancestors 'none'"].join('; ');

const server = createServer(async (request, response) => {
  response.setHeader('Content-Security-Policy', csp);
  response.setHeader('X-Content-Type-Options', 'nosniff');
  if (request.method !== 'GET') { response.writeHead(405).end(); return; }
  const pathname = new URL(request.url ?? '/', origin).pathname;
  if (pathname === '/health') {
    response.writeHead(200, { 'Content-Type': 'application/json' }).end(JSON.stringify({ status: 'ready' }));
    return;
  }
  const target = path.resolve(dist, `.${pathname === '/' ? '/interaction.html' : pathname}`);
  if (!target.startsWith(dist + path.sep)) { response.writeHead(404).end(); return; }
  try {
    const info = await stat(target);
    if (!info.isFile()) throw new Error('Not a file');
    const bytes = await readFile(target);
    response.writeHead(200, { 'Content-Type': contentTypes[path.extname(target)] ?? 'application/octet-stream' });
    response.end(bytes);
  } catch { response.writeHead(404).end(); }
});

const wss = new WebSocketServer({ server, path: '/ws', maxPayload: 4096, perMessageDeflate: false,
  verifyClient: info => info.origin === origin });
const state = { sceneEpoch: 1, revision: 0, events: [] };
const commands = new Map();
const send = (socket, payload) => socket.send(JSON.stringify(payload));
const broadcast = payload => {
  const encoded = JSON.stringify(payload);
  for (const client of wss.clients) if (client.readyState === WebSocket.OPEN) client.send(encoded);
};

wss.on('connection', socket => {
  send(socket, { kind: 'snapshot', schemaVersion: 1, sceneEpoch: state.sceneEpoch,
    revision: state.revision, world: { width: 16, height: 9, planeZ: 0 }, events: state.events });
  socket.on('message', data => {
    let message;
    try { message = JSON.parse(data.toString('utf8')); }
    catch { send(socket, { kind: 'rejected', reason: 'INVALID_JSON' }); return; }
    const commandId = typeof message?.commandId === 'string' ? message.commandId : undefined;
    const reject = reason => send(socket, { kind: 'rejected', commandId, reason });
    if (message?.kind !== 'intent' || !commandId || !/^[a-zA-Z0-9-]{8,64}$/.test(commandId)) {
      reject('INVALID_INTENT'); return;
    }
    const signature = JSON.stringify([message.sceneEpoch, message.action, message.x, message.y]);
    const previous = commands.get(commandId);
    if (previous) {
      if (previous.signature !== signature) reject('COMMAND_CONFLICT');
      else send(socket, previous.ack);
      return;
    }
    if (message.sceneEpoch !== state.sceneEpoch) { reject('STALE_EPOCH'); return; }
    if (!['feed', 'boat'].includes(message.action) ||
        typeof message.x !== 'number' || !Number.isFinite(message.x) ||
        typeof message.y !== 'number' || !Number.isFinite(message.y)) {
      reject('INVALID_COORDINATE'); return;
    }
    if (Math.abs(message.x) > 7.5 || Math.abs(message.y) > 4) { reject('OUTSIDE_WATER'); return; }
    const event = { id: `evt-${state.revision + 1}`, action: message.action,
      x: Math.round(message.x * 10000) / 10000, y: Math.round(message.y * 10000) / 10000 };
    state.revision++;
    state.events.push(event);
    if (state.events.length > 100) state.events.shift();
    const ack = { kind: 'ack', commandId, accepted: true, sceneEpoch: state.sceneEpoch,
      revision: state.revision, eventId: event.id };
    commands.set(commandId, { signature, ack });
    if (commands.size > 1000) commands.delete(commands.keys().next().value);
    send(socket, ack);
    broadcast({ kind: 'event', schemaVersion: 1, sceneEpoch: state.sceneEpoch,
      revision: state.revision, event });
  });
});

server.listen(port, '127.0.0.1', () => console.log(`RISK-05 interaction probe ready on 127.0.0.1:${port}`));
