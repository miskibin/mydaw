// Data for the in-browser mock backend: the catalog of crates/engine/src/catalog.rs and the
// demo song of crates/engine/src/demo.rs. Only the mock uses these; the real UI reads `fx_catalog`.
import { VOL_UNITY, type BuiltinKind, type Catalog, type Clip, type EqBand, type Inst, type Note, type ParamDef, type Preset, type Project } from './types';
import { defaultBands, loadPreset, newFx } from '../lib/presets';
import { builtinChannel, INSTS } from '../lib/pattern';

const p = (id: string, label: string, min: number, max: number, def: number, unit: string, log = false): ParamDef => ({ id, label, min, max, def, unit, log });
/** catalog.rs `choice`: a discrete parameter (value = index) shown as buttons. */
const choice = (id: string, label: string, def: number, options: string[]): ParamDef => ({ id, label, min: 0, max: options.length - 1, def, unit: '', log: false, options });

const PARAMS: Record<BuiltinKind, ParamDef[]> = {
  eq: [p('out', 'Output', -18, 18, 0, 'dB')],
  comp: [p('thr', 'Thresh', -60, 0, -18, 'dB'), p('ratio', 'Ratio', 1, 20, 4, ':1', true), p('att', 'Attack', 0.1, 100, 10, 'ms', true),
    p('rel', 'Release', 10, 1000, 150, 'ms', true), p('gain', 'Makeup', 0, 24, 0, 'dB')],
  sat: [p('drive', 'Drive', 0, 36, 6, 'dB'), p('mix', 'Mix', 0, 100, 100, '%'), p('out', 'Output', -24, 6, 0, 'dB')],
  delay: [p('time', 'Time', 10, 1500, 375, 'ms', true), p('fb', 'Feedback', 0, 95, 35, '%'), p('mix', 'Mix', 0, 100, 25, '%')],
  reverb: [p('size', 'Decay', 0.3, 8, 2.2, 's', true), p('pre', 'Pre-delay', 0, 200, 20, 'ms'), p('lo', 'Low cut', 20, 1000, 120, 'Hz', true),
    p('hi', 'High cut', 1000, 20000, 8000, 'Hz', true), p('width', 'Width', 0, 100, 100, '%'), p('mix', 'Mix', 0, 100, 25, '%')],
  limit: [p('gain', 'Gain', 0, 24, 0, 'dB'), p('ceil', 'Ceiling', -12, 0, -0.3, 'dB')],
  fet: [p('input', 'Input', 0, 36, 8, 'dB'), p('output', 'Output', -24, 12, 0, 'dB'), choice('ratio', 'Ratio', 0, ['4', '8', '12', '20', 'All']),
    p('attack', 'Attack', 20, 800, 300, 'µs', true), p('release', 'Release', 50, 1100, 300, 'ms', true), p('mix', 'Mix', 0, 100, 100, '%')],
  deess: [p('thr', 'Threshold', -60, 0, -32, 'dB'), p('range', 'Range', 0, 24, 8, 'dB'), p('freq', 'Freq', 2000, 16000, 6000, 'Hz', true),
    choice('mode', 'Mode', 0, ['Split', 'Wide']), p('look', 'Lookahead', 0, 15, 5, 'ms'), choice('listen', 'Listen', 0, ['Off', 'S'])],
  denoise: [p('reduction', 'Reduction', 0, 40, 12, 'dB'), p('sens', 'Sensitivity', 0, 100, 50, '%'), p('smooth', 'Smoothing', 0, 100, 50, '%'),
    choice('learn', 'Profile', 0, ['Adaptive', 'Hold']), choice('listen', 'Listen', 0, ['Off', 'Noise'])],
  tune: [choice('key', 'Key', 0, ['C', 'C#', 'D', 'D#', 'E', 'F', 'F#', 'G', 'G#', 'A', 'A#', 'B']),
    choice('scale', 'Scale', 0, ['Chromatic', 'Major', 'Minor', 'Maj pent', 'Min pent']),
    p('retune', 'Retune', 0, 400, 20, 'ms'), p('humanize', 'Humanize', 0, 100, 0, '%'), p('flex', 'Flex', 0, 100, 0, '%'),
    p('vibrato', 'Vibrato', 0, 100, 0, '%'), p('transpose', 'Transpose', -12, 12, 0, 'st'), p('formant', 'Formant', -12, 12, 0, 'st'),
    choice('voice', 'Voice', 0, ['Auto', 'Low male', 'Male', 'Female', 'Soprano'])],
  voice: [choice('mode', 'Mode', 0, ['Transpose', 'Quantize', 'Robot']), p('pitch', 'Pitch', -12, 12, 0, 'st'), p('formant', 'Formant', -12, 12, 0, 'st'),
    choice('link', 'Link', 0, ['Off', 'On']), p('drive', 'Drive', 0, 30, 0, 'dB'), p('out', 'Output', -24, 12, 0, 'dB')],
};
const NAMES: Record<BuiltinKind, string> = { eq: 'Parametric EQ', comp: 'Compressor', sat: 'Saturator', delay: 'Delay', reverb: 'Reverb', limit: 'Limiter',
  fet: 'FET Compressor', deess: 'De-esser', tune: 'Pitch Correct', denoise: 'Denoiser', voice: 'Voice' };

type BandEdit = [number, boolean | null, number | null, number | null, number | null];
function eqp(name: string, out: number, edits: BandEdit[]): Preset {
  const bands: EqBand[] = defaultBands();
  for (const [i, on, f, g, q] of edits) {
    const b = bands[i];
    if (on != null) b.on = on; if (f != null) b.f = f; if (g != null) b.g = g; if (q != null) b.q = q;
  }
  return { name, params: { out }, bands };
}
const pr = (name: string, params: Record<string, number>): Preset => ({ name, params });
const dly = (name: string, beats: number | null, time: number, fb: number, mix: number): Preset =>
  ({ name, params: { time, fb, mix }, ...(beats != null ? { beats } : {}) });
const T = true, n = null;

const PRESETS: Record<BuiltinKind, Preset[]> = {
  eq: [
    eqp('Flat', 0, []),
    eqp('Wokal — obecność', 0, [[0, T, 100, n, n], [2, n, 300, -2.5, 1.2], [4, n, 3500, 3, 0.9], [5, n, 10000, 2.5, n]]),
    eqp('Kick — punch', 0, [[0, T, 30, n, n], [1, n, 60, 3, n], [2, n, 350, -4, 1.4], [4, n, 4000, 3, 1.2]]),
    eqp('Bas — czysty dół', 0, [[0, T, 35, n, n], [1, n, 80, 2, n], [2, n, 250, -3, n], [6, T, 8000, n, n]]),
    eqp('Mud cut', 0, [[0, T, 60, n, n], [2, n, 300, -4, 1.5]]),
    eqp('Air / połysk', 0, [[4, n, 5000, 1, n], [5, n, 12000, 4, n]]),
    eqp('Telefon / lo-fi', 2, [[0, T, 400, n, n], [3, n, 1500, 4, 0.8], [6, T, 3200, n, n]]),
    eqp('Master — smile', 0, [[1, n, 80, 1.5, n], [3, n, 1000, -0.5, 0.6], [5, n, 10000, 1.5, n]]),
  ],
  comp: [
    pr('Domyślny', { thr: -18, ratio: 4, att: 10, rel: 150, gain: 0 }),
    pr('Wokal — wyrównanie', { thr: -20, ratio: 3, att: 5, rel: 120, gain: 4 }),
    pr('Drum bus — glue', { thr: -14, ratio: 2, att: 30, rel: 200, gain: 2 }),
    pr('Kick — punch', { thr: -16, ratio: 4, att: 25, rel: 80, gain: 3 }),
    pr('Bas — kontrola', { thr: -22, ratio: 5, att: 3, rel: 90, gain: 4 }),
    pr('Delikatny', { thr: -12, ratio: 1.5, att: 20, rel: 250, gain: 1 }),
    pr('Smash', { thr: -35, ratio: 12, att: 0.5, rel: 60, gain: 12 }),
  ],
  sat: [
    pr('Domyślny', { drive: 6, mix: 100, out: 0 }), pr('Subtelny', { drive: 2, mix: 50, out: 0 }),
    pr('Ciepła taśma', { drive: 4, mix: 100, out: -1 }), pr('Lampa', { drive: 10, mix: 80, out: -3 }),
    pr('Gruby bas', { drive: 18, mix: 60, out: -4 }), pr('Rozwalony', { drive: 30, mix: 100, out: -8 }),
  ],
  delay: [
    dly('Ósemka 1/8', 0.5, 0, 35, 20), dly('Ćwierćnuta 1/4', 1, 0, 30, 22), dly('Kropkowana 1/8', 0.75, 0, 40, 22),
    dly('Triola 1/8T', 1 / 3, 0, 35, 20), dly('Slapback', null, 90, 0, 25), dly('Długi dub', 1.5, 0, 65, 30),
  ],
  reverb: [
    pr('Domyślny', { size: 2.2, pre: 20, lo: 120, hi: 8000, width: 100, mix: 25 }),
    pr('Mały pokój', { size: 0.6, pre: 5, lo: 150, hi: 9000, width: 70, mix: 18 }),
    pr('Bęben — krótki', { size: 0.9, pre: 0, lo: 200, hi: 8000, width: 60, mix: 15 }),
    pr('Wokal plate', { size: 1.8, pre: 30, lo: 250, hi: 7000, width: 100, mix: 22 }),
    pr('Duża sala', { size: 3.2, pre: 25, lo: 120, hi: 6500, width: 100, mix: 25 }),
    pr('Ambient', { size: 5, pre: 120, lo: 300, hi: 12000, width: 100, mix: 45 }),
    pr('Katedra', { size: 7, pre: 60, lo: 100, hi: 5000, width: 100, mix: 35 }),
  ],
  limit: [
    pr('Bezpieczny', { gain: 0, ceil: -0.3 }), pr('Streaming −1 dB', { gain: 3, ceil: -1 }),
    pr('Głośny master', { gain: 6, ceil: -0.3 }), pr('Bardzo głośny', { gain: 10, ceil: -0.1 }),
  ],
  fet: [
    pr('Wokal — klasyk', { input: 8, output: -2, ratio: 0, attack: 400, release: 250, mix: 100 }),
    pr('Wokal — z przodu', { input: 12, output: -4, ratio: 1, attack: 200, release: 150, mix: 100 }),
    pr('Bas — kontrola', { input: 10, output: -3, ratio: 2, attack: 600, release: 400, mix: 100 }),
    pr('Równoległa kompresja', { input: 18, output: -6, ratio: 4, attack: 100, release: 120, mix: 45 }),
    pr('All buttons — smash', { input: 20, output: -10, ratio: 4, attack: 60, release: 100, mix: 100 }),
  ],
  deess: [
    pr('Wokal — domyślny', { thr: -32, range: 8, freq: 6000, mode: 0, look: 5, listen: 0 }),
    pr('Ostre „s”', { thr: -36, range: 12, freq: 6500, mode: 0, look: 8, listen: 0 }),
    pr('Delikatnie', { thr: -26, range: 5, freq: 7000, mode: 0, look: 5, listen: 0 }),
    pr('Wokal żeński', { thr: -32, range: 8, freq: 8000, mode: 0, look: 6, listen: 0 }),
    pr('Wokal męski', { thr: -32, range: 8, freq: 5000, mode: 0, look: 6, listen: 0 }),
    pr('Szerokopasmowy', { thr: -30, range: 6, freq: 6000, mode: 1, look: 5, listen: 0 }),
  ],
  denoise: [
    pr('Wokal — delikatnie', { reduction: 8, sens: 35, smooth: 50 }),
    pr('Wokal — szum pokoju', { reduction: 14, sens: 50, smooth: 60 }),
    pr('Telefon / dyktafon', { reduction: 20, sens: 65, smooth: 70 }),
    pr('Mocno', { reduction: 30, sens: 80, smooth: 80 }),
  ],
  tune: [
    pr('Naturalny', { retune: 40, humanize: 30, flex: 40, vibrato: 60 }),
    pr('Delikatna korekta', { retune: 80, humanize: 50, flex: 70, vibrato: 80 }),
    pr('Pop — mocno', { retune: 10, humanize: 10, flex: 10, vibrato: 30 }),
    pr('Efekt robota', { retune: 0 }),
  ],
  voice: [
    pr('Domyślny', {}), pr('Głębszy, pełniejszy', { formant: -2 }),
    pr('Oktawa w dół', { pitch: -12, formant: -2 }), pr('Oktawa w górę', { pitch: 12, formant: 2 }),
    pr('Zmiana płci ↓', { pitch: -4, formant: -4 }), pr('Zmiana płci ↑', { pitch: 4, formant: 4 }),
    pr('Chipmunk', { pitch: 7, link: 1 }), pr('Demon', { pitch: -7, formant: -6, drive: 12, out: -4 }),
    pr('Hard tune + drive', { mode: 1, drive: 10, out: -3 }), pr('Robot', { mode: 2, drive: 6, out: -2 }),
  ],
};

/** catalog.rs `BUILTIN` (also the order of the "add effect" menu). */
export const BUILTIN: BuiltinKind[] = ['denoise', 'tune', 'voice', 'eq', 'deess', 'comp', 'fet', 'sat', 'delay', 'reverb', 'limit'];

export function mockCatalog(): Catalog {
  return {
    types: Object.fromEntries(BUILTIN.map(k => [k, { name: NAMES[k], params: PARAMS[k] }])),
    presets: Object.fromEntries(BUILTIN.map(k => [k, PRESETS[k]])),
  };
}

const TC = ['#4f8ef7', '#5aa9e6', '#69c3d6', '#47b8a8', '#5fbf8a', '#7c8cf8', '#e0a44a', '#9a8cf0', '#d9678f', '#8b93a1'];
const COLORS = [0, 6, 5, 3, 4, 8, 9, 9];

function base(names: string[], insNames: string[], cat: Catalog): Project {
  const p: Project = {
    version: 2, bpm: 128, tracks: [], patterns: [], sources: [], clips: [], loop: null, nextId: 1000, automations: [],
    inserts: [{ id: 0, name: 'Master', color: '#d5dae2', vol: VOL_UNITY, pan: 0, muted: false, solo: false, fx: [] }],
  };
  insNames.forEach((name, i) => p.inserts.push({ id: i + 1, name, color: TC[COLORS[i]], vol: VOL_UNITY, pan: 0, muted: false, solo: false, fx: [] }));
  names.forEach((name, i) => p.tracks.push({ id: i + 1, name, color: TC[COLORS[i]], muted: false, solo: false, arm: false, vol: VOL_UNITY, h: 56, insert: i + 1 }));
  p.inserts[0].fx.push(newFx(cat, 900, 'limit', p.bpm));
  // the Channel Rack: the four built-ins (ids 1000..1003), like demo.rs
  p.channels = INSTS.map(i => builtinChannel(p.nextId++, i));
  return p;
}

export function emptyProject(cat: Catalog): Project {
  return base([1, 2, 3, 4, 5, 6, 7, 8].map(i => 'Track ' + i), [1, 2, 3, 4, 5, 6, 7, 8].map(i => 'Insert ' + i), cat);
}

export function demoProject(cat: Catalog): Project {
  const p = base(['Drums', 'Hats', 'Bass', 'Pad', 'Perc', 'Vocal', 'Track 7', 'Track 8'],
    ['Drums', 'Hats', 'Bass', 'Pad', 'Perc', 'Vocal', 'Insert 7', 'Insert 8'], cat);
  p.tracks[5].arm = true;
  const ch = (i: Inst) => p.channels!.find(c => c.kind === 'inst' && c.inst === i)!.id;
  const note = (inst: Inst, step: number, len: number, key?: number): Note => key != null ? { ch: ch(inst), step, len, p: key } : { ch: ch(inst), step, len };
  p.patterns = [
    { id: 1, name: 'Beat', color: '#4f8ef7', len: 4,
      notes: ([[0, 'kick'], [4, 'kick'], [8, 'kick'], [12, 'kick'], [10, 'kick'], [4, 'clap'], [12, 'clap']] as [number, Inst][]).map(([s, i]) => note(i, s, 1)) },
    { id: 2, name: 'Hats', color: '#e0a44a', len: 4, notes: [2, 6, 10, 14, 15, 7].map(s => note('hat', s, 1)) },
    { id: 3, name: 'Bass', color: '#7c8cf8', len: 8,
      notes: [[0, 3, 33], [4, 2, 33], [6, 2, 40], [8, 4, 36], [14, 2, 38], [16, 3, 29], [20, 2, 29], [22, 2, 36], [24, 6, 31], [30, 2, 34]].map(([s, l, k]) => note('bass', s, l, k)) },
  ];
  const clip = (id: number, track: number, start: number, len: number, ref: number): Clip =>
    ({ id, track, start, len, offset: 0, kind: 'pat', ref, muted: false, gain: 0, fadeIn: 0, fadeOut: 0, pan: 0, norm: false, rev: false });
  p.clips = [clip(1, 0, 0, 16, 1), clip(2, 0, 16, 16, 1), clip(3, 1, 8, 24, 2), clip(4, 2, 0, 32, 3)];
  const eq = loadPreset(cat, newFx(cat, 902, 'eq', p.bpm), 1, p.bpm);
  p.inserts[6].fx = [eq, newFx(cat, 903, 'comp', p.bpm), newFx(cat, 904, 'reverb', p.bpm)];
  p.inserts[1].fx = [loadPreset(cat, newFx(cat, 905, 'comp', p.bpm), 2, p.bpm)];
  return p;
}
