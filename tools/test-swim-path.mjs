import assert from 'node:assert/strict';
import { PreviewSwimWorld } from '../prototypes/wasm-webgl/src/swim-path.ts';

const world = new PreviewSwimWorld(12345);
const replay = new PreviewSwimWorld(12345);
const otherSeed = new PreviewSwimWorld(54321);
const initial = world.positions();
assert.equal(initial.length, 3);
assert.equal(initial[0].id, 'local-preview-fish');
const pace = new Map(initial.map(fish => [fish.id, []]));
const seen = new Set();
let paused = false;
let differentSeeds = false;
for (let tick = 0; tick < 3600; tick++) {
  const before = world.positions();
  const after = world.step();
  assert.deepEqual(after, replay.step(), 'same preview seed must replay exactly');
  const alternate = otherSeed.step();
  differentSeeds ||= Math.abs(after[0].x - alternate[0].x) > .1;
  for (const [index, fish] of after.entries()) {
    seen.add(fish.mode);
    assert(Math.abs(fish.x) <= 4.8 && Math.abs(fish.y) <= 3.3 &&
      Math.abs(fish.depth) <= 1.3, `fish left the aquarium: ${JSON.stringify(fish)}`);
    const motion = [fish.x - before[index].x, fish.y - before[index].y,
      fish.depth - before[index].depth];
    const speed = Math.hypot(...motion);
    if (speed > .005) {
      const head = [fish.heading.x, fish.heading.y, fish.headingDepth];
      const dot = motion.reduce((total, value, part) => total + value * head[part], 0) / speed;
      assert(dot > .99, `fish moved tail-first: ${JSON.stringify({ tick, fish, dot })}`);
      pace.get(fish.id).push(speed);
    }
    paused ||= fish.mode === 'explore' && speed < 1e-6;
  }
  assert(Math.hypot(after[0].x - after[1].x, after[0].y - after[1].y,
    after[0].depth - after[1].depth) > .01, 'fish must not share one path');
}
assert(differentSeeds, 'different drawings must not repeat an identical route');
assert(paused, 'a fish should sometimes stop before investigating');
for (const mode of ['cruise', 'explore', 'approach', 'startled'])
  assert(seen.has(mode), `behavior ${mode} never appeared`);
for (const [id, samples] of pace) {
  assert(Math.max(...samples) > Math.min(...samples) * 1.6,
    `${id} never slowed or accelerated`);
}
assert.notDeepEqual(world.positions()[0], initial[0], 'the demonstration must not loop');
console.log('Preview behavior: distinct paths, changing pace, investigation, social reaction, head-first motion — PASS');
