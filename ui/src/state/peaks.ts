// Waveform peaks from `source_peaks`, cached per (source, norm, rev). Consumers poll `getPeaks`
// while drawing; a finished load bumps `peaksVersion` so canvases redraw.
import { createSignal } from 'solid-js';
import { be } from '../api/backend';
import type { Clip, Id } from '../api/types';
import { tuneIsNeutral } from '../lib/pitch';

const cache = new Map<string, Float32Array | 'loading' | 'error'>();
const [peaksVersion, setVersion] = createSignal(0);
export { peaksVersion };

export function getPeaks(id: Id, norm = false, rev = false): Float32Array | null {
  const key = id + (norm ? 'n' : '') + (rev ? 'r' : '');
  const v = cache.get(key);
  if (v instanceof Float32Array) return v;
  if (v) return null;
  cache.set(key, 'loading');
  be().sourcePeaks(id, norm, rev)
    .then(p => { cache.set(key, p); setVersion(x => x + 1); })
    .catch(e => { console.warn('[wave] source_peaks', id, e); cache.set(key, 'error'); });
  return null;
}

/** Forget cached peaks of a source (e.g. after trim, the id changes anyway). */
export function dropPeaks(id: Id) {
  for (const k of [...cache.keys()]) if (k.replace(/[nr]+$/, '') === String(id)) cache.delete(k);
}

/** Min/max of peaks over pair indices [i0, i1). */
export function peakRange(pk: Float32Array, i0: number, i1: number): [number, number] {
  const n = pk.length >> 1;
  let mn = 0, mx = 0;
  const a = Math.max(0, i0), b = Math.min(n, Math.max(i0 + 1, i1));
  for (let j = a; j < b; j++) { const lo = pk[2 * j], hi = pk[2 * j + 1]; if (lo < mn) mn = lo; if (hi > mx) mx = hi; }
  return [mn, mx];
}

// ---- clips with pitch edits: what the clip actually plays (`clip_peaks`) ----

interface ClipEntry { key: string; pk: Float32Array | null; want: string; timer: number; busy: boolean }
const clipCache = new Map<Id, ClipEntry>();
let gestureOpen: () => boolean = () => false;
/** edit.ts tells us when a drag is in progress (the engine re-renders only at its end). */
export function setGestureProbe(f: () => boolean) { gestureOpen = f; }

const clipKey = (c: Clip) => c.ref + '|' + (c.norm ? 1 : 0) + (c.rev ? 1 : 0) + '|' + JSON.stringify(c.tune ?? null);

/**
 * Peaks to draw for an audio clip: the source's, or — when the clip has pitch edits — the
 * rendered result from `clip_peaks`. While a new render is pending the previous peaks stay
 * on screen (no flicker); requests are debounced and never sent during a gesture.
 */
export function getClipPeaks(c: Clip): Float32Array | null {
  if (tuneIsNeutral(c.tune)) { clipCache.delete(c.id); return getPeaks(c.ref, c.norm, c.rev); }
  const key = clipKey(c);
  let e = clipCache.get(c.id);
  if (!e) { e = { key: '', pk: null, want: '', timer: 0, busy: false }; clipCache.set(c.id, e); }
  if (e.key !== key && e.want !== key) {
    e.want = key;
    clearTimeout(e.timer);
    const ent = e, id = c.id;
    const fire = () => {
      if (gestureOpen() || ent.busy) { ent.timer = window.setTimeout(fire, 120); return; }
      const k = ent.want; ent.busy = true;
      be().clipPeaks(id)
        .then(p => { if (ent.want === k) { ent.key = k; ent.pk = p; setVersion(x => x + 1); } })
        .catch(err => { console.warn('[wave] clip_peaks', id, err); ent.key = k; })
        .finally(() => { ent.busy = false; if (ent.want !== ent.key) { ent.timer = window.setTimeout(fire, 60); } });
    };
    e.timer = window.setTimeout(fire, 90);
  }
  return e.pk ?? getPeaks(c.ref, c.norm, c.rev);
}

/** True while the rendered peaks of a tuned clip lag behind its current edits (engine re-rendering). */
export function clipPeaksPending(c: Clip): boolean {
  if (tuneIsNeutral(c.tune)) return false;
  const e = clipCache.get(c.id);
  return !e || e.key !== clipKey(c);
}
