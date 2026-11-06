import type { LoopRange, Project } from '../api/types';

/** Beat range for a song or loop bounce — mirrors `Engine::export_range`. */
export function exportRange(project: Project, useLoop: boolean): { from: number; to: number } {
  if (useLoop) {
    const l = project.loop;
    if (l && l.b > l.a) return { from: l.a, to: l.b };
  }
  return { from: 0, to: songEnd(project) };
}

/** End of the last clip, rounded up to a bar; at least one bar. */
export function songEnd(project: Project): number {
  const end = Math.max(4, ...project.clips.map(c => c.start + c.len));
  return Math.ceil(end / 4) * 4;
}

/** Wall-clock length of a bounce including the effect tail. */
export function exportSeconds(from: number, to: number, bpm: number, tail: number): number {
  const beats = Math.max(0, to - from);
  return (beats * 60) / Math.max(1, bpm) + Math.max(0, tail);
}

export function fmtExportLen(seconds: number): string {
  if (seconds < 60) return seconds.toFixed(1).replace(/\.0$/, '') + ' s';
  const m = Math.floor(seconds / 60);
  const s = Math.round(seconds - m * 60);
  return m + ':' + String(s).padStart(2, '0');
}

export function hasLoop(loop: LoopRange | null | undefined): boolean {
  return !!loop && loop.b > loop.a;
}
