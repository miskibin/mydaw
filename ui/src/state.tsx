/**
 * The boundary with the engine.
 *
 * Everything that crosses the IPC wall goes through here: one place that holds
 * the engine's answers, one place that reports its refusals, and one place that
 * decides what an undoable gesture is. Views read signals and call commands;
 * none of them import `@tauri-apps/api`, and none of them keep a second copy of
 * engine state.
 *
 * Three things are deliberately **not** signals:
 *
 * - `meters`, a plain mutable holder. A 30-60 Hz stream must not drive the
 *   reactive scheduler; `ui/frameLoop.ts` paints from it instead.
 * - `playhead`, which extrapolates between packets and is read once per frame.
 * - `undo`, whose depth is surfaced through one revision counter rather than by
 *   making the stack reactive.
 */

import {
  createContext,
  createEffect,
  createSignal,
  onCleanup,
  useContext,
  type Accessor,
  type JSX,
} from 'solid-js';

import { getBackend, type Backend } from './api/backend';
import type {
  AssetStatus,
  AudioSourceDto,
  ClapPluginInfoDto,
  ClapTarget,
  EffectInfoDto,
  InputDeviceDto,
  InputStatusDto,
  InsertReadoutsDto,
  InsertTarget,
  MeterFrame,
  MixerState,
  PatternDto,
  PatternSummaryDto,
  PlaylistDto,
  PlayMode,
  ProjectInfo,
  RecordedTakeDto,
  SequencerState,
  SourceDto,
  StreamInfo,
  TrackDto,
  UiSnapshot,
  Vst3PluginInfoDto,
} from './api/types';
import {
  DEFAULT_OCTAVE,
  DEFAULT_VELOCITY,
  clampOctave,
  isPianoEvent,
  midiKeyFor,
} from './lib/keyboard';
import { Playhead } from './lib/playhead';
import {
  playlistEntry,
  patternEntry,
  snapshotPlaylist,
  UndoStack,
  type PlaylistSnapshot,
  type UndoEntry,
} from './lib/undo';
import { channelClip, withSlicesAsNotes, withStepsConvertedToNotes } from './lib/pattern';
import { clampDb, formatDb, MAX_DB, MIN_DB } from './panels/mixer/scale';

/** How long a pattern edit waits for the next one before it is pushed. */
const PATTERN_PUSH_DEBOUNCE_MS = 50;
/** How often a visible insert chain's meters are re-read. */
const READOUT_POLL_MS = 66;
/** How long the parameter readout stays up after the last change. */
const PARAM_TOAST_MS = 2600;
/**
 * How long a master-gain gesture may pause before we refetch the mixer.
 *
 * Same window as `WHEEL_COMMIT_MS` in `ui/drag.ts`: a drag has a release, a
 * wheel does not, and one `get_tracks` per notch would sit a round trip between
 * clicks. Kept as a number rather than imported, because this module must not
 * depend on the view layer.
 */
const MASTER_GAIN_SETTLE_MS = 180;

/**
 * The contributed panel the dev-only IPC benchmark announces itself with.
 * Absent in every normal session, which is what keeps its chunk unfetched.
 */
const IPC_BENCH_AUTO_PANEL = 'ipc-bench-auto';

/** Whether the shell has finished its first round trip. */
export type KernelState = 'connecting' | 'ready' | 'error';

/** The latest meter packet, outside the reactive graph. */
export class MeterBus {
  latest: MeterFrame | null = null;

  push(frame: MeterFrame): void {
    this.latest = frame;
  }
}

/** What the parameter readout in the status bar is showing. */
export interface ParamReadout {
  /** `Reverb Bus · Send` -- the thing and the parameter. */
  name: string;
  /** The value, already formatted in its own unit. */
  value: string;
}

/** Which strip the mixer and the inspector are pointed at. */
export type StripRef =
  | { kind: 'track'; index: number }
  | { kind: 'bus'; index: number }
  | { kind: 'master' };

/** The strip a `StripRef` names, as an insert-chain target. */
export function insertTargetFor(strip: StripRef): InsertTarget {
  return strip.kind === 'master'
    ? { kind: 'master', index: 0 }
    : { kind: strip.kind, index: strip.index };
}

export interface AppState {
  readonly backend: Backend;
  readonly meters: MeterBus;
  readonly playhead: Playhead;
  readonly undo: UndoStack;

  // --- engine state --------------------------------------------------------
  kernelState: Accessor<KernelState>;
  kernelError: Accessor<string | null>;
  snapshot: Accessor<UiSnapshot | undefined>;
  sequencer: Accessor<SequencerState | undefined>;
  mixer: Accessor<MixerState | undefined>;
  effects: Accessor<EffectInfoDto[]>;
  sources: Accessor<SourceDto[]>;
  audioSources: Accessor<AudioSourceDto[]>;
  clapPlugins: Accessor<ClapPluginInfoDto[]>;
  vst3Plugins: Accessor<Vst3PluginInfoDto[]>;
  scanning: Accessor<boolean>;
  project: Accessor<ProjectInfo>;
  /** Changes only when a different session is created or opened. */
  projectRevision: Accessor<number>;
  streamInfo: Accessor<StreamInfo | undefined>;
  refreshStreamInfo(): Promise<void>;
  assets: Accessor<AssetStatus[]>;
  playing: Accessor<boolean>;
  octave: Accessor<number>;
  setOctave(octave: number): void;
  heldNotes: Accessor<number[]>;
  /** Bumped whenever the undo stack changes, so labels can be reactive. */
  undoRevision: Accessor<number>;

  // --- derived -------------------------------------------------------------
  /** The pattern the rack and the roll are editing. */
  pattern: Accessor<PatternDto | undefined>;
  patterns: Accessor<PatternSummaryDto[]>;
  playlist: Accessor<PlaylistDto | undefined>;
  tracks: Accessor<TrackDto[]>;
  playMode: Accessor<PlayMode>;
  bpm: Accessor<number>;
  /** The MIDI key a rack channel's grid triggers, for a note the roll adds. */
  channelKey(channel: number): number;
  /**
   * Turn this channel's switched-on steps into one-step piano-roll notes.
   *
   * Opening the roll on a sequencer row would otherwise be empty: the engine
   * stores steps and notes separately, and the roll only draws notes. Same
   * no-op-by-reference contract as the other pattern edits. Returns how many
   * notes were created.
   */
  promoteStepsToRoll(channel: number): number;
  /**
   * Lay a sliced channel's chop across the roll: one note per slice, in order,
   * on sixteenths, from `rootKey` up.
   *
   * The break played back as it was recorded, which is what a slicer hands you
   * to start rearranging. Returns how many notes were written -- fewer than
   * `sliceCount` only when the chop runs past MIDI 127.
   */
  dumpSlicesToRoll(channel: number, rootKey: number, sliceCount: number): number;

  // --- the parameter readout -----------------------------------------------
  param: Accessor<ParamReadout | null>;
  /** Show a value for a moment. Every continuous gesture calls this. */
  say(name: string, value: string): void;

  // --- commands ------------------------------------------------------------
  refresh(): Promise<void>;
  report(error: unknown): void;
  play(): void;
  stop(): void;
  togglePlay(): void;
  seek(samplePos: number): void;
  setPlayMode(mode: PlayMode): void;
  /** Whether SONG repeats the arrangement. PAT loops regardless. */
  looping: Accessor<boolean>;
  setLoop(looping: boolean): void;
  setBpm(bpm: number): void;

  /**
   * Master output level, in dB. The navbar knob and the mixer fader both read
   * this, including the value a gesture has already sent but the engine has not
   * echoed yet.
   */
  masterGainDb: Accessor<number>;
  /** Set the master fader. Optimistic; a quiet moment later it reconciles. */
  setMasterGain(db: number): void;

  /** Run a command that answers with the whole mixer, and adopt it. */
  mixerCommand(run: (backend: Backend) => Promise<MixerState>): Promise<void>;
  /** Run a command that answers with the whole sequencer, and adopt it. */
  sequencerCommand(run: (backend: Backend) => Promise<SequencerState>): Promise<void>;

  // Pattern editing, as gestures. `startPatternGesture` snapshots, every
  // `editPattern` in between paints and pushes, and `commitPatternGesture`
  // turns the pair into one undo entry -- so a forty-note drag is one Ctrl+Z.
  startPatternGesture(label: string): void;
  editPattern(edit: (pattern: PatternDto) => PatternDto): void;
  commitPatternGesture(): void;
  /** Start, edit and commit in one go: a click, a menu item, a note operation. */
  editPatternOnce(label: string, edit: (pattern: PatternDto) => PatternDto): void;

  // The arrangement, likewise -- except the edits are engine commands rather
  // than a local shape, so the snapshot is of what came back.
  startPlaylistGesture(label: string): void;
  commitPlaylistGesture(): void;

  /**
   * Record a gesture that has **already been applied**, from a view that owns
   * its own inverse.
   *
   * The pattern and playlist gestures above snapshot for you; a destructive
   * sample edit cannot be snapshotted (the "before" is a file), so the engine
   * hands back both pointers and the sample editor builds the entry itself. See
   * `sampleEditEntry`.
   */
  pushUndo(entry: UndoEntry): void;
  /** Undo/redo, reporting what was undone in the status bar. */
  undoLast(): void;
  redoLast(): void;
  /** Throw the history away, e.g. after an edit no inverse could survive. */
  clearHistory(reason: string): void;
  /** One pattern per occupied rack channel; playlist clips spread onto new lanes. */
  splitPatternByChannel(patternId: number): void;

  selectTrack(index: number): void;
  toggleGui(target: ClapTarget, open: boolean): void;

  /** Poll one chain's meters while it is on screen. `null` stops the poll. */
  watchReadouts(target: InsertTarget | null): void;
  readouts: Accessor<InsertReadoutsDto[]>;

  // --- recording -----------------------------------------------------------
  //
  // The whole input path lives here rather than in the transport bar, for the
  // reason everything else does: views do not talk to the engine. The bar reads
  // `inputStatus()` and calls these; Settings reads `inputDevices()`.

  /** Capture devices, as of the last {@link refreshInputDevices}. */
  inputDevices: Accessor<InputDeviceDto[]>;
  /** What the recorder is doing. `undefined` until the first answer arrives. */
  inputStatus: Accessor<InputStatusDto | undefined>;
  /** Re-enumerate. Called on mount and by Settings' Refresh button. */
  refreshInputDevices(): Promise<void>;
  /** Choose the capture device. `null` follows the system default. */
  setInputDevice(deviceId: string | null): Promise<void>;
  /** Open the capture device and start metering, or release it. */
  setInputArmed(armed: boolean): Promise<void>;
  /** Start a take, arming first if needed. */
  startRecording(): Promise<void>;
  /**
   * Stop, load the take, and add a rack channel that plays it.
   *
   * Resolves with the new channel's index so the caller can open its sample
   * editor — one gesture from REC to a playable, editable channel. `null` when
   * the take could not be kept, with the reason already in the status bar.
   */
  finishRecording(): Promise<number | null>;
  /** Throw the take away. The device stays armed. */
  cancelRecording(): Promise<void>;

  loadDevices(rescan?: boolean): Promise<void>;
  refreshAudioSources(): Promise<void>;
  startAssetDownload(id: string): void;
  newProject(): void;
  openProject(): void;
  saveProject(): void;
  saveProjectAs(): void;
  resubscribeMeters(): Promise<void>;
}

const UNTITLED_PROJECT: ProjectInfo = {
  path: null,
  name: 'Untitled',
  missingPlugins: 0,
  warnings: [],
};

const AppStateContext = createContext<AppState>();

export function AppStateProvider(props: { children: JSX.Element }): JSX.Element {
  const state = createAppState();
  return <AppStateContext.Provider value={state}>{props.children}</AppStateContext.Provider>;
}

export function useAppState(): AppState {
  const state = useContext(AppStateContext);
  if (!state) throw new Error('useAppState must be used inside <AppStateProvider>');
  return state;
}

function createAppState(): AppState {
  const backend = getBackend();
  const meters = new MeterBus();
  const playhead = new Playhead();
  const undo = new UndoStack();

  const [kernelState, setKernelState] = createSignal<KernelState>('connecting');
  const [kernelError, setKernelError] = createSignal<string | null>(null);
  const [snapshot, setSnapshot] = createSignal<UiSnapshot>();
  const [sequencer, setSequencer] = createSignal<SequencerState>();
  const [mixer, setMixer] = createSignal<MixerState>();
  const [masterGainOverride, setMasterGainOverride] = createSignal<number | null>(null);
  const [effects, setEffects] = createSignal<EffectInfoDto[]>([]);
  const [sources, setSources] = createSignal<SourceDto[]>([]);
  const [audioSources, setAudioSources] = createSignal<AudioSourceDto[]>([]);
  const [clapPlugins, setClapPlugins] = createSignal<ClapPluginInfoDto[]>([]);
  const [vst3Plugins, setVst3Plugins] = createSignal<Vst3PluginInfoDto[]>([]);
  const [scanning, setScanning] = createSignal(false);
  const [project, setProject] = createSignal<ProjectInfo>(UNTITLED_PROJECT);
  const [projectRevision, setProjectRevision] = createSignal(0);
  const [streamInfo, setStreamInfo] = createSignal<StreamInfo>();
  const [assets, setAssets] = createSignal<AssetStatus[]>([]);
  const [playing, setPlaying] = createSignal(false);
  const [octave, setOctaveSignal] = createSignal(DEFAULT_OCTAVE);
  const [heldNotes, setHeldNotes] = createSignal<number[]>([]);
  const [param, setParam] = createSignal<ParamReadout | null>(null);
  const [undoRevision, setUndoRevision] = createSignal(0);
  const [readouts, setReadouts] = createSignal<InsertReadoutsDto[]>([]);
  const [inputDevices, setInputDevices] = createSignal<InputDeviceDto[]>([]);
  const [inputStatus, setInputStatus] = createSignal<InputStatusDto>();

  function report(error: unknown): void {
    const message = error instanceof Error ? error.message : String(error);
    setKernelError(message);
    console.error('[mydaw]', message);
  }

  // --- the parameter readout -----------------------------------------------

  let toastTimer: ReturnType<typeof setTimeout> | undefined;
  function say(name: string, value: string): void {
    setParam({ name, value });
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => setParam(null), PARAM_TOAST_MS);
  }

  // --- fetching ------------------------------------------------------------

  async function refresh(): Promise<void> {
    try {
      const [snap, seq, mix, catalogue, sourceList, info, stream, audio] = await Promise.all([
        backend.getUiSnapshot(),
        backend.getPattern(),
        backend.getTracks(),
        backend.getEffects(),
        backend.getSources(),
        backend.getProjectInfo(),
        backend.getStreamInfo(),
        backend.getAudioSources(),
      ]);
      setSnapshot(snap);
      setSequencer(seq);
      setMixer(mix);
      setEffects(catalogue);
      setSources(sourceList);
      setProject(info);
      setStreamInfo(stream);
      setAudioSources(audio);
      playhead.setSampleRate(seq.sampleRate > 0 ? seq.sampleRate : stream.sampleRate);
      setKernelState('ready');
      setKernelError(null);
    } catch (error) {
      setKernelState('error');
      report(error);
    }
  }

  async function refreshAudioSources(): Promise<void> {
    try {
      setAudioSources(await backend.getAudioSources());
    } catch (error) {
      report(error);
    }
  }

  async function refreshStreamInfo(): Promise<void> {
    try {
      setStreamInfo(await backend.getStreamInfo());
    } catch {
      /* the status bar is a readout, not an error surface */
    }
  }

  /**
   * The plugin list, kept out of `refresh` on purpose. Opening a module runs
   * vendor `init()`, which is how unlicensed plugins pop an activation window,
   * so the first call only reads the on-disk cache. `rescan` is the Rescan
   * button: that is the only path that opens plugins.
   */
  let devicesRequested = false;
  async function loadDevices(rescan = false): Promise<void> {
    if (devicesRequested && !rescan) return;
    devicesRequested = true;
    setScanning(true);
    try {
      const [clap, vst3] = await Promise.all([
        backend.getClapPlugins(rescan),
        backend.getVst3Plugins(rescan),
      ]);
      setClapPlugins(clap);
      setVst3Plugins(vst3);
      setKernelError(null);
    } catch (error) {
      report(error);
    } finally {
      setScanning(false);
    }
  }

  // --- command funnels -----------------------------------------------------

  async function mixerCommand(run: (backend: Backend) => Promise<MixerState>): Promise<void> {
    try {
      releaseHeldNotes();
      setMixer(await run(backend));
      setKernelError(null);
      setSources(await backend.getSources());
    } catch (error) {
      report(error);
      try {
        setMixer(await backend.getTracks());
      } catch {
        // The refetch failing is already reported by the error above.
      }
    }
  }

  /**
   * Master volume, the same number the mixer fader and the navbar knob share.
   *
   * A gesture writes here immediately and tells the engine in the same beat;
   * `get_tracks` is asked for once the pointer or the wheel goes quiet, which
   * is when the override drops and the engine's copy takes over.
   */
  const masterGainDb = (): number =>
    masterGainOverride() ?? mixer()?.master.gainDb ?? 0;

  let masterGainTimer: ReturnType<typeof setTimeout> | undefined;
  function setMasterGain(db: number): void {
    const mix = mixer();
    const next = clampDb(db, mix?.minGainDb ?? MIN_DB, mix?.maxGainDb ?? MAX_DB);
    setMasterGainOverride(next);
    say('Master · Volume', `${formatDb(next)} dB`);
    void backend.setMasterGain(next).catch(report);
    clearTimeout(masterGainTimer);
    masterGainTimer = setTimeout(() => {
      void mixerCommand((b) => b.getTracks()).finally(() => setMasterGainOverride(null));
    }, MASTER_GAIN_SETTLE_MS);
  }

  async function sequencerCommand(
    run: (backend: Backend) => Promise<SequencerState>,
  ): Promise<void> {
    try {
      adoptSequencer(await run(backend));
      setKernelError(null);
    } catch (error) {
      report(error);
      try {
        adoptSequencer(await backend.getPattern());
      } catch {
        // Likewise.
      }
    }
  }

  function adoptSequencer(next: SequencerState): void {
    setSequencer(next);
    if (next.sampleRate > 0) playhead.setSampleRate(next.sampleRate);
  }

  // --- transport -----------------------------------------------------------

  function play(): void {
    backend.transportPlay().catch(report);
    setPlaying(true);
  }

  function stop(): void {
    backend.transportStop().catch(report);
    setPlaying(false);
  }

  function togglePlay(): void {
    if (playing()) stop();
    else play();
  }

  function seek(samplePos: number): void {
    const position = Math.max(0, Math.round(samplePos));
    backend.transportSeek(position).catch(report);
    // Move the drawn playhead now rather than waiting for the next packet: a
    // click on a ruler that takes 30 ms to land reads as a dropped click.
    playhead.reset(position, playing());
  }

  function setPlayMode(mode: PlayMode): void {
    void sequencerCommand((b) => b.setPlayMode(mode));
    say('Transport · Mode', mode === 'pat' ? 'Pattern' : 'Song');
  }

  function setLoop(looping: boolean): void {
    void sequencerCommand((b) => b.setLoop(looping));
    say('Transport · Loop', looping ? 'Song repeats' : 'Plays once');
  }

  function setBpm(next: number): void {
    const previous = sequencer()?.bpm ?? 130;
    const current = sequencer();
    if (current) setSequencer({ ...current, bpm: next });
    backend.setBpm(next).catch((error) => {
      report(error);
      const rolled = sequencer();
      if (rolled) setSequencer({ ...rolled, bpm: previous });
    });
  }

  // --- pattern editing -----------------------------------------------------

  const pattern = (): PatternDto | undefined => sequencer()?.patternSet.active;

  /** Overwrite the active pattern locally, so the view repaints this frame. */
  function setLocalPattern(next: PatternDto): void {
    const current = sequencer();
    if (!current) return;
    setSequencer({
      ...current,
      patternSet: { ...current.patternSet, active: next },
    });
  }

  let pushTimer: ReturnType<typeof setTimeout> | undefined;
  let pendingPush: PatternDto | undefined;
  let pendingExpected: PatternDto | undefined;

  /**
   * Push the pattern the user has been editing.
   *
   * Debounced, because a drag emits an edit per pointer move and the engine
   * only needs the last one. The echoed pattern is deliberately **not** adopted
   * on success: an edit made while the push was in flight would be clobbered by
   * it. A failed push refetches, because then the engine is the one that is
   * right.
   */
  function schedulePush(next: PatternDto, expected: PatternDto): void {
    pendingExpected ??= structuredClone(expected);
    pendingPush = next;
    clearTimeout(pushTimer);
    pushTimer = setTimeout(() => {
      const payload = pendingPush;
      const expected = pendingExpected;
      pendingPush = undefined;
      pendingExpected = undefined;
      if (!payload) return;
      backend.setPattern(payload, expected).catch((error) => {
        report(error);
        backend.getPattern().then(adoptSequencer).catch(() => {});
      });
    }, PATTERN_PUSH_DEBOUNCE_MS);
  }

  /** Flush a pending push now, so an undo entry cannot race it. */
  async function flushPush(): Promise<void> {
    clearTimeout(pushTimer);
    const payload = pendingPush;
    const expected = pendingExpected;
    pendingPush = undefined;
    pendingExpected = undefined;
    if (!payload) return;
    try {
      await backend.setPattern(payload, expected);
    } catch (error) {
      report(error);
      await backend.getPattern().then(adoptSequencer).catch(() => {});
    }
  }

  let gestureLabel: string | null = null;
  let gestureBefore: PatternDto | null = null;

  function startPatternGesture(label: string): void {
    const current = pattern();
    if (!current) return;
    gestureLabel = label;
    backend.setHistoryGroup(label);
    gestureBefore = structuredClone(current);
  }

  function editPattern(edit: (pattern: PatternDto) => PatternDto): void {
    const current = pattern();
    if (!current) return;
    const next = edit(current);
    // The `with*` helpers answer with the same reference when nothing changed,
    // which is what makes a drag that stays inside one cell free.
    if (next === current) return;
    setLocalPattern(next);
    schedulePush(next, current);
  }

  function commitPatternGesture(): void {
    const before = gestureBefore;
    const label = gestureLabel;
    gestureBefore = null;
    gestureLabel = null;
    const after = pattern();
    if (!before || !label || !after) { backend.setHistoryGroup(); return; }
    if (JSON.stringify(before) === JSON.stringify(after)) { backend.setHistoryGroup(); return; }
    void flushPush();
    backend.setHistoryGroup();
    undo.push(patternEntry(backend, label, before, structuredClone(after)));
    setUndoRevision(undo.revision);
  }

  function editPatternOnce(label: string, edit: (pattern: PatternDto) => PatternDto): void {
    startPatternGesture(label);
    editPattern(edit);
    commitPatternGesture();
  }

  // --- arrangement editing -------------------------------------------------

  let playlistLabel: string | null = null;
  let playlistBefore: PlaylistSnapshot | null = null;

  function startPlaylistGesture(label: string): void {
    const current = sequencer()?.playlist;
    if (!current) return;
    playlistLabel = label;
    backend.setHistoryGroup(label);
    playlistBefore = snapshotPlaylist(current);
  }

  function commitPlaylistGesture(): void {
    const before = playlistBefore;
    const label = playlistLabel;
    playlistBefore = null;
    playlistLabel = null;
    backend.setHistoryGroup();
    const current = sequencer()?.playlist;
    if (!before || !label || !current) return;
    const after = snapshotPlaylist(current);
    if (JSON.stringify(before) === JSON.stringify(after)) return;
    undo.push(playlistEntry(backend, label, before, after));
    setUndoRevision(undo.revision);
  }

  function pushUndo(entry: UndoEntry): void {
    undo.push(entry);
    setUndoRevision(undo.revision);
  }

  function clearHistory(reason: string): void {
    if (backend.sharedHistory) return;
    if (undo.undoLabel() === null && undo.redoLabel() === null) return;
    undo.clear();
    setUndoRevision(undo.revision);
    say('Undo history', `cleared by ${reason}`);
  }

  function splitPatternByChannel(patternId: number): void {
    const before = new Set((sequencer()?.patternSet.patterns ?? []).map((pattern) => pattern.id));
    void sequencerCommand((backend) => backend.splitPatternByChannel(patternId)).then(() => {
      const created = (sequencer()?.patternSet.patterns ?? []).filter(
        (pattern) => !before.has(pattern.id),
      );
      if (created.length === 0) return;
      clearHistory('splitting by channel');
      say('Split by channel', `${created.length + 1} patterns`);
    });
  }

  async function replay(direction: 'undo' | 'redo'): Promise<void> {
    try {
      const label =
        backend.sharedHistory ? (direction === 'undo' ? undo.undoLabel() : undo.redoLabel()) :
          direction === 'undo' ? await undo.undo() : await undo.redo();
      if (backend.sharedHistory) {
        undo.setShared(await backend.replaySessionHistory(direction));
        await refresh();
      }
      setUndoRevision(undo.revision);
      if (label === null) {
        say(direction === 'undo' ? 'Undo' : 'Redo', 'nothing to do');
        return;
      }
      adoptSequencer(await backend.getPattern());
      say(direction === 'undo' ? 'Undo' : 'Redo', label);
    } catch (error) {
      setUndoRevision(undo.revision);
      report(error);
      try {
        adoptSequencer(await backend.getPattern());
      } catch {
        // Already reported.
      }
    }
  }

  function undoLast(): void {
    void flushPush().then(() => replay('undo'));
  }

  function redoLast(): void {
    void flushPush().then(() => replay('redo'));
  }

  // --- recording -----------------------------------------------------------

  /**
   * How often the input level and the elapsed time are re-read.
   *
   * Only while the device is open. A meter wants to look continuous, and 16 Hz
   * is the slowest that still does for a bar that is 40 px wide; the reason it
   * is a poll rather than a `Channel` (which the mixer meters use) is that the
   * meter pump is single-subscription and one number twice a second more than
   * pays for a second stream.
   */
  const INPUT_POLL_MS = 60;
  let inputTimer: ReturnType<typeof setInterval> | undefined;

  /** Keep the poll running exactly while the device is open. */
  function syncInputPoll(status: InputStatusDto | undefined): void {
    const open = status !== undefined && status.state !== 'idle';
    if (open === (inputTimer !== undefined)) return;
    if (!open) {
      clearInterval(inputTimer);
      inputTimer = undefined;
      return;
    }
    inputTimer = setInterval(() => {
      backend
        .getInputStatus()
        .then(adoptInputStatus)
        // A readout, not an error surface: a failed poll is the next poll's
        // problem, and the status bar has better things to say.
        .catch(() => {});
    }, INPUT_POLL_MS);
  }

  function adoptInputStatus(status: InputStatusDto): void {
    setInputStatus(status);
    syncInputPoll(status);
    // The device died under a running take. The engine has already salvaged
    // whatever it captured; all that is left is to say so, once.
    if (status.fault && status.fault !== lastInputFault) {
      report(new Error(status.fault));
    }
    lastInputFault = status.fault;
  }
  let lastInputFault = '';

  async function refreshInputDevices(): Promise<void> {
    try {
      const [devices, status] = await Promise.all([
        backend.getInputDevices(),
        backend.getInputStatus(),
      ]);
      setInputDevices(devices);
      adoptInputStatus(status);
    } catch (error) {
      report(error);
    }
  }

  /** Run one input gesture, adopting the status it answers with. */
  async function inputCommand(
    run: (backend: Backend) => Promise<InputStatusDto>,
  ): Promise<boolean> {
    try {
      adoptInputStatus(await run(backend));
      setKernelError(null);
      return true;
    } catch (error) {
      report(error);
      // The engine's state machine is the authority; a refused gesture must not
      // leave the button drawing a state the engine is not in.
      await syncInputStatus();
      return false;
    }
  }

  /** Re-read the status, so the button never draws a state the engine is not in. */
  async function syncInputStatus(): Promise<void> {
    await backend.getInputStatus().then(adoptInputStatus).catch(() => {});
  }

  async function finishRecording(): Promise<number | null> {
    let take: RecordedTakeDto;
    try {
      take = await backend.stopRecording();
      setKernelError(null);
    } catch (error) {
      report(error);
      await syncInputStatus();
      return null;
    }
    await syncInputStatus();

    // The same command a dropped WAV takes, so a take and an import cannot
    // produce two different kinds of channel.
    let channel: number | null = null;
    try {
      const next = await backend.addSampleChannel(take.source.path);
      setMixer(next);
      channel = next.tracks.length - 1;
      setSources(await backend.getSources());
    } catch (error) {
      // The take is on disk and loaded either way -- it is in the Audio
      // browser -- so this is a partial success, not a lost recording.
      report(error);
    }
    await refreshAudioSources();

    const seconds = take.source.durationSeconds;
    if (take.limitReached) {
      say(take.source.name, `${seconds.toFixed(1)} s — stopped at the length limit`);
    } else if (take.droppedSamples > 0) {
      say(take.source.name, `${seconds.toFixed(1)} s — ${take.droppedSamples} samples dropped`);
    } else if (take.peak < 0.001) {
      say(take.source.name, `${seconds.toFixed(1)} s — silent, check the input`);
    } else {
      say(take.source.name, `${seconds.toFixed(1)} s recorded`);
    }
    return channel;
  }

  // --- selection and plugins -----------------------------------------------

  function selectTrack(index: number): void {
    void mixerCommand((b) => b.setSelectedTrack(index));
  }

  function toggleGui(target: ClapTarget, open: boolean): void {
    const state = mixer();
    if (!state) return;
    const slot =
      target.slot === 'master' ? state.master : state.tracks[target.index];
    const isVst3 = Boolean(slot?.vst3);
    const call = isVst3
      ? open
        ? backend.openVst3Gui(target)
        : backend.closeVst3Gui(target)
      : open
        ? backend.openClapGui(target)
        : backend.closeClapGui(target);
    call
      .then(() => setKernelError(null))
      .catch(report)
      .finally(() => {
        backend.getTracks().then(setMixer).catch(() => {});
      });
  }

  // --- insert meters -------------------------------------------------------

  let readoutTarget: InsertTarget | null = null;
  let readoutTimer: ReturnType<typeof setInterval> | undefined;

  /**
   * Poll one chain's meters while it is visible, and only then.
   *
   * A gain-reduction bar wants frame rate; twenty knob positions do not. This
   * is the whole reason the engine splits `readouts` out of the parameter
   * values -- and the reason nothing polls when no chain with meters is up.
   */
  function watchReadouts(target: InsertTarget | null): void {
    const same =
      readoutTarget && target
        ? readoutTarget.kind === target.kind && readoutTarget.index === target.index
        : readoutTarget === target;
    if (same) return;
    readoutTarget = target;
    clearInterval(readoutTimer);
    readoutTimer = undefined;
    setReadouts([]);
    if (!target) return;
    const poll = () => {
      const wanted = readoutTarget;
      if (!wanted) return;
      backend
        .getChainReadouts(wanted)
        .then((next) => {
          // A slow answer that arrived after the target moved is not ours.
          if (readoutTarget === wanted) setReadouts(next);
        })
        .catch(() => {});
    };
    poll();
    readoutTimer = setInterval(poll, READOUT_POLL_MS);
  }

  // --- assets and the project ----------------------------------------------

  function startAssetDownload(id: string): void {
    backend
      .startAssetDownload(id)
      .then(setAssets)
      .then(() => setKernelError(null))
      .catch(report);
  }

  async function projectCommand(
    run: () => Promise<ProjectInfo | null>,
    label: string,
  ): Promise<void> {
    try {
      const info = await run();
      // `null` is a cancelled dialog: nothing changed, nothing to report.
      if (info === null) return;
      setProject(info);
      setKernelError(null);
      undo.clear();
      setUndoRevision(undo.revision);
      await refresh();
      if (label === 'new' || label === 'opened') setProjectRevision((revision) => revision + 1);
      say('Project', `${label} ${info.name}`);
      for (const warning of info.warnings) console.warn('[mydaw]', warning);
      if (info.warnings.length > 0) setKernelError(info.warnings.join(' · '));
    } catch (error) {
      report(error);
    }
  }

  function newProject(): void {
    void projectCommand(() => backend.newProject(), 'new');
  }

  function openProject(): void {
    void projectCommand(() => backend.openProject(), 'opened');
  }

  function saveProject(): void {
    void projectCommand(() => backend.saveProject(false), 'saved');
  }

  function saveProjectAs(): void {
    void projectCommand(() => backend.saveProject(true), 'saved');
  }

  // --- the meter stream ----------------------------------------------------

  let detach: (() => void) | undefined;
  let detachSession: (() => void) | undefined;
  if (backend.sharedHistory) {
    undo.setShared({ undoLabel: null, redoLabel: null });
    void backend.getSessionHistory().then((history) => {
      undo.setShared(history);
      setUndoRevision(undo.revision);
    }).catch(report);
    void backend.subscribeSession((change) => {
      undo.setShared(change.history);
      setUndoRevision(undo.revision);
      if (change.origin !== 'gui') {
        void flushPush().then(refresh).catch(report);
      }
    }).then((unsubscribe) => { detachSession = unsubscribe; }).catch(report);
  }

  function handleMeterFrame(frame: MeterFrame): void {
    meters.push(frame);
    playhead.update(frame.samplePos, frame.playing);
    // Only when it actually changed: this runs 30 times a second.
    if (frame.playing !== playing()) setPlaying(frame.playing);
  }

  async function resubscribeMeters(): Promise<void> {
    detach?.();
    detach = undefined;
    try {
      detach = await backend.subscribeMeters(handleMeterFrame);
    } catch (error) {
      report(error);
    }
  }

  // --- the computer-keyboard piano -----------------------------------------
  //
  // Held notes live in a map keyed on the *physical* key, so shifting octave
  // mid-hold still releases the note that was actually started.

  const held = new Map<string, number>();

  function releaseHeldNotes(): void {
    for (const key of held.values()) backend.noteOff(key).catch(() => {});
    held.clear();
    setHeldNotes([]);
  }

  function onKeyDown(event: KeyboardEvent): void {
    if (!isPianoEvent(event) || event.repeat) return;
    const key = midiKeyFor(event.code, octave());
    if (key === null || held.has(event.code)) return;
    event.preventDefault();
    held.set(event.code, key);
    setHeldNotes([...held.values()].sort((a, b) => a - b));
    backend.noteOn(key, DEFAULT_VELOCITY).catch(report);
  }

  function onKeyUp(event: KeyboardEvent): void {
    const key = held.get(event.code);
    if (key === undefined) return;
    held.delete(event.code);
    setHeldNotes([...held.values()].sort((a, b) => a - b));
    backend.noteOff(key).catch(() => {});
  }

  // --- global accelerators -------------------------------------------------
  //
  // Only the ones that belong to the window rather than to a view: the views
  // own their own tools, and a shortcut that fires from anywhere has to be one
  // a user means from anywhere.

  function onAccelerator(event: KeyboardEvent): void {
    if (event.defaultPrevented) return;
    const target = event.target as HTMLElement | null;
    const typing =
      target !== null &&
      (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.tagName === 'SELECT' || target.isContentEditable);
    if ((event.ctrlKey || event.metaKey) && !event.altKey) {
      const key = event.key.toLowerCase();
      if (key === 'n') {
        event.preventDefault();
        newProject();
        return;
      }
      if (key === 'o') {
        event.preventDefault();
        openProject();
        return;
      }
      if (key === 's') {
        event.preventDefault();
        if (event.shiftKey) saveProjectAs();
        else saveProject();
        return;
      }
      if (key === 'z' && !event.shiftKey) {
        event.preventDefault();
        undoLast();
        return;
      }
      if (key === 'y' || (key === 'z' && event.shiftKey)) {
        event.preventDefault();
        redoLast();
        return;
      }
    }
    const controlOwnsSpace = target?.closest('button, [role="button"], [role="slider"], [role="menuitem"]');
    if (event.code === 'Space' && !typing && !controlOwnsSpace && !event.ctrlKey && !event.metaKey) {
      event.preventDefault();
      togglePlay();
    }
  }

  // --- wiring --------------------------------------------------------------

  void refresh().then(() => {
    void resubscribeMeters();
    backend.getAssetStatus().then(setAssets).catch(() => {});
    // Once, at startup: the transport bar needs to know whether a REC button is
    // worth offering, and that is the only thing this answers while idle.
    void refreshInputDevices();
    // The dev-only IPC benchmark announces itself as a contributed panel; the
    // chunk is lazy, so a normal session never downloads it.
    if (snapshot()?.panels.some((panel) => panel.id === IPC_BENCH_AUTO_PANEL)) {
      void import('./dev/ipcBench')
        .then((bench) =>
          bench.runAutoMatrix({
            subscribeMeters: (onFrame) => backend.subscribeMeters(onFrame),
            play: () => backend.transportPlay(),
            stop: () => backend.transportStop(),
            onFrame: handleMeterFrame,
            restore: resubscribeMeters,
          }),
        )
        .catch(report);
    }
  });

  window.addEventListener('keydown', onAccelerator);
  window.addEventListener('keydown', onKeyDown);
  window.addEventListener('keyup', onKeyUp);
  window.addEventListener('blur', releaseHeldNotes);

  // Keep polling while a download is in flight, and only then.
  //
  // An instrument that becomes ready has to reach the source picker without the
  // user going and touching something else: installing is a background job that
  // finishes minutes after the click, and `mixerCommand` -- the only other
  // place that refetches sources -- may not run for the rest of the session. So
  // the poll watches for the transition itself and refetches once, on the tick
  // that sees it, rather than on every tick.
  const assetTimer = setInterval(() => {
    if (!assets().some((asset) => asset.state === 'downloading')) return;
    const before = assets()
      .filter((asset) => asset.state === 'ready')
      .map((asset) => asset.id);
    backend
      .getAssetStatus()
      .then((next) => {
        setAssets(next);
        const appeared = next.some(
          (asset) => asset.state === 'ready' && !before.includes(asset.id),
        );
        if (appeared) return backend.getSources().then(setSources);
        return undefined;
      })
      .catch(() => {});
  }, 500);

  createEffect(() => {
    document.title = `MYDAW — ${project().name}`;
  });

  onCleanup(() => {
    window.removeEventListener('keydown', onAccelerator);
    window.removeEventListener('keydown', onKeyDown);
    window.removeEventListener('keyup', onKeyUp);
    window.removeEventListener('blur', releaseHeldNotes);
    clearInterval(assetTimer);
    clearInterval(readoutTimer);
    clearInterval(inputTimer);
    // Release the capture device with the window. A microphone still held by a
    // reloading webview is one the next session cannot open.
    if (inputStatus()?.state !== 'idle') {
      void backend.setInputArmed(false).catch(() => {});
    }
    clearTimeout(pushTimer);
    clearTimeout(toastTimer);
    clearTimeout(masterGainTimer);
    releaseHeldNotes();
    detach?.();
    detachSession?.();
  });

  return {
    backend,
    meters,
    playhead,
    undo,
    kernelState,
    kernelError,
    snapshot,
    sequencer,
    mixer,
    effects,
    sources,
    audioSources,
    clapPlugins,
    vst3Plugins,
    scanning,
    project,
    projectRevision,
    streamInfo,
    refreshStreamInfo,
    assets,
    playing,
    octave,
    setOctave(next) {
      const clamped = clampOctave(next);
      setOctaveSignal(clamped);
      say('Keyboard · Octave', `C${clamped}`);
    },
    heldNotes,
    undoRevision,
    pattern,
    patterns: () => sequencer()?.patternSet.patterns ?? [],
    playlist: () => sequencer()?.playlist,
    tracks: () => mixer()?.tracks ?? [],
    playMode: () => sequencer()?.playMode ?? 'pat',
    bpm: () => sequencer()?.bpm ?? 130,
    channelKey: (channel: number) =>
      pattern()?.channels.find((clip) => clip.channel === channel)?.steps?.key ?? 60,
    promoteStepsToRoll(channel: number): number {
      const current = pattern();
      if (!current) return 0;
      const key =
        current.channels.find((clip) => clip.channel === channel)?.steps?.key ?? 60;
      const after = withStepsConvertedToNotes(current, channel, key);
      if (after === current) return 0;
      editPatternOnce('Convert to piano roll', (target) =>
        withStepsConvertedToNotes(target, channel, key),
      );
      const count = channelClip(after, channel)?.notes.length ?? 0;
      const name =
        mixer()?.tracks.find((track) => track.index === channel)?.name ?? 'Channel';
      say(`${name} · Piano roll`, `${count} steps became notes`);
      return count;
    },
    dumpSlicesToRoll(channel: number, rootKey: number, sliceCount: number): number {
      const current = pattern();
      if (!current) return 0;
      const after = withSlicesAsNotes(current, channel, rootKey, sliceCount);
      if (after === current) return 0;
      editPatternOnce('Slices to piano roll', (target) =>
        withSlicesAsNotes(target, channel, rootKey, sliceCount),
      );
      const count = channelClip(after, channel)?.notes.length ?? 0;
      const name =
        mixer()?.tracks.find((track) => track.index === channel)?.name ?? 'Channel';
      say(`${name} · Piano roll`, `${count} slice${count === 1 ? '' : 's'} became notes`);
      return count;
    },
    param,
    say,
    refresh,
    report,
    play,
    stop,
    togglePlay,
    seek,
    setPlayMode,
    looping: () => sequencer()?.looping ?? true,
    setLoop,
    setBpm,
    mixerCommand,
    setMasterGain,
    masterGainDb,
    sequencerCommand,
    startPatternGesture,
    editPattern,
    commitPatternGesture,
    editPatternOnce,
    startPlaylistGesture,
    commitPlaylistGesture,
    pushUndo,
    undoLast,
    redoLast,
    clearHistory,
    splitPatternByChannel,
    selectTrack,
    toggleGui,
    watchReadouts,
    readouts,
    inputDevices,
    inputStatus,
    refreshInputDevices,
    async setInputDevice(deviceId) {
      if (await inputCommand((b) => b.setInputDevice(deviceId))) {
        const name =
          deviceId === null
            ? 'System default'
            : (inputDevices().find((device) => device.id === deviceId)?.name ?? deviceId);
        say('Audio input', name);
      }
    },
    async setInputArmed(armed) {
      if (await inputCommand((b) => b.setInputArmed(armed))) {
        say('Audio input', armed ? `Armed · ${inputStatus()?.deviceName ?? ''}` : 'Released');
      }
    },
    async startRecording() {
      await inputCommand((b) => b.startRecording());
    },
    finishRecording,
    async cancelRecording() {
      if (await inputCommand((b) => b.cancelRecording())) {
        say('Recording', 'take discarded');
      }
    },
    loadDevices,
    refreshAudioSources,
    startAssetDownload,
    newProject,
    openProject,
    saveProject,
    saveProjectAs,
    resubscribeMeters,
  };
}
