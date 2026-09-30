// A local demonstration of the server's behavior states. The shared world is
// still simulated by Rust; this preview only animates the draft in this tab.
type Point3 = { x: number; y: number; depth: number };
export type PreviewPose = Point3 & { id: string; heading: { x: number; y: number };
  headingDepth: number; mode: 'cruise' | 'explore' | 'approach' | 'startled' };
type Mode = PreviewPose['mode'];
type FishState = { id: string; index: number; position: Point3; target: Point3;
  resume?: Point3; heading: Point3; baseSpeed: number; mode: Mode;
  untilTick: number; restUntilTick: number; nextExploreTick: number;
  peer?: string; generation: number; nextWanderTick: number };

const TICKS_PER_SECOND = 20;
const limits = { x: 4.8, y: 3.3, depth: 1.3 };
const BODY_HALF_LENGTH = 1.05;
const BODY_CLEARANCE = .88;

function distance(a: Point3, b: Point3): number {
  return Math.hypot(a.x - b.x, a.y - b.y, a.depth - b.depth);
}

function normalize(point: Point3): Point3 {
  const length = Math.hypot(point.x, point.y, point.depth);
  return length > 1e-6 ? { x: point.x / length, y: point.y / length,
    depth: point.depth / length } : { x: 1, y: 0, depth: 0 };
}

function angleDifference(from: number, to: number): number {
  return Math.atan2(Math.sin(to - from), Math.cos(to - from));
}

function dot(a: Point3, b: Point3): number {
  return a.x * b.x + a.y * b.y + a.depth * b.depth;
}

function subtract(a: Point3, b: Point3): Point3 {
  return { x: a.x - b.x, y: a.y - b.y, depth: a.depth - b.depth };
}

function bodyEndpoints(position: Point3, heading: Point3): [Point3, Point3] {
  const forward = normalize({ x: heading.x, y: 0, depth: heading.depth });
  const axis = { x: forward.x * BODY_HALF_LENGTH, y: 0,
    depth: forward.depth * BODY_HALF_LENGTH };
  return [subtract(position, axis), { x: position.x + axis.x,
    y: position.y, depth: position.depth + axis.depth }];
}

// Distance between two oriented body axes, minus their combined thickness.
// A slightly negative gap represents the rare gentle contact; it must not grow.
export function bodyGap(position: Point3, heading: Point3,
  otherPosition: Point3, otherHeading: Point3): number {
  const [a, b] = bodyEndpoints(position, heading);
  const [c, d] = bodyEndpoints(otherPosition, otherHeading);
  const u = subtract(b, a), v = subtract(d, c), w = subtract(a, c);
  const aa = dot(u, u), bb = dot(u, v), cc = dot(v, v);
  const dd = dot(u, w), ee = dot(v, w);
  const denominator = aa * cc - bb * bb;
  let s = denominator > 1e-8 ? Math.max(0, Math.min(1,
    (bb * ee - cc * dd) / denominator)) : 0;
  let t = Math.max(0, Math.min(1, (bb * s + ee) / cc));
  s = Math.max(0, Math.min(1, (bb * t - dd) / aa));
  t = Math.max(0, Math.min(1, (bb * s + ee) / cc));
  const closest = { x: w.x + u.x * s - v.x * t,
    y: w.y + u.y * s - v.y * t,
    depth: w.depth + u.depth * s - v.depth * t };
  return Math.hypot(closest.x, closest.y, closest.depth) - BODY_CLEARANCE;
}

export class PreviewSwimWorld {
  private readonly fish: FishState[];
  private readonly seed: number;
  private tick = 0;

  constructor(seed: number) {
    this.seed = seed;
    const starts: Point3[] = [
      { x: -2.8, y: 0, depth: -0.7 },
      { x: 2.4, y: -1.1, depth: 0.5 },
      { x: 0.3, y: 1.1, depth: 0.4 },
    ];
    const ids = ['local-preview-fish', 'preview-neighbor-1', 'preview-neighbor-2'];
    this.fish = ids.map((id, index) => ({ id, index, position: starts[index],
      target: this.randomTarget(index, 0),
      heading: { x: index === 1 ? -1 : 1, y: 0, depth: 0 },
      baseSpeed: [.95, .72, 1.15][index], mode: 'cruise', untilTick: 0,
      restUntilTick: 0, nextExploreTick: 160 + index * 90,
      generation: 0, nextWanderTick: 220 + index * 75 }));
  }

  private random(index: number, salt: number): number {
    let value = (this.seed ^ Math.imul(index + 1, 0x9e3779b9) ^
      Math.imul(salt + 1, 0x85ebca6b)) >>> 0;
    value ^= value >>> 16;
    value = Math.imul(value, 0x7feb352d);
    value ^= value >>> 15;
    value = Math.imul(value, 0x846ca68b);
    value ^= value >>> 16;
    return (value >>> 0) / 0xffffffff;
  }

  private randomTarget(index: number, generation: number): Point3 {
    const salt = generation * 3;
    const far = (generation + index) % 2 === 0;
    return { x: -4.1 + this.random(index, salt) * 8.2,
      y: -2.6 + this.random(index, salt + 1) * 5.2,
      depth: (far ? 1 : -1) * (.65 + this.random(index, salt + 2) * .55) };
  }

  private resume(fish: FishState): void {
    fish.mode = 'cruise';
    fish.target = fish.resume ?? this.randomTarget(fish.index, ++fish.generation);
    fish.resume = undefined;
    fish.peer = undefined;
  }

  private startSocial(): void {
    const available = this.fish.filter(fish => fish.mode === 'cruise');
    if (available.length < 2) return;
    const first = Math.floor(this.random(0, this.tick) * available.length);
    for (let offset = 0; offset < available.length; offset++) {
      const initiator = available[(first + offset) % available.length];
      const peer = available.filter(fish => fish !== initiator &&
        distance(fish.position, initiator.position) < 3.8)
        .sort((a, b) => distance(a.position, initiator.position) -
          distance(b.position, initiator.position))[0];
      if (!peer) continue;
      initiator.mode = 'approach';
      initiator.untilTick = this.tick + 100;
      initiator.resume = initiator.target;
      initiator.peer = peer.id;
      initiator.target = peer.position;
      return;
    }
  }

  private startExploration(fish: FishState): void {
    const surface = this.random(fish.index, this.tick) > .5;
    fish.mode = 'explore';
    fish.untilTick = this.tick + 180;
    fish.restUntilTick = this.tick + 16;
    fish.resume = fish.target;
    fish.target = { x: Math.max(-4.1, Math.min(4.1, fish.position.x +
      (this.random(fish.index, this.tick + 1) - .5) * 1.6)),
    y: surface ? 2.85 : -2.85, depth: surface ? -.6 : .6 };
    fish.nextExploreTick = this.tick + 360 +
      Math.floor(this.random(fish.index, this.tick + 2) * 180);
  }

  private move(fish: FishState): void {
    if (fish.mode === 'explore' && this.tick < fish.restUntilTick) return;
    if (fish.mode === 'approach') {
      const peer = this.fish.find(other => other.id === fish.peer);
      if (!peer || peer.mode !== 'cruise') { this.resume(fish); return; }
      fish.target = peer.position;
    }
    if (fish.mode === 'cruise' && (distance(fish.position, fish.target) < .3 ||
        this.tick >= fish.nextWanderTick)) {
      fish.target = this.randomTarget(fish.index, ++fish.generation);
      fish.nextWanderTick = this.tick + 180 +
        Math.floor(this.random(fish.index, fish.generation + 3000) * 160);
    }
    const delta = { x: fish.target.x - fish.position.x,
      y: fish.target.y - fish.position.y,
      depth: fish.target.depth - fish.position.depth };
    const remaining = Math.hypot(delta.x, delta.y, delta.depth);
    if (remaining < .12) return;
    const desired = normalize(delta);
    const currentYaw = Math.atan2(fish.heading.depth, fish.heading.x);
    const wantedYaw = Math.atan2(desired.depth, desired.x);
    const yaw = currentYaw + Math.max(-.09, Math.min(.09,
      angleDifference(currentYaw, wantedYaw)));
    const currentPitch = Math.asin(Math.max(-1, Math.min(1, fish.heading.y)));
    const wantedPitch = Math.asin(Math.max(-1, Math.min(1, desired.y)));
    const pitch = currentPitch + Math.max(-.07, Math.min(.07, wantedPitch - currentPitch));
    const direction = { x: Math.cos(yaw) * Math.cos(pitch), y: Math.sin(pitch),
      depth: Math.sin(yaw) * Math.cos(pitch) };
    const wave = .5 + .5 * Math.sin(this.tick / (38 + fish.index * 11) +
      fish.index * 2.3 + this.seed * .001);
    const pace = fish.mode === 'approach' || fish.mode === 'startled' ? 1.35 :
      fish.mode === 'explore' ? .65 : .42 + .58 * wave;
    const step = Math.min(remaining, fish.baseSpeed * pace / TICKS_PER_SECOND);
    const next = { x: Math.max(-limits.x, Math.min(limits.x,
      fish.position.x + direction.x * step)),
    y: Math.max(-limits.y, Math.min(limits.y, fish.position.y + direction.y * step)),
    depth: Math.max(-limits.depth, Math.min(limits.depth,
      fish.position.depth + direction.depth * step)) };
    const actual = { x: next.x - fish.position.x, y: next.y - fish.position.y,
      depth: next.depth - fish.position.depth };
    const actualLength = Math.hypot(actual.x, actual.y, actual.depth);
    const nextHeading = actualLength > 1e-5 ? normalize(actual) : fish.heading;
    if (actualLength > 1e-5) fish.heading =
      Math.hypot(actual.x, actual.depth) > .01 ? nextHeading :
        { ...fish.heading, y: nextHeading.y };
    fish.position = next;
  }

  private separateBodies(previous: Point3[]): void {
    for (let pass = 0; pass < 8; pass++) {
      let changed = false;
      for (let left = 0; left < this.fish.length; left++) for (let right = left + 1;
        right < this.fish.length; right++) {
        const a = this.fish[left], b = this.fish[right];
        const gap = bodyGap(a.position, a.heading, b.position, b.heading);
        if (gap >= -.08) continue;
        const sign = a.position.y >= b.position.y ? 1 : -1;
        const roomA = sign > 0 ? limits.y - a.position.y : a.position.y + limits.y;
        const roomB = sign > 0 ? b.position.y + limits.y : limits.y - b.position.y;
        const needed = Math.min(.2, -.08 - gap);
        const moveA = Math.min(roomA, needed / 2);
        const moveB = Math.min(roomB, needed - moveA);
        a.position = { ...a.position, y: a.position.y + sign * moveA };
        b.position = { ...b.position, y: b.position.y - sign * moveB };
        changed = true;
      }
      if (!changed) break;
    }
    for (const [index, fish] of this.fish.entries()) {
      const motion = subtract(fish.position, previous[index]);
      if (Math.hypot(motion.x, motion.y, motion.depth) > 1e-5) {
        const nextHeading = normalize(motion);
        fish.heading = Math.hypot(motion.x, motion.depth) > .01 ? nextHeading :
          { ...fish.heading, y: nextHeading.y };
      }
    }
  }

  step(): PreviewPose[] {
    this.tick++;
    const previous = this.fish.map(fish => ({ ...fish.position }));
    for (const fish of this.fish) {
      if (fish.mode !== 'cruise' && fish.untilTick <= this.tick) this.resume(fish);
    }
    if (this.tick % 120 === 0) this.startSocial();
    const pursued = new Set(this.fish.filter(fish => fish.mode === 'approach')
      .map(fish => fish.peer));
    for (const fish of this.fish) {
      if (fish.mode === 'cruise' && this.tick >= fish.nextExploreTick &&
          !pursued.has(fish.id)) this.startExploration(fish);
      this.move(fish);
    }
    this.separateBodies(previous);
    for (const fish of this.fish) {
      if (fish.mode !== 'approach') continue;
      const peer = this.fish.find(other => other.id === fish.peer);
      if (!peer || peer.mode !== 'cruise') { this.resume(fish); continue; }
      if (bodyGap(fish.position, fish.heading,
        peer.position, peer.heading) > .3) continue;
      this.resume(fish);
      peer.mode = 'startled';
      peer.untilTick = this.tick + 65;
      peer.resume = peer.target;
      const away = normalize({ x: peer.position.x - fish.position.x,
        y: peer.position.y - fish.position.y,
        depth: peer.position.depth - fish.position.depth });
      peer.target = { x: Math.max(-4.1, Math.min(4.1, peer.position.x + away.x * 2.5)),
        y: Math.max(-2.85, Math.min(2.85, peer.position.y + away.y * 2.5)),
        depth: Math.max(-1.05, Math.min(1.05,
          peer.position.depth + away.depth * 2.5)) };
    }
    return this.positions();
  }

  positions(): PreviewPose[] {
    return this.fish.map(fish => ({ id: fish.id, ...fish.position,
      heading: { x: fish.heading.x, y: fish.heading.y },
      headingDepth: fish.heading.depth, mode: fish.mode }));
  }
}
