// Grid snapping. `snap` is the select value: -1 = auto, 0 = off, otherwise beats.
export const SNAPS: [string, number][] = [
  ['Snap: auto', -1], ['Snap: brak', 0], ['Snap: 1/4 beat', 0.25], ['Snap: 1/3 beat', 1 / 3],
  ['Snap: 1/2 beat', 0.5], ['Snap: beat', 1], ['Snap: bar', 4],
];

export function snapName(u: number): string {
  if (u === 0.25) return '1/4 beat';
  if (Math.abs(u - 1 / 3) < 1e-6) return '1/3 beat';
  if (u === 0.5) return '1/2 beat';
  if (u === 1) return 'beat';
  if (u === 2) return '2 beaty';
  if (u === 4) return 'bar';
  return 'brak';
}

/** Smallest of [¼, ½, 1, 2, 4] beats whose on-screen width is at least 10 px. */
export function autoSnap(pxPerBeat: number): number {
  return [0.25, 0.5, 1, 2, 4].find(u => u * pxPerBeat >= 10) ?? 4;
}

export function snapRes(snap: number, pxPerBeat: number): number {
  return snap < 0 ? autoSnap(pxPerBeat) : snap;
}

export function snapRound(b: number, unit: number): number {
  return unit ? Math.round(b / unit) * unit : Math.round(b * 64) / 64;
}
export function snapFloor(b: number, unit: number): number {
  return unit ? Math.floor(b / unit + 1e-9) * unit : Math.round(b * 64) / 64;
}

export function snapLabel(snap: number, pxPerBeat: number): string {
  return snap < 0 ? 'Snap: auto · ' + snapName(autoSnap(pxPerBeat)) : (SNAPS.find(s => s[1] === snap)?.[0] ?? 'Snap');
}
