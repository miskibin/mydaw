/**
 * Stereo correlation and a width/pan reading of a goniometer drag.
 *
 * Correlation is Pearson of L against R over a hop: 1 is mono, 0 is uncorrelated,
 * −1 is out of phase. Width 100 % is the identity; grabbing the cloud wider
 * writes the width parameter, grabbing it sideways writes pan.
 */

/** Pearson correlation of two equal-length channels. */
export function correlation(left: readonly number[], right: readonly number[]): number {
  const n = Math.min(left.length, right.length);
  if (n < 2) return 1;
  let sumL = 0;
  let sumR = 0;
  for (let i = 0; i < n; i++) {
    sumL += left[i] ?? 0;
    sumR += right[i] ?? 0;
  }
  const meanL = sumL / n;
  const meanR = sumR / n;
  let num = 0;
  let denL = 0;
  let denR = 0;
  for (let i = 0; i < n; i++) {
    const dl = (left[i] ?? 0) - meanL;
    const dr = (right[i] ?? 0) - meanR;
    num += dl * dr;
    denL += dl * dl;
    denR += dr * dr;
  }
  const den = Math.sqrt(denL * denR);
  if (den < 1e-12) return 1;
  return Math.min(1, Math.max(-1, num / den));
}

/**
 * A drag on the goniometer: x is pan (−1…1), y is width (0…200), origin centre.
 * Up is more width, matching "taller cloud = wider".
 */
export function goniometerFromPoint(
  x: number,
  y: number,
  width: number,
  height: number,
): { pan: number; widthPct: number } {
  const pan = width > 0 ? Math.min(1, Math.max(-1, (2 * x) / width - 1)) : 0;
  const widthPct = height > 0 ? Math.min(200, Math.max(0, (1 - y / height) * 200)) : 100;
  return { pan, widthPct };
}
