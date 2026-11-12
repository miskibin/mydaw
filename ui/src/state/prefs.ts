// User preferences kept on this computer (localStorage), applied live. The theme lives in
// ui/theme.ts; audio and MIDI devices are saved by the engine.
import { createSignal } from 'solid-js';
import { setLang, type Lang } from '../lib/i18n';
import { isTauri } from '../api/backend';
import * as A from './actions';
import { S } from './store';

export interface Prefs {
  /** interface language */
  lang: Lang;
  /** interface zoom, 0.8–1.5 */
  uiScale: number;
  /** instrument icons on playlist clips */
  clipIcons: boolean;
  /** play notes while drawing / clicking them in the step sequencer and piano roll */
  noteAudition: boolean;
  /** autosave interval in minutes (0 = off): a saved project is saved to its file, one that
   *  was never saved gets a crash-recovery copy (app/src/recovery.rs) */
  autosaveMin: number;
  /** show the tool hint in the toolbar's hint line */
  hints: boolean;
}

const KEY = 'wave.prefs';
const DEFAULTS: Prefs = { lang: 'en', uiScale: 1, clipIcons: true, noteAudition: true, autosaveMin: 2, hints: true };

function load(): Prefs {
  try { const v = JSON.parse(localStorage.getItem(KEY) ?? '{}'); return { ...DEFAULTS, ...v, lang: v.lang === 'pl' ? 'pl' : 'en' }; } catch { return { ...DEFAULTS }; }
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
  if (k === 'lang') setLang(p.lang);
  if (k === 'uiScale') void applyScale(p.uiScale);
  if (k === 'autosaveMin') {
    if (timer) clearInterval(timer);
    timer = null;
    if (p.autosaveMin > 0) timer = setInterval(() => {
      if (!S.path) void A.recoveryTick(); // also drops a stale copy once nothing is unsaved
      else if (S.dirty) void A.projectSave(false);
    }, p.autosaveMin * 60_000);
  }
}

async function applyScale(z: number) {
  if (isTauri()) {
    try { const { getCurrentWebview } = await import('@tauri-apps/api/webview'); await getCurrentWebview().setZoom(z); return; } catch { /* fall back to CSS */ }
  }
  // browser (and a webview that refuses setZoom): CSS zoom. 100vh is measured before the zoom,
  // so the app would be z times the window's height and its bottom (the mixer) cut off: --ui-zoom
  // lets the layout divide it back out.
  const st = document.documentElement.style as CSSStyleDeclaration & { zoom: string };
  st.zoom = z === 1 ? '' : String(z);
  if (z === 1) st.removeProperty('--ui-zoom'); else st.setProperty('--ui-zoom', String(z));
}

/** Load and apply saved preferences. Call once at startup. */
export function initPrefs() {
  const p = load();
  setPrefsSig(p);
  setLang(p.lang);
  for (const k of Object.keys(p) as (keyof Prefs)[]) if (p[k] !== DEFAULTS[k]) applyPref(k, p);
  // autosave is a timer, not a value: it must start at the default interval too (it never ran
  // until the setting was changed once)
  if (p.autosaveMin === DEFAULTS.autosaveMin) applyPref('autosaveMin', p);
}
