import assert from 'node:assert/strict';
import { SWIM_LAP_SECONDS, swimPath } from '../prototypes/wasm-webgl/src/swim-path.ts';

const near = swimPath(0);
const turningAway = swimPath(SWIM_LAP_SECONDS / 4);
const far = swimPath(SWIM_LAP_SECONDS / 2);
const turningBack = swimPath(SWIM_LAP_SECONDS * 3 / 4);
const lapEnd = swimPath(SWIM_LAP_SECONDS);
assert(near.depth < -1.3 && near.heading.x > .9);
assert(turningAway.x > 3.9 && turningAway.headingDepth > .9);
assert(far.depth > 1.3 && far.heading.x < -.9);
assert(turningBack.x < -3.9 && turningBack.headingDepth < -.75);
assert(Math.hypot(near.x - lapEnd.x, near.y - lapEnd.y,
  near.depth - lapEnd.depth) < 1e-8, 'the lap must join without a position jump');

const samples = Array.from({ length: SWIM_LAP_SECONDS * 20 }, (_, index) =>
  swimPath(index / 20));
for (const point of samples) {
  assert(Math.abs(point.x) < 5 && Math.abs(point.y) < .6 && Math.abs(point.depth) <= 1.4);
  assert(Math.abs(Math.hypot(point.heading.x, point.heading.y, point.headingDepth) - 1) < 1e-6);
}
assert(samples.filter(point => Math.abs(point.heading.x) > .7).length > samples.length * .65,
  'the fish should be seen mostly from its painted side');
for (let index = 0; index < samples.length; index++) {
  const from = samples[index];
  const to = swimPath((index + 1) / 20);
  const travel = [to.x - from.x, to.y - from.y, to.depth - from.depth];
  const dot = from.heading.x * travel[0] + from.heading.y * travel[1] +
    from.headingDepth * travel[2];
  assert(dot > 0, `fish moves tail-first at sample ${index}`);
}
console.log('Swimming path: near, turn away, far, turn back, continuous 3D lap — PASS');
