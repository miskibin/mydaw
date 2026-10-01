/** Browser gestures shared by library and search results. */
export type SampleAction = 'preview' | 'replace' | 'add';

export function sampleKeyAction(key: string, shift: boolean): SampleAction | null {
  if (key === 'Enter') return shift ? 'add' : 'replace';
  if (key === ' ') return 'preview';
  return null;
}

/** A monotonically increasing ticket; stale async results never win. */
export class LatestRequest {
  private generation = 0;
  begin(): number {
    return ++this.generation;
  }
  current(ticket: number): boolean {
    return ticket === this.generation;
  }
  cancel(): void {
    this.generation++;
  }
}

export function waveformPath(peaks: readonly [number, number][]): string {
  if (peaks.length === 0) return '';
  const x = (i: number) => ((i + 0.5) / peaks.length) * 256;
  return peaks
    .map(
      (p, i) => `M${x(i).toFixed(2)} ${(24 - p[1] * 22).toFixed(2)}V${(24 - p[0] * 22).toFixed(2)}`,
    )
    .join('');
}
