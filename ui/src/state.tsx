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
  ClapPluginInfoDto,
  ClapTarget,
  MeterFrame,
  MixerState,
  PanelManifestDto,
  PatternDto,
  PluginStatusDto,
  ProjectInfo,
  SourceDto,
  StreamInfo,
  Vst3PluginInfoDto,
} from './api/types';
import {
  clampOctave,
  DEFAULT_OCTAVE,
  DEFAULT_VELOCITY,
  drumPadKeyFor,
  isPianoEvent,
  midiKeyFor,
  OCTAVE_DOWN_CODE,
  OCTAVE_UP_CODE,
} from './lib/keyboard';
import { clearedPattern, withToggledStep } from './lib/pattern';
import { Playhead } from './lib/playhead';

/** Connection/boot state of the kernel, surfaced in the status bar. */
export type KernelState = 'connecting' | 'ready' | 'error';

/**
 * Meter frames arrive at 30-60 Hz. Routing them through a Solid signal would
 * re-run every dependent computation at that rate, so they land in a plain
 * mutable holder instead: canvas panels poll `latest` from their own
 * requestAnimationFrame loop, which is the only consumer that needs every
 * frame.
 */
export class MeterBus {
  latest: MeterFrame | null = null;

  push(frame: MeterFrame): void {
    this.latest = frame;
  }
}

/**
 * What the inspector is looking at.
 *
 * The engine's `selected` is a track index and nothing else -- it is what the
 * keyboard piano plays -- so the master strip needs a selection state the engine
 * has no field for. Modelling it as a union here rather than as "index, or -1"
 * keeps the two ideas from being confused at the one place that matters: only a
 * `track` selection is ever sent to `set_selected_track`.
 */
export type Selection = { kind: 'track'; index: number } | { kind: 'master' };

export interface AppState {
  readonly backend: Backend;
  readonly meters: MeterBus;
  readonly playhead: Playhead;
  readonly kernelState: Accessor<KernelState>;
  readonly kernelError: Accessor<string | null>;
  readonly plugins: Accessor<PluginStatusDto[]>;
  readonly panels: Accessor<PanelManifestDto[]>;
  /** Last known transport state, updated from meter frames and transport calls. */
  readonly playing: Accessor<boolean>;
  /** Octave the computer-keyboard piano is currently playing in. */
  readonly octave: Accessor<number>;
  /** MIDI key numbers currently held down, lowest first. */
  readonly heldNotes: Accessor<number[]>;
  /** Every instrument the engine can play, with the current one marked. */
  readonly sources: Accessor<SourceDto[]>;
  /**
   * The CLAP audio effects that may go on the master insert.
   *
   * Fetched here rather than by the mixer panel because it comes from the same
   * one-off scan `sources()` does, and a panel that is closed and reopened
   * should not pay for the walk again.
   */
  readonly masterEffects: Accessor<ClapPluginInfoDto[]>;
  /** The source currently playing, or `undefined` before the list arrives. */
  readonly currentSource: Accessor<SourceDto | undefined>;
  /**
   * Whether the drum pads (`C V B N`) are live, i.e. the drum kit is selected.
   * The piano keys work either way.
   */
  readonly drumPadsActive: Accessor<boolean>;
  /** The step grid, or `undefined` before the first fetch. */
  readonly pattern: Accessor<PatternDto | undefined>;
  /** Session tempo in beats per minute. */
  readonly bpm: Accessor<number>;
  /** The project's tracks and master strip, or `undefined` before the first fetch. */
  readonly mixer: Accessor<MixerState | undefined>;
  /**
   * Which project is open: the window title, the status bar, and the answer to
   * whether Save has anywhere to write.
   */
  readonly project: Accessor<ProjectInfo>;
  /** The open audio device, or `undefined` before the first fetch. */
  readonly streamInfo: Accessor<StreamInfo | undefined>;
  /** Every scanned CLAP plugin, both kinds. Fetched once, rescanned on demand. */
  readonly clapPlugins: Accessor<ClapPluginInfoDto[]>;
  /** Every scanned VST3 class, both kinds. */
  readonly vst3Plugins: Accessor<Vst3PluginInfoDto[]>;
  /** The VST3 classes eligible for the master insert. */
  readonly vst3MasterEffects: Accessor<Vst3PluginInfoDto[]>;
  /** Whether a plugin scan is in flight, so the button can say so. */
  readonly scanning: Accessor<boolean>;
  /** Downloadable instrument assets. Always empty when the backend has no such surface. */
  readonly assets: Accessor<AssetStatus[]>;
  /** Whether this backend implements the asset commands at all. */
  readonly assetsSupported: boolean;
  /** What the inspector is looking at: one track, or the master strip. */
  readonly selection: Accessor<Selection>;
  play(): void;
  stop(): void;
  refresh(): void;
  /**
   * Make sure the scanned device lists have been fetched at least once.
   *
   * Called by the browser's Devices tab and by the device palette the first time
   * either is opened. Idempotent, and free after the first call.
   */
  ensureDevices(): void;
  /** Re-walk the plugin directories, then refresh every scanned list. */
  rescanPlugins(): void;
  /**
   * Point the inspector at a track or the master.
   *
   * A track selection is also pushed to the engine, because that is what decides
   * which instrument the computer-keyboard piano plays. Selecting the master
   * leaves the engine's selection where it was: there is no instrument on the
   * master to play, and silently moving the piano's target because somebody
   * clicked a fader would be a surprise.
   */
  select(selection: Selection): void;
  /** Add a track running the default built-in instrument. */
  addTrack(): void;
  /** Remove one track. Rejects for the last one, reported in the status bar. */
  removeTrack(index: number): void;
  /** Start one asset's download. A no-op when the backend has no asset surface. */
  startAssetDownload(id: string): void;
  /**
   * The four project commands.
   *
   * Each replaces or writes the whole session, so each is a `void` action that
   * reports through `kernelError` rather than something to await: there is
   * nothing for a caller to do with the answer that is not already done here.
   * A cancelled dialog changes nothing and says nothing.
   */
  newProject(): void;
  openProject(): void;
  /** Overwrite the open project, or ask where to put one that has no path yet. */
  saveProject(): void;
  /** Always ask where to write, even for a project already on disk. */
  saveProjectAs(): void;
  /**
   * Put `source` at the head of `track`, whichever kind of source it is.
   *
   * The dispatch lives here rather than in each picker because both the mixer
   * strip and the transport bar offer the same mixed list: a built-in is named
   * by id, and a CLAP instrument is placed by bundle path plus plugin id,
   * because the host has to load a file rather than look a name up in a table.
   */
  placeSource(track: number, source: SourceDto): void;
  /**
   * Open or close one placed plugin's native editor window.
   *
   * Several can be open at once, so this addresses one slot rather than "the"
   * editor. The mixer is refreshed afterwards because `guiOpen` lives on the
   * slot inside `MixerState`, and the button's label is drawn from it.
   */
  toggleClapGui(target: ClapTarget, open: boolean): void;
  /**
   * Open or close whatever plugin is in `target`, in whichever format it is.
   *
   * The dispatch is here rather than at each button because the answer lives in
   * `MixerState` -- a slot is a `clap` or a `vst3`, never both -- and a component
   * that decided for itself would be a second copy of that rule.
   */
  toggleGui(target: ClapTarget, open: boolean): void;
  /**
   * Run one mixer command and adopt the state it returns.
   *
   * Every mixer command answers with the whole `MixerState`, so this is the one
   * place the panel's state is written and there is no partial update to merge.
   * A rejected edit re-fetches, which is the only case where the UI and the
   * engine can really disagree.
   */
  mixerCommand(run: (backend: Backend) => Promise<MixerState>): void;
  /**
   * Toggle one cell of the grid. `accent` is the shift-click variant; see
   * `lib/pattern.ts` for exactly what each does.
   *
   * The local grid updates immediately and the backend is told once the user
   * stops clicking (see `PATTERN_PUSH_DEBOUNCE_MS`) -- a click has to feel
   * instant, and a drag across four cells is one message, not four.
   */
  toggleStep(rowIndex: number, stepIndex: number, accent: boolean): void;
  /** Switch every cell off. */
  clearPattern(): void;
  /**
   * Replace the pattern with `edit(current)` and push it, debounced.
   *
   * The general form of `toggleStep` and `clearPattern`, and what the piano
   * roll uses for all of its editing: a note is placed, moved, resized or
   * deleted by handing in the pure function that does it (see `lib/pattern.ts`),
   * so this file stays the one place that knows *when* an edit reaches Rust and
   * never grows a second copy of *what* an edit is.
   *
   * Returning the pattern unchanged -- which every helper does for an edit that
   * would not be legal -- is a no-op all the way down: same object, same
   * reference, nothing redrawn and nothing sent.
   */
  editPattern(edit: (pattern: PatternDto) => PatternDto): void;
  /** Set the session tempo. Out-of-range values are reported, not clamped. */
  setBpm(bpm: number): void;
  /**
   * Re-attach the app's own meter handler.
   *
   * Only one subscriber exists on the Rust side, so anything that takes the
   * meter stream over for a while (today: the IPC benchmark) has to give it
   * back.
   */
  resubscribeMeters(): Promise<void>;
}

const AppStateContext = createContext<AppState>();

/**
 * How long the grid waits after the last click before telling the backend.
 *
 * Long enough that dragging a row of hi-hats is one message; short enough that
 * a single click is published before the ear could notice. The local grid is
 * updated immediately either way, so this is never felt as latency.
 */
const PATTERN_PUSH_DEBOUNCE_MS = 50;

/**
 * Manifest id the shell contributes under `MYDAW_IPC_BENCH=1`. Its presence in
 * the snapshot is the whole auto-run signal: no extra command, and no cost at
 * all in a normal session, where the bench module is never even imported.
 * Mirrors `IPC_BENCH_AUTO_PANEL_ID` in `app/src/ipc_bench.rs`.
 */
const IPC_BENCH_AUTO_PANEL = 'ipc-bench-auto';

/**
 * What the title and the status bar say before the first `getProjectInfo`
 * answers. Mirrors `ProjectInfoDto::untitled` in `app/src/project_bridge.rs`: a
 * session nobody has saved looks exactly like a session nobody has asked about
 * yet, so the window title never flickers through a placeholder on the way to
 * the truth.
 */
const UNTITLED_PROJECT: ProjectInfo = {
  path: null,
  name: 'Untitled',
  missingPlugins: 0,
  warnings: [],
};

function createAppState(): AppState {
  const backend = getBackend();
  const meters = new MeterBus();
  const playhead = new Playhead();

  const [kernelState, setKernelState] = createSignal<KernelState>('connecting');
  const [kernelError, setKernelError] = createSignal<string | null>(null);
  const [plugins, setPlugins] = createSignal<PluginStatusDto[]>([]);
  const [panels, setPanels] = createSignal<PanelManifestDto[]>([]);
  const [playing, setPlaying] = createSignal(false);
  const [octave, setOctave] = createSignal(DEFAULT_OCTAVE);
  const [heldNotes, setHeldNotes] = createSignal<number[]>([]);
  const [sources, setSources] = createSignal<SourceDto[]>([]);
  const [masterEffects, setMasterEffects] = createSignal<ClapPluginInfoDto[]>([]);
  const [pattern, setPattern] = createSignal<PatternDto | undefined>();
  const [bpm, setBpmSignal] = createSignal(130);
  const [mixer, setMixer] = createSignal<MixerState | undefined>();
  const [project, setProject] = createSignal<ProjectInfo>(UNTITLED_PROJECT);
  const [streamInfo, setStreamInfo] = createSignal<StreamInfo | undefined>();
  const [clapPlugins, setClapPlugins] = createSignal<ClapPluginInfoDto[]>([]);
  const [vst3Plugins, setVst3Plugins] = createSignal<Vst3PluginInfoDto[]>([]);
  const [vst3MasterEffects, setVst3MasterEffects] = createSignal<Vst3PluginInfoDto[]>([]);
  const [scanning, setScanning] = createSignal(false);
  const [assets, setAssets] = createSignal<AssetStatus[]>([]);
  const [selection, setSelection] = createSignal<Selection>({ kind: 'track', index: 0 });

  const assetsSupported = typeof backend.getAssetStatus === 'function';

  const currentSource = () => sources().find((source) => source.selected);
  const drumPadsActive = () => currentSource()?.id === 'drum-kit';

  const reportError = (err: unknown) =>
    setKernelError(err instanceof Error ? err.message : String(err));

  /** Adopt a `SequencerState` from the backend, including the device rate. */
  const adoptSequencerState = (state: {
    bpm: number;
    sampleRate: number;
    pattern: PatternDto;
  }) => {
    setBpmSignal(state.bpm);
    setPattern(state.pattern);
    // Reported as 0 when no device is open; the playhead keeps its default.
    if (state.sampleRate > 0) playhead.setSampleRate(state.sampleRate);
  };

  /**
   * The scanned device lists: every CLAP plugin, every VST3 class, and the two
   * effect subsets the master insert picker offers.
   *
   * Deliberately **not** part of `refresh()`. `get_vst3_plugins` opens every
   * installed `.vst3` module the first time it is called, which on a stocked
   * machine is the slowest thing MYDAW does; paying for it at launch would make
   * every session wait for a list most of them never look at. So it runs the
   * first time something needs it -- the browser's Devices tab, or the device
   * palette -- and `scanning()` is what those two draw while it does.
   *
   * `getSources` and `getMasterEffects` are refreshed alongside because they are
   * served from the same cache: a rescan that left them alone would hide a
   * newly installed instrument from the Sounds tab until the next launch.
   */
  let devicesRequested = false;
  const loadDevices = (rescan: boolean) => {
    if (devicesRequested && !rescan) return;
    devicesRequested = true;
    setScanning(true);
    void Promise.all([
      backend.getClapPlugins(rescan).then(setClapPlugins),
      backend.getVst3Plugins(rescan).then(setVst3Plugins),
      backend.getMasterEffects().then(setMasterEffects),
      backend.getVst3MasterEffects().then(setVst3MasterEffects),
      backend.getSources().then(setSources),
    ])
      .then(() => setKernelError(null))
      .catch(reportError)
      .finally(() => setScanning(false));
  };

  const refreshAssets = () => {
    if (!backend.getAssetStatus) return;
    void backend.getAssetStatus().then(setAssets).catch(reportError);
  };

  const refresh = () => {
    setKernelState('connecting');
    void backend.getSources().then(setSources).catch(reportError);
    void backend.getMasterEffects().then(setMasterEffects).catch(reportError);
    void backend.getTracks().then(setMixer).catch(reportError);
    void backend.getPattern().then(adoptSequencerState).catch(reportError);
    void backend.getProjectInfo().then(setProject).catch(reportError);
    void backend.getStreamInfo().then(setStreamInfo).catch(reportError);
    refreshAssets();
    // A scan already paid for stays paid for across a project load.
    if (devicesRequested) loadDevices(false);
    backend
      .getUiSnapshot()
      .then((snapshot) => {
        setPlugins(snapshot.plugins);
        setPanels(snapshot.panels);
        setKernelError(null);
        setKernelState('ready');
        if (snapshot.panels.some((panel) => panel.id === IPC_BENCH_AUTO_PANEL)) {
          startAutoBench();
        }
      })
      .catch((err: unknown) => {
        setKernelError(err instanceof Error ? err.message : String(err));
        setKernelState('error');
      });
  };

  // Meter subscription: feeds the bus (for canvases) and the playhead anchor.
  let detach: (() => void) | undefined;
  let disposed = false;

  const handleMeterFrame = (frame: MeterFrame) => {
    meters.push(frame);
    playhead.update(frame.samplePos, frame.playing);
    if (frame.playing !== playing()) setPlaying(frame.playing);
  };

  const subscribeMeters = (): Promise<void> =>
    backend
      .subscribeMeters(handleMeterFrame)
      .then((unsubscribe) => {
        if (disposed) unsubscribe();
        else detach = unsubscribe;
      })
      .catch((err: unknown) => {
        setKernelError(err instanceof Error ? err.message : String(err));
      });

  /**
   * Dev-only (`MYDAW_IPC_BENCH=1`): run the whole transport matrix, write the
   * results and let Rust close the app. The module is imported dynamically, so
   * a normal build never loads a byte of it, and the guard keeps a second
   * `refresh()` from starting a second run over the top of the first.
   */
  let autoBenchStarted = false;
  const startAutoBench = () => {
    if (autoBenchStarted) return;
    autoBenchStarted = true;
    void import('./dev/ipcBench').then((bench) =>
      bench.runAutoMatrix({
        subscribeMeters: (onFrame) => backend.subscribeMeters(onFrame),
        play: () => backend.transportPlay(),
        stop: () => backend.transportStop(),
        onFrame: handleMeterFrame,
        restore: subscribeMeters,
      }),
    );
  };

  // --- the computer-keyboard piano ---------------------------------------
  //
  // Held notes live in a Map from the *physical key* to the MIDI key it started,
  // not in the signal: the note-off has to release whatever the key-down played,
  // even if the octave has been shifted in between. The signal is only what the
  // status bar renders.
  const held = new Map<string, number>();

  const publishHeld = () => setHeldNotes([...held.values()].sort((a, b) => a - b));

  const reportNoteError = reportError;

  const releaseKey = (code: string) => {
    const key = held.get(code);
    if (key === undefined) return;
    held.delete(code);
    publishHeld();
    void backend.noteOff(key).catch(reportNoteError);
  };

  /** Release everything. Used when focus leaves, so no note can stick on. */
  const releaseAll = () => {
    for (const code of [...held.keys()]) releaseKey(code);
  };

  /** Send a note-on and remember which physical key started it. */
  const pressKey = (code: string, key: number) => {
    if (held.has(code)) return;
    held.set(code, key);
    publishHeld();
    void backend.noteOn(key, DEFAULT_VELOCITY).catch((err: unknown) => {
      // The engine never got the note, so the UI must not claim it is held.
      held.delete(code);
      publishHeld();
      reportNoteError(err);
    });
  };

  /**
   * The project accelerators: Ctrl+N, Ctrl+O, Ctrl+S and Ctrl+Shift+S.
   *
   * Deliberately ahead of `isPianoEvent`, and not filtered by it: that guard's
   * whole job is to keep Ctrl chords *out* of the piano, so asking it about one
   * would always answer no. For the same reason these fire while a field has
   * focus -- Ctrl+S in the tempo box is Save, in every editor anybody has used.
   *
   * `event.code`, like the piano, so a shortcut stays under the same finger on
   * a French or Dvorak layout. `preventDefault` unconditionally, because these
   * are the webview's own Save and Open otherwise, and returning true is how
   * the caller knows the event was spoken for.
   */
  const handleProjectShortcut = (event: KeyboardEvent): boolean => {
    if (!event.ctrlKey || event.altKey || event.metaKey) return false;
    let run: () => void;
    if (event.code === 'KeyS') run = event.shiftKey ? saveProjectAs : saveProject;
    // Save As is the only shifted one, so any other Shift+chord is somebody
    // else's -- Ctrl+Shift+N is the browser's new window, not a new project.
    else if (event.shiftKey) return false;
    else if (event.code === 'KeyO') run = openProject;
    else if (event.code === 'KeyN') run = newProject;
    else return false;
    event.preventDefault();
    // Held down, this repeats at the OS key-repeat rate. One press is one save.
    if (!event.repeat) run();
    return true;
  };

  const handleKeyDown = (event: KeyboardEvent) => {
    if (handleProjectShortcut(event)) return;
    // Auto-repeat would retrigger the note dozens of times a second.
    if (event.repeat || !isPianoEvent(event)) return;

    // Drum pads first, and only while the kit is selected: `C V B N` mean
    // nothing to the other instruments, and leaving them inert there keeps the
    // status-bar hint honest.
    if (drumPadsActive()) {
      const pad = drumPadKeyFor(event.code);
      if (pad !== null) {
        event.preventDefault();
        pressKey(event.code, pad);
        return;
      }
    }

    if (event.code === OCTAVE_DOWN_CODE || event.code === OCTAVE_UP_CODE) {
      const delta = event.code === OCTAVE_UP_CODE ? 1 : -1;
      setOctave((current) => clampOctave(current + delta));
      event.preventDefault();
      return;
    }

    const key = midiKeyFor(event.code, octave());
    if (key === null) return;
    event.preventDefault();
    pressKey(event.code, key);
  };

  const handleKeyUp = (event: KeyboardEvent) => releaseKey(event.code);

  if (typeof window !== 'undefined') {
    window.addEventListener('keydown', handleKeyDown);
    window.addEventListener('keyup', handleKeyUp);
    // Alt-tabbing away with a key down never delivers its keyup.
    window.addEventListener('blur', releaseAll);
  }

  refresh();
  void subscribeMeters();

  /**
   * Keep the selection inside the project.
   *
   * A load, or a `remove_track` that the engine refused differently from what
   * the optimistic adjustment assumed, can leave the inspector pointed at a row
   * that is no longer there. Falling back to the engine's own selection rather
   * than to zero keeps the keyboard piano and the inspector looking at the same
   * track, which is the invariant the whole selection exists to hold.
   */
  createEffect(() => {
    const state = mixer();
    if (!state) return;
    const current = selection();
    if (current.kind !== 'track') return;
    if (current.index >= 0 && current.index < state.tracks.length) return;
    setSelection({ kind: 'track', index: state.selected });
  });

  onCleanup(() => {
    disposed = true;
    detach?.();
    if (assetTimer !== undefined) clearInterval(assetTimer);
    if (pushTimer !== undefined) clearTimeout(pushTimer);
    if (typeof window !== 'undefined') {
      window.removeEventListener('keydown', handleKeyDown);
      window.removeEventListener('keyup', handleKeyUp);
      window.removeEventListener('blur', releaseAll);
    }
    releaseAll();
  });

  const play = () => {
    setPlaying(true);
    void backend.transportPlay().catch((err: unknown) => {
      setPlaying(false);
      setKernelError(err instanceof Error ? err.message : String(err));
    });
  };

  /**
   * Stop the transport, leaving the playhead where it is.
   *
   * Deliberately no rewind. `transport_stop` keeps the position -- the Rust
   * comment on it says in as many words that the UI decides what Stop means
   * visually -- and there is no seek command to move it with. Zeroing the local
   * playhead here (which the previous shell did) made the readout flick to
   * `00:00.000` and then jump back the moment the next meter frame re-anchored
   * it to where the engine really was. A readout that lies for one frame is
   * worse than one that tells you the transport is parked at bar 3.
   */
  const stop = () => {
    setPlaying(false);
    playhead.update(playhead.positionSamples(), false);
    void backend.transportStop().catch((err: unknown) => {
      setKernelError(err instanceof Error ? err.message : String(err));
    });
  };

  // --- the project --------------------------------------------------------

  /**
   * Run one project command and adopt what it answers with.
   *
   * Held notes are released first, for the same reason `mixerCommand` does it
   * and rather more urgently: the instrument that received the note-on is about
   * to be destroyed, and a note-off sent after that reaches nothing at all.
   *
   * A `null` answer is the user closing the dialog. Nothing changed, so nothing
   * is adopted and nothing is reported -- a cancelled Save that left a line in
   * the status bar would read as a failed one.
   *
   * Success re-runs the whole `refresh()` rather than any single fetch, because
   * a load replaces the mixer, the sources, the pattern and the tempo in one
   * go. There is no partial update that would be right, and refreshing the one
   * thing the caller happened to think of is how a stale strip survives a load.
   */
  const projectCommand = (run: (api: Backend) => Promise<ProjectInfo | null>) => {
    releaseAll();
    void run(backend)
      .then((info) => {
        if (!info) return;
        setProject(info);
        setKernelError(null);
        refresh();
      })
      .catch(reportError);
  };

  const newProject = () => projectCommand((api) => api.newProject());
  const openProject = () => projectCommand((api) => api.openProject());
  const saveProject = () => projectCommand((api) => api.saveProject(false));
  const saveProjectAs = () => projectCommand((api) => api.saveProject(true));

  /**
   * The window title is the project's name.
   *
   * Worth an effect of its own because the title is the one piece of UI that is
   * not this app's to draw: it is what the taskbar and the alt-tab list show,
   * and it is how somebody with two MYDAW windows open tells them apart.
   *
   * No dirty marker. Nothing tracks unsaved edits yet, and an asterisk that
   * never appeared would be a promise the app does not keep -- worse than not
   * offering one.
   */
  createEffect(() => {
    if (typeof document !== 'undefined') document.title = `MYDAW — ${project().name}`;
  });

  /**
   * Ask for one plugin editor to be opened or closed.
   *
   * Unlike the mixer commands this answers with a `ClapGuiStatus` rather than a
   * whole `MixerState`, but the strips draw the button from the slot's
   * `guiOpen`, so the mixer is re-fetched either way -- including on failure,
   * where the window may well have opened before whatever went wrong.
   */
  const toggleClapGui = (target: ClapTarget, open: boolean) => {
    const request = open ? backend.openClapGui(target) : backend.closeClapGui(target);
    void request
      .then(() => setKernelError(null))
      // The message is the backend's own sentence -- most often "This plugin has
      // no editor.", which is an answer rather than a fault.
      .catch(reportError)
      .finally(() => {
        void backend.getTracks().then(setMixer).catch(reportError);
      });
  };

  /**
   * Run one mixer command and adopt the state it answers with.
   *
   * Held notes are released first for the same reason `selectSource` does it:
   * a command can change which track the keyboard addresses (or what is on the
   * one it already addresses), and a note-off has to reach the instrument that
   * received the note-on.
   */
  const mixerCommand = (run: (backend: Backend) => Promise<MixerState>) => {
    releaseAll();
    void run(backend)
      .then((state) => {
        setMixer(state);
        setKernelError(null);
        // Selection and per-track sources both move the "which instrument is
        // playing" answer the transport bar shows.
        void backend.getSources().then(setSources).catch(reportError);
      })
      .catch((err: unknown) => {
        reportError(err);
        void backend.getTracks().then(setMixer).catch(reportError);
      });
  };

  /**
   * Put a source at the head of a track, dispatching on how it is placed.
   *
   * Goes through `mixerCommand` for all three kinds, which releases every held
   * note first: the note-offs have to reach the *old* instrument, or a key still
   * down when the source changes leaves a voice sounding on a node that no
   * longer receives its release.
   */
  const placeSource = (track: number, source: SourceDto) => {
    if (source.kind === 'clap' || source.kind === 'vst3') {
      const { bundlePath, pluginId } = source;
      if (bundlePath === null || pluginId === null) {
        // Rust sets both on every hosted source, so this is a contract breach
        // rather than a user-facing condition -- reported, not silently ignored.
        reportError(
          new Error(`'${source.label}' is a ${source.kind} source with no bundle to load`),
        );
        return;
      }
      mixerCommand((api) =>
        source.kind === 'clap'
          ? api.setTrackSourceClap(track, bundlePath, pluginId)
          : api.setTrackSourceVst3(track, bundlePath, pluginId),
      );
      return;
    }
    mixerCommand((api) => api.setTrackSource(track, source.id));
  };

  /**
   * Point the inspector at a track or at the master.
   *
   * The engine is only told about a *track*: its `selected` is which instrument
   * the keyboard piano plays, and the master has no instrument. Clicking the
   * master fader must not silently move where the next key press sounds.
   */
  const select = (next: Selection) => {
    setSelection(next);
    if (next.kind !== 'track') return;
    if (mixer()?.selected === next.index) return;
    mixerCommand((api) => api.setSelectedTrack(next.index));
  };

  /**
   * Add a track and select it.
   *
   * The selection is moved *after* the reply rather than optimistically: until
   * the engine has answered, `mixer()` still describes a project the new index
   * is out of range of, and the clamp below would helpfully put the selection
   * straight back where it was.
   */
  const addTrack = () => {
    releaseAll();
    const number = (mixer()?.tracks.length ?? 0) + 1;
    void backend
      .addTrack('poly-synth', `Track ${number}`)
      .then((state) => {
        setMixer(state);
        const index = state.tracks.length - 1;
        setSelection({ kind: 'track', index });
        setKernelError(null);
        return backend.setSelectedTrack(index);
      })
      .then(setMixer)
      .then(() => backend.getSources().then(setSources))
      .catch((err: unknown) => {
        reportError(err);
        void backend.getTracks().then(setMixer).catch(reportError);
      });
  };

  const removeTrack = (index: number) => {
    mixerCommand((api) => api.removeTrack(index));
    setSelection((current) => {
      if (current.kind !== 'track') return current;
      // Everything above the hole shifts down; the removed row's own slot is
      // taken by its successor, or by the last track when it was the last row.
      if (current.index > index) return { kind: 'track', index: current.index - 1 };
      return current;
    });
  };

  const startAssetDownload = (id: string) => {
    const start = backend.startAssetDownload;
    if (!start) return;
    void start
      .call(backend, id)
      .then(setAssets)
      .then(() => setKernelError(null))
      .catch(reportError);
  };

  /**
   * Poll the asset list while anything is downloading.
   *
   * A poll rather than a stream because a download is a minutes-long operation
   * whose progress bar is happy at 2 Hz, and one more `Channel` on the Rust side
   * is a cost this does not justify. Stops the moment nothing is in flight.
   */
  let assetTimer: ReturnType<typeof setInterval> | undefined;
  createEffect(() => {
    const busy = assets().some((asset) => asset.state === 'downloading');
    if (busy && assetTimer === undefined) {
      assetTimer = setInterval(refreshAssets, 500);
    } else if (!busy && assetTimer !== undefined) {
      clearInterval(assetTimer);
      assetTimer = undefined;
    }
  });

  /**
   * Open or close whichever format sits in `target`.
   *
   * The slot in `MixerState` is the authority: a track has a `clap` or a `vst3`
   * or neither, and only the mixer knows which. Asking the wrong command is not
   * harmless -- `open_vst3_gui` on a CLAP slot rejects with a sentence about a
   * missing VST3 plugin, which would read as a broken button.
   */
  const toggleGui = (target: ClapTarget, open: boolean) => {
    const state = mixer();
    const slotOwner =
      target.slot === 'master' ? state?.master : state?.tracks[target.index];
    const isVst3 = !!slotOwner?.vst3;
    if (!isVst3) {
      toggleClapGui(target, open);
      return;
    }
    const request = open ? backend.openVst3Gui(target) : backend.closeVst3Gui(target);
    void request
      .then(() => setKernelError(null))
      .catch(reportError)
      .finally(() => {
        void backend.getTracks().then(setMixer).catch(reportError);
      });
  };

  // --- the step grid -----------------------------------------------------
  //
  // The local pattern is what the grid renders and what gets pushed; the
  // backend's reply is deliberately *not* adopted on success. A reply that
  // arrived while the user was still clicking would overwrite edits made since
  // the request was sent, and it never carries anything new -- Rust echoes what
  // it was given. On failure the whole state is re-fetched instead, which is
  // the one case where the two really can disagree.
  let pushTimer: ReturnType<typeof setTimeout> | undefined;

  const pushPattern = () => {
    const current = pattern();
    if (!current) return;
    void backend
      .setPattern(current)
      .then(() => setKernelError(null))
      .catch((err: unknown) => {
        reportError(err);
        void backend.getPattern().then(adoptSequencerState).catch(reportError);
      });
  };

  const schedulePush = () => {
    if (pushTimer !== undefined) clearTimeout(pushTimer);
    pushTimer = setTimeout(() => {
      pushTimer = undefined;
      pushPattern();
    }, PATTERN_PUSH_DEBOUNCE_MS);
  };

  const toggleStep = (rowIndex: number, stepIndex: number, accent: boolean) => {
    setPattern((current) =>
      current ? withToggledStep(current, rowIndex, stepIndex, accent) : current,
    );
    schedulePush();
  };

  const clearPattern = () => {
    setPattern((current) => (current ? clearedPattern(current) : current));
    schedulePush();
  };

  const editPattern = (edit: (pattern: PatternDto) => PatternDto) => {
    const before = pattern();
    if (!before) return;
    const after = edit(before);
    // Reference equality, which every helper in `lib/pattern.ts` preserves for
    // an edit it refused. A drag that has not yet crossed a grid line produces
    // dozens of these a second, and none of them should redraw or be sent.
    if (after === before) return;
    setPattern(after);
    schedulePush();
  };

  const setBpm = (next: number) => {
    if (next === bpm()) return;
    const previous = bpm();
    // Optimistic: the tempo readout and the moving step highlight both follow
    // this signal, and waiting a round trip to move a number the user just
    // typed reads as a stuck control.
    setBpmSignal(next);
    void backend
      .setBpm(next)
      .then((stored) => {
        setBpmSignal(stored);
        setKernelError(null);
      })
      .catch((err: unknown) => {
        setBpmSignal(previous);
        reportError(err);
      });
  };

  return {
    backend,
    meters,
    playhead,
    kernelState,
    kernelError,
    plugins,
    panels,
    playing,
    octave,
    heldNotes,
    sources,
    masterEffects,
    currentSource,
    drumPadsActive,
    pattern,
    bpm,
    mixer,
    project,
    streamInfo,
    clapPlugins,
    vst3Plugins,
    vst3MasterEffects,
    scanning,
    assets,
    assetsSupported,
    selection,
    play,
    stop,
    refresh,
    ensureDevices: () => loadDevices(false),
    rescanPlugins: () => loadDevices(true),
    select,
    addTrack,
    removeTrack,
    startAssetDownload,
    toggleGui,
    newProject,
    openProject,
    saveProject,
    saveProjectAs,
    placeSource,
    toggleClapGui,
    mixerCommand,
    toggleStep,
    clearPattern,
    editPattern,
    setBpm,
    resubscribeMeters: subscribeMeters,
  };
}

export function AppStateProvider(props: { children: JSX.Element }): JSX.Element {
  const state = createAppState();
  return <AppStateContext.Provider value={state}>{props.children}</AppStateContext.Provider>;
}

export function useAppState(): AppState {
  const state = useContext(AppStateContext);
  if (!state) throw new Error('useAppState must be used inside <AppStateProvider>');
  return state;
}
