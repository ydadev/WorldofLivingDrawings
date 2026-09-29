// Authoring source for the RISK-02 fish. X points from nose to tail;
// Y is up; +Z and -Z are the two matching painted sides.
export const species = [
  {
    id: 'coral',
    title: 'Круглая рыбка',
    templateVersion: 1,
    bounds: [-1.8, 1.9, -1.25, 1.25],
    body: [
      [-1.28, 0.07, 0.045], [-1.12, 0.26, 0.14], [-0.88, 0.46, 0.28],
      [-0.52, 0.62, 0.38], [-0.08, 0.68, 0.42], [0.37, 0.60, 0.35],
      [0.73, 0.39, 0.23], [1.05, 0.15, 0.09],
    ],
    fins: [
      { id: 'tail-top', points: [[0.98, 0.12], [1.62, 0.68], [1.38, 0.02]], depth: 0.055 },
      { id: 'tail-bottom', points: [[0.98, -0.12], [1.38, -0.02], [1.62, -0.68]], depth: 0.055 },
      { id: 'dorsal', points: [[-0.63, 0.55], [-0.15, 1.02], [0.40, 0.55]], depth: 0.06 },
      { id: 'pelvic', points: [[-0.28, -0.62], [0.15, -0.94], [0.43, -0.57]], depth: 0.05 },
    ],
    eye: [-0.95, 0.15, 0.105],
  },
  {
    id: 'stream',
    title: 'Быстрая рыбка',
    templateVersion: 1,
    bounds: [-2.0, 2.1, -1.1, 1.1],
    body: [
      [-1.50, 0.055, 0.035], [-1.27, 0.18, 0.11], [-0.94, 0.29, 0.19],
      [-0.48, 0.36, 0.25], [0.03, 0.36, 0.26], [0.51, 0.30, 0.22],
      [0.91, 0.20, 0.14], [1.23, 0.09, 0.065],
    ],
    fins: [
      { id: 'tail-top', points: [[1.17, 0.07], [1.88, 0.54], [1.57, 0.01]], depth: 0.04 },
      { id: 'tail-bottom', points: [[1.17, -0.07], [1.57, -0.01], [1.88, -0.54]], depth: 0.04 },
      { id: 'dorsal', points: [[-0.63, 0.34], [-0.26, 0.69], [0.14, 0.35]], depth: 0.045 },
      { id: 'pelvic', points: [[0.03, -0.35], [0.35, -0.60], [0.58, -0.29]], depth: 0.04 },
    ],
    eye: [-1.19, 0.075, 0.075],
  },
];

export function paintUV(fish, x, y) {
  const [left, right, bottom, top] = fish.bounds;
  return [(x - left) / (right - left), (top - y) / (top - bottom)];
}

export function bodyDepth(fish, x, y) {
  const sections = fish.body;
  let a = sections[0], b = sections[1];
  for (let i = 0; i < sections.length - 1; i++) {
    if (x >= sections[i][0] && x <= sections[i + 1][0]) {
      a = sections[i]; b = sections[i + 1]; break;
    }
  }
  const t = Math.max(0, Math.min(1, (x - a[0]) / (b[0] - a[0])));
  const height = a[1] + t * (b[1] - a[1]);
  const depth = a[2] + t * (b[2] - a[2]);
  return depth * Math.sqrt(Math.max(0, 1 - (y / height) ** 2));
}
