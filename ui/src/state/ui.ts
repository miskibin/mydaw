// UI-only state (not saved, not undone).
import { createStore } from 'solid-js/store';
import type { ClipKind, Id } from '../api/types';

export type Tool = 'draw' | 'paint' | 'erase' | 'mute' | 'slice' | 'select';

/** `z`: stacking order — the most recently opened/focused window has the highest. */
export interface PlugWin { fxId: Id; x: number; y: number; w: number; z: number }
export interface PitchWin { clipId: Id; x: number; y: number; w: number; h: number }
export interface EditorWin { clipId: Id; x: number; y: number; w: number; h: number }
export interface MusicWin { clipId?: Id; fxId?: Id }

export type MenuItem =
  | { t: 'title'; label: string }
  | { t: 'sep' }
  | { t: 'item'; label: string; kbd?: string; act: () => void; danger?: boolean; disabled?: boolean; icon?: string; iconColor?: string; title?: string }
  | { t: 'swatches'; current?: string; pick: (c: string) => void }
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
  musicWin: MusicWin | null;
  readout: string;
  headW: number; pickW: number; pickOpen: boolean; browserTab: 'project' | 'library';
  ghost: { x: number; y: number; name: string; color: string } | null;
  dragOver: boolean;
  eqSel: Record<number, number>;
  fetMode: Record<number, 'gr' | 'out'>;
  pitchEd: PitchWin | null;
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
  plugs: [], plugsHidden: false, editor: null, menu: null, help: false, settings: false, exportOpen: false, exportBusy: false, vstPicker: null, musicWin: null,
  readout: 'Gotowe',
  headW: 196, pickW: 220, pickOpen: true, browserTab: 'project',
  ghost: null, dragOver: false, eqSel: {}, fetMode: {}, pitchEd: null, takesOpen: {}, compSel: null, autoEdit: null, zTop: 1, marquee: null,
  playing: false, previewing: false, previewPath: null,
});

export const say = (t: string) => setU('readout', t);
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
