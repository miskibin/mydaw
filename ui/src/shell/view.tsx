/**
 * What the shell is *showing* -- as opposed to what the engine holds.
 *
 * Kept apart from `state.tsx` on purpose: none of this crosses the IPC wall, so
 * none of it belongs in the file whose job is that wall. Views read and write
 * these signals directly; nothing here is persisted, because a tool choice or a
 * zoom level that outlived a session would be a surprise rather than a
 * convenience.
 */

import { createContext, createSignal, useContext, type Accessor, type JSX, type Setter } from 'solid-js';

import type { StripRef } from '../state';

/** The three main views. The piano roll is a fullscreen overlay on top of one. */
export type MainView = 'playlist' | 'rack' | 'mixer';

/** Which tree the browser is showing. */
export type BrowserTab = 'sounds' | 'plugins' | 'audio';

/** Which half of the right-hand panel is up. */
export type SideTab = 'picker' | 'inspector';

/** What the picker is offering to paint with. */
export type PickerTab = 'patterns' | 'audio';

/** What a click on a lane paints. */
export type Brush =
  | { kind: 'pattern'; id: number }
  | { kind: 'audio'; id: number }
  | null;

/** The roll's pointer tools. */
export type RollTool = 'draw' | 'paint' | 'select' | 'delete' | 'slice';

/** The playlist's pointer tools. */
export type PlaylistTool = 'paint' | 'select' | 'delete';

/** Which end of a send the mixer's knob row is showing. */
export type SendMode = 'from' | 'into';

/** Snap choices, in ticks. `0` is off. */
export const ROLL_SNAPS: readonly [number, string][] = [
  [240, '1/16'],
  [480, '1/8'],
  [960, '1/4'],
  [3840, 'bar'],
  [0, 'off'],
];

/** The playlist snaps in bars, because that is how an arrangement is spoken. */
export const PLAYLIST_SNAPS: readonly [number, string][] = [
  [3840, 'bar'],
  [1920, '1/2'],
  [960, '1/4'],
  [240, '1/16'],
  [0, 'off'],
];

/** Zoom bounds, shared by the roll and the playlist. */
export const MIN_ZOOM = 0.4;
export const MAX_ZOOM = 4;

export interface ViewState {
  view: Accessor<MainView>;
  go(view: MainView): void;
  /** The view the roll will fall back to, so Escape lands where you came from. */
  previousView: Accessor<MainView>;

  rollOpen: Accessor<boolean>;
  /** The rack channel the roll is editing. */
  rollChannel: Accessor<number>;
  openRoll(channel: number): void;
  closeRoll(): void;

  browserOpen: Accessor<boolean>;
  setBrowserOpen: Setter<boolean>;
  browserTab: Accessor<BrowserTab>;
  setBrowserTab: Setter<BrowserTab>;

  sideOpen: Accessor<boolean>;
  setSideOpen: Setter<boolean>;
  sideTab: Accessor<SideTab>;
  setSideTab: Setter<SideTab>;
  pickerTab: Accessor<PickerTab>;
  setPickerTab: Setter<PickerTab>;

  /** What painting on a playlist lane places. */
  brush: Accessor<Brush>;
  setBrush: Setter<Brush>;

  /** The strip the mixer highlights and the inspector describes. */
  strip: Accessor<StripRef>;
  setStrip: Setter<StripRef>;
  sendMode: Accessor<SendMode>;
  setSendMode: Setter<SendMode>;

  rollTool: Accessor<RollTool>;
  setRollTool: Setter<RollTool>;
  rollSnap: Accessor<number>;
  setRollSnap: Setter<number>;
  rollZoom: Accessor<number>;
  setRollZoom: Setter<number>;
  ghosts: Accessor<boolean>;
  setGhosts: Setter<boolean>;
  /** Indices into the edited channel's note list. */
  selection: Accessor<number[]>;
  setSelection: Setter<number[]>;

  playlistTool: Accessor<PlaylistTool>;
  setPlaylistTool: Setter<PlaylistTool>;
  playlistSnap: Accessor<number>;
  setPlaylistSnap: Setter<number>;
  playlistZoom: Accessor<number>;
  setPlaylistZoom: Setter<number>;
  /** The clip the playlist has selected, by id. */
  selectedClip: Accessor<number | null>;
  setSelectedClip: Setter<number | null>;

  /** Whether the window is wide enough for the top bar's optional readouts. */
  wideBar: Accessor<boolean>;
}

const ViewContext = createContext<ViewState>();

/** Below this the top bar drops its tempo, signature and position readouts. */
const WIDE_BAR_PX = 1100;

export function ViewProvider(props: { children: JSX.Element }): JSX.Element {
  const [view, setView] = createSignal<MainView>('rack');
  const [previousView, setPreviousView] = createSignal<MainView>('rack');
  const [rollOpen, setRollOpen] = createSignal(false);
  const [rollChannel, setRollChannel] = createSignal(0);
  const [browserOpen, setBrowserOpen] = createSignal(true);
  const [browserTab, setBrowserTab] = createSignal<BrowserTab>('sounds');
  const [sideOpen, setSideOpen] = createSignal(true);
  const [sideTab, setSideTab] = createSignal<SideTab>('inspector');
  const [pickerTab, setPickerTab] = createSignal<PickerTab>('patterns');
  const [brush, setBrush] = createSignal<Brush>(null);
  const [strip, setStrip] = createSignal<StripRef>({ kind: 'track', index: 0 });
  const [sendMode, setSendMode] = createSignal<SendMode>('from');
  const [rollTool, setRollTool] = createSignal<RollTool>('draw');
  const [rollSnap, setRollSnap] = createSignal(ROLL_SNAPS[0]![0]);
  const [rollZoom, setRollZoom] = createSignal(1);
  const [ghosts, setGhosts] = createSignal(true);
  const [selection, setSelection] = createSignal<number[]>([]);
  const [playlistTool, setPlaylistTool] = createSignal<PlaylistTool>('paint');
  const [playlistSnap, setPlaylistSnap] = createSignal(PLAYLIST_SNAPS[0]![0]);
  const [playlistZoom, setPlaylistZoom] = createSignal(1);
  const [selectedClip, setSelectedClip] = createSignal<number | null>(null);
  const [wideBar, setWideBar] = createSignal(
    typeof window === 'undefined' ? true : window.innerWidth >= WIDE_BAR_PX,
  );

  const onResize = () => setWideBar(window.innerWidth >= WIDE_BAR_PX);
  window.addEventListener('resize', onResize);

  const state: ViewState = {
    view,
    go(next) {
      setView(next);
      setRollOpen(false);
      setPreviousView(next);
    },
    previousView,
    rollOpen,
    rollChannel,
    openRoll(channel) {
      setPreviousView(view());
      setRollChannel(channel);
      setSelection([]);
      setRollOpen(true);
    },
    closeRoll() {
      setRollOpen(false);
      setSelection([]);
    },
    browserOpen,
    setBrowserOpen,
    browserTab,
    setBrowserTab,
    sideOpen,
    setSideOpen,
    sideTab,
    setSideTab,
    pickerTab,
    setPickerTab,
    brush,
    setBrush,
    strip,
    setStrip,
    sendMode,
    setSendMode,
    rollTool,
    setRollTool,
    rollSnap,
    setRollSnap,
    rollZoom,
    setRollZoom,
    ghosts,
    setGhosts,
    selection,
    setSelection,
    playlistTool,
    setPlaylistTool,
    playlistSnap,
    setPlaylistSnap,
    playlistZoom,
    setPlaylistZoom,
    selectedClip,
    setSelectedClip,
    wideBar,
  };

  return <ViewContext.Provider value={state}>{props.children}</ViewContext.Provider>;
}

export function useView(): ViewState {
  const state = useContext(ViewContext);
  if (!state) throw new Error('useView must be used inside <ViewProvider>');
  return state;
}

/** The label a snap value goes by, from either table. */
export function snapLabel(snaps: readonly [number, string][], value: number): string {
  return snaps.find(([ticks]) => ticks === value)?.[1] ?? 'off';
}

/** The next snap in the cycle. */
export function nextSnap(snaps: readonly [number, string][], value: number): number {
  const index = snaps.findIndex(([ticks]) => ticks === value);
  return snaps[(index + 1) % snaps.length]![0];
}

/** Clamp a zoom factor into the shared bounds. */
export function clampZoom(zoom: number): number {
  return Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, zoom));
}
