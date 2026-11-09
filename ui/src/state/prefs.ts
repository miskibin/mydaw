// User preferences kept on this computer (localStorage), applied live. The theme lives in
// ui/theme.ts; audio and MIDI devices are saved by the engine.
import { createSignal } from 'solid-js';
import { isTauri } from '../api/backend';
import * as A from './actions';
import { S } from './store';

export interface Prefs {
  /** interface zoom, 0.8–1.5 */
  uiScale: number;
  /** instrument icons on playlist clips */
  clipIcons: boolean;
  /** play notes while drawing / clicking them in the step sequencer and piano roll */
  noteAudition: boolean;
  /** autosave interval in minutes (0 = off); only projects that were saved once */
  autosaveMin: number;
  /** show hints in the status bar */
  hints: boolean;
}

const KEY = 'wave.prefs';
const DEFAULTS: Prefs = { uiScale: 1, clipIcons: true, noteAudition: false, autosaveMin: 0, hints: true };

function load(): Prefs {
  try { const v = JSON.parse(localStorage.getItem(KEY) ?? '{}'); return { ...DEFAULTS, ...v }; } catch { return { ...DEFAULTS }; }
}

const [prefs, setPrefsSig] = createSignal<Prefs>(DEFAULTS);
export { prefs };

export function setPref<K extends keyof Prefs>(k: K, v: Prefs[K]) {
  const next = { ...prefs(), [k]: v };
  setPrefsSig(next);
  try { localStorage.setItem(KEY, JSON.stringify(next)); } catch { /* private window */ }
  applyPref(k, next);
}

export function resetPrefs() {
  setPrefsSig({ ...DEFAULTS });
  try { localStorage.removeItem(KEY); } catch { /* ignore */ }
  for (const k of Object.keys(DEFAULTS) as (keyof Prefs)[]) applyPref(k, DEFAULTS);
}

let timer: ReturnType<typeof setInterval> | null = null;

function applyPref(k: keyof Prefs, p: Prefs) {
  if (k === 'uiScale') void applyScale(p.uiScale);
  if (k === 'autosaveMin') {
    if (timer) clearInterval(timer);
    timer = null;
    if (p.autosaveMin > 0) timer = setInterval(() => { if (S.path && S.dirty) void A.projectSave(false); }, p.autosaveMin * 60_000);
  }
}

async function applyScale(z: number) {
  if (isTauri()) {
    try { const { getCurrentWebview } = await import('@tauri-apps/api/webview'); await getCurrentWebview().setZoom(z); return; } catch { /* fall back to CSS */ }
  }
  (document.documentElement.style as CSSStyleDeclaration & { zoom: string }).zoom = z === 1 ? '' : String(z);
}

/** Load and apply saved preferences. Call once at startup. */
export function initPrefs() {
  const p = load();
  setPrefsSig(p);
  for (const k of Object.keys(p) as (keyof Prefs)[]) if (p[k] !== DEFAULTS[k]) applyPref(k, p);
}
