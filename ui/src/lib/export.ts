import type { Insert, LoopRange, Project } from '../api/types';

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

/**
 * The inserts a stem export writes a file for, in mixer order — mirrors `Engine::stem_inserts`: not the
 * master, not muted, with a track that has an unmuted clip or an unmuted channel sent to it. (The engine
 * also drops one that renders silent, so the real count can be lower.)
 */
export function stemInserts(project: Project): Insert[] {
  return project.inserts.filter(m => m.id !== 0 && !m.muted).filter(m => {
    const onTrack = project.tracks.some((t, i) => t.insert === m.id && !t.muted && project.clips.some(c => c.track === i && !c.muted));
    const onChannel = (project.channels ?? []).some(c => c.insert === m.id && !c.mute);
    return onTrack || onChannel;
  });
}

/** A file name safe on Windows, macOS and Linux — mirrors `actions::safe_file_stem`. */
export function safeFileStem(name: string): string {
  // eslint-disable-next-line no-control-regex
  let s = name.replace(/[\u0000-\u001f\u007f<>:"/\\|?*]/g, '_').trim().replace(/[. ]+$/, '');
  s = Array.from(s).slice(0, 100).join('').replace(/ +$/, '');
  if (!s) return 'Insert';
  const base = s.split('.')[0].toUpperCase();
  return /^(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])$/.test(base) ? '_' + s : s;
}

/** File stems for `names` in one folder: made safe and unique with " (2)", " (3)"; case-insensitive — mirrors `actions::unique_file_stems`. */
export function uniqueFileStems(names: string[]): string[] {
  const taken = new Set<string>();
  return names.map(n => {
    const base = safeFileStem(n);
    let cand = base;
    for (let k = 2; taken.has(cand.toLowerCase()); k++) cand = `${base} (${k})`;
    taken.add(cand.toLowerCase());
    return cand;
  });
}
