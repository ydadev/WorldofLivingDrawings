import { createServer } from 'node:http';
import { readFile, stat } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { WebSocketServer, WebSocket } from 'ws';

const root = path.dirname(fileURLToPath(import.meta.url));
const dist = path.join(root, 'dist');
const port = Number(process.env.LDW_LIVE_PORT ?? 4188);
const origin = `http://127.0.0.1:${port}`;
const sessionId = '00000000-0000-4000-8000-000000000001';
const sceneId = '00000000-0000-4000-8000-000000000002';
const types = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8', '.json': 'application/json', '.wasm': 'application/wasm',
  '.glb': 'model/gltf-binary', '.png': 'image/png' };
const csp = ["default-src 'none'", "script-src 'self' 'wasm-unsafe-eval'", "style-src 'self'",
  "img-src 'self' data: blob:", `connect-src 'self' ws://127.0.0.1:${port}`, "worker-src 'self'", "object-src 'none'",
  "base-uri 'none'", "frame-src 'self'", "frame-ancestors 'self'"].join('; ');
const state = { revision: 0, actions: [], commands: [], outcomes: new Map(), viewerRole: null };
const json = (response, status, value) => response.writeHead(status,
  { 'Content-Type': 'application/json' }).end(JSON.stringify(value));

const server = createServer(async (request, response) => {
  response.setHeader('Content-Security-Policy', csp);
  response.setHeader('X-Content-Type-Options', 'nosniff');
  const pathname = new URL(request.url ?? '/', origin).pathname;
  if (pathname === '/health') return json(response, 200, { status: 'ready' });
  if (pathname === '/drop') {
    for (const client of wss.clients) client.close();
    return json(response, 200, { dropped: true });
  }
  if (pathname === '/probe') return json(response, 200,
    { revision: state.revision, commands: state.commands, actions: state.actions });
  if (pathname.startsWith('/api/')) {
    if (request.method === 'POST' && request.headers.origin !== origin)
      return json(response, 403, { error: 'ORIGIN_DENIED' });
    if (request.method === 'POST' && pathname === '/api/login') {
      response.setHeader('Set-Cookie', 'fixture-owner=1; Path=/; HttpOnly; SameSite=Strict');
      return json(response, 200, { role: 'owner', csrf: 'fixture-owner-csrf' });
    }
    if (request.method === 'POST' && pathname === `/api/sessions/${sessionId}/pair`) {
      response.setHeader('Set-Cookie', 'fixture-controller=1; Path=/; HttpOnly; SameSite=Strict');
      return json(response, 200, { participant_id: 'fixture-participant', csrf: 'fixture-controller-csrf' });
    }
    if (request.method === 'POST' && pathname === `/api/sessions/${sessionId}/viewer-claims`) {
      state.viewerRole = null;
      response.setHeader('Set-Cookie', 'fixture-claim=1; Path=/; HttpOnly; SameSite=Strict');
      return json(response, 200, { claim_id: '00000000-0000-4000-8000-000000000003',
        code: '87654321', csrf: 'fixture-claim-csrf', expires_in_seconds: 300 });
    }
    if (request.method === 'POST' && pathname === `/api/sessions/${sessionId}/viewer-claims/approve`) {
      let body = '';
      for await (const chunk of request) body += chunk.toString('utf8');
      const values = JSON.parse(body);
      if (values.code !== '87654321') return json(response, 403, { error: 'ACCESS_DENIED' });
      state.viewerRole = values.interact ? 'viewer_interact' : 'viewer';
      response.writeHead(204).end(); return;
    }
    if (request.method === 'POST' && pathname ===
        `/api/sessions/${sessionId}/viewer-claims/00000000-0000-4000-8000-000000000003/activate`) {
      if (!state.viewerRole) return json(response, 202, { error: 'VIEWER_PENDING' });
      response.setHeader('Set-Cookie', 'fixture-viewer=1; Path=/; HttpOnly; SameSite=Strict');
      return json(response, 200, { role: state.viewerRole, csrf: 'fixture-viewer-csrf' });
    }
    if (request.method === 'POST' && pathname === '/api/sessions')
      return json(response, 200, { session_id: sessionId, scene_id: sceneId });
    if (request.method === 'POST' && pathname === `/api/sessions/${sessionId}/invitation`)
      return json(response, 200, { pin: '123456', expires_in_seconds: 300 });
    if (request.method === 'GET' && pathname === `/api/sessions/${sessionId}/scene`)
      return json(response, 200, { session_id: sessionId, scene_id: sceneId,
        world_id: 'underwater', world_version: 1, scene_epoch: 1, revision: state.revision });
    return json(response, 404, { error: 'NOT_FOUND' });
  }
  if (request.method !== 'GET') { response.writeHead(405).end(); return; }
  const target = path.resolve(dist, `.${pathname === '/' ? '/world.html' : pathname}`);
  if (!target.startsWith(dist + path.sep)) { response.writeHead(404).end(); return; }
  try {
    if (!(await stat(target)).isFile()) throw new Error('not a file');
    response.writeHead(200, { 'Content-Type': types[path.extname(target)] ?? 'application/octet-stream' });
    response.end(await readFile(target));
  } catch { response.writeHead(404).end(); }
});

const wss = new WebSocketServer({ noServer: true, maxPayload: 8192, perMessageDeflate: false });
server.on('upgrade', (request, socket, head) => {
  if (request.headers.origin !== origin ||
      new URL(request.url ?? '/', origin).pathname !== `/api/sessions/${sessionId}/ws`) {
    socket.destroy(); return;
  }
  wss.handleUpgrade(request, socket, head, client => wss.emit('connection', client));
});
const send = (socket, payload) => socket.send(JSON.stringify(payload));
const broadcast = payload => {
  const message = JSON.stringify(payload);
  for (const client of wss.clients) if (client.readyState === WebSocket.OPEN) client.send(message);
};
const delta = event => ({ type: 'delta', schemaVersion: 1, sceneId, sceneEpoch: 1,
  revision: ++state.revision, simulationTick: 0, upsert: [], remove: [], event });

wss.on('connection', socket => {
  socket.on('message', raw => {
    let message;
    try { message = JSON.parse(raw.toString('utf8')); } catch { socket.close(); return; }
    if (message.type === 'hello') {
      send(socket, { type: 'snapshot', schemaVersion: 1, sceneId, sceneEpoch: 1,
        revision: state.revision, simulationTick: 0, simulationVersion: 1,
        worldId: 'underwater', worldVersion: 1, entities: [], activeActions: state.actions,
        pendingInteractions: [], resources: {}, reservations: [], serverTime: Date.now() });
      return;
    }
    if (message.type === 'status') {
      send(socket, state.outcomes.get(message.commandId) ??
        { type: 'status', commandId: message.commandId, known: false });
      return;
    }
    if (message.type !== 'command') return;
    if (state.outcomes.has(message.commandId)) {
      send(socket, state.outcomes.get(message.commandId)); return;
    }
    const point = message.point;
    const accepted = ['feed', 'boat'].includes(message.interactionId) &&
      Number.isFinite(point?.x) && Number.isFinite(point?.y) &&
      Math.abs(point.x) <= 7.5 && Math.abs(point.y) <= 4 &&
      !(message.interactionId === 'boat' && point.y > 2.5);
    const code = accepted ? 'ACCEPTED' : message.interactionId === 'boat' && point?.y > 2.5
      ? 'INVALID_BOAT_ROUTE' : 'OUTSIDE_WATER';
    const ack = { type: 'ack', commandId: message.commandId, accepted, code,
      sceneId, sceneEpoch: 1, revision: accepted ? state.revision + 1 : state.revision };
    state.outcomes.set(message.commandId, ack);
    state.commands.push({ interactionId: message.interactionId, point, accepted });
    if (!accepted) { send(socket, ack); return; }
    send(socket, ack);
    broadcast(delta({ type: 'interaction_requested', commandId: message.commandId,
      interactionId: message.interactionId, point }));
    const id = `${message.interactionId}-${message.commandId.replace(/-/g, '')}`;
    const action = message.interactionId === 'feed'
      ? { id, interactionId: 'feed', point, remaining: 10, expiresAtTick: 300 }
      : { id, interactionId: 'boat', point, position: { x: -7.05, y: point.y },
          entry: { x: -7.05, y: point.y }, exit: { x: 7.05, y: point.y }, expiresAtTick: 600 };
    state.actions.push(action);
    setTimeout(() => broadcast(delta({ type: 'interaction_state', activeActions: state.actions,
      appliedCommandIds: [message.commandId], simulationTick: 0 })), 20);
  });
});

server.listen(port, '127.0.0.1', () => console.log(`Live UI probe ready on 127.0.0.1:${port}`));
