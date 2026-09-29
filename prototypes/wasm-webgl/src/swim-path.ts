// A closed lap through the aquarium's volume. Most of the lap is viewed from
// the side; at each horizontal turn the fish points into/out of the water.
export const SWIM_LAP_SECONDS = 24;

export function swimPath(seconds: number): { x: number; y: number; depth: number;
  heading: { x: number; y: number }; headingDepth: number } {
  const angle = seconds * 2 * Math.PI / SWIM_LAP_SECONDS;
  const rate = 2 * Math.PI / SWIM_LAP_SECONDS;
  const x = 4.3 * Math.sin(angle) + .25 * Math.sin(3 * angle);
  const y = .35 * Math.sin(2 * angle + .5) + .2 * Math.sin(3 * angle + .8);
  const depth = -1.4 * Math.cos(angle);
  const dx = rate * (4.3 * Math.cos(angle) + .75 * Math.cos(3 * angle));
  const dy = rate * (.7 * Math.cos(2 * angle + .5) + .6 * Math.cos(3 * angle + .8));
  const dz = rate * 1.4 * Math.sin(angle);
  const speed = Math.hypot(dx, dy, dz);
  return { x, y, depth, heading: { x: dx / speed, y: dy / speed },
    headingDepth: dz / speed };
}
