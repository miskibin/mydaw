import { createEffect, createRoot, createSignal } from 'solid-js';
import { be } from '../api/backend';
import type { Id, MidiStatus } from '../api/types';
import { U } from './ui';

/**
 * MIDI keyboard state for the UI: status from the backend, a short activity flash on every key,
 * and the target channel. "Follow" (default) sends the keyboard to the channel selected in the
 * Channel Rack / piano roll; a fixed channel can be chosen in Settings instead.
 */
const [status, setStatus] = createSignal<MidiStatus | null>(null);
const [flash, setFlash] = createSignal(false);
const FOLLOW_KEY = 'wave.midiFollow';
const readFollow = () => { try { return localStorage.getItem(FOLLOW_KEY) !== '0'; } catch { return true; } };
const [follow, setFollowSig] = createSignal(readFollow());

export const midiStatus = status;
export const midiFlash = flash;
export const midiFollow = follow;

let started = false;
let flashTimer: ReturnType<typeof setTimeout> | undefined;

/** Call once at app start: loads status and listens for device and note events. */
export function startMidi() {
  if (started) return; started = true;
  be().midiStatus().then(setStatus).catch(() => {});
  void be().on('midi-status', setStatus);
  void be().on('midi-note', ev => {
    setStatus(s => (s ? { ...s, lastNote: ev.note, noteCount: ev.count } : s));
    if (ev.note.on) { setFlash(true); clearTimeout(flashTimer); flashTimer = setTimeout(() => setFlash(false), 120); }
  });
  createRoot(() => createEffect(() => {
    const ch = U.patWin?.ch ?? null;
    if (follow() && ch != null && status()?.target !== ch) void setMidiTarget(ch);
  }));
}

export async function selectMidiDevice(id: string | null) { setStatus(await be().midiSelectDevice(id)); }
export async function setMidiTarget(ch: Id | null) { setStatus(await be().midiSetTarget(ch)); }
export function setMidiFollow(on: boolean) {
  setFollowSig(on);
  try { localStorage.setItem(FOLLOW_KEY, on ? '1' : '0'); } catch { /* private window */ }
}

const NAMES = ['C', 'C#', 'D', 'D#', 'E', 'F', 'F#', 'G', 'G#', 'A', 'A#', 'B'];
/** MIDI key → "C4" (60 = C4, as in the piano roll). */
export const keyName = (k: number) => NAMES[k % 12] + (Math.floor(k / 12) - 1);
