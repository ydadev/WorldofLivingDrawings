import assert from 'node:assert/strict';
import { SceneConnection } from '../web/transport/src/index.ts';

class FakeSocket {
  onopen = null;
  onmessage = null;
  onclose = null;
  onerror = null;
  sent = [];
  closed = false;
  send(text) { this.sent.push(JSON.parse(text)); }
  close() { this.closed = true; this.onclose?.(); }
  open() { this.onopen?.(); }
  receive(message) { this.onmessage?.({ data: JSON.stringify(message) }); }
}

const sockets = [];
const states = [];
const snapshots = [];
const deltas = [];
const positions = [];
const results = [];
let now = 1_000_000;
const connection = new SceneConnection({
  sessionId: 'session-id', csrf: 'csrf-value', origin: 'https://world.example.test',
  createSocket: url => { const socket = new FakeSocket(); sockets.push({ socket, url }); return socket; },
  now: () => now, random: () => 0.5,
  onState: state => states.push(state),
  onSnapshot: snapshot => snapshots.push(snapshot),
  onDelta: delta => deltas.push(delta),
  onPositions: frame => positions.push(frame),
  onCommandResult: (id, result) => results.push({ id, result }),
});
const snapshot = (revision, epoch = 1) => ({
  type: 'snapshot', schemaVersion: 1, sceneId: 'scene-id', sceneEpoch: epoch,
  revision, simulationTick: 0, simulationVersion: 1, serverTime: now,
  worldId: 'underwater', worldVersion: 1, entities: [], activeActions: [],
  pendingInteractions: [], resources: {}, reservations: [],
});
const delta = revision => ({ type: 'delta', schemaVersion: 1, sceneId: 'scene-id',
  sceneEpoch: 1, revision, simulationTick: 0, upsert: [], remove: [],
  event: { type: 'interaction_requested', commandId: 'event', interactionId: 'feed', point: { x: 1, y: 1 } },
});

connection.start();
assert.equal(connection.sendInteraction('feed', { x: 0, y: 0 }), null, 'offline clicks are not queued');
assert.equal(sockets[0].url, 'wss://world.example.test/api/sessions/session-id/ws');
const first = sockets[0].socket;
first.open();
assert.deepEqual(first.sent[0], { type: 'hello', csrf: 'csrf-value' });
first.receive(snapshot(0));
assert.equal(connection.connectionState, 'ready');
const frame = (tick, revision = 0, epoch = 1) => ({
  type: 'positions', schemaVersion: 1, sceneId: 'scene-id', sceneEpoch: epoch, revision,
  simulationTick: tick, positions: [{ id: 'fish-id', position: { x: 1, y: 2 },
    heading: { x: 1, y: 0 } }],
});
first.receive(frame(10));
first.receive(frame(10));
first.receive(frame(9));
first.receive(frame(20, 2));
assert.equal(positions.length, 1, 'only a new tick based on an applied revision is accepted');
const firstId = connection.sendInteraction('feed', { x: 1, y: -1 });
assert.equal(typeof firstId, 'string');
assert.equal(first.sent.at(-1).commandId, firstId);
first.receive({ type: 'ack', commandId: firstId, accepted: true, code: 'ACCEPTED',
  sceneId: 'scene-id', sceneEpoch: 1, revision: 1 });
assert.equal(results.length, 1);
first.receive(delta(1));
assert.equal(deltas.length, 1);
first.receive(frame(20, 1));
assert.equal(positions.length, 2);
first.receive(delta(3));
assert.equal(first.closed, true, 'revision gap discards the socket');
assert.equal(sockets.length, 2, 'revision gap requests a fresh snapshot');
assert.equal(connection.sendInteraction('feed', { x: 0, y: 0 }), null, 'actions wait for snapshot');

const second = sockets[1].socket;
second.open();
second.receive(snapshot(3));
assert.equal(snapshots.at(-1).revision, 3);
second.receive(frame(5, 3));
assert.equal(positions.length, 3, 'a fresh snapshot resets the logical tick cursor');
second.receive(frame(30, 3, 2));
assert.equal(positions.length, 3, 'old or wrong-epoch frames are ignored');
const pendingId = connection.sendInteraction('boat', { x: 2, y: 1 });
const original = second.sent.at(-1);
second.close();
assert.equal(connection.connectionState, 'offline');
connection.reconnectNow();
const third = sockets[2].socket;
third.open();
third.receive(snapshot(3));
assert.deepEqual(third.sent.at(-1), { type: 'status', commandId: pendingId });
third.receive({ type: 'status', commandId: pendingId, known: false });
assert.deepEqual(third.sent.at(-1), original, 'unknown live command retries the same ID and expiry');
third.receive({ type: 'ack', commandId: pendingId, accepted: true, code: 'ACCEPTED',
  sceneId: 'scene-id', sceneEpoch: 1, revision: 4 });
assert.equal(results.at(-1).id, pendingId);

const expiredId = connection.sendInteraction('feed', { x: 0, y: 0 });
now += 11_000;
third.close();
connection.reconnectNow();
const fourth = sockets[3].socket;
fourth.open();
fourth.receive(snapshot(4));
fourth.receive({ type: 'status', commandId: expiredId, known: false });
assert.deepEqual(results.at(-1), { id: expiredId, result: null },
  'expired unknown action is not replayed as a new click');
assert.equal(fourth.sent.filter(item => item.type === 'command').length, 0);
connection.stop();
assert.equal(connection.connectionState, 'offline');
assert(states.includes('syncing') && states.includes('ready'));
console.log('Realtime transport reconnect, dedup, gap and expiry: PASS');
