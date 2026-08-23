/**
 * What the shell is *showing* -- as opposed to what the engine holds.
 *
 * Kept apart from `state.tsx` on purpose: none of this crosses the IPC wall, so
 * none of it belongs in the file whose job is that wall. Views read and write
 * these signals directly; nothing here is persisted, because a tool choice or a
 * zoom level that outlived a session would be a surprise rather than a
 * convenience.
 */

import {
  createContext,
  createEffect,
  createSignal,
  onCleanup,
  useContext,
  type Accessor,
  type JSX,
  type Setter,
} from 'solid-js';

import { clampBrowserWidth, DEFAULT_BROWSER_WIDTH } from '../lib/browserSize';
import { setTypingPianoEnabled } from '../lib/keyboard';
import type { StripRef } from '../state';

/** The three main views. The piano roll is a fullscreen overlay on top of one. */
export type MainView = 'playlist' | 'rack' | 'mixer' | 'settings';

/** Which tree the browser is showing. */
export type BrowserTab = 'instruments' | 'files';

/** Which half of the right-hand panel is up. */
export type SideTab = 'picker' | 'inspector';

/** What the picker is offering to paint with. */
export type PickerTab = 'patterns' | 'audio';

/** What a click on a lane paints. */
export type Brush =
  | { kind: 'pattern'; id: number }
  | { kind: 'audio'; id: number }
  | null;

/**
 * Which insert a floating plugin window is showing.
 *
 * By **id** and not by chain position, because the editor can reorder the chain
 * it is looking at: an index would leave it pointed at whatever moved into that
 * slot the moment somebody pressed the "move earlier" button.
 */
export interface InsertEditorRef {
  strip: StripRef;
  insertId: number;
}

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

/** How tall a playlist lane is before anyone drags it. */
export const DEFAULT_LANE_H = 72;
/** Below this the S/M row has nowhere to go; above it, the lane is a wall. */
export const MIN_LANE_H = 56;
export const MAX_LANE_H = 160;

/** A lane's drawn height, from the map the playlist keeps. */
export function laneHeightOf(heights: Record<number, number>, laneId: number): number {
  return heights[laneId] ?? DEFAULT_LANE_H;
}

/** Clamp a dragged lane height into the bounds above. */
export function clampLaneHeight(height: number): number {
  return Math.min(MAX_LANE_H, Math.max(MIN_LANE_H, Math.round(height)));
}

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

  /**
   * Open plugin windows, keyed by insert id so reordering does not lose one.
   * Several can sit on the desktop at once, like hosted VSTs.
   */
  insertEditors: Accessor<InsertEditorRef[]>;
  focusedInsertId: Accessor<number | null>;
  openInsertEditor(ref: InsertEditorRef): void;
  focusInsertEditor(insertId: number): void;
  closeInsertEditor(insertId?: number): void;

  /**
   * The rack channel whose sample/instrument window is up, or `null`.
   *
   * Built-ins and the sampler have no native editor, so this is that window.
   * Opening a plugin GUI does not use it — those are OS windows.
   */
  sampleEditor: Accessor<number | null>;
  openSampleEditor(channel: number): void;
  closeSampleEditor(): void;

  browserOpen: Accessor<boolean>;
  setBrowserOpen: Setter<boolean>;
  /** Session-persistent width; survives collapsing and reopening the pane. */
  browserWidth: Accessor<number>;
  setBrowserWidth: Setter<number>;
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
  /** Vertical zoom: how tall each key row is, independent of time zoom. */
  rollZoomY: Accessor<number>;
  setRollZoomY: Setter<number>;
  /**
   * FL's "typing keyboard to piano" (Ctrl+T). When on, Q is C4 (at the default
   * octave) and letter keys play notes instead of being view shortcuts.
   */
  typingKeyboard: Accessor<boolean>;
  setTypingKeyboard: Setter<boolean>;
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
  /** Clip ids the playlist has selected. Empty means none. */
  selectedClips: Accessor<number[]>;
  setSelectedClips: Setter<number[]>;
  /**
   * Whether the arrangement scrolls to keep up with a playing playhead.
   *
   * Lives here rather than inside `Playlist` because the view is unmounted
   * every time you look at the rack, and a follow toggle that reset itself on
   * F6/F5 would be a setting nobody could turn off.
   */
  playlistFollow: Accessor<boolean>;
  setPlaylistFollow: Setter<boolean>;
  /**
   * How tall each playlist lane is drawn, in pixels, by **lane id**.
   *
   * By id and not by index, because removing a lane shifts every index above it
   * and a height that followed the index would slide onto its neighbour. Lanes
   * absent from the map are {@link DEFAULT_LANE_H}.
   */
  laneHeights: Accessor<Record<number, number>>;
  setLaneHeights: Setter<Record<number, number>>;

  /** Pattern id whose name is being typed, or `null`. Driven by F2. */
  renamingPattern: Accessor<number | null>;
  setRenamingPattern: Setter<number | null>;

  /** Whether the window is wide enough for the top bar's optional readouts. */
  wideBar: Accessor<boolean>;
  /**
   * Whether the top bar has room for the record readout -- the input meter, the
   * take counter and the release button.
   *
   * A second, higher breakpoint rather than {@link wideBar}, because the readout
   * is only mounted while the capture device is open and its budget is therefore
   * *on top of* the bar's widest layout. Below it the REC button stands alone:
   * it still shows armed and recording, and the status bar reports the take.
   */
  recordBar: Accessor<boolean>;
}

const ViewContext = createContext<ViewState>();

/** Below this the top bar drops its tempo, signature and position readouts. */
const WIDE_BAR_PX = 1100;

/**
 * Below this the top bar drops the record readout and keeps only the button.
 *
 * Measured, not guessed: the widest layout of the bar is about 1200 px of
 * content, and the readout adds about 80 more while a take is running. Anything
 * narrower than this and the view tabs would start sliding off the right edge.
 */
const RECORD_BAR_PX = 1280;

export function ViewProvider(props: { children: JSX.Element }): JSX.Element {
  const [view, setView] = createSignal<MainView>('rack');
  const [previousView, setPreviousView] = createSignal<MainView>('rack');
  const [rollOpen, setRollOpen] = createSignal(false);
  const [rollChannel, setRollChannel] = createSignal(0);
  const [insertEditors, setInsertEditors] = createSignal<InsertEditorRef[]>([]);
  const [focusedInsertId, setFocusedInsertId] = createSignal<number | null>(null);
  const [sampleEditor, setSampleEditor] = createSignal<number | null>(null);
  const [browserOpen, setBrowserOpen] = createSignal(true);
  const [browserWidth, setBrowserWidth] = createSignal(DEFAULT_BROWSER_WIDTH);
  const [browserTab, setBrowserTab] = createSignal<BrowserTab>('instruments');
  const [sideOpen, setSideOpen] = createSignal(true);
  const [sideTab, setSideTab] = createSignal<SideTab>('inspector');
  const [pickerTab, setPickerTab] = createSignal<PickerTab>('patterns');
  const [brush, setBrush] = createSignal<Brush>(null);
  const [strip, setStrip] = createSignal<StripRef>({ kind: 'track', index: 0 });
  const [sendMode, setSendMode] = createSignal<SendMode>('from');
  const [rollTool, setRollTool] = createSignal<RollTool>('draw');
  const [rollSnap, setRollSnap] = createSignal(ROLL_SNAPS[0]![0]);
  const [rollZoom, setRollZoom] = createSignal(1);
  const [rollZoomY, setRollZoomY] = createSignal(1);
  const [typingKeyboard, setTypingKeyboardSignal] = createSignal(false);
  const [ghosts, setGhosts] = createSignal(true);
  const [selection, setSelection] = createSignal<number[]>([]);
  const [playlistTool, setPlaylistTool] = createSignal<PlaylistTool>('paint');
  const [playlistSnap, setPlaylistSnap] = createSignal(PLAYLIST_SNAPS[0]![0]);
  const [playlistZoom, setPlaylistZoom] = createSignal(1);
  const [selectedClips, setSelectedClips] = createSignal<number[]>([]);
  const [playlistFollow, setPlaylistFollow] = createSignal(true);
  const [laneHeights, setLaneHeights] = createSignal<Record<number, number>>({});
  const [renamingPattern, setRenamingPattern] = createSignal<number | null>(null);
  const [wideBar, setWideBar] = createSignal(
    typeof window === 'undefined' ? true : window.innerWidth >= WIDE_BAR_PX,
  );
  const [recordBar, setRecordBar] = createSignal(
    typeof window === 'undefined' ? true : window.innerWidth >= RECORD_BAR_PX,
  );
  const [viewportWidth, setViewportWidth] = createSignal(
    typeof window === 'undefined' ? 1_600 : window.innerWidth,
  );

  const onResize = () => {
    setWideBar(window.innerWidth >= WIDE_BAR_PX);
    setRecordBar(window.innerWidth >= RECORD_BAR_PX);
    setViewportWidth(window.innerWidth);
  };
  window.addEventListener('resize', onResize);
  onCleanup(() => window.removeEventListener('resize', onResize));

  createEffect(() => {
    const viewport = viewportWidth();
    const inspectorOpen = sideOpen();
    setBrowserWidth((width) => clampBrowserWidth(width, viewport, inspectorOpen));
  });

  const state: ViewState = {
    view,
    go(next) {
      const current = view();
      // Playlist lands on the picker so a brush is in front for painting.
      // Inspector stays one click away; we do not overwrite it if Playlist is
      // already up (F5 while painting, for example).
      if (next === 'playlist' && current !== next) {
        setSideTab('picker');
        setSideOpen(true);
      }
      // Settings is an overlay on the studio: remember where Escape should land.
      // Sample editors stay up across Playlist/Rack/Mixer so a clip opened from
      // the arrangement is not dismissed by glancing at the mixer.
      if (next === 'settings' && current !== 'settings') {
        setPreviousView(current);
      }
      setView(next);
      setRollOpen(false);
      setRenamingPattern(null);
    },
    previousView,
    rollOpen,
    rollChannel,
    openRoll(channel) {
      setPreviousView(view());
      setRollChannel(channel);
      setSelection([]);
      setSampleEditor(null);
      setRollOpen(true);
    },
    closeRoll() {
      setRollOpen(false);
      setSelection([]);
    },
    insertEditors,
    focusedInsertId,
    openInsertEditor(ref) {
      setInsertEditors((open) => {
        if (open.some((entry) => entry.insertId === ref.insertId)) return open;
        return [...open, ref];
      });
      setFocusedInsertId(ref.insertId);
    },
    focusInsertEditor(insertId) {
      setFocusedInsertId(insertId);
    },
    closeInsertEditor(insertId) {
      setInsertEditors((open) => {
        const next = insertId === undefined ? [] : open.filter((entry) => entry.insertId !== insertId);
        const focused = focusedInsertId();
        if (focused === null || insertId === undefined || focused === insertId) {
          setFocusedInsertId(next.at(-1)?.insertId ?? null);
        }
        return next;
      });
    },
    sampleEditor,
    openSampleEditor(channel) {
      setSampleEditor(channel);
    },
    closeSampleEditor() {
      setSampleEditor(null);
    },
    browserOpen,
    setBrowserOpen,
    browserWidth,
    setBrowserWidth,
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
    rollZoomY,
    setRollZoomY,
    typingKeyboard,
    setTypingKeyboard(value) {
      const next = setTypingKeyboardSignal(value);
      setTypingPianoEnabled(next);
      return next;
    },
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
    selectedClips,
    setSelectedClips,
    playlistFollow,
    setPlaylistFollow,
    laneHeights,
    setLaneHeights,
    renamingPattern,
    setRenamingPattern,
    wideBar,
    recordBar,
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
