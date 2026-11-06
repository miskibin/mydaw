// Waveform peaks from `source_peaks`, cached per (source, norm, rev). Consumers poll `getPeaks`
// while drawing; a finished load bumps `peaksVersion` so canvases redraw.
import { createSignal } from 'solid-js';
import { be } from '../api/backend';
import type { Id } from '../api/types';

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
