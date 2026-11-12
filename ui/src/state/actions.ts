// Commands shared by the toolbar, keyboard, menus and views.
import { batch } from 'solid-js';
import { be } from '../api/backend';
import { t, tName } from '../lib/i18n';
import { VOL_UNITY, type AnyBuiltinKind, type Clip, type Fx, type Id, type Insert, type Op, type RecoveredProject, type Track } from '../api/types';
import { clampMove, duplicateSpan, insertTrackOps, moveClips, muteOps, pasteClips, sliceClip, TOTAL } from '../lib/clipOps';
import { clamp, fmtPos } from '../lib/format';
import { loadPreset as loadPresetFx, newFx, presetInfo, presetList } from '../lib/presets';
import { snapName, snapRes } from '../lib/snap';
import { deletePoints } from '../lib/envEdit';
import { acceptReply, commit, endWheelGestures, enqueue, resync } from './edit';
import { flushNotebook } from './notebook';
import { clearPeaks } from './peaks';
import { modePattern, openRack, patMode, setPatMode, stopPreview, togglePreview } from './pattern';
import { allocId, findFx, insName, plain, S, setTransport } from './store';
import { say, setU, U } from './ui';
import { confirmRecover, confirmUnsaved } from '../ui/Confirm';

// ---------- transport ----------
/** Recording is on but no track is armed: the engine plays and records nothing. */
const NO_ARMED = () => t('d.act.noarmed');
const armedTrack = () => S.project.tracks.find(t => t.arm);
/** Recording with nothing armed: arm the first track with no clips at all (waits until the engine has it). */
async function autoArm() {
  if (armedTrack()) return;
  const used = new Set(S.project.clips.map(c => c.track));
  const i = S.project.tracks.findIndex((_, k) => !used.has(k));
  if (i < 0) return;
  armTrack(i);
  await enqueue(() => Promise.resolve());
  say(t('d.act.recon1', { name: S.project.tracks[i].name }));
}
export async function play(from = U.start) {
  if (U.patPlaying) await stopPreview();
  setU('playing', true);
  try {
    if (S.transport.rec) await autoArm();
    await be().transportPlay(from); setTransport({ playing: true });
    say(!S.transport.rec ? 'Play' : armedTrack() ? t('d.act.recon', { name: armedTrack()!.name }) : NO_ARMED());
  }
  catch (e) { setU('playing', false); say(t('d.act.playerr', { err: String(e) })); }
}
/** `rewind` also moves the start marker (and the stopped playhead) to the beginning of the song. */
export async function stop(rewind = false) {
  if (U.patPlaying) void stopPreview();
  setU('playing', false);
  if (rewind) setU('start', 0);
  try { await be().transportStop(); setTransport({ playing: false }); } catch (e) { console.warn(e); }
  say(rewind ? t('d.act.stop.rew') : 'Stop');
}
/** Space: stop whatever plays; else PAT mode loops the current pattern, SONG mode plays the song
 * (recording always runs the song). */
export function togglePlay() {
  if (U.playing) return stop();
  if (U.patPlaying) { void stopPreview(); say('Stop'); return; }
  const p = patMode() && !S.transport.rec ? modePattern() : undefined;
  if (p) return togglePreview(p.id);
  // SONG with an empty playlist would play silence: play the pattern being edited and say why
  const cur = modePattern();
  if (!S.transport.rec && !S.project.clips.length && cur?.notes.length) { say(t('d.act.songEmpty', { name: cur.name })); return togglePreview(cur.id); }
  return play();
}
/**
 * The PAT / SONG switch (L): while something plays, what plays switches with it (FL) — the song
 * hands over to the current pattern's loop, the loop to the song from its start marker. A
 * recording keeps going.
 */
export function switchMode(on: boolean) {
  if (on === patMode()) return;
  setPatMode(on);
  if (S.transport.rec && U.playing) return;
  if (on && U.playing) { const p = modePattern(); if (p) void togglePreview(p.id); }
  else if (!on && U.patPlaying) void play(U.start);
}
/**
 * A click on the playlist's time ruler (FL): SONG mode from there. A pattern loop that is
 * playing hands over to the song at that position, so moving between them needs no L key.
 */
export function seekSong(b: number) {
  const from = Math.max(0, b);
  if (patMode()) setPatMode(false);
  if (U.patPlaying) { setU('start', from); void play(from); return; }
  seek(from);
}
export function seek(b: number) {
  setU('start', Math.max(0, b));
  if (U.playing) void be().transportPlay(U.start);
  say('Start: ' + fmtPos(U.start));
}
export async function setTransportFlag(k: 'metro' | 'countIn' | 'rec' | 'monitor', v: boolean) {
  setTransport({ [k]: v });
  try { const t = await be().transportSet({ [k]: v }); setTransport(t); } catch (e) { say(t('d.act.err', { err: String(e) })); }
}
export async function toggleRec() {
  const on = !S.transport.rec;
  if (on) await autoArm();
  void setTransportFlag('rec', on);
  say(!on ? (U.playing ? 'Punch out' : t('d.act.recoff'))
    : !armedTrack() ? NO_ARMED()
    : U.playing ? t('d.act.punchin', { name: armedTrack()!.name })
    : t(S.transport.countIn ? 'd.act.recspace.ci' : 'd.act.recspace', { name: armedTrack()!.name }));
}
export function toggleMonitor() {
  const on = !S.transport.monitor;
  void setTransportFlag('monitor', on);
  say(t(on ? 'd.act.mon.on' : 'd.act.mon.off'));
}

// ---------- history / project ----------
export async function undo() { if (!S.canUndo) return say(t('d.act.noUndo')); flushNotebook(); endWheelGestures(); acceptReply(await enqueue(() => be().undo())); say(t('d.act.undone')); }
export async function redo() { if (!S.canRedo) return say(t('d.act.noRedo')); flushNotebook(); endWheelGestures(); acceptReply(await enqueue(() => be().redo())); say(t('d.act.redone')); }
/** Ask about unsaved changes; false = the user cancelled (or the save failed). */
export async function guardUnsaved(what: string): Promise<boolean> {
  const a = await confirmUnsaved(what);
  if (a === 'cancel') return false;
  if (a === 'save') return projectSave(false);
  return true;
}
export async function projectNew() {
  if (!(await guardUnsaved(t('d.act.guard.new')))) return;
  flushNotebook();
  try { acceptReply(await enqueue(() => be().projectNew())); afterProjectLoad(); if (!S.project.clips.length) openRack(); say(t('d.act.newproj')); } catch (e) { say(t('d.act.newfail', { err: String(e) })); }
}
export async function projectOpen() {
  if (!(await guardUnsaved(t('d.act.guard.open')))) return;
  flushNotebook();
  try {
    const r = await enqueue(() => be().projectOpen());
    if (r.cancelled) return;
    acceptReply(r.state); afterProjectLoad();
    const path = S.path ?? '';
    say(r.warnings.length ? t('d.act.openedwarn', { path, n: r.warnings.length, first: r.warnings[0] }) : t('d.act.opened', { path }));
  } catch (e) { say(t('d.act.openfail', { err: String(e) })); }
}
export async function projectSave(saveAs = false): Promise<boolean> {
  flushNotebook();
  try {
    const r = await enqueue(() => be().projectSave(saveAs));
    if (r.cancelled) return false; // the save dialog was dismissed: nothing was written
    say(t('d.act.saved', { path: r.path })); await resync(); return true;
  } catch (e) { say(t('d.act.savefail', { err: String(e) })); return false; }
}
/** Autosave tick for a project without a file: keeps its crash-recovery copy current. */
export async function recoveryTick() {
  try { await enqueue(() => be().recoveryWrite()); } catch (e) { console.warn('[wave] recovery copy:', e); }
}
/**
 * Startup: offer back the unsaved projects a WAVE that crashed or was killed left behind,
 * newest first. "Not now" keeps them for the next start; one restore ends the round.
 */
export async function offerRecovery() {
  let list: RecoveredProject[];
  try { list = await be().recoveryList(); } catch { return; }
  for (const r of list) {
    const a = await confirmRecover(r.savedMs);
    if (a === 'later') return;
    if (a === 'discard') { await be().recoveryDiscard(r.id).catch(e => say(t('d.act.err', { err: String(e) }))); continue; }
    if (!(await guardUnsaved(t('d.act.guard.open')))) return;
    flushNotebook();
    try {
      const res = await enqueue(() => be().recoveryRestore(r.id));
      acceptReply(res.state); afterProjectLoad();
      say(res.warnings.length ? t('d.act.openedwarn', { path: t('d.ui.unnamed'), n: res.warnings.length, first: res.warnings[0] }) : t('d.act.recovered'));
    } catch (e) { say(t('d.act.recoverfail', { err: String(e) })); }
    return;
  }
}
/** Window close: ask about unsaved changes, then really close. */
export async function closeWindowGuarded(): Promise<boolean> {
  return guardUnsaved(t('d.act.guard.close'));
}
/** Escape on the export dialog: abort an in-flight bounce, otherwise close. */
export function cancelExportOrClose() {
  if (!U.exportOpen) return;
  if (U.exportBusy) {
    void be().exportCancel().catch(() => {});
    say(t('d.act.exportcancel'));
  }
  setU({ exportOpen: false, exportBusy: false });
}
function afterProjectLoad() {
  clearPeaks();
  batch(() => {
    setU({ sel: [], plugs: [], editor: null, start: 0, menu: null });
    const m = S.project.inserts[1] ?? S.project.inserts[0];
    if (m) setU({ mixSel: m.id, fxSel: m.fx[0]?.id ?? null });
    const p = S.project.patterns[0];
    setU({ cur: p ? { kind: 'pat', ref: p.id } : null, pat: p?.id ?? null, selCh: S.project.channels?.[0]?.id ?? null });
  });
}

// ---------- tempo / loop ----------
export const snapUnit = (alt = false) => (alt || U.alt ? 0 : snapRes(U.snap, U.bw));
export function setLoop(loop: { a: number; b: number } | null, label = t('d.act.loop')) {
  commit([{ op: 'setLoop', loop }], label);
}

// ---------- selection & clips ----------
export const selClips = (): Clip[] => { const s = new Set(U.sel); return S.project.clips.filter(c => s.has(c.id)); };
let clipboard: Clip[] = [];
/** Where Ctrl+V puts the next copy: right after the copied block, then after each paste (chains like the piano roll). */
let pasteAt: number | null = null;
export const hasClipboard = () => clipboard.length > 0;

/** Delete / Backspace with envelope points selected: they go (one undo step; one point always stays). */
export function deleteAutoPoints() {
  const s = U.autoSel; if (!s) return;
  const c = S.project.clips.find(x => x.id === s.clip), a = c && S.project.automations?.find(x => x.id === c.ref);
  if (!a) { setU('autoSel', null); return; }
  const pts = deletePoints(plain(a.points), new Set(s.idx));
  if (!pts) { say(t('a.pl.autoMinPoint')); return; }
  commit([{ op: 'putAutomation', automation: { ...plain(a), points: pts } }], t('a.pl.deletePoint'));
  setU('autoSel', null);
  say(t('a.pl.pointsDeleted', { n: s.idx.length }));
}
export function delIds(ids: Id[]) {
  if (!ids.length) return;
  commit([{ op: 'removeClips', ids: [...ids] }], t('d.act.op.delclips'));
  const s = new Set(ids);
  setU('sel', U.sel.filter(x => !s.has(x)));
}
export function duplicate() {
  const cs = selClips(); if (!cs.length) return;
  const span = duplicateSpan(cs, snapUnit());
  if (Math.max(...cs.map(c => c.start + c.len)) + span > TOTAL) return say(t('d.act.nospace'));
  const copies = cs.map(c => ({ ...plain(c), id: allocId(), start: c.start + span }));
  commit([{ op: 'putClips', clips: copies }], t('d.act.op.dup'));
  setU('sel', copies.map(c => c.id));
  say(t('d.act.duplicated', { n: copies.length }));
}
export function copy() { const cs = selClips(); if (!cs.length) return; clipboard = cs.map(plain); pasteAt = Math.max(...clipboard.map(c => c.start + c.len)); say(t('d.act.copied', { n: cs.length })); }
export function cut() { copy(); delIds(U.sel); }
export function paste(at?: { beat: number; track: number }) {
  if (!clipboard.length) return;
  const beat = at ? at.beat : pasteAt ?? U.start;
  const copies = pasteClips(clipboard, beat, at ? at.track : null, S.project.tracks.length, allocId);
  if (!at) pasteAt = Math.max(...copies.map(c => c.start + c.len));
  commit([{ op: 'putClips', clips: copies }], t('d.act.op.paste'));
  setU('sel', copies.map(c => c.id));
  say(t('d.act.pasted', { n: copies.length, pos: fmtPos(beat) }));
}
/** Playlist resize mode: trim ↔ stretch (Shift+M, the switch in the playlist's corner). */
export function toggleStretchMode() {
  setU('stretchMode', !U.stretchMode);
  say(t(U.stretchMode ? 'a.pl.stretchOn' : 'a.pl.stretchOff'));
}
/** How the selected audio clips (or `c`) stretch: keeping pitch, or resampled (speed and pitch together). */
export function setStretchMethod(c: Clip, resample: boolean) {
  const ids = U.sel.includes(c.id) ? U.sel : [c.id];
  const cs = S.project.clips.filter(x => ids.includes(x.id) && x.kind === 'audio' && !!x.resample !== resample).map(plain);
  if (!cs.length) return;
  commit([{ op: 'putClips', clips: cs.map(x => ({ ...x, resample })) }], t(resample ? 'a.pl.resampleOp' : 'a.pl.keepPitchOp'));
}
export function toggleMuteSel() {
  const cs = selClips(); if (!cs.length) return;
  commit(muteOps(cs.map(plain), !cs.every(c => c.muted)), t('d.act.op.muteclips'));
}
export function nudge(db: number, dt: number) {
  const cs = selClips().map(plain); if (!cs.length) return;
  const [d, dtr] = clampMove(cs, db, dt, S.project.tracks.length);
  if (!d && !dtr) return;
  commit([{ op: 'putClips', clips: moveClips(cs, d, dtr) }], t('d.act.op.move'));
}
/** Split clip `id` at beat `b`; with `withSel`, when it is part of the selection, every selected clip that crosses `b` (one undo step). */
export function sliceAt(id: Id, b: number, withSel = true) {
  const ids = withSel && U.sel.includes(id) ? U.sel : [id];
  const r = S.project.clips.filter(x => ids.includes(x.id)).flatMap(c => sliceClip(plain(c), b, allocId()) ?? []);
  if (!r.length) return;
  commit([{ op: 'putClips', clips: r }], t('d.act.op.split'));
  say(t('d.act.cut', { pos: fmtPos(b) }));
}
export function selectAll() { setU('sel', S.project.clips.map(c => c.id)); }
export function makeUnique(clip: Clip) {
  if (clip.kind === 'audio') return void makeUniqueAudio(clip);
  const p = S.project.patterns.find(x => x.id === clip.ref); if (!p) return;
  const baseName = p.name.replace(/ #\d+$/, '');
  const count = S.project.patterns.filter(x => x.name.startsWith(baseName)).length;
  const np = { ...plain(p), id: allocId(), name: baseName + ' #' + (count + 1) };
  commit([{ op: 'putPattern', pattern: np }, { op: 'putClips', clips: [{ ...plain(clip), ref: np.id }] }], 'Make unique');
  setU('cur', { kind: 'pat', ref: np.id });
  say(t('d.act.newpat', { name: np.name }));
}
/**
 * FL's "Make unique" for audio: the clip — with the selected clips of the same sample on its
 * track, when it is selected — gets its own copy of the sample on a new track right below,
 * routed to a free mixer insert (shown in the Mixer). One undo step (`Backend.makeUnique`).
 */
async function makeUniqueAudio(clip: Clip) {
  const ids = U.sel.includes(clip.id)
    ? S.project.clips.filter(c => U.sel.includes(c.id) && c.kind === 'audio' && c.ref === clip.ref && c.track === clip.track).map(c => c.id)
    : [clip.id];
  endWheelGestures();
  try {
    // queued behind pending edits; then the new state, so the names below are the engine's
    const r = await enqueue(() => be().makeUnique(ids));
    await resync();
    setU({ sel: ids, cur: { kind: 'audio', ref: r.sourceId }, mixSel: r.insertId, fxSel: null });
    const name = S.project.sources.find(s => s.id === r.sourceId)?.name ?? '';
    say(t('d.act.madeUnique', { name, n: S.project.inserts.findIndex(m => m.id === r.insertId) }));
  } catch (e) {
    say(t('d.act.uniqueFail', { err: String(e) }));
    void resync();
  }
}
export function setSrcProp(clip: Clip, patch: { name?: string; color?: string }) {
  if (clip.kind === 'auto') {
    const a = S.project.automations?.find(x => x.id === clip.ref); if (a) commit([{ op: 'putAutomation', automation: { ...plain(a), ...patch } }], t('d.act.op.auto'));
  } else if (clip.kind === 'pat') {
    const p = S.project.patterns.find(x => x.id === clip.ref); if (p) commit([{ op: 'putPattern', pattern: { ...plain(p), ...patch } }], 'Pattern');
  } else {
    const s = S.project.sources.find(x => x.id === clip.ref); if (s) commit([{ op: 'putSource', source: { ...plain(s), ...patch } }], t('d.act.op.source'));
  }
}

// ---------- tracks ----------
export function trackOp(i: number, patch: Partial<Track>): Op | null {
  const t = S.project.tracks[i]; if (!t) return null;
  return { op: 'putTrack', track: { ...plain(t), ...patch } };
}
export function setTrack(i: number, patch: Partial<Track>, label = t('d.act.track')) { const op = trackOp(i, patch); if (op) commit([op], label); }
export function armTrack(i: number) {
  const on = !S.project.tracks[i]?.arm;
  const ops = S.project.tracks.map((t, j) => ({ t, arm: j === i ? on : false })).filter(x => x.t.arm !== x.arm)
    .map(x => ({ op: 'putTrack', track: { ...plain(x.t), arm: x.arm } }) as Op);
  commit(ops, t('d.act.op.arm'));
  say(on ? t('d.act.recon2', { name: S.project.tracks[i].name }) : t('d.act.disarmed'));
}
/** Mute and solo are mutually exclusive per channel: turning one on clears the other (one undo step). */
export function toggleTrackMute(i: number) {
  const t = S.project.tracks[i]; if (!t) return;
  setTrack(i, t.muted ? { muted: false } : { muted: true, solo: false }, 'Mute');
}
/** Solo is exclusive (FL): soloing a track un-solos the others; Ctrl+click adds to the solo set. */
export function toggleTrackSolo(i: number, add = false) {
  const t = S.project.tracks[i]; if (!t) return;
  if (t.solo) { setTrack(i, { solo: false }, 'Solo'); return; }
  const ops = S.project.tracks.map((x, j) => j === i ? trackOp(j, { solo: true, muted: false }) : (!add && x.solo ? trackOp(j, { solo: false }) : null)).filter((o): o is Op => !!o);
  commit(ops, 'Solo');
}
export function toggleInsertMute(id: Id) {
  const m = S.project.inserts.find(x => x.id === id); if (!m) return;
  setInsert(id, m.muted ? { muted: false } : { muted: true, solo: false }, 'Mute');
}
export function toggleInsertSolo(id: Id, add = false) {
  const m = S.project.inserts.find(x => x.id === id); if (!m || id === 0) return;
  if (m.solo) { setInsert(id, { solo: false }, 'Solo'); return; }
  const ops = S.project.inserts.map(x => x.id === id ? insertOp(x.id, { solo: true, muted: false }) : (!add && x.solo ? insertOp(x.id, { solo: false }) : null)).filter((o): o is Op => !!o);
  commit(ops, 'Solo');
}
export function removeTrack(id: number) {
  commit([{ op: 'removeTrack', id }], t('d.act.op.deltrack'));
}

export function insertTrack(i: number) {
  commit(insertTrackOps(plain(S.project), i, allocId()), t('d.act.op.addtrack'));
}
export const addTrack = () => insertTrack(S.project.tracks.length);

// ---------- mixer ----------
export function insertOp(id: Id, patch: Partial<Insert>): Op | null {
  const m = S.project.inserts.find(x => x.id === id); if (!m) return null;
  const { fx: _fx, ...rest } = plain(m); void _fx;
  return { op: 'putInsert', insert: { ...rest, fx: [], ...patch } as Insert };
}
export function setInsert(id: Id, patch: Partial<Insert>, label = t('d.act.mixer')) { const op = insertOp(id, patch); if (op) commit([op], label); }
export function selectInsert(id: Id) {
  const m = S.project.inserts.find(x => x.id === id);
  if (m && U.mixSel !== id) setU({ mixSel: id, fxSel: m.fx[0]?.id ?? null });
}
export function addInsert() {
  const id = allocId();
  commit([{ op: 'putInsert', insert: { id, name: 'Insert ' + S.project.inserts.length, color: '#8b93a1', vol: VOL_UNITY, pan: 0, muted: false, solo: false, fx: [] } }], t('d.act.op.addinsert'));
  setU({ mixSel: id, fxSel: null });
}
export function removeInsert(id: Id) {
  const m = S.project.inserts.find(x => x.id === id);
  m?.fx.forEach(f => closePlug(f.id));
  commit([{ op: 'removeInsert', id }], t('d.act.op.delinsert'));
  if (U.mixSel === id) setU({ mixSel: 0, fxSel: null });
}

// ---------- effects ----------
export function fxOp(fx: Fx, insId?: Id): Op {
  const ins = insId ?? findFx(fx.id)?.m.id ?? 0;
  return { op: 'putFx', insertId: ins, fx: plain(fx) };
}
export function addFx(insId: Id, type: AnyBuiltinKind, preset?: number) {
  if (!S.catalog) return;
  let f = newFx(S.catalog, allocId(), type, S.project.bpm);
  if (preset != null && preset > 0 && presetList(S.catalog, type)[preset]) f = loadPresetFx(S.catalog, f, preset, S.project.bpm);
  commit([{ op: 'putFx', insertId: insId, fx: f }], t('d.act.op.addfx'));
  setU({ mixSel: insId, fxSel: f.id });
  openPlug(f.id);
  const pr = presetList(S.catalog, type)[preset ?? 0];
  say(t('d.act.added', { name: tName(S.catalog.types[type]?.name ?? type) }) + (pr ? t('d.act.added.preset', { name: tName(pr.name) }) : ''));
}
export function addVst(insId: Id, info: { path: string; cid: string; name: string; vendor: string }) {
  const f: Fx = { id: allocId(), type: 'vst3', bypass: false, preset: null, params: {}, plugin: { format: 'vst3', path: info.path, cid: info.cid, name: info.name, vendor: info.vendor } };
  commit([{ op: 'putFx', insertId: insId, fx: f }], t('d.act.op.addvst'));
  setU({ mixSel: insId, fxSel: f.id });
  openPlug(f.id);
  say(t('d.act.added', { name: info.name }));
}
export function removeFx(fxId: Id) {
  closePlug(fxId);
  commit([{ op: 'removeFx', fxId }], t('d.act.op.delfx'));
  if (U.fxSel === fxId) setU('fxSel', null);
}
export function toggleBypass(fxId: Id) {
  const ff = findFx(fxId); if (!ff) return;
  commit([fxOp({ ...plain(ff.f), bypass: !ff.f.bypass }, ff.m.id)], 'Bypass');
  say(fxName(ff.f) + (ff.f.bypass ? ' · bypass' : t('d.act.active')));
}
/** FL-style: bypass every effect of an insert, or turn them all back on if all are bypassed. */
/** Effects that were on when "N FX" switched an insert off, so switching back restores them. */
const fxWasOn = new Map<Id, Set<Id>>();
/** Switch an effect to another built-in type in place (e.g. clean compressor ↔ FET); default preset. */
export function swapFxType(fxId: Id, type: AnyBuiltinKind) {
  const ff = findFx(fxId); if (!ff || !S.catalog || ff.f.type === type) return;
  const idx = ff.m.fx.findIndex(f => f.id === fxId);
  const f = newFx(S.catalog, allocId(), type, S.project.bpm);
  f.bypass = ff.f.bypass;
  commit([{ op: 'removeFx', fxId }, { op: 'putFx', insertId: ff.m.id, fx: f, index: idx }], t('d.act.op.comptype'));
  closePlug(fxId); openPlug(f.id);
  say(t('d.act.defaults', { name: tName(S.catalog.types[type]?.name ?? type) }));
}
export function toggleInsertFx(insId: Id) {
  const m = S.project.inserts.find(x => x.id === insId); if (!m || !m.fx.length) return;
  const on = m.fx.some(f => !f.bypass);
  if (on) fxWasOn.set(m.id, new Set(m.fx.filter(f => !f.bypass).map(f => f.id)));
  const keep = fxWasOn.get(m.id);
  const restore = !on && keep && m.fx.some(f => keep.has(f.id));
  commit(m.fx.map(f => fxOp({ ...plain(f), bypass: on ? true : restore ? !keep!.has(f.id) : false }, m.id)), on ? t('d.act.op.fxoff') : t('d.act.op.fxon'));
  if (!on) fxWasOn.delete(m.id);
  say(insName(m) + t(on ? 'd.act.fxoff' : 'd.act.fxon'));
}
export const fxName = (f: Fx) => f.type === 'vst3' ? (f.plugin?.name ?? 'VST3') : (S.catalog?.types[f.type]?.name ?? f.type);
export function loadPreset(fxId: Id, i: number) {
  const ff = findFx(fxId); if (!ff || !S.catalog) return;
  const list = presetList(S.catalog, ff.f.type); if (!list.length) return;
  const nf = loadPresetFx(S.catalog, plain(ff.f), i, S.project.bpm);
  commit([fxOp(nf, ff.m.id)], 'Preset');
  say(fxName(ff.f) + ' · ' + t('d.act.preset', { i: (nf.preset ?? 0) + 1, n: list.length, name: tName(list[nf.preset ?? 0].name) }));
}
export function stepPreset(fxId: Id, d: number) {
  const ff = findFx(fxId); if (!ff) return;
  const info = presetInfo(S.catalog, ff.f, S.project.bpm);
  loadPreset(fxId, info.idx < 0 ? (d > 0 ? 0 : -1) : info.idx + d);
}

// ---------- windows ----------
const PLUG_W: Partial<Record<string, number>> = { eq: 780, reverb: 640, vst3: 520, tune: 800, fet: 700, deess: 680, denoise: 640, voice: 700, delay: 760, warp: 820 };
/** Open (or bring to front) an effect window. The newest/most recently focused window is always on top. */
/** Playlist to the front: effect windows go behind it (they keep their place and state). */
export function playlistFront() { if ((U.plugs.length || U.rackWin || U.rollWin) && !U.plugsHidden) setU('plugsHidden', true); }
export function showPlugs() { setU('plugsHidden', false); }

export function openPlug(fxId: Id) {
  if (U.plugsHidden) setU('plugsHidden', false);
  const ex = U.plugs.find(p => p.fxId === fxId);
  if (ex) { plugFront(fxId, true); return; }
  const ty = findFx(fxId)?.f.type ?? '';
  const w = Math.min(PLUG_W[ty] ?? 580, window.innerWidth - 40), n = U.plugs.length;
  const z = U.zTop + 1;
  setU({ zTop: z, plugs: [...U.plugs, { fxId, x: clamp(140 + (n % 8) * 28, 10, window.innerWidth - w - 10), y: clamp(64 + (n % 8) * 28, 10, window.innerHeight - 200), w, z }] });
}
export const closePlug = (fxId: Id) => setU('plugs', U.plugs.filter(p => p.fxId !== fxId));
/** Raise a window above all others (array order stays put, so canvases are never re-created). */
export function plugFront(fxId: Id, force = false) {
  const p = U.plugs.find(x => x.fxId === fxId); if (!p) return;
  if (U.plugsHidden) setU('plugsHidden', false);
  if (!force && p.z === U.zTop) return;
  const z = U.zTop + 1;
  setU('zTop', z);
  setU('plugs', x => x.fxId === fxId, 'z', z);
}
export const topPlug = () => U.plugs.reduce<typeof U.plugs[number] | null>((a, p) => (!a || p.z > a.z ? p : a), null);
export function movePlug(fxId: Id, x: number, y: number) { setU('plugs', p => p.fxId === fxId, { x, y }); }
export function openEditor(id: Id) {
  const c = S.project.clips.find(x => x.id === id); if (!c || c.kind !== 'audio') return;
  if (U.previewing) void be().previewStop();
  const w = Math.min(780, window.innerWidth - 40), h = Math.min(420, window.innerHeight - 100);
  setU({ editor: { clipId: id, x: Math.max(20, (window.innerWidth - w) / 2), y: 64, w, h }, sel: [id] });
}
export function closeEditor() { if (U.previewing) void be().previewStop(); setU('editor', null); }
/** The note-based pitch editor of an audio clip. */
export function openPitchEditor(id: Id) {
  const c = S.project.clips.find(x => x.id === id); if (!c || c.kind !== 'audio') return;
  const w = Math.min(1100, window.innerWidth - 40), h = Math.min(560, window.innerHeight - 90);
  setU({ pitchEd: { clipId: id, x: Math.max(20, (window.innerWidth - w) / 2), y: Math.max(44, (window.innerHeight - h) / 2 - 20), w, h }, sel: [id] });
}
export const closePitchEditor = () => setU('pitchEd', null);

export function openMusicAnalysis(args: { clipId?: Id; fxId?: Id }) {
  setU('musicWin', args);
}
export const closeMusicAnalysis = () => setU('musicWin', null);

/** Set project BPM and scale `clipId`'s length/offset so it still covers the same seconds of audio. */
export function applyDetectedTempo(bpm: number, clipId?: Id) {
  const nb = clamp(Math.round(bpm * 100) / 100, 20, 999);
  const oldBpm = S.project.bpm;
  const ops: Op[] = [{ op: 'setBpm', bpm: nb }];
  if (clipId != null && Math.abs(nb - oldBpm) > 1e-6) {
    const c = S.project.clips.find(x => x.id === clipId);
    if (c && c.kind === 'audio') {
      const ratio = nb / oldBpm;
      const nc: Clip = {
        ...plain(c),
        offset: c.offset * ratio,
        len: clamp(c.len * ratio, 1 / 16, TOTAL - c.start),
      };
      ops.push({ op: 'putClips', clips: [nc] });
    }
  }
  commit(ops, t('d.act.op.detected'));
  say(t('d.act.tempoSet', { bpm: nb.toFixed(2) }));
}

/** Apply detected key/scale (1 = Major, 2 = Minor) to one Pitch Correct instance or all of them in the project. */
export function applyKeyToTune(root: number, minor: boolean, targetFxId?: Id | 'all') {
  const scale = minor ? 2 : 1;
  const ops: Op[] = [];
  for (const ins of S.project.inserts) {
    for (const f of ins.fx) {
      if (f.type !== 'tune') continue;
      if (targetFxId != null && targetFxId !== 'all' && f.id !== targetFxId) continue;
      ops.push(fxOp({ ...plain(f), params: { ...plain(f.params), key: root, scale } }, ins.id));
    }
  }
  if (!ops.length) return;
  commit(ops, t('d.act.op.pckey'));
  const names = ['C', 'C#', 'D', 'D#', 'E', 'F', 'F#', 'G', 'G#', 'A', 'A#', 'B'];
  say(t('d.act.pckey', { key: names[root % 12], mode: t(minor ? 'd.act.minor' : 'd.act.major') }) + (ops.length > 1 ? t('d.act.inst', { n: ops.length }) : ''));
}

// ---------- view ----------
/** Playlist zoom limits, px per beat. */
export const BW_MIN = 4, BW_MAX = 96;
export function zoomAt(f: number) { setU('bw', clamp(U.bw * f, BW_MIN, BW_MAX)); }
export function setSnap(v: number) {
  setU('snap', v);
  say(v < 0 ? t('d.act.snapauto', { name: snapName(snapRes(-1, U.bw)) }) : t('d.act.snap', { name: snapName(v) }));
}

// ---------- auto vocal ----------
let autoVocalBusy = false;
/** Analyse an audio clip and put the calibrated vocal chain on its track's insert (one undo step).
 *  Effects of the types the chain brings are replaced; others (e.g. an EQ) stay in front. */
export async function autoVocal(clipId: Id, style: 'natural' | 'hard' = 'natural') {
  const c = S.project.clips.find(x => x.id === clipId);
  if (!c || c.kind !== 'audio' || !S.catalog || autoVocalBusy) return;
  const tr = S.project.tracks[c.track]; if (!tr) return;
  const ins = S.project.inserts.find(m => m.id === tr.insert);
  if (!ins || ins.id === 0) { say(t('d.act.av.assign')); return; }
  autoVocalBusy = true;
  say(t('d.act.av.analyze'));
  try {
    // the rider (≈1.6 s look-ahead) is baked into the audio instead of living on the insert,
    // so playback and monitoring get no extra delay; a clip that was baked before is not baked twice
    const src = S.project.sources.find(x => x.id === c.ref);
    const baked = !!src && / \(rider\)$/.test(src.name);
    const plan = await be().autoVocalPlan({ clipId, style, bakeRider: true });
    if (plan.riderBake && !baked) {
      say(t('d.act.av.level'));
      await enqueue(() => be().bakeRider({ clipId, params: plan.riderBake!.params }));
      await resync();
    }
    // The chain leaves the vocal far under the limiter; bring it up to mix level in the limiter
    // (its gain + −1 dBTP ceiling): to sit under the beat when the plan knows it, else −18 LUFS.
    const makeup = Math.round(clamp(plan.trackGainDb ?? (-18 - plan.stages.lufsAfter), 0, 24) * 10) / 10;
    const types = new Set<string>(plan.chain.map(p => p.type));
    types.add('rider'); // an old live rider would ride the vocal a second time
    const ops: Op[] = ins.fx.filter(f => types.has(f.type)).map(f => ({ op: 'removeFx', fxId: f.id }) as Op);
    for (const p of plan.chain) {
      const f = newFx(S.catalog, allocId(), p.type, S.project.bpm);
      f.params = { ...f.params, ...p.params };
      f.mix = p.mix;
      f.preset = null; // calibrated values, not a preset (the header then reads "Własne ustawienia")
      f.bypass = !p.enabled;
      if (p.type === 'limit') f.params.gain = clamp((f.params.gain ?? 0) + makeup, 0, 24);
      ops.push({ op: 'putFx', insertId: ins.id, fx: f });
    }
    commit(ops, t('d.act.op.av'));
    setU({ mixSel: ins.id });
    const a = plan.analysis, st = plan.stages;
    say(t(plan.riderBake ? 'd.act.av.done.rb' : 'd.act.av.done', { n: plan.chain.filter(p => p.enabled).length, name: insName(ins) })
      + t('d.act.av.spread', { a: st.iqr400Db[0].toFixed(1), b: st.iqr400Db[1].toFixed(1) })
      + (a.key ? t('d.act.av.key', { key: a.key.name }) : t('d.act.av.nokey'))
      + (plan.warnings.length ? ' · ' + plan.warnings[0] : ''));
  } catch (e) { say(t('d.act.av.fail', { err: String(e) })); }
  autoVocalBusy = false;
}

/** Auto vocal on a comp: flatten it to one audio clip first (Ctrl+Z undoes both steps). */
export async function autoVocalComp(clipId: Id, style: 'natural' | 'hard' = 'natural') {
  const c = S.project.clips.find(x => x.id === clipId); if (!c || c.kind !== 'comp') return;
  const { track, start } = c;
  say(t('d.act.av.flatten'));
  try { await enqueue(() => be().flattenComp(clipId)); } catch (e) { say(t('d.act.av.flatfail', { err: String(e) })); return; }
  const a = S.project.clips.filter(x => x.kind === 'audio' && x.track === track && Math.abs(x.start - start) < 1e-6)[0]
    ?? S.project.clips.filter(x => x.kind === 'audio' && x.track === track).sort((p, q) => Math.abs(p.start - start) - Math.abs(q.start - start))[0];
  if (!a) { say(t('d.act.av.noflat')); return; }
  await autoVocal(a.id, style);
}
