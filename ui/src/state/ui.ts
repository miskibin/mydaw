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
  /** automation clip whose envelope is being edited inline on the playlist */
  autoEdit: Id | null;
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
  ghost: null, dragOver: false, eqSel: {}, fetMode: {}, pitchEd: null, pat: 1, selCh: null, rackWin: null, rollWin: null, rackGraph: null, patPlaying: false, takesOpen: {}, compSel: null, autoEdit: null, zTop: 1, marquee: null,
  playing: false, previewing: false, previewPath: null,
});

// Until the first status message the readout follows the language (a live switch re-translates "Ready").
let readoutPristine = true;
createRoot(() => createEffect(() => { const ready = t('d.ui.ready'); if (readoutPristine) setU('readout', ready); }));
export const say = (msg: string) => { readoutPristine = false; setU('readout', msg); };
export const closeMenu = () => { if (U.menu) setU('menu', null); };
export function menuPos(x: number, y: number, h: number) {
  return { x: Math.max(4, Math.min(x, window.innerWidth - 240)), y: Math.max(4, Math.min(y, window.innerHeight - h)) };
}
export function openMenu(x: number, y: number, items: MenuItem[], h = 300) {
  setU('menu', { ...menuPos(x, y, h), items });
}
export function openRename(x: number, y: number, value: string, done: (v: string) => void) {
  openMenu(x, y, [{ t: 'title', label: 'Nazwa — Enter zatwierdza' }, { t: 'rename', value, done }], 80);
}
