import assert from 'node:assert/strict';
import { PreviewSwimWorld, bodyGap } from '../prototypes/wasm-webgl/src/swim-path.ts';

const world = new PreviewSwimWorld(12345);
const replay = new PreviewSwimWorld(12345);
const otherSeed = new PreviewSwimWorld(54321);
const initial = world.positions();
assert.equal(initial.length, 3);
assert.equal(initial[0].id, 'local-preview-fish');
const pace = new Map(initial.map(fish => [fish.id, []]));
const depthRange = new Map(initial.map(fish => [fish.id, { min: fish.depth, max: fish.depth }]));
const idle = new Map(initial.map(fish => [fish.id, 0]));
const longestIdle = new Map(initial.map(fish => [fish.id, 0]));
const seen = new Set();
let paused = false;
let differentSeeds = false;
let nearby = false;
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
    const range = depthRange.get(fish.id);
    range.min = Math.min(range.min, fish.depth);
    range.max = Math.max(range.max, fish.depth);
    const still = speed < .001 && fish.mode !== 'explore' ? idle.get(fish.id) + 1 : 0;
    idle.set(fish.id, still);
    longestIdle.set(fish.id, Math.max(longestIdle.get(fish.id), still));
    if (speed > .005) {
      const head = [fish.heading.x, fish.heading.y, fish.headingDepth];
      const dot = motion.reduce((total, value, part) => total + value * head[part], 0) / speed;
      assert(dot > .1,
        `fish moved tail-first: ${JSON.stringify({ tick, fish, dot })}`);
      pace.get(fish.id).push(speed);
    }
    paused ||= fish.mode === 'explore' && speed < 1e-6;
  }
  assert(Math.hypot(after[0].x - after[1].x, after[0].y - after[1].y,
    after[0].depth - after[1].depth) > .01, 'fish must not share one path');
  for (let left = 0; left < after.length; left++) for (let right = left + 1;
    right < after.length; right++) {
    const a = after[left], b = after[right];
    const gap = bodyGap(a, { ...a.heading, depth: a.headingDepth },
      b, { ...b.heading, depth: b.headingDepth });
    assert(gap >= -.35, `fish bodies passed through each other: ${JSON.stringify({ tick, gap, a, b })}`);
    nearby ||= gap < .5;
  }
}
assert(differentSeeds, 'different drawings must not repeat an identical route');
assert(paused, 'a fish should sometimes stop before investigating');
assert(nearby, 'fish should be able to pass at a small distance');
for (const mode of ['cruise', 'explore', 'approach', 'startled'])
  assert(seen.has(mode), `behavior ${mode} never appeared`);
for (const [id, samples] of pace) {
  assert(samples.length > 1200, `${id} was stuck instead of swimming (${samples.length} steps)`);
  assert(longestIdle.get(id) < 80, `${id} waited instead of passing another fish`);
  const range = depthRange.get(id);
  assert(range.min < -.55 && range.max > .55,
    `${id} did not swim away from the glass and return`);
  assert(Math.max(...samples) > Math.min(...samples) * 1.6,
    `${id} never slowed or accelerated: ${Math.min(...samples)}..${Math.max(...samples)} (${samples.length} steps)`);
}
assert.notDeepEqual(world.positions()[0], initial[0], 'the demonstration must not loop');
for (let seed = 1; seed <= 12; seed++) {
  const crowded = new PreviewSwimWorld(seed * 7919);
  let previousFrame = crowded.positions();
  for (let tick = 0; tick < 2400; tick++) {
    const poses = crowded.step();
    for (let left = 0; left < poses.length; left++) for (let right = left + 1;
      right < poses.length; right++) {
      const a = poses[left], b = poses[right];
      const gap = bodyGap(a, { ...a.heading, depth: a.headingDepth },
        b, { ...b.heading, depth: b.headingDepth });
      assert(gap >= -.35,
        `seed ${seed}, tick ${tick}: fish bodies passed through each other (${gap}): ${JSON.stringify({ a, b })}`);
    }
    if (tick % 1 === 0) {
      for (const fraction of [.25, .5, .75, 1]) {
        const blended = poses.map((pose, index) => {
          const start = previousFrame[index];
          const travel = { x: pose.x - start.x, depth: pose.depth - start.depth };
          const length = Math.hypot(travel.x, travel.depth);
          return { x: start.x + (pose.x - start.x) * fraction,
            y: start.y + (pose.y - start.y) * fraction,
            depth: start.depth + (pose.depth - start.depth) * fraction,
            heading: length > .01 ? { x: travel.x / length, y: 0,
              depth: travel.depth / length } :
              { ...start.heading, depth: start.headingDepth } };
        });
        for (let left = 0; left < blended.length; left++) for (let right = left + 1;
          right < blended.length; right++) {
          const a = blended[left], b = blended[right];
          const gap = bodyGap(a, a.heading, b, b.heading);
          assert(gap >= -.35,
            `seed ${seed}, tick ${tick}, interpolation ${fraction}: bodies cross (${gap}): ${JSON.stringify({ a, b, previousFrame, poses })}`);
        }
      }
      previousFrame = poses;
    }
  }
}
console.log('Preview behavior: distinct paths, changing pace, investigation, social reaction, head-first motion — PASS');
