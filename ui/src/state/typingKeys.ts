// The computer keyboard as a piano (FL Studio "Typing keyboard to piano keyboard", Ctrl+T).
// Two rows: Z S X D C V G B H N J M , L . ; / from the base C (C4 by default) and
// Q 2 W 3 E R 5 T 6 Y 7 U I 9 O 0 P [ = ] an octave up (Q = C5). Physical keys (event.code),
// so the layout does not matter. While it is on, those keys play instead of their shortcuts;
// PageDown / PageUp move the octave.
import { createSignal } from 'solid-js';
import { be } from '../api/backend';
import type { Id } from '../api/types';
import { t } from '../lib/i18n';
import { noteName } from '../lib/pitch';
import { say } from './ui';

const LOW = ['KeyZ', 'KeyS', 'KeyX', 'KeyD', 'KeyC', 'KeyV', 'KeyG', 'KeyB', 'KeyH', 'KeyN', 'KeyJ', 'KeyM', 'Comma', 'KeyL', 'Period', 'Semicolon', 'Slash'];
const HIGH = ['KeyQ', 'Digit2', 'KeyW', 'Digit3', 'KeyE', 'KeyR', 'Digit5', 'KeyT', 'Digit6', 'KeyY', 'Digit7', 'KeyU', 'KeyI', 'Digit9', 'KeyO', 'Digit0', 'KeyP', 'BracketLeft', 'Equal', 'BracketRight'];
const OFFSET = new Map<string, number>([...LOW.map((c, i) => [c, i] as const), ...HIGH.map((c, i) => [c, 12 + i] as const)]);
const KEY = 'wave.typingKeys';

function load(): { on: boolean; base: number } {
  try { const v = JSON.parse(localStorage.getItem(KEY) ?? '{}'); return { on: !!v.on, base: typeof v.base === 'number' ? v.base : 60 }; } catch { return { on: false, base: 60 }; }
}
const init = load();
const [typingOn, setOn] = createSignal(init.on);
const [typingBase, setBase] = createSignal(init.base);
/** Keys sounding right now (MIDI numbers), for the piano roll's keyboard. */
const [typingHeld, setHeld] = createSignal<number[]>([]);
export { typingBase, typingHeld, typingOn };

const save = () => { try { localStorage.setItem(KEY, JSON.stringify({ on: typingOn(), base: typingBase() })); } catch { /* private window */ } };
const held = new Map<string, { ch: Id; key: number }>();
const syncHeld = () => setHeld([...held.values()].map(h => h.key));

export const baseText = () => noteName(typingBase()) + '–' + noteName(typingBase() + 31);

export function setTypingOn(on: boolean) {
  if (!on) releaseAll();
  setOn(on); save();
  say(on ? t('b.tk.on', { z: noteName(typingBase()), q: noteName(typingBase() + 12) })
    : t('b.tk.off'));
}
export function shiftOctave(d: number) {
  const b = Math.min(96, Math.max(12, typingBase() + 12 * d));
  if (b === typingBase()) return;
  releaseAll(); setBase(b); save();
  say(t('b.tk.shift', { z: noteName(b), q: noteName(b + 12) }));
}

/** True when the key is a piano key in typing mode (then shortcuts must leave it alone). */
export function isTypingKey(e: KeyboardEvent): boolean {
  return typingOn() && !e.ctrlKey && !e.metaKey && !e.altKey && (OFFSET.has(e.code) || e.code === 'PageUp' || e.code === 'PageDown');
}

/** Key down: plays on `ch` (from `target`). Returns true when the key was taken. */
export function typingDown(e: KeyboardEvent, target: () => Id | null): boolean {
  if (!isTypingKey(e)) return false;
  e.preventDefault();
  if (e.code === 'PageUp' || e.code === 'PageDown') { if (!e.repeat) shiftOctave(e.code === 'PageUp' ? 1 : -1); return true; }
  if (e.repeat || held.has(e.code)) return true;
  const ch = target();
  if (ch == null) { say(t('b.tk.noCh')); return true; }
  const key = Math.min(127, typingBase() + OFFSET.get(e.code)!);
  held.set(e.code, { ch, key }); syncHeld();
  be().typingNote({ ch, key, vel: e.shiftKey ? 1 : 0.8, on: true }).catch(err => console.warn(err));
  return true;
}
export function typingUp(e: KeyboardEvent) {
  const h = held.get(e.code); if (!h) return;
  held.delete(e.code); syncHeld();
  be().typingNote({ ch: h.ch, key: h.key, on: false }).catch(err => console.warn(err));
}
/** Window lost focus / mode off: no key stays stuck. */
export function releaseAll() {
  for (const h of held.values()) be().typingNote({ ch: h.ch, key: h.key, on: false }).catch(() => {});
  held.clear(); syncHeld();
}
