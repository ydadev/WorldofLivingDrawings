// Two straight, head-first passes joined by turns through the aquarium depth.
// The whole lap has constant speed; the heading is the tangent of this path.
export const SWIM_LAP_SECONDS = 24;
const SIDE = 3.5;
const DEPTH = 1.4;
const STRAIGHT = SIDE * 2;
const TURN = Math.PI * DEPTH;
const LAP_LENGTH = STRAIGHT * 2 + TURN * 2;

export function swimPath(seconds: number): { x: number; y: number; depth: number;
  heading: { x: number; y: number }; headingDepth: number } {
  const distance = ((seconds % SWIM_LAP_SECONDS) + SWIM_LAP_SECONDS) % SWIM_LAP_SECONDS /
    SWIM_LAP_SECONDS * LAP_LENGTH;
  let x: number, depth: number, dx: number, dz: number;
  if (distance < STRAIGHT) {
    x = -SIDE + distance; depth = -DEPTH; dx = 1; dz = 0;
  } else if (distance < STRAIGHT + TURN) {
    const angle = (distance - STRAIGHT) / DEPTH;
    x = SIDE + DEPTH * Math.sin(angle);
    depth = -DEPTH * Math.cos(angle);
    dx = Math.cos(angle); dz = Math.sin(angle);
  } else if (distance < STRAIGHT * 2 + TURN) {
    x = SIDE - (distance - STRAIGHT - TURN);
    depth = DEPTH; dx = -1; dz = 0;
  } else {
    const angle = (distance - STRAIGHT * 2 - TURN) / DEPTH;
    x = -SIDE - DEPTH * Math.sin(angle);
    depth = DEPTH * Math.cos(angle);
    dx = -Math.cos(angle); dz = -Math.sin(angle);
  }
  const bobAngle = distance * 4 * Math.PI / LAP_LENGTH + .5;
  const y = .15 * Math.sin(bobAngle);
  const dy = .15 * 4 * Math.PI / LAP_LENGTH * Math.cos(bobAngle);
  const speed = Math.hypot(dx, dy, dz);
  return { x, y, depth, heading: { x: dx / speed, y: dy / speed },
    headingDepth: dz / speed };
}
