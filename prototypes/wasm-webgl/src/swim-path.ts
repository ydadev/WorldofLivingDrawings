// One unambiguous forward lap: right along the glass, away at the right edge,
// left at the back, and toward the glass at the left edge. The heading is the
// tangent of this same 3D path, so the nose always leads the displacement.
export const SWIM_LAP_SECONDS = 24;

export function swimPath(seconds: number): { x: number; y: number; depth: number;
  heading: { x: number; y: number }; headingDepth: number } {
  const angle = seconds * 2 * Math.PI / SWIM_LAP_SECONDS;
  const rate = 2 * Math.PI / SWIM_LAP_SECONDS;
  const x = 4.3 * Math.sin(angle);
  const y = .35 * Math.sin(2 * angle + .5);
  const depth = -1.4 * Math.cos(angle);
  const dx = rate * 4.3 * Math.cos(angle);
  const dy = rate * .7 * Math.cos(2 * angle + .5);
  const dz = rate * 1.4 * Math.sin(angle);
  const speed = Math.hypot(dx, dy, dz);
  return { x, y, depth, heading: { x: dx / speed, y: dy / speed },
    headingDepth: dz / speed };
}
