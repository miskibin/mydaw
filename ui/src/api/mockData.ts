// Data for the in-browser mock backend: the catalog of crates/engine/src/catalog.rs and the
// demo song of crates/engine/src/demo.rs. Only the mock uses these; the real UI reads `fx_catalog`.
import { VOL_UNITY, type AnyBuiltinKind, type Catalog, type Clip, type EqBand, type Inst, type Note, type ParamDef, type Preset, type Project } from './types';
import { defaultBands, loadPreset, newFx } from '../lib/presets';
import { builtinChannel, INSTS } from '../lib/pattern';

const p = (id: string, label: string, min: number, max: number, def: number, unit: string, log = false): ParamDef => ({ id, label, min, max, def, unit, log });
/** catalog.rs `choice`: a discrete parameter (value = index) shown as buttons. */
const choice = (id: string, label: string, def: number, options: string[]): ParamDef => ({ id, label, min: 0, max: options.length - 1, def, unit: '', log: false, options });

const PARAMS: Record<AnyBuiltinKind, ParamDef[]> = {
  eq: [p('out', 'Output', -18, 18, 0, 'dB')],
  comp: [p('thr', 'Thresh', -60, 0, -18, 'dB'), p('ratio', 'Ratio', 1, 20, 4, ':1', true), p('att', 'Attack', 0.1, 100, 10, 'ms', true),
    p('rel', 'Release', 10, 1000, 150, 'ms', true), p('gain', 'Makeup', 0, 24, 0, 'dB'), choice('det', 'Detect', 0, ['Peak', 'RMS'])],
  // catalog.rs SAT (dsp::saturator::idx order)
  sat: [choice('style', 'Style', 0, ['Taśma', 'Konsola', 'Transformator', 'Trioda', 'Pentoda']), p('drive', 'Drive', 0, 100, 40, '%'),
    choice('punish', 'Punish', 0, ['Off', 'On']), p('lowcut', 'Low cut', 10, 1000, 10, 'Hz', true), p('thump', 'Thump', 0, 100, 0, '%'),
    p('tone', 'Tone', -100, 100, 0, '%'), p('highcut', 'High cut', 500, 20000, 20000, 'Hz', true), choice('steep', 'Steep', 0, ['Off', 'On']),
    choice('auto', 'Auto gain', 1, ['Off', 'On']), p('mix', 'Mix', 0, 100, 100, '%'), p('out', 'Output', -24, 12, 0, 'dB'),
    choice('hq', 'Oversampling', 0, ['2×', '4×'])],
  delay: [p('time', 'Time', 10, 1500, 375, 'ms', true), p('fb', 'Feedback', 0, 95, 35, '%'), p('mix', 'Mix', 0, 100, 25, '%')],
  // catalog.rs REVERB / LIMIT (v2)
  reverb: [p('size', 'Decay', 0.2, 6, 1, 's', true), p('pre', 'Pre-delay', 0, 150, 25, 'ms'), p('lo', 'Low cut', 20, 1000, 300, 'Hz', true),
    p('hi', 'High cut', 2000, 16000, 7000, 'Hz', true), p('width', 'Width', 0, 100, 100, '%'), p('mix', 'Mix', 0, 100, 15, '%'),
    p('tone', 'Tone', -100, 100, 0, '%'), choice('space', 'Space', 0, ['Plate', 'Room', 'Hall', 'Ambience']), p('duck', 'Ducking', 0, 12, 0, 'dB'),
    p('early', 'Early', 0, 100, 50, '%'), p('bass', 'Bass decay', 0.5, 1.5, 1, '×'), p('mod', 'Modulation', 0, 100, 50, '%')],
  limit: [p('gain', 'Gain', 0, 24, 0, 'dB'), p('ceil', 'Ceiling', -12, 0, -1, 'dBTP'), p('rel', 'Release', 10, 1000, 80, 'ms', true),
    choice('auto', 'Auto release', 1, ['Off', 'On']), p('look', 'Lookahead', 1, 5, 3, 'ms'), p('link', 'Stereo link', 0, 100, 100, '%'), p('out', 'Output', -12, 0, 0, 'dB')],
  fet: [p('input', 'Input', 0, 36, 8, 'dB'), p('output', 'Output', -24, 12, 0, 'dB'), choice('ratio', 'Ratio', 0, ['4', '8', '12', '20', 'All']),
    p('attack', 'Attack', 20, 800, 300, 'µs', true), p('release', 'Release', 50, 1100, 300, 'ms', true), p('mix', 'Mix', 0, 100, 100, '%')],
  deess: [p('thr', 'Threshold', -60, 0, -32, 'dB'), p('range', 'Range', 0, 24, 8, 'dB'), p('freq', 'Freq', 2000, 16000, 6000, 'Hz', true),
    choice('mode', 'Mode', 0, ['Split', 'Wide']), p('look', 'Lookahead', 0, 15, 5, 'ms'), choice('listen', 'Listen', 0, ['Off', 'S']),
    choice('detect', 'Detect', 0, ['Absolute', 'Relative'])],
  denoise: [p('reduction', 'Reduction', 0, 40, 12, 'dB'), p('sens', 'Sensitivity', 0, 100, 50, '%'), p('smooth', 'Smoothing', 0, 100, 50, '%'),
    choice('learn', 'Profile', 0, ['Adaptive', 'Hold']), choice('listen', 'Listen', 0, ['Off', 'Noise'])],
  tune: [choice('key', 'Key', 0, ['C', 'C#', 'D', 'D#', 'E', 'F', 'F#', 'G', 'G#', 'A', 'A#', 'B']),
    choice('scale', 'Scale', 0, ['Chromatic', 'Major', 'Minor', 'Maj pent', 'Min pent']),
    p('retune', 'Retune', 0, 400, 20, 'ms'), p('humanize', 'Humanize', 0, 100, 0, '%'), p('flex', 'Flex', 0, 100, 0, '%'),
    p('vibrato', 'Vibrato', 0, 100, 0, '%'), p('transpose', 'Transpose', -12, 12, 0, 'st'), p('formant', 'Formant', -12, 12, 0, 'st'),
    choice('voice', 'Voice', 0, ['Auto', 'Low male', 'Male', 'Female', 'Soprano'])],
  voice: [choice('mode', 'Mode', 0, ['Transpose', 'Quantize', 'Robot']), p('pitch', 'Pitch', -12, 12, 0, 'st'), p('formant', 'Formant', -12, 12, 0, 'st'),
    choice('link', 'Link', 0, ['Off', 'On']), p('drive', 'Drive', 0, 30, 0, 'dB'), p('out', 'Output', -24, 12, 0, 'dB')],
  // catalog.rs RIDER (order = rider::idx)
  rider: [p('amount', 'Amount', 0, 100, 50, '%'), p('phrase', 'Phrase', 0, 100, 20, '%'), p('up', 'Max boost', 0, 12, 5, 'dB'),
    p('down', 'Max cut', 0, 12, 6, 'dB'), choice('speed', 'Speed', 0, ['Sung', 'Rap']), choice('auto', 'Auto target', 1, ['Off', 'On']),
    p('target', 'Target', -60, 0, -24, 'dB'), p('floor', 'Noise floor', -100, -20, -70, 'dB'), choice('look', 'Look-ahead', 1, ['Short', 'Long']),
    p('out', 'Output', -24, 24, 0, 'dB'), p('gate', 'Gate', -100, -10, -60, 'dB')],
};
const NAMES: Record<AnyBuiltinKind, string> = { eq: 'Parametric EQ', comp: 'Compressor', sat: 'Saturator', delay: 'Delay', reverb: 'Reverb', limit: 'Limiter',
  fet: 'FET Compressor', deess: 'De-esser', tune: 'Pitch Correct', denoise: 'Denoiser', voice: 'Voice', rider: 'Vocal Rider' };

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

const PRESETS: Record<AnyBuiltinKind, Preset[]> = {
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
    pr('Domyślny', { style: 0, drive: 40 }),
    pr('Wokal — ciepło', { style: 0, drive: 35, lowcut: 80, tone: 10, highcut: 16000 }),
    pr('Wokal — lampa', { style: 3, drive: 45, lowcut: 70, highcut: 15000 }),
    pr('Rap — ziarno', { style: 1, drive: 58, lowcut: 100, tone: 25, highcut: 14000 }),
    pr('Bębny — smash', { style: 1, drive: 80, lowcut: 30, thump: 30, mix: 70 }),
    pr('Bas — waga', { style: 2, drive: 50, lowcut: 35, thump: 40, tone: -20 }),
    pr('Lo-fi głośnik', { style: 4, drive: 60, lowcut: 300, thump: 35, tone: 20, highcut: 4500, steep: 1 }),
    pr('Równoległe zgniecenie', { style: 4, drive: 90, punish: 1, mix: 30 }),
    pr('Taśma — klej na bus', { style: 0, drive: 22, highcut: 18000 }),
  ],
  delay: [
    dly('Ósemka 1/8', 0.5, 0, 35, 20), dly('Ćwierćnuta 1/4', 1, 0, 30, 22), dly('Kropkowana 1/8', 0.75, 0, 40, 22),
    dly('Triola 1/8T', 1 / 3, 0, 35, 20), dly('Slapback', null, 90, 0, 25), dly('Długi dub', 1.5, 0, 65, 30),
  ],
  reverb: ([
    ['Vocal Plate', 1.0, 25, 300, 7000, 100, 15, 0, 0, 3, 0.8], ['Vocal Room', 0.6, 10, 250, 8000, 90, 12, -10, 1, 0, 0.9],
    ['Vocal Hall', 2.0, 35, 250, 6500, 100, 14, -20, 2, 4, 0.8], ['Ambience', 0.4, 0, 200, 9000, 100, 12, 10, 3, 0, 1.0],
    ['Ballada — długi plate', 2.4, 40, 350, 6000, 100, 18, -30, 0, 6, 0.8], ['Bębny — pokój', 0.7, 0, 150, 9000, 80, 12, 0, 1, 0, 1.0],
    ['Wysyłka — hala 100 %', 2.2, 20, 200, 7000, 100, 100, -10, 2, 0, 0.9],
  ] as [string, ...number[]][]).map(([name, size, pre, lo, hi, width, mix, tone, space, duck, bass]) =>
    pr(name, { size, pre, lo, hi, width, mix, tone, space, duck, early: 50, bass, mod: 50 })),
  limit: [
    pr('Bezpieczny', { gain: 0, ceil: -1 }), pr('Streaming −1 dBTP', { gain: 4, ceil: -1, rel: 80 }),
    pr('Głośny master', { gain: 7, ceil: -1, rel: 120 }), pr('Bardzo głośny', { gain: 10, ceil: -0.5, rel: 150, look: 5 }),
    pr('Przezroczysty', { gain: 3, ceil: -1, rel: 250, look: 5 }),
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
  rider: [
    pr('Wokal — śpiew', { amount: 50, phrase: 20, up: 5, down: 6, speed: 0, auto: 1, look: 1 }),
    pr('Rap', { amount: 50, phrase: 20, up: 5, down: 6, speed: 1, auto: 1, look: 1 }),
    pr('Delikatnie', { amount: 30, phrase: 10, up: 3, down: 4, speed: 0, auto: 1, look: 1 }),
    pr('Mocno', { amount: 80, phrase: 30, up: 8, down: 9, speed: 0, auto: 1, look: 1 }),
    pr('Na żywo (krótki look-ahead)', { amount: 50, phrase: 20, up: 5, down: 6, speed: 0, auto: 1, look: 0 }),
  ],
};

/** catalog.rs `BUILTIN` (also the order of the "add effect" menu). */
export const BUILTIN: AnyBuiltinKind[] = ['denoise', 'tune', 'voice', 'eq', 'rider', 'deess', 'comp', 'fet', 'sat', 'delay', 'reverb', 'limit'];

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
