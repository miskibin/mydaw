// UI-only state (not saved, not undone).
import { createEffect, createRoot } from 'solid-js';
import { createStore } from 'solid-js/store';
import { t } from '../lib/i18n';
import type { ClipKind, Id } from '../api/types';
import type { MenuGlyph } from '../ui/menuIcons';

export type Tool = 'draw' | 'paint' | 'erase' | 'mute' | 'slice' | 'select';

/** `z`: stacking order — the most recently opened/focused window has the highest. */
export interface PlugWin { fxId: Id; x: number; y: number; w: number; z: number }
export interface PitchWin { clipId: Id; x: number; y: number; w: number; h: number }
export interface EditorWin { clipId: Id; x: number; y: number; w: number; h: number }
export interface MusicWin { clipId?: Id; fxId?: Id }
/**
 * A pattern window: the Channel Rack (F6) or the Piano roll (F7). Both live in the effect-window
 * layer (F5 / a playlist click hide them with the effect windows, "Okna" brings them back) and
 * both show the current pattern (`UiState.pat`); the piano roll edits the selected channel.
 */
export interface PatWin { x: number; y: number; w: number; h: number; z: number }

export type MenuItem =
  | { t: 'title'; label: string }
  | { t: 'sep' }
  /** Small section heading inside a menu. */
  | { t: 'sec'; label: string }
  /** Opens a nested menu on hover. `glyph` overrides the icon guessed from the label. */
  | { t: 'sub'; label: string; items: MenuItem[]; glyph?: MenuGlyph }
  /** `icon` (24×24 stroke path in a coloured tile) wins over `glyph`, which wins over the label guess. */
  | { t: 'item'; label: string; kbd?: string; act: () => void; danger?: boolean; disabled?: boolean; icon?: string; iconColor?: string; title?: string; glyph?: MenuGlyph;
      /** A toggle: shows a check mark in the icon slot while on (and nothing while off). */
      checked?: boolean }
  | { t: 'swatches'; current?: string; pick: (c: string) => void }
  /** Row of instrument icons; picking the current one clears it. */
  | { t: 'icons'; current?: string | null; color?: string; pick: (id: string | null) => void }
  | { t: 'gain'; clipId: Id }
  | { t: 'rename'; value: string; done: (v: string) => void }
  /** A row of small toggle buttons (one click picks, menu stays coherent with the current value). */
  | { t: 'chips'; label?: string; options: string[]; active: number; pick: (i: number) => void; disabled?: boolean; close?: boolean };

export interface Menu { x: number; y: number; items: MenuItem[] }

export interface UiState {
  tool: Tool; snap: number; bw: number;
  sel: Id[]; cur: { kind: ClipKind; ref: Id } | null;
  start: number; follow: boolean; alt: boolean;
  mixOpen: boolean; mixMin: boolean; mixH: number; mixSel: Id; fxSel: Id | null; fxDrag: Id | null;
  plugs: PlugWin[];
  /** effect windows tucked behind the playlist (FL-style: clicking the playlist brings it to the front) */
  plugsHidden: boolean;
  editor: EditorWin | null; menu: Menu | null; help: boolean; settings: boolean; exportOpen: boolean;
  /** True while `exportAudio` is in flight (so Escape can abort and report). */
  exportBusy: boolean;
  vstPicker: { insId: Id } | null;
  /** add-effect picker, anchored at (x, y) (−1 = centred) */
  fxPicker: { insId: Id; x: number; y: number } | null;
  musicWin: MusicWin | null;
  readout: string;
  headW: number; pickW: number; pickOpen: boolean; browserTab: 'project' | 'library';
  ghost: { x: number; y: number; name: string; color: string } | null;
  dragOver: boolean;
  eqSel: Record<number, number>;
  fetMode: Record<number, 'gr' | 'out'>;
  pitchEd: PitchWin | null;
  /** The current pattern (FL's pattern selector): what the Channel Rack, the Piano roll and PAT mode show. */
  pat: Id | null;
  /** The selected Channel Rack channel: the Piano roll's channel, the graph editor's, the typing keyboard's. */
  selCh: Id | null;
  rackWin: PatWin | null;
  rollWin: PatWin | null;
  /** The Channel Rack's graph editor (per-step velocity / key of the selected channel) is shown. */
  rackGraph: 'vel' | 'key' | null;
  /** True while the engine loops a pattern on its own (telemetry `patBeat` ≥ 0). */
  patPlaying: boolean;
  /** track id → take lanes expanded */
  takesOpen: Record<number, boolean>;
  /** comp time selection (comp beats): the last swiped range, the target of Ctrl+↑/↓ */
  compSel: { clipId: Id; from: number; to: number } | null;
  /** selected points of one automation clip's envelope (indices into its points) */
  autoSel: { clip: Id; idx: number[] } | null;
  /** next z for plugin windows (monotonic) */
  zTop: number;
  marquee: { x0: number; y0: number; x1: number; y1: number } | null;
  /** True while the engine reports playback (from telemetry flags / transport replies). */
  playing: boolean;
  previewing: boolean;
  previewPath: string | null;
}

export const [U, setU] = createStore<UiState>({
  tool: 'draw', snap: -1, bw: 22, sel: [], cur: { kind: 'pat', ref: 1 },
  start: 0, follow: true, alt: false,
  mixOpen: true, mixMin: false, mixH: 300, mixSel: 6, fxSel: 903, fxDrag: null,
  plugs: [], plugsHidden: false, editor: null, menu: null, help: false, settings: false, exportOpen: false, exportBusy: false, vstPicker: null, fxPicker: null, musicWin: null,
  readout: t('d.ui.ready'),
  headW: 196, pickW: 220, pickOpen: true, browserTab: 'project',
  ghost: null, dragOver: false, eqSel: {}, fetMode: {}, pitchEd: null, pat: 1, selCh: null, rackWin: null, rollWin: null, rackGraph: null, patPlaying: false, takesOpen: {}, compSel: null, autoSel: null, zTop: 1, marquee: null,
  playing: false, previewing: false, previewPath: null,
});

// Until the first status message the readout follows the language (a live switch re-translates "Ready").
let readoutPristine = true;
createRoot(() => createEffect(() => { const ready = t('d.ui.ready'); if (readoutPristine) setU('readout', ready); }));

// The working set-up survives a restart: tool, snap, zoom, follow and the panel layout used to
// reset on every launch (the mixer reopened, the browser went back to the Project tab, ...).
const LAYOUT_KEY = 'wave.layout';
const TOOLS_OK: Tool[] = ['draw', 'paint', 'erase', 'mute', 'slice', 'select'];
const num = (v: unknown, lo: number, hi: number): number | undefined => (typeof v === 'number' && isFinite(v) && v >= lo && v <= hi ? v : undefined);
const bool = (v: unknown): boolean | undefined => (typeof v === 'boolean' ? v : undefined);
/** A saved layout as a patch of valid fields only (pure: tested). */
export function parseLayout(raw: string | null): Partial<UiState> {
  let v: Record<string, unknown>;
  try { v = JSON.parse(raw ?? '{}') ?? {}; } catch { return {}; }
  if (typeof v !== 'object') return {};
  const p: Partial<UiState> = {
    tool: TOOLS_OK.includes(v.tool as Tool) ? (v.tool as Tool) : undefined,
    snap: num(v.snap, -1, 64), bw: num(v.bw, 4, 96), follow: bool(v.follow),
    mixOpen: bool(v.mixOpen), mixMin: bool(v.mixMin), mixH: num(v.mixH, 100, 2000),
    headW: num(v.headW, 120, 600), pickW: num(v.pickW, 140, 800), pickOpen: bool(v.pickOpen),
    browserTab: v.browserTab === 'project' || v.browserTab === 'library' ? v.browserTab : undefined,
  };
  for (const k of Object.keys(p) as (keyof UiState)[]) if (p[k] === undefined) delete p[k];
  return p;
}
try { setU(parseLayout(localStorage.getItem(LAYOUT_KEY))); } catch { /* private window */ }
createRoot(() => createEffect(() => {
  const s = JSON.stringify({ tool: U.tool, snap: U.snap, bw: U.bw, follow: U.follow, mixOpen: U.mixOpen, mixMin: U.mixMin, mixH: U.mixH, headW: U.headW, pickW: U.pickW, pickOpen: U.pickOpen, browserTab: U.browserTab });
  try { localStorage.setItem(LAYOUT_KEY, s); } catch { /* private window */ }
}));
export const say = (msg: string) => { readoutPristine = false; setU('readout', msg); };
export const closeMenu = () => { if (U.menu) setU('menu', null); };
export function menuPos(x: number, y: number, h: number) {
  return { x: Math.max(4, Math.min(x, window.innerWidth - 240)), y: Math.max(4, Math.min(y, window.innerHeight - h)) };
}
export function openMenu(x: number, y: number, items: MenuItem[], h = 300) {
  setU('menu', { ...menuPos(x, y, h), items });
}
export function openRename(x: number, y: number, value: string, done: (v: string) => void, title = t('d.ui.renameTitle')) {
  openMenu(x, y, [{ t: 'title', label: title }, { t: 'rename', value, done }], 80);
}
