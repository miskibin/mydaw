// Note-based pitch editor of an audio clip. The engine analyses the source (`pitch_analyze`)
// and renders `clip.tune` offline; here the user sees notes as blobs (thickness = level) at their
// edited pitch with the original (faint) and edited (bright) pitch curve through them, and edits
// them with tools: Main (pitch, snapped to the scale; Alt = free), Drift, Formant, Gain, Split,
// Pencil. Every drag is one gesture (= one undo step); the engine re-renders when it ends.
// "Do referencji" (Tune to Reference) takes the base pitch from another performance of the same
// melody (`pitch_target`, previewed live while the parameters change, stored on Apply); the
// reference pitch and its notes are drawn too, and stretches the alignment is unsure of are
// marked — they are left uncorrected. Manual edits then apply on top of the target.
import { lang, t } from '../lib/i18n';
import type { TranslationKey } from '../lib/i18n';
import { isTypingKey } from '../state/typingKeys';
import { setNoteClip } from '../state/noteClipboard';
import { sungToPattern } from '../lib/vocalNotes';
import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show } from 'solid-js';
import { be } from '../api/backend';
import type { Clip, ClipTune, F0ModelStatus, Melody, MelodyChannel, Op, PitchAnalysis, PitchTarget, RefTune } from '../api/types';
import { clamp, dbg, dbText, fmtPos } from '../lib/format';
import {
  addPencil, CORRECT_PRESETS, correctPitch, DEFAULT_REF, dragShift, editedCurve, editNotes, erasePencil, inScale, initTune, isBlack, lowConfidenceRuns,
  mergeNotes, MIN_NOTE, noteCenters, NOTE_NAMES, noteName, scaleMask, splitNear, splitNote,
} from '../lib/pitch';
import * as A from '../state/actions';
import { commit, Gesture, inGesture } from '../state/edit';
import { clipPeaksPending, getClipPeaks, peakRange, peaksVersion } from '../state/peaks';
import { plain, S } from '../state/store';
import { tele } from '../state/telemetry';
import { say, setU, U } from '../state/ui';
import { dblCheck, track } from '../ui/drag';
import { addFrameTask, fitCanvas, fonts, Layer } from '../ui/frameLoop';
import { FXMETA, Icon } from '../ui/icons';
import { NumField, parseNum, Segmented } from '../ui/Knob';
import { rgba, T, theme, themeVersion } from '../ui/theme';
import { moveWindow } from './FxWindow';
import { PitchCorrectButton } from './PitchCorrectButton';

type Tool = 'main' | 'drift' | 'formant' | 'gain' | 'split' | 'pencil';
const TOOLS: { id: Tool; key: string; label: TranslationKey; icon: string; tip: TranslationKey }[] = [
  { id: 'main', key: '1', label: 'd.pitch.tool.main', icon: 'M12 3v18 M7 8l5-5 5 5 M7 16l5 5 5-5', tip: 'd.pitch.tip.main' },
  { id: 'drift', key: '2', label: 'd.pitch.tool.drift', icon: 'M3 12c2-4 4-4 6 0s4 4 6 0 4-4 6 0', tip: 'd.pitch.tip.drift' },
  { id: 'formant', key: '3', label: 'd.pitch.tool.formant', icon: 'M4 18c3-12 5-12 8 0s5-8 8-8', tip: 'd.pitch.tip.formant' },
  { id: 'gain', key: '4', label: 'd.pitch.tool.gain', icon: 'M4 9h4l5-4v14l-5-4H4z M17 9a4 4 0 0 1 0 6', tip: 'd.pitch.tip.gain' },
  { id: 'split', key: '5', label: 'd.pitch.tool.split', icon: 'M12 3v18 M5 8h4 M15 16h4', tip: 'd.pitch.tip.split' },
  { id: 'pencil', key: '6', label: 'd.pitch.tool.pencil', icon: 'M4 20l4.5-1L19 8.5 15.5 5 5 15.5z', tip: 'd.pitch.tip.pencil' },
];
const SCALES = ['Chromatic', 'Major', 'Minor', 'Maj pent', 'Min pent'];
const KW = 46, RH = 22, BH = 48;

const anCache = new Map<string, PitchAnalysis>();

export function PitchEditor() {
  const clip = () => U.pitchEd ? S.project.clips.find(c => c.id === U.pitchEd!.clipId && c.kind === 'audio') : undefined;
  return <Show when={clip()}>{c => <PitchBody clipId={c().id} />}</Show>;
}

function PitchBody(props: { clipId: number }) {
  let cv!: HTMLCanvasElement, area!: HTMLDivElement, phead!: HTMLDivElement, status!: HTMLSpanElement, root!: HTMLDivElement;
  const E = () => U.pitchEd!;
  const c = () => S.project.clips.find(x => x.id === props.clipId)!;
  const src = () => S.project.sources.find(s => s.id === c()?.ref);
  const spb = () => 60 / S.project.bpm;
  // the editor works in source seconds; a stretched clip plays `st` clip seconds per source second
  const st = () => c().stretch ?? 1;
  const t0 = () => c().offset * spb() / st(), t1 = () => (c().offset + c().len) * spb() / st();
  /** Source seconds per clip beat. */
  const spbSrc = () => spb() / st();
  const anKey = () => c().ref + '|' + (c().norm ? 1 : 0) + (c().rev ? 1 : 0);
  const [an, setAn] = createSignal<PitchAnalysis | null>(anCache.get(anKey()) ?? null);
  const [err, setErr] = createSignal<string | null>(null);
  const load = () => {
    const k = anKey();
    const hit = anCache.get(k); if (hit) { setAn(hit); return; }
    setAn(null); setErr(null);
    be().pitchAnalyze(props.clipId).then(a => { anCache.set(k, a); if (anKey() === k) { setAn(a); fitView(a); } }).catch(e => setErr(String(e?.message ?? e)));
  };
  createEffect(() => { anKey(); load(); });

  // ---- editor state
  const tuneFx = () => { const tr = S.project.tracks[c().track]; return S.project.inserts.find(m => m.id === tr?.insert)?.fx.find(f => f.type === 'tune'); };
  const [tool, setTool] = createSignal<Tool>('main');
  const [sel, setSel] = createSignal<Set<number>>(new Set());
  const [key, setKey] = createSignal(Math.round(tuneFx()?.params.key ?? 0));
  const [scale, setScale] = createSignal(Math.round(tuneFx()?.params.scale ?? 0));
  // correction macro: Strength (how far to the scale note), Speed (ms to get there), Flatten (wobble removed)
  const POP = CORRECT_PRESETS[1];
  const [centerPct, setCenterPct] = createSignal(POP.strength);
  const [speedMs, setSpeedMs] = createSignal(POP.speed);
  const [driftPct, setDriftPct] = createSignal(POP.flatten);
  const preset = () => CORRECT_PRESETS.findIndex(x => x.strength === centerPct() && x.speed === speedMs() && x.flatten === driftPct());
  const pickPreset = (i: number) => { const x = CORRECT_PRESETS[i]; setCenterPct(x.strength); setSpeedMs(x.speed); setDriftPct(x.flatten); };
  const [hover, setHover] = createSignal<{ note: number; split: number } | null>(null);
  const [marq, setMarq] = createSignal<{ x0: number; y0: number; x1: number; y1: number } | null>(null);
  const [pencilLive, setPencilLive] = createSignal<[number, number][] | null>(null);
  const view = { t: 0, pps: 100, lo: 48, hi: 72 };
  const [viewV, setViewV] = createSignal(0);
  const bumpView = () => setViewV(v => v + 1);
  let size = { W: 0, H: 0 };

  const fitView = (a: PitchAnalysis | null = an()) => {
    const W = Math.max(200, size.W || E().w - 2);
    view.t = t0(); view.pps = (W - KW) / Math.max(0.2, t1() - t0());
    // fit the sung range by time: 2nd–98th percentile of note time, so a stray blob can't zoom out to octaves
    const vis = a ? a.notes.filter(n => n.end > t0() && n.start < t1()).sort((x, y) => x.pitch - y.pitch) : [];
    let lo = 55, hi = 67;
    if (vis.length) {
      const tot = vis.reduce((s, n) => s + (n.end - n.start), 0);
      const at = (q: number) => { let acc = 0; for (const n of vis) { acc += n.end - n.start; if (acc >= q * tot) return n.pitch; } return vis[vis.length - 1].pitch; };
      lo = at(0.02); hi = at(0.98);
    }
    const mid = (lo + hi) / 2, span = Math.max(14, hi - lo + 7);
    view.lo = mid - span / 2; view.hi = mid + span / 2;
    bumpView();
  };

  // ---- Tune to Reference
  const applied = () => c().tune?.reference ?? null;
  const [mode, setMode] = createSignal(applied() ? 1 : 0); // 0 manual, 1 reference
  // recordings worth comparing with: audio the playlist or a comp plays, long enough to hold a phrase (no drum hits
  // or sampler one-shots), other than this take
  const refSources = () => {
    const used = new Set<number>([...S.project.clips.filter(x => x.kind === 'audio').map(x => x.ref), ...(S.project.comps ?? []).flatMap(cp => cp.takes.map(tk => tk.ref))]);
    return S.project.sources.filter(s => s.id !== c().ref && used.has(s.id) && s.dur >= 1);
  };
  // melodies: channels whose notes play over this clip, best fit first
  const [melAll, setMelodies] = createSignal<MelodyChannel[]>([]);
  // drum channels match no singing: hidden while something fits at least a little
  const melodies = () => { const all = melAll(), fit = all.filter(m => m.fit >= 0.1); return fit.length ? fit : all; };
  const [melStale, setMelStale] = createSignal<Melody | null>(null);
  const melKey = () => JSON.stringify([S.project.patterns.map(pt => [pt.id, pt.len, pt.swing, pt.notes]), S.project.clips.filter(x => x.kind === 'pat').map(x => [x.ref, x.start, x.len, x.offset, x.muted]),
    c().start, c().len, c().offset, c().stretch, S.project.bpm, (S.project.channels ?? []).map(ch => [ch.id, ch.root, ch.name]),
    // the stored melody too: Apply, Update and undo change it without touching the piano roll
    applied()?.melody ?? null]);
  let melTimer = 0;
  createEffect(on(melKey, () => {
    clearTimeout(melTimer);
    melTimer = window.setTimeout(() => {
      be().melodyChannels(props.clipId).then(setMelodies).catch(() => setMelodies([]));
      // the stored melody against what the piano roll plays now
      const m = applied()?.melody;
      if (!m) { setMelStale(null); return; }
      be().melodyReference(props.clipId, m.channel, m.span).then(r => setMelStale(JSON.stringify(r.melody.notes) === JSON.stringify(m.notes) ? null : r.melody)).catch(() => setMelStale(null));
    }, 250);
  }));
  onCleanup(() => clearTimeout(melTimer));
  const useMelody = (channel: number, span?: [number, number] | null) =>
    be().melodyReference(props.clipId, channel, span)
      .then(r => { setDraft(d => ({ ...DEFAULT_REF, ...(d ?? {}), source: 0, influence: 1, melody: r.melody })); setMode(1); })
      .catch(e => say(String(e?.message ?? e)));
  const [draft, setDraft] = createSignal<RefTune | null>(applied() ? { ...applied()! } : null);
  const [target, setTarget] = createSignal<PitchTarget | null>(null);
  const [tgtBusy, setTgtBusy] = createSignal(false);
  const [tgtErr, setTgtErr] = createSignal<string | null>(null);
  const [model, setModel] = createSignal<F0ModelStatus | null>(null);
  const [dl, setDl] = createSignal<number | null>(null);
  const [askModel, setAskModel] = createSignal(false);
  const withKey = (d: RefTune): RefTune => ({ ...d, key: key(), scale: scale() });
  const activeTarget = () => (mode() === 1 ? target() : null);
  const sameRef = (a: RefTune, b: RefTune | null) => !!b && a.source === b.source && a.correction === b.correction && a.vibrato === b.vibrato
    && a.influence === b.influence && (a.octave ?? null) === (b.octave ?? null) && (a.key ?? 0) === (b.key ?? 0) && (a.scale ?? 0) === (b.scale ?? 0)
    && (a.speed ?? null) === (b.speed ?? null) && JSON.stringify(a.melody ?? null) === JSON.stringify(b.melody ?? null);
  const pending = () => { const d = draft(); return mode() === 1 && !!d && !sameRef(withKey(d), applied()); };
  const patchDraft = (p: Partial<RefTune>) => setDraft(d => d && { ...d, ...p });
  const startDraft = () => {
    if (draft()) return;
    // a melody that fits the singing first (that is what was sung along with), else a recording
    const m = melodies().find(x => x.fit >= 0.3);
    if (m) { void useMelody(m.channel); return; }
    const s = refSources()[0];
    if (s) setDraft({ source: s.id, ...DEFAULT_REF });
    else if (melodies()[0]) void useMelody(melodies()[0].channel);
  };
  // the Piano roll's "vocal reference" button opens the editor with a melody request
  createEffect(on(() => U.pitchEd?.melody, req => {
    if (!req) return;
    setU('pitchEd', ed => ed && { ...ed, melody: undefined });
    void useMelody(req.channel, req.span);
  }));
  // an undo/redo of Apply brings the stored parameters back into the panel
  createEffect(on(() => JSON.stringify(applied()), () => { const a = applied(); if (a) { setDraft({ ...DEFAULT_REF, ...a }); setMode(1); } }, { defer: true }));
  let tgtSeq = 0, tgtTimer = 0;
  createEffect(() => {
    const d = draft(), m = mode(), a = an();
    JSON.stringify(c().tune?.notes ?? null); key(); scale();
    clearTimeout(tgtTimer);
    if (m !== 1 || !d || !a) { tgtSeq++; setTarget(null); setTgtErr(null); setTgtBusy(false); return; }
    const seq = ++tgtSeq, params = withKey(d);
    tgtTimer = window.setTimeout(() => {
      setTgtBusy(true);
      be().pitchTarget(props.clipId, params)
        .then(t => { if (seq === tgtSeq) { setTarget(t); setTgtErr(null); } })
        .catch(e => { if (seq === tgtSeq) { setTarget(null); setTgtErr(String(e?.message ?? e)); } })
        .finally(() => { if (seq === tgtSeq) setTgtBusy(false); });
    }, 120);
  });
  onCleanup(() => clearTimeout(tgtTimer));
  const lowRuns = createMemo(() => { const t = activeTarget(); return t && !t.reason ? lowConfidenceRuns(t) : []; });

  const tune = (): ClipTune | null => { const t = c().tune; return t && (t.notes.length || t.pencil?.length || t.reference) ? t : null; };
  const tuneOrInit = (): ClipTune => {
    const t = tune(); if (!t) return initTune(an()!);
    const p = plain(t); if (!p.notes.length) p.notes = initTune(an()!).notes;
    return p;
  };
  const notes = () => tuneOrInit().notes;
  const centers = createMemo(() => { const a = an(); if (!a) return []; return noteCenters(a, notes(), activeTarget()); });
  const edited = createMemo(() => { const a = an(); return a ? editedCurve(a, tune(), tune()?.notes.length ? centers() : undefined, activeTarget()) : new Float32Array(0); });
  const showEdited = () => !!tune() || !!activeTarget();
  const mask = () => scaleMask(scale());
  const putTune = (t: ClipTune | null): Op => ({ op: 'putClips', clips: [{ ...plain(c()), tune: t } as Clip] });

  // ---- geometry
  const X = (t: number) => KW + (t - view.t) * view.pps;
  const tAt = (x: number) => view.t + (x - KW) / view.pps;
  const noteH = () => size.H - RH - BH;
  const rowH = () => noteH() / (view.hi - view.lo);
  const Y = (m: number) => RH + (view.hi - m) * rowH();
  const mAt = (y: number) => view.hi - (y - RH) / rowH();
  const pitchOf = (i: number) => { const n = notes()[i], cc = centers()[i]; return cc == null ? null : cc + n.shift / 100; };

  // ---- hit testing
  const halfThick = (lv: number) => Math.max(2.5, rowH() * (0.16 + 0.42 * clamp(lv, 0, 1)));
  const hitNote = (x: number, y: number): number => {
    const ns = notes(), t = tAt(x);
    for (let i = 0; i < ns.length; i++) {
      const n = ns[i]; if (t < n.start || t >= n.end) continue;
      const p = pitchOf(i); if (p == null) continue;
      const a = an()!, lv = a.levels[Math.min(a.levels.length - 1, Math.floor(t * a.rate))] * dbg(n.gain);
      if (Math.abs(y - Y(p)) <= halfThick(lv) + 4) return i;
    }
    return -1;
  };
  const hitSplit = (x: number, y: number): number => {
    const ns = notes(), i = splitNear(ns, tAt(x), 5 / view.pps);
    if (i < 0) return -1;
    const p1 = pitchOf(i), p2 = pitchOf(i + 1); if (p1 == null || p2 == null) return -1;
    const ya = Y(Math.max(p1, p2)) - rowH(), yb = Y(Math.min(p1, p2)) + rowH();
    return y >= ya && y <= yb ? i : -1;
  };

  // ---- drawing
  const layer = new Layer();
  let dirty = true;
  createEffect(() => {
    viewV(); sel(); hover(); tool(); key(); scale(); marq(); pencilLive(); an(); peaksVersion(); edited(); themeVersion(); lang(); activeTarget(); lowRuns();
    JSON.stringify(c().tune ?? null); c().offset; c().len; c().stretch; c().gain; S.project.bpm; src()?.color; S.project.loop?.a; S.project.loop?.b; U.start;
    dirty = true;
  });
  onMount(() => {
    root.focus();
    const stop = addFrameTask(() => {
      const W = area.clientWidth, H = area.clientHeight;
      if (!W || !H) return;
      if (W !== size.W || H !== size.H) { const first = !size.W; size = { W, H }; if (first) fitView(); dirty = true; }
      if (dirty) { dirty = false; draw(W, H); }
      // playhead (telemetry), as a DOM line — no canvas redraw per frame
      const f = tele.frame;
      let ts = -1;
      if (f.playing && f.beat >= c().start && f.beat < c().start + c().len) ts = t0() + (f.beat - c().start) * spbSrc();
      else if (f.previewing && f.previewPos >= 0) ts = f.previewPos;
      // follow playback a page at a time (like the Playlist), unless the user is dragging something
      if (ts >= 0 && f.playing && !inGesture() && (X(ts) > W - 8 || X(ts) < KW)) { view.t = ts - (W - KW) * 0.1 / view.pps; bumpView(); }
      if (ts >= 0 && X(ts) >= KW && X(ts) <= W) { phead.style.opacity = '1'; phead.style.transform = `translateX(${X(ts)}px)`; }
      else if (phead.style.opacity !== '0') phead.style.opacity = '0';
      const busy = !inGesture() && clipPeaksPending(c());
      const txt = !an() ? '' : busy ? t('d.pitch.rendering') : '';
      if (status.textContent !== txt) status.textContent = txt;
    });
    onCleanup(stop);
  });

  const draw = (W: number, H: number) => {
    fitCanvas(cv, W, H);
    const g = cv.getContext('2d')!, dpr = window.devicePixelRatio || 1;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    const a = an(), accent = FXMETA.tune.color, col = src()?.color ?? accent;
    const rh = rowH();
    // static-ish background (rows + keyboard) cached per view/scale
    const bgKey = [view.lo.toFixed(3), view.hi.toFixed(3), key(), scale(), W, H].join('|');
    g.drawImage(layer.get(W, H, bgKey, (b, w) => {
      b.fillStyle = T.viz; b.fillRect(0, 0, w, H);
      b.font = '500 9px "Geist Mono Variable", monospace'; b.textBaseline = 'middle';
      for (let m = Math.floor(view.lo); m <= Math.ceil(view.hi); m++) {
        const y = Y(m + 0.5), inS = inScale(m, key(), mask()), root = ((m - key()) % 12 + 12) % 12 === 0;
        b.fillStyle = root ? rgba(accent, 0.08 * T.tint) : inS ? T.vizPanel : T.viz; b.fillRect(KW, y, w - KW, rh);
        b.fillStyle = T.vizGrid; b.fillRect(KW, Math.round(y + rh) - 0.5, w - KW, 1);
        const blk = isBlack(m);
        b.fillStyle = blk ? T.keyB : T.keyW; b.fillRect(0, y + 0.5, KW - 2, Math.max(1, rh - 1));
        if (!inS) { b.fillStyle = rgba(T.viz, 0.5); b.fillRect(0, y + 0.5, KW - 2, rh - 1); }
        if (root) { b.fillStyle = accent; b.fillRect(KW - 5, y + 1, 3, rh - 2); }
        if (rh >= 10 && (root || m % 12 === 0 || rh >= 15)) { b.fillStyle = blk ? T.keyBText : T.keyWText; b.fillText(NOTE_NAMES[((m % 12) + 12) % 12] + (Math.floor(m / 12) - 1), 4, y + rh / 2 + 0.5); }
      }
      b.textBaseline = 'alphabetic';
    }) as CanvasImageSource, 0, 0, W, H);
    g.save(); g.beginPath(); g.rect(KW, 0, W - KW, H); g.clip();
    // beat grid + ruler
    const sp = spbSrc(), b0 = c().start + (tAt(KW) - t0()) / sp, b1 = c().start + (tAt(W) - t0()) / sp;
    const beatX = (bt: number) => X(t0() + (bt - c().start) * sp);
    const step = view.pps * sp < 14 ? 4 : 1;
    g.fillStyle = T.rulerBg; g.fillRect(KW, 0, W - KW, RH);
    g.font = '500 9px "Geist Mono Variable", monospace';
    for (let bt = Math.ceil(b0 / step) * step; bt <= b1; bt += step) {
      const x = Math.round(beatX(bt)) + 0.5, bar = bt % 4 === 0;
      g.strokeStyle = bar ? T.vizGrid2 : T.vizGrid; g.beginPath(); g.moveTo(x, RH); g.lineTo(x, H - BH); g.stroke();
      if (bar || view.pps * sp > 40) { g.fillStyle = bar ? T.fg3 : T.fg5; g.fillText(fmtPos(bt), x + 3, 14); }
    }
    g.fillStyle = T.vizAxis; g.fillRect(KW, RH - 1, W - KW, 1);
    // the song's loop range (ruler band + faint column) and start marker, as in the Playlist
    const lp = S.project.loop;
    if (lp) {
      const xa = X(tOfBeat(lp.a)), xb = X(tOfBeat(lp.b));
      if (xb > KW && xa < W) {
        g.fillStyle = rgba(T.green, 0.05); g.fillRect(xa, RH, xb - xa, H - RH - BH);
        g.fillStyle = rgba(T.green, 0.35); g.fillRect(xa, RH - 5, xb - xa, 4);
        g.fillStyle = T.green; g.fillRect(Math.round(xa), RH - 7, 1, 7); g.fillRect(Math.round(xb) - 1, RH - 7, 1, 7);
      }
    }
    const xs = X(tOfBeat(U.start));
    if (xs >= KW && xs <= W) { g.fillStyle = T.fg3; g.beginPath(); g.moveTo(xs - 4, RH - 8); g.lineTo(xs + 4, RH - 8); g.lineTo(xs, RH - 2); g.closePath(); g.fill(); }
    // outside the clip region
    g.fillStyle = T.shade;
    if (X(t0()) > KW) g.fillRect(KW, RH, X(t0()) - KW, H - RH);
    if (X(t1()) < W) g.fillRect(X(t1()), RH, W - X(t1()), H - RH);
    if (!a) { g.restore(); return; }
    const vt0 = Math.max(0, tAt(KW) - 0.05), vt1 = tAt(W) + 0.05;
    const f0 = Math.max(0, Math.floor(vt0 * a.rate)), f1 = Math.min(a.curve.length, Math.ceil(vt1 * a.rate));
    // unvoiced but audible (sibilants, breaths): hatched — left untouched by the editor
    const hatch = (xa: number, xb: number, ya: number, yb: number, alpha: number) => {
      g.save(); g.beginPath(); g.rect(xa, ya, xb - xa, yb - ya); g.clip();
      g.strokeStyle = rgba(T.fg4, alpha); g.lineWidth = 1; g.beginPath();
      for (let x = xa - (yb - ya); x < xb; x += 6) { g.moveTo(x, yb); g.lineTo(x + (yb - ya), ya); }
      g.stroke(); g.restore();
    };
    const runs: [number, number][] = [];
    let rs = -1;
    for (let f = f0; f < f1; f++) {
      const un = a.curve[f] == null && a.levels[f] > 0.03;
      if (un && rs < 0) rs = f; else if (!un && rs >= 0) { runs.push([rs, f]); rs = -1; }
    }
    if (rs >= 0) runs.push([rs, f1]);
    for (const [fa, fb] of runs) hatch(X(fa / a.rate), X(fb / a.rate), RH, H - BH, 0.12);
    // Tune to Reference: stretches left uncorrected (unsure alignment), the reference's notes as outlines
    const tg = activeTarget(), refOk = !!tg && !tg.reason;
    for (const [ta, tb] of lowRuns()) {
      if (tb < vt0 || ta > vt1) continue;
      g.fillStyle = rgba(T.amber, 0.08); g.fillRect(X(ta), RH, X(tb) - X(ta), H - RH - BH);
      g.fillStyle = rgba(T.amber, 0.7); g.fillRect(X(ta), RH, X(tb) - X(ta), 3);
    }
    if (tg && refOk) {
      g.setLineDash([3, 3]); g.strokeStyle = rgba(T.blue, 0.55); g.lineWidth = 1;
      for (const b of tg.notes) { if (b.end < vt0 || b.start > vt1) continue; g.strokeRect(X(b.start) + 0.5, Y(b.pitch) - rh * 0.35, X(b.end) - X(b.start), rh * 0.7); }
      g.setLineDash([]);
    }
    // note blobs
    const ns = notes(), cs = centers(), s = sel(), hv = hover();
    for (let i = 0; i < ns.length; i++) {
      const n = ns[i]; if (n.end < vt0 || n.start > vt1) continue;
      const cc = cs[i]; if (cc == null) continue;
      const pm = cc + n.shift / 100, yc = Y(pm), gn = dbg(n.gain);
      const fa = Math.floor(n.start * a.rate), fb = Math.min(a.levels.length, Math.ceil(n.end * a.rate));
      const stepF = Math.max(1, Math.floor((fb - fa) / Math.max(4, (X(n.end) - X(n.start)) / 3)));
      const top: number[] = [];
      for (let f = fa; f < fb; f += stepF) top.push(X(f / a.rate), halfThick(a.levels[f] * gn));
      top.push(X(n.end), halfThick(a.levels[Math.max(fa, fb - 1)] * gn));
      g.beginPath();
      g.moveTo(top[0], yc);
      for (let k = 0; k < top.length; k += 2) g.lineTo(top[k], yc - top[k + 1]);
      for (let k = top.length - 2; k >= 0; k -= 2) g.lineTo(top[k], yc + top[k + 1]);
      g.closePath();
      const isSel = s.has(i), isHov = hv?.note === i;
      g.fillStyle = rgba(col, isSel ? 0.62 : 0.34); g.fill();
      g.lineJoin = 'round'; g.strokeStyle = isSel ? T.ink : isHov ? rgba(col, 1) : rgba(col, 0.7); g.lineWidth = isSel ? 1.5 : 1; g.stroke();
      // label: note + cents off, and edits
      const w = X(n.end) - X(n.start);
      if (w > 34 && rh > 8) {
        const cents = Math.round((pm - Math.round(pm)) * 100);
        let lab = noteName(pm) + (Math.abs(cents) >= 3 ? (cents > 0 ? ' +' : ' −') + Math.abs(cents) : '');
        if (n.drift) lab += ' · D' + Math.round(n.drift * 100) + '%';
        if (n.formant) lab += ' · F' + (n.formant > 0 ? '+' : '') + Math.round(n.formant);
        if (n.gain) lab += ' · ' + dbText(n.gain);
        g.font = '500 9px "Geist Mono Variable", monospace'; g.fillStyle = isSel ? T.fgHi : T.fg3;
        g.fillText(lab, X(n.start) + 3, yc - Math.max(halfThick(n.gain ? 0.6 : 0.5), 6) - 4);
      }
    }
    // split markers (touching notes)
    for (let i = 0; i + 1 < ns.length; i++) {
      if (Math.abs(ns[i].end - ns[i + 1].start) > 1e-6 || ns[i].end < vt0 || ns[i].end > vt1) continue;
      const p1 = pitchOf(i), p2 = pitchOf(i + 1); if (p1 == null || p2 == null) continue;
      const x = Math.round(X(ns[i].end)) + 0.5;
      g.strokeStyle = hv?.split === i ? T.ink : rgba(T.fg3, 0.7); g.lineWidth = hv?.split === i ? 2 : 1;
      g.beginPath(); g.moveTo(x, Y(Math.max(p1, p2)) - rh * 0.6); g.lineTo(x, Y(Math.min(p1, p2)) + rh * 0.6); g.stroke();
    }
    // pitch curves: original faint, reference dashed, edited bright
    const curve = (val: (f: number) => number | null, stroke: string, lw: number) => {
      g.beginPath(); let pen = false;
      for (let f = f0; f < f1; f++) {
        const v = val(f);
        if (v == null || Number.isNaN(v)) { pen = false; continue; }
        const x = X(f / a.rate), y = Y(v);
        if (!pen) { g.moveTo(x, y); pen = true; } else g.lineTo(x, y);
      }
      g.strokeStyle = stroke; g.lineWidth = lw; g.stroke();
    };
    const ed = edited();
    curve(f => a.curve[f], rgba(T.pre, showEdited() ? 0.45 : 0.8), 1);
    if (tg && refOk) { g.setLineDash([5, 3]); curve(f => tg.referenceF0[f - tg.start] ?? null, rgba(T.blue, 0.85), 1.2); g.setLineDash([]); }
    if (showEdited()) curve(f => ed[f], accent, 1.6);
    const pl = pencilLive();
    if (pl && pl.length > 1) { g.beginPath(); pl.forEach(([t, m], k) => (k ? g.lineTo(X(t), Y(m)) : g.moveTo(X(t), Y(m)))); g.strokeStyle = T.ink; g.lineWidth = 2; g.stroke(); }
    // bottom strip: what the clip plays (rendered) + unvoiced stretches
    const by = H - BH;
    g.fillStyle = T.vizPanel; g.fillRect(KW, by, W - KW, BH);
    g.fillStyle = T.vizAxis; g.fillRect(KW, by, W - KW, 1);
    const pk = getClipPeaks(c());
    if (pk) {
      const mid = by + BH / 2 + 2, gn = dbg(c().gain);
      g.fillStyle = rgba(col, 0.75); g.beginPath();
      for (let x = KW; x < W; x++) {
        const i0 = Math.floor(tAt(x) * 100), i1 = Math.floor(tAt(x + 1) * 100);
        if (i0 < 0 || i0 * 2 >= pk.length) continue;
        const [mn, mx] = peakRange(pk, i0, i1);
        const ya = mid - clamp(mx * gn, 0, 1) * (BH / 2 - 4), yb = mid - clamp(mn * gn, -1, 0) * (BH / 2 - 4);
        g.rect(x, ya, 1, Math.max(0.6, yb - ya));
      }
      g.fill();
    }
    for (const [fa, fb] of runs) { g.fillStyle = rgba(T.amber, 0.12); g.fillRect(X(fa / a.rate), by + 1, X(fb / a.rate) - X(fa / a.rate), BH - 1); hatch(X(fa / a.rate), X(fb / a.rate), by + 1, H, 0.35); }
    g.font = '600 8px "Geist Variable", sans-serif'; g.fillStyle = T.fg5; g.fillText(t('d.pitch.unvoiced'), KW + 6, by + 11);
    // marquee
    const mq = marq();
    if (mq) { g.fillStyle = rgba(T.blue, 0.1); g.strokeStyle = T.blue; g.lineWidth = 1; const x = Math.min(mq.x0, mq.x1), y = Math.min(mq.y0, mq.y1); g.fillRect(x, y, Math.abs(mq.x1 - mq.x0), Math.abs(mq.y1 - mq.y0)); g.strokeRect(x + 0.5, y + 0.5, Math.abs(mq.x1 - mq.x0), Math.abs(mq.y1 - mq.y0)); }
    g.restore();
  };

  // ---- pointer
  const pos = (e: PointerEvent | WheelEvent | MouseEvent) => { const r = cv.getBoundingClientRect(); return { x: e.clientX - r.left, y: e.clientY - r.top }; };
  const targets = (i: number) => sel().has(i) ? [...sel()] : [i];
  const editDrag = (e: PointerEvent, i: number, label: string, patch: (n: ClipTune['notes'][number], i: number, dy: number, ev: PointerEvent) => Partial<ClipTune['notes'][number]>) => {
    const idx = targets(i); if (!sel().has(i)) setSel(new Set([i]));
    const base = tuneOrInit(), sy = e.clientY;
    let g: Gesture | null = null;
    track(ev => {
      const dy = (sy - ev.clientY) * (ev.shiftKey ? 0.25 : 1);
      if (!g) { if (Math.abs(dy) < 2) return; g = new Gesture(label); }
      const t = editNotes(base, idx, (n, k) => patch(base.notes[k], k, dy, ev));
      g.update([putTune(t)]);
      const n0 = t.notes[i];
      say(label + ' · ' + (tool() === 'main' ? noteName((centers()[i] ?? 0) + n0.shift / 100) + ' (' + (n0.shift >= 0 ? '+' : '') + Math.round(n0.shift) + ' ct)' : tool() === 'drift' ? Math.round(n0.drift * 100) + '%' : tool() === 'formant' ? (n0.formant >= 0 ? '+' : '') + Math.round(n0.formant) + ' ct' : dbText(n0.gain)));
    }, () => { g?.end(); }, 'ns-resize');
  };

  const down = (e: PointerEvent) => {
    root.focus();
    if (!an()) return;
    const { x, y } = pos(e);
    if (e.button === 1 || (e.button === 0 && e.altKey && tool() !== 'pencil' && tool() !== 'main')) return pan(e);
    if (e.button !== 0) return;
    e.preventDefault();
    if (y < RH) return rulerDown(e, x);
    if (x < KW) return;
    const i = hitNote(x, y), sp = hitSplit(x, y), tk = tool();
    if (tk === 'pencil') return pencil(e);
    if (tk === 'split' || (tk === 'main' && dblCheck('pnote' + props.clipId))) {
      if (sp >= 0) { const nt = mergeNotes(tuneOrInit(), sp); if (nt) { commit([putTune(nt)], t('d.pitch.op.merge')); setSel(new Set<number>()); say(t('d.pitch.say.merged')); } return; }
      if (i >= 0) { const nt = splitNote(tuneOrInit(), i, tAt(x)); if (nt) { commit([putTune(nt)], t('d.pitch.op.split')); setSel(new Set([i + 1])); say(t('d.pitch.say.split', { s: tAt(x).toFixed(3) })); } else say(t('d.pitch.say.tooclose', { ms: MIN_NOTE * 1000 })); }
      return;
    }
    if (i < 0) return marquee(e, x, y);
    if (e.ctrlKey || e.metaKey || e.shiftKey) { const s2 = new Set(sel()); s2.has(i) ? s2.delete(i) : s2.add(i); setSel(s2); return; }
    const k0 = key(), m0 = mask();
    if (tk === 'main') editDrag(e, i, t('d.pitch.op.pitch'), (n, k, dy, ev) => ({ shift: dragShift(centers()[k] ?? 0, n.shift, dy / rowH() * 100, k0, m0, ev.altKey) }));
    else if (tk === 'drift') editDrag(e, i, t('d.pitch.op.drift'), (n, _k, dy) => ({ drift: clamp(n.drift + dy / 120, 0, 1) }));
    else if (tk === 'formant') editDrag(e, i, t('d.pitch.op.formant'), (n, _k, dy) => ({ formant: Math.round(clamp(n.formant + dy * 3, -1200, 1200)) }));
    else if (tk === 'gain') editDrag(e, i, t('d.pitch.op.gain'), (n, _k, dy) => ({ gain: Math.round(clamp(n.gain + dy * 0.1, -24, 12) * 10) / 10 }));
  };
  const marquee = (e: PointerEvent, x: number, y: number) => {
    const add = e.ctrlKey || e.metaKey || e.shiftKey, base = add ? new Set(sel()) : new Set<number>();
    if (!add) setSel(new Set<number>());
    const r = cv.getBoundingClientRect();
    track(ev => {
      const x1 = ev.clientX - r.left, y1 = ev.clientY - r.top;
      setMarq({ x0: x, y0: y, x1, y1 });
      const ta = tAt(Math.min(x, x1)), tb = tAt(Math.max(x, x1)), ma = mAt(Math.max(y, y1)), mb = mAt(Math.min(y, y1));
      const s2 = new Set(base);
      notes().forEach((n, i) => { const p = pitchOf(i); if (p != null && n.end > ta && n.start < tb && p + 0.5 > ma && p - 0.5 < mb) s2.add(i); });
      setSel(s2);
    }, () => { setMarq(null); if (sel().size) say(t('d.pitch.say.selected', { n: sel().size })); }, 'crosshair');
  };
  const pan = (e: PointerEvent) => {
    e.preventDefault();
    let lx = e.clientX, ly = e.clientY;
    track(ev => { view.t -= (ev.clientX - lx) / view.pps; const dm = (ev.clientY - ly) / rowH(); view.lo += dm; view.hi += dm; lx = ev.clientX; ly = ev.clientY; bumpView(); }, null, 'grabbing');
  };
  // ---- ruler: like the Playlist's — click = start marker (playback jumps there), drag = loop range
  // (the project's loop, one undo step), double-click = no loop. Snaps to 1/16 beat; Alt = free.
  const beatOf = (ts: number) => c().start + (ts - t0()) / spbSrc();
  const tOfBeat = (b: number) => t0() + (b - c().start) * spbSrc();
  const snapBeat = (b: number, free: boolean) => Math.max(0, free ? b : Math.round(b * 4) / 4);
  const rulerDown = (e: PointerEvent, x: number) => {
    if (dblCheck('pruler' + props.clipId)) { if (S.project.loop) { A.setLoop(null, t('a.pl.deleteLoop')); say(t('a.pl.loopDeleted')); } return; }
    const r = cv.getBoundingClientRect(), b0 = snapBeat(beatOf(tAt(x)), e.altKey);
    let g: Gesture | null = null;
    track(ev => {
      const b1 = snapBeat(beatOf(tAt(ev.clientX - r.left)), ev.altKey);
      if (!g && Math.abs(ev.clientX - e.clientX) > 4) g = new Gesture(t('a.pl.loop'));
      const a = Math.min(b0, b1), z = Math.max(b0, b1), now = S.project.loop;
      if (g && z - a > 1e-6 && !(now && now.a === a && now.b === z)) g.update([{ op: 'setLoop', loop: { a, b: z } }]);
    }, () => {
      if (!g) { A.seekSong(b0); return; }
      g.end();
      if (S.project.loop) { A.seekSong(S.project.loop.a); say(t('d.pitch.say.loop', { a: fmtPos(S.project.loop.a), b: fmtPos(S.project.loop.b) })); }
    }, 'ew-resize');
  };
  const pencil = (e: PointerEvent) => {
    const erase = e.altKey, r = cv.getBoundingClientRect();
    const pts: [number, number][] = [];
    const addPt = (ev: PointerEvent) => {
      const t = clamp(tAt(ev.clientX - r.left), t0(), t1()), m = mAt(ev.clientY - r.top);
      const last = pts[pts.length - 1];
      if (last && Math.abs(t - last[0]) < 0.004) return;
      if (last) { // keep the run continuous (< 30 ms between points)
        const steps = Math.ceil(Math.abs(t - last[0]) / 0.02);
        for (let k = 1; k < steps; k++) pts.push([last[0] + (t - last[0]) * k / steps, last[1] + (m - last[1]) * k / steps]);
      }
      pts.push([t, m]);
      setPencilLive(erase ? null : pts.slice().sort((p, q) => p[0] - q[0]));
    };
    addPt(e);
    track(addPt, () => {
      setPencilLive(null);
      if (pts.length < 2 && !erase) return;
      const base = tuneOrInit(), ta = Math.min(...pts.map(p => p[0])), tb = Math.max(...pts.map(p => p[0]));
      const nt: ClipTune = { ...base, pencil: erase ? erasePencil(base.pencil, ta, tb) : addPencil(base.pencil, pts) };
      commit([putTune(nt)], erase ? t('d.pitch.op.erase') : t('d.pitch.op.pencil'));
      say(erase ? t('d.pitch.say.erased') : t('d.pitch.say.drew', { s: (tb - ta).toFixed(2) }));
    }, 'crosshair');
  };
  const move = (e: PointerEvent) => {
    if (e.buttons || !an()) return;
    const { x, y } = pos(e);
    const i = x > KW && y > RH && y < size.H - BH ? hitNote(x, y) : -1, sp = x > KW ? hitSplit(x, y) : -1;
    const h = hover();
    if (!h || h.note !== i || h.split !== sp) setHover({ note: i, split: sp });
    const t = tool();
    cv.style.cursor = y < RH ? 'pointer' : t === 'pencil' ? 'crosshair' : t === 'split' ? (sp >= 0 ? 'col-resize' : i >= 0 ? 'text' : 'default') : i >= 0 ? 'ns-resize' : 'default';
  };
  const wheel = (e: WheelEvent) => {
    e.preventDefault(); e.stopPropagation();
    const { x, y } = pos(e), d = e.deltaY || e.deltaX;
    if (e.ctrlKey || e.metaKey) {
      const t = tAt(x), k = Math.pow(1.0015, -d);
      view.pps = clamp(view.pps * k, 10, 4000); view.t = t - (x - KW) / view.pps;
    } else if (e.shiftKey || Math.abs(e.deltaX) > Math.abs(e.deltaY)) view.t += d / view.pps * 0.8;
    else if (e.altKey) {
      const m = mAt(y), k = Math.pow(1.0015, d), span = clamp((view.hi - view.lo) * k, 6, 60);
      view.lo = m - (m - view.lo) / (view.hi - view.lo) * span; view.hi = view.lo + span;
    } else { const dm = -d / rowH() * 0.25; view.lo += dm; view.hi += dm; }
    bumpView();
  };
  const onKey = (e: KeyboardEvent) => {
    // any form control: Space opens a focused Key/Scale/Reference select, it must not start playback
    if ((e.target as HTMLElement).closest?.('input, select, textarea, [contenteditable]') || isTypingKey(e)) return;
    const k = e.key.toLowerCase(), m = e.ctrlKey || e.metaKey;
    const tl = TOOLS.find(x => x.key === e.key);
    if (tl && !m) { e.stopPropagation(); setTool(tl.id); say(t('d.pitch.say.tool', { name: t(tl.label) })); return; }
    if (m && k === 'a') { e.preventDefault(); e.stopPropagation(); setSel(new Set(notes().map((_, i) => i))); return; }
    if (m && k === 'c' && !e.shiftKey) { e.preventDefault(); e.stopPropagation(); copyNotes(); return; }
    if (e.key === 'Escape' && sel().size) { e.stopPropagation(); setSel(new Set<number>()); return; }
    if ((e.key === 'Delete' || e.key === 'Backspace') && sel().size) { e.preventDefault(); e.stopPropagation(); resetNotes(); return; }
    if (e.key === 'f' && !m) { e.stopPropagation(); fitView(); return; }
    // Space plays the song (never a pattern loop) from the start marker, so the vocal is heard in its mix
    if (e.code === 'Space' && !m) { e.preventDefault(); e.stopPropagation(); if (!e.repeat) void A.toggleSong(); return; }
    // everything else (Space, Esc to close, Ctrl+Z, F8/F9…) stays global
  };

  // ---- macros
  const scope = () => sel().size ? [...sel()] : notes().map((_, i) => i);
  /** Copy notes (Ctrl+C): the selected notes (all without a selection) at the pitch they sound at, for Ctrl+V in the Piano roll. */
  const copyNotes = () => {
    const a = an(); if (!a) return;
    const ns = notes();
    const sung = scope().flatMap(i => {
      const n = ns[i], p = pitchOf(i); if (p == null) return [];
      const f0 = Math.max(0, Math.floor(n.start * a.rate)), f1 = Math.min(a.levels.length, Math.max(f0 + 1, Math.ceil(n.end * a.rate)));
      let lv = 0; for (let f = f0; f < f1; f++) lv += a.levels[f] ?? 0;
      return [{ start: n.start, end: n.end, pitch: p, level: lv / Math.max(1, f1 - f0) * dbg(n.gain) }];
    });
    const r = sungToPattern(sung, { start: c().start, t0: t0(), t1: t1(), spbSrc: spbSrc() });
    if (!r.notes.length) { say(t('d.pitch.copy.none')); return; }
    setNoteClip(r.notes, 0, 0);
    say(t('d.pitch.copy.done', { n: r.notes.length }));
  };
  const correct = () => {
    const a = an(); if (!a) return;
    const nt = correctPitch(tuneOrInit(), a, scope(), centerPct(), driftPct(), key(), mask(), speedMs());
    commit([putTune(nt)], t('d.pitch.op.correct'));
    say(t('d.pitch.say.corrected', { n: scope().length, c: centerPct(), s: speedMs(), d: driftPct(), key: NOTE_NAMES[key()], scale: SCALES[scale()] }));
  };
  const applyRef = () => {
    const d = draft(), tg = target(); if (!d || !tg || tg.reason) return;
    const base = tune() ? plain(tune()!) : { notes: [], pencil: [] };
    commit([putTune({ ...base, reference: withKey(d) })], t('d.pitch.op.match'));
    const name = d.melody ? melName(d.melody.channel) : S.project.sources.find(s => s.id === d.source)?.name ?? '';
    say(t('d.pitch.say.matched', { name, c: Math.round(d.correction * 100), v: Math.round(d.vibrato * 100), i: Math.round(d.influence * 100) }));
  };
  /** One line on the reference: matching, an error, why it is not used, or how well it fits. */
  const refStatus = () => tgtBusy() ? t('d.pitch.matching')
    : tgtErr() ? t('d.pitch.error', { err: tgtErr()! })
    : target()?.reason ? t('d.pitch.nocorr', { why: reasonText(target()!.reason!) })
    : target() ? t(draft()?.melody ? 'd.pitch.mel.fit' : 'd.pitch.fit', { n: Math.round(target()!.score * 100) }) + (target()!.octave ? t('d.pitch.fit.oct', { o: (target()!.octave > 0 ? '+' : '') + target()!.octave }) : '')
      + (lowRuns().length ? t(lowRuns().length === 1 ? 'd.pitch.low.one' : 'd.pitch.low.many', { n: lowRuns().length }) : '')
    : '';
  const reasonText = (r: string) => r === 'the reference does not match this take' ? t('d.pitch.mismatch')
    : r === 'the melody has no notes over this clip' ? t('d.pitch.mel.none')
    : r === 'no melody note plays while this take is sung' ? t('d.pitch.mel.nowhere') : r;
  const melName = (ch: number) => (S.project.channels ?? []).find(x => x.id === ch)?.name ?? '?';
  /** The piano roll changed under a stored melody: take its notes again (one undo step). */
  const updateMelody = () => {
    const m = melStale(), tu = tune(); if (!m || !tu?.reference) return;
    commit([putTune({ ...plain(tu), reference: { ...plain(tu.reference), melody: m } })], t('d.pitch.op.melupdate'));
    setMelStale(null); say(t('d.pitch.say.melupdated', { name: melName(m.channel), n: m.notes.length }));
  };
  const refValue = () => { const d = draft(); return !d ? '' : d.melody ? 'm' + d.melody.channel : 's' + d.source; };
  const pickRef = (v: string) => {
    if (v[0] === 'm') { void useMelody(+v.slice(1)); return; }
    patchDraft({ source: +v.slice(1), melody: null });
  };
  const offRef = () => {
    const tu = tune(); if (!tu?.reference) return;
    const rest: ClipTune = { ...plain(tu), reference: null };
    commit([putTune(rest.notes.length || rest.pencil?.length ? rest : null)], t('d.pitch.op.refoff'));
    say(t('d.pitch.say.refoff'));
  };
  const pickMode = (i: number) => {
    if (i === 0 && applied()) offRef();
    if (i === 1) startDraft();
    setMode(i);
  };
  const refreshAnalysis = () => { anCache.clear(); setTarget(null); load(); };
  onMount(() => {
    be().f0ModelStatus().then(setModel).catch(() => {});
    const un = be().on('model-progress', p => setDl(p.error ? null : p.total ? p.done / p.total : null));
    onCleanup(() => { un.then(f => f()); });
  });
  const installModel = () => {
    setAskModel(false); setDl(0);
    be().f0ModelInstall()
      .then(st => { setModel(st); setDl(null); refreshAnalysis(); say(t('d.pitch.say.rmvpe')); })
      .catch(e => { setDl(null); say(t('d.pitch.say.rmvpefail', { err: String(e?.message ?? e) })); });
  };
  const resetNotes = () => {
    if (!tune()) return;
    if (!sel().size) {
      const r = tune()!.reference;
      commit([putTune(r ? { notes: [], pencil: [], reference: r } : null)], t('d.pitch.op.reset'));
      say(t(r ? 'd.pitch.say.reset1' : 'd.pitch.say.reset2'));
      return;
    }
    const nt = editNotes(tuneOrInit(), sel(), () => ({ shift: 0, drift: 0, formant: 0, gain: 0, speed: null }));
    commit([putTune(nt)], t('d.pitch.op.resetnotes')); say(t('d.pitch.say.resetn', { n: sel().size }));
  };
  const previewing = () => U.previewing;
  /** Solo: the take alone with its edits, from the start marker when it is inside the clip, else from its start. */
  const solo = () => {
    if (previewing()) { void be().previewStop(); return; }
    const ts = tOfBeat(U.start), from = ts > t0() && ts < t1() ? ts : t0();
    be().previewClip(c().id, from).catch(e => say(String(e?.message ?? e)));
  };
  const resize = (e: PointerEvent) => {
    if (e.button !== 0) return; e.preventDefault(); e.stopPropagation();
    const sx = e.clientX, sy = e.clientY, o = { ...E() };
    track(ev => setU('pitchEd', ed => ed && { ...ed, w: clamp(o.w + ev.clientX - sx, 720, window.innerWidth - 20), h: clamp(o.h + ev.clientY - sy, 380, window.innerHeight - 20) }), null, 'nwse-resize');
  };
  // over the plugin / Channel Rack / Piano roll layer when it was used after them
  const aboveWins = () => { const z = E().z ?? 0; return !U.plugsHidden && [...U.plugs.map(p => p.z), U.rackWin?.z ?? 0, U.rollWin?.z ?? 0].every(o => z > o); };
  void theme; void fonts;
  return (
    <div ref={root} tabIndex={0} class="win pitchwin" style={{ left: E().x + 'px', top: E().y + 'px', width: E().w + 'px', height: E().h + 'px', 'z-index': aboveWins() ? 46 : 44 }}
      onKeyDown={onKey} onPointerDown={A.pitchFront} data-pitched>
      <div class="winhd" onPointerDown={e => moveWindow(e, () => E(), (x, y) => setU('pitchEd', ed => ed && { ...ed, x, y }))}>
        <span class="tile" style={{ width: '20px', height: '20px', background: FXMETA.tune.color + '26', color: FXMETA.tune.color }}><Icon d={FXMETA.tune.icon} size={13} /></span>
        <span class="title">{t('d.pitch.title')}</span>
        <span style={{ width: '6px', height: '6px', 'border-radius': '2px', background: src()?.color ?? 'var(--fg5)' }} />
        <span class="sub">{src()?.name ?? ''} · {S.project.tracks[c().track]?.name ?? ''}</span>
        <span ref={status} class="rendering" />
        <div style={{ flex: 1 }} />
        <span onPointerDown={e => e.stopPropagation()}>
          <Segmented small options={[t('d.pitch.mode.manual'), t('d.pitch.mode.ref')]} value={mode} pick={pickMode} color={FXMETA.tune.color} title={t('d.pitch.mode.tip')} />
        </span>
        <button class="smallbtn" classList={{ playing: U.playing }} title={t('d.pitch.play.tip')} onPointerDown={e => e.stopPropagation()} onClick={() => void A.toggleSong()}>{U.playing ? t('d.pitch.stop') : t('d.pitch.play')}</button>
        <button class="smallbtn" classList={{ playing: previewing() }} title={t('d.pitch.solo.tip')} onPointerDown={e => e.stopPropagation()} onClick={solo}>{previewing() ? t('d.pitch.stop') : t('d.pitch.solo')}</button>
        <button class="iconbtn" title={t('d.pitch.fit.tip')} onPointerDown={e => e.stopPropagation()} onClick={() => fitView()}><Icon d="M4 9V4h5 M20 9V4h-5 M4 15v5h5 M20 15v5h-5" size={13} /></button>
        <PitchCorrectButton insert={() => S.project.tracks[c().track]?.insert ?? null} keyScale={() => ({ key: key(), scale: scale() })} />
        <button class="closebtn" title={t('d.pitch.close')} onClick={A.closePitchEditor}>×</button>
      </div>
      <div class="ptools">
        <div class="seg tools">
          <For each={TOOLS}>{tl => (
            <button classList={{ on: tool() === tl.id }} title={t(tl.label) + ' (' + tl.key + ') — ' + t(tl.tip)} onClick={() => setTool(tl.id)}>
              <Icon d={tl.icon} size={14} />
            </button>
          )}</For>
        </div>
        <label class="plab" title={t('d.pitch.key.tip')}>{t('d.pitch.key')}
          <select class="sel" value={key()} onChange={e => setKey(+e.currentTarget.value)}><For each={NOTE_NAMES}>{(n, i) => <option value={i()}>{n}</option>}</For></select>
        </label>
        <select class="sel" title={t('d.pitch.key.tip')} value={scale()} onChange={e => setScale(+e.currentTarget.value)}><For each={SCALES}>{(n, i) => <option value={i()}>{n}</option>}</For></select>
        <div class="vsep" />
        <Show when={mode() === 0}>
          <Segmented small options={CORRECT_PRESETS.map(x => t(('d.pitch.preset.' + x.id) as TranslationKey))} value={preset} pick={pickPreset} color={FXMETA.tune.color} title={t('d.pitch.style.tip')} />
          <NumField label={t('d.pitch.strength')} width={64} title={t('d.pitch.strength.tip')} value={centerPct} text={v => Math.round(v) + '%'} parse={parseNum} drag={(v, d) => clamp(Math.round(v + d * 0.5), 0, 100)} apply={v => setCenterPct(Math.round(clamp(v, 0, 100)))} />
          <NumField label={t('d.pitch.speed')} width={64} title={t('d.pitch.speed.tip')} value={speedMs} text={v => Math.round(v) + ' ms'} parse={parseNum} drag={(v, d) => clamp(Math.round(v + d), 0, 400)} apply={v => setSpeedMs(Math.round(clamp(v, 0, 400)))} />
          <NumField label={t('d.pitch.flatten')} width={70} title={t('d.pitch.flatten.tip')} value={driftPct} text={v => Math.round(v) + '%'} parse={parseNum} drag={(v, d) => clamp(Math.round(v + d * 0.5), 0, 100)} apply={v => setDriftPct(Math.round(clamp(v, 0, 100)))} />
          <button class="smallbtn primary" disabled={!an()} title={t('d.pitch.macro.tip') + '\n' + t('d.pitch.explain', { c: centerPct(), s: speedMs() === 0 ? t('d.pitch.instant') : speedMs() + ' ms', d: driftPct(), key: NOTE_NAMES[key()], scale: t(('d.pitch.scale.' + scale()) as TranslationKey) })} onClick={correct}>{sel().size ? t('d.pitch.correct.sel', { n: sel().size }) : t('d.pitch.correct.all')}</button>
        </Show>
        <Show when={mode() === 1}>
          <Show when={(refSources().length || melodies().length || draft()?.melody) && draft()} fallback={<span class="refnote">{t('d.pitch.refnote.empty')}</span>}>
            <label class="reflab" title={t('d.pitch.ref.tip')}>
              <span>{t('d.pitch.reference')}</span>
              <select class="sel" value={refValue()} onChange={e => pickRef(e.currentTarget.value)}>
                <Show when={melodies().length || draft()?.melody}>
                  <optgroup label={t('d.pitch.ref.melodies')}>
                    <Show when={draft()?.melody && !melodies().some(m => m.channel === draft()!.melody!.channel)}>
                      <option value={'m' + draft()!.melody!.channel}>♪ {melName(draft()!.melody!.channel)}</option>
                    </Show>
                    <For each={melodies()}>{m => <option value={'m' + m.channel}>♪ {m.name} · {t('d.pitch.ref.fit', { n: Math.round(m.fit * 100) })}</option>}</For>
                  </optgroup>
                </Show>
                <Show when={refSources().length}>
                  <optgroup label={t('d.pitch.ref.recordings')}>
                    <For each={refSources()}>{s => <option value={'s' + s.id}>{s.name}</option>}</For>
                  </optgroup>
                </Show>
              </select>
            </label>
            <NumField label={t('d.pitch.correction')} width={64} title={t('d.pitch.correction.tip')} value={() => Math.round(draft()!.correction * 100)} text={v => Math.round(v) + '%'} parse={parseNum}
              drag={(v, d) => clamp(Math.round(v + d * 0.5), 0, 100)} apply={v => patchDraft({ correction: clamp(Math.round(v), 0, 100) / 100 })} />
            <NumField label="VIBRATO" width={58} title={t('d.pitch.vibrato.tip')} value={() => Math.round(draft()!.vibrato * 100)} text={v => Math.round(v) + '%'} parse={parseNum}
              drag={(v, d) => clamp(Math.round(v + d * 0.5), 0, 100)} apply={v => patchDraft({ vibrato: clamp(Math.round(v), 0, 100) / 100 })} />
            <NumField label={t('d.pitch.speed')} width={64} title={t('d.pitch.refspeed.tip')} value={() => draft()!.speed ?? 0} text={v => (v ? Math.round(v) + ' ms' : t('d.pitch.instant'))} parse={parseNum}
              drag={(v, d) => clamp(Math.round(v + d), 0, 400)} apply={v => patchDraft({ speed: Math.round(clamp(v, 0, 400)) || null })} />
            <Show when={!draft()!.melody}><NumField label={t('d.pitch.influence')} width={66} title={t('d.pitch.influence.tip')} value={() => Math.round(draft()!.influence * 100)} text={v => Math.round(v) + '%'} parse={parseNum}
              drag={(v, d) => clamp(Math.round(v + d * 0.5), 0, 100)} apply={v => patchDraft({ influence: clamp(Math.round(v), 0, 100) / 100 })} /></Show>
            <label class="plab" title={t('d.pitch.octave.tip')}>{t('d.pitch.octave')}
              <select class="sel" value={[null, -24, -12, 0, 12, 24].indexOf(draft()!.octave ?? null)} onChange={e => patchDraft({ octave: [null, -24, -12, 0, 12, 24][+e.currentTarget.value] })}>
                <For each={[t('d.pitch.auto'), '−24', '−12', '0', '+12', '+24']}>{(o, i) => <option value={i()}>{o}</option>}</For>
              </select>
            </label>
            <button class="smallbtn primary" disabled={!pending() || !target() || !!target()?.reason || tgtBusy()} onClick={applyRef}>{applied() ? t('d.pitch.apply.changes') : t('d.pitch.apply')}</button>
            <Show when={applied()}><button class="smallbtn" onClick={offRef} title={t('d.pitch.off.tip')}>{t('d.pitch.off')}</button></Show>
            <Show when={melStale() && applied()?.melody}>
              <span class="refnote warn">{t('d.pitch.mel.changed')}</span>
              <button class="smallbtn" title={t('d.pitch.mel.update.tip')} onClick={updateMelody}>{t('d.pitch.mel.update')}</button>
            </Show>
            <span class="refnote" classList={{ err: !!tgtErr() || !!target()?.reason }} title={refStatus()}>{refStatus()}</span>
          </Show>
          <span class="reflegend"><i style={{ background: 'var(--fg4)' }} />{t('d.pitch.legend.orig')} <i class="dash" />{t('d.pitch.legend.ref')} <i style={{ background: FXMETA.tune.color }} />{t('d.pitch.legend.out')}</span>
          <Show when={model()}>{m => (
            <span class="detchip" title={t('d.pitch.det', { name: m().backend === 'rmvpe' ? t('d.pitch.det.rmvpe') : t('d.pitch.det.yin') }) + (m().error ? ' · ' + m().error : '')}>
              {m().backend === 'rmvpe' ? 'RMVPE' : 'YIN'}
            </span>
          )}</Show>
          <Show when={model() && !model()!.installed}>
            <button class="smallbtn" disabled={dl() != null} onClick={() => setAskModel(true)} title={t('d.pitch.dl.tip')}>
              {dl() != null ? t('d.pitch.dl.busy', { n: Math.round(dl()! * 100) }) : t('d.pitch.dl')}
            </button>
          </Show>
        </Show>
        <div style={{ flex: 1 }} />
        <button class="smallbtn" disabled={!an()} title={an() ? t('d.pitch.copy.tip') : t('d.pitch.copy.wait')} onClick={copyNotes}>{sel().size ? t('d.pitch.copy.sel', { n: sel().size }) : t('d.pitch.copy.all')}</button>
        <button class="smallbtn" disabled={!tune()} title={sel().size ? t('d.pitch.reset.tip1') : t('d.pitch.reset.tip2')} onClick={resetNotes}>{sel().size ? t('d.pitch.reset.notes') : t('d.pitch.reset')}</button>
      </div>
      <div ref={area} class="parea">
        <canvas ref={cv} style={{ position: 'absolute', inset: 0, width: '100%', height: '100%', 'touch-action': 'none' }}
          onPointerDown={down} onPointerMove={move} onPointerLeave={() => setHover(null)} onWheel={wheel} onContextMenu={e => e.preventDefault()} />
        <div ref={phead} class="pphead" />
        <Show when={!an() && !err()}><div class="pstate"><span class="spin" />{t('d.pitch.analyzing')}</div></Show>
        <Show when={err()}><div class="pstate err">{t('d.pitch.anfail', { err: err()! })} <button class="smallbtn" onClick={load}>{t('d.pitch.retry')}</button></div></Show>
        <Show when={askModel() && model()}>{m => (
          <div class="pstate">
            <div class="consent">
              <b>{t('d.pitch.ask', { mb: Math.round(m().bytes / 1e6) })}</b>
              <p>{m().licenseNote}</p>
              <p class="mono sel-text">{m().licenseUrl}</p>
              <p class="mono">{t('d.pitch.ask.sha', { path: m().path })}</p>
              <div class="row">
                <button class="smallbtn" onClick={() => navigator.clipboard?.writeText(m().licenseUrl).then(() => say(t('d.pitch.say.licensecopied')))}>{t('d.pitch.copylink')}</button>
                <div style={{ flex: 1 }} />
                <button class="smallbtn" onClick={() => setAskModel(false)}>{t('d.pitch.cancel')}</button>
                <button class="smallbtn primary" onClick={installModel}>{t('d.pitch.dlinstall')}</button>
              </div>
            </div>
          </div>
        )}</Show>
      </div>
      <div class="pstatus mono">
        <span>{an() ? t('d.pitch.count', { n: notes().length }) : ''}{sel().size ? ' · ' + t('d.pitch.selcount', { n: sel().size }) : ''}</span>
        <span>{t(TOOLS.find(x => x.id === tool())!.tip)}</span>
        <span>{t('d.pitch.hint')}</span>
      </div>
      <div class="resizer" onPointerDown={resize} title={t('d.pitch.resize')} />
    </div>
  );
}
