// Sending edits to the engine: discrete commits, drag gestures (transient ops throttled to
// animation frames), and the rules for accepting `state` events while edits are in flight.
//
// Every mutating call goes through one serial queue: Tauri runs async commands concurrently,
// and begin_gesture / apply_ops / end_gesture must reach the engine in the order we sent them.
import { be } from '../api/backend';
import type { Clip, Op, StateDto } from '../api/types';
import { setGestureProbe } from './peaks';
import { applyLocal, replaceState, S } from './store';
import { say } from './ui';

let inflight = 0;          // queued/unanswered mutating requests
let gestures = 0;          // gestures between begin and end
let lastReplyRev = 0;      // newest revision the engine reported for one of *our* batches
let stash: StateDto | null = null;

/** True while a drag gesture is open (begin_gesture sent, end_gesture not yet answered). */
export const inGesture = () => gestures > 0;
setGestureProbe(inGesture);

let queue: Promise<unknown> = Promise.resolve();
/** Run `fn` after every previously queued engine call has finished. */
export function enqueue<T>(fn: () => Promise<T>): Promise<T> {
  inflight++;
  const p = queue.then(fn, fn);
  queue = p.catch(() => {}).finally(() => { inflight--; settle(); });
  return p;
}

/** Called for every `state` event. */
export function onStateEvent(dto: StateDto) {
  if (inflight > 0 || gestures > 0) {
    if (!stash || dto.revision >= stash.revision) stash = dto;
    return;
  }
  if (dto.revision < lastReplyRev) return; // older than our last edit
  replaceState(dto);
}

function settle() {
  if (inflight > 0 || gestures > 0 || !stash) return;
  const s = stash; stash = null;
  if (s.revision >= lastReplyRev && s.revision >= S.revision) replaceState(s);
}

const noteRev = (r: unknown) => { const v = (r as { revision?: number } | null)?.revision; if (typeof v === 'number') lastReplyRev = Math.max(lastReplyRev, v); };

/** A reply that carries a whole state (undo/redo/new/open) always wins. */
export function acceptReply(dto: StateDto) {
  lastReplyRev = dto.revision; stash = null;
  replaceState(dto);
}

export async function resync(err?: unknown) {
  if (err) { console.warn('[wave] edit rejected:', err); say('Błąd: ' + String((err as Error)?.message ?? err)); }
  try { const dto = await be().getState(); lastReplyRev = dto.revision; stash = null; replaceState(dto); } catch (e) { console.error(e); }
}

/** One undo step: apply locally, send to the engine. */
export function commit(ops: Op[], label: string) {
  if (!ops.length) return;
  endWheelGestures();
  applyLocal(ops);
  enqueue(() => be().applyOps(ops, { label })).then(noteRev, e => resync(e));
}

// ---- gestures ----

function opKey(op: Op): string {
  switch (op.op) {
    case 'putTrack': return 't' + op.track.id;
    case 'putInsert': return 'i' + op.insert.id;
    case 'putFx': return 'f' + op.fx.id;
    case 'moveFx': return 'm' + op.fxId;
    case 'putPattern': return 'p' + op.pattern.id;
    case 'putSource': return 's' + op.source.id;
    case 'setBpm': return 'bpm';
    case 'setLoop': return 'loop';
    case 'putAutomation': return 'a' + op.automation.id;
    default: return op.op;
  }
}

export class Gesture {
  private putClips = new Map<number, Clip>();
  private removed = new Set<number>();
  private other = new Map<string, Op>();
  private order: string[] = [];
  private scheduled = false;
  private busy = false;
  private done = false;
  changed = false;

  constructor(private label: string, wheel = false) {
    if (!wheel) endWheelGestures();
    gestures++;
    enqueue(() => be().beginGesture(label)).catch(e => console.warn(e));
  }

  /** Apply locally now; coalesce and send at the next animation frame. */
  update(ops: Op[]) {
    if (this.done || !ops.length) return;
    applyLocal(ops);
    this.changed = true;
    for (const op of ops) {
      if (op.op === 'putClips') { for (const c of op.clips) { this.putClips.set(c.id, c); this.removed.delete(c.id); } this.mark('clips'); }
      else if (op.op === 'removeClips') { for (const id of op.ids) { this.removed.add(id); this.putClips.delete(id); } this.mark('remove'); }
      else { this.other.set(opKey(op), op); this.mark(opKey(op)); }
    }
    if (!this.scheduled) {
      this.scheduled = true;
      requestAnimationFrame(() => { this.scheduled = false; this.flush(); });
    }
  }
  private mark(k: string) { if (!this.order.includes(k)) this.order.push(k); }
  private take(): Op[] {
    const ops: Op[] = [];
    for (const k of this.order) {
      if (k === 'clips') { if (this.putClips.size) ops.push({ op: 'putClips', clips: [...this.putClips.values()] }); }
      else if (k === 'remove') { if (this.removed.size) ops.push({ op: 'removeClips', ids: [...this.removed] }); }
      else { const op = this.other.get(k); if (op) ops.push(op); }
    }
    this.putClips.clear(); this.removed.clear(); this.other.clear(); this.order = [];
    return ops;
  }
  private flush() {
    if (this.busy || this.done) return; // a pending reply flushes again; end() sends the rest
    const ops = this.take();
    if (!ops.length) return;
    this.busy = true;
    enqueue(() => be().applyOps(ops, { label: this.label, transient: true }))
      .then(noteRev, e => console.warn('[wave] transient op rejected', e))
      .finally(() => { this.busy = false; if (this.order.length) this.flush(); });
  }

  /** Flush what is left and close the undo step. */
  end(label?: string) {
    if (this.done) return;
    this.done = true;
    const ops = this.take();
    if (ops.length) enqueue(() => be().applyOps(ops, { label: this.label, transient: true })).then(noteRev, e => console.warn(e));
    enqueue(() => be().endGesture(label ?? this.label)).catch(e => console.warn(e)).finally(() => { gestures--; settle(); });
  }
}

/** Wheel edits: a gesture that closes itself after the wheel goes quiet. */
const wheelG = new Map<string, { g: Gesture; t: number }>();
export function wheelEdit(key: string, label: string, ops: Op[]) {
  let w = wheelG.get(key);
  if (!w) {
    for (const k of [...wheelG.keys()]) if (k !== key) closeWheel(k);
    w = { g: new Gesture(label, true), t: 0 }; wheelG.set(key, w);
  }
  w.g.update(ops);
  clearTimeout(w.t);
  w.t = window.setTimeout(() => closeWheel(key), 450);
}
function closeWheel(key: string) {
  const w = wheelG.get(key); if (!w) return;
  clearTimeout(w.t); wheelG.delete(key); w.g.end();
}
/** Gestures do not nest in the engine: close any open wheel gesture first. */
export function endWheelGestures() { for (const k of [...wheelG.keys()]) closeWheel(k); }
