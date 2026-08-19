import {
  createContext,
  createSignal,
  onCleanup,
  useContext,
  type Accessor,
  type JSX,
} from 'solid-js';
import { getBackend, type Backend } from './api/backend';
import type {
  MeterFrame,
  PanelManifestDto,
  PatternDto,
  PluginStatusDto,
  SourceDto,
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
  /** Low-rate mirror of the playhead, for text readouts (~10 Hz). */
  readonly positionSeconds: Accessor<number>;
  /** Octave the computer-keyboard piano is currently playing in. */
  readonly octave: Accessor<number>;
  /** MIDI key numbers currently held down, lowest first. */
  readonly heldNotes: Accessor<number[]>;
  /** Every instrument the engine can play, with the current one marked. */
  readonly sources: Accessor<SourceDto[]>;
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
  play(): void;
  stop(): void;
  refresh(): void;
  /** Put a different instrument at the head of the engine's chain. */
  selectSource(id: string): void;
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

/** Text readout refresh rate. Deliberately far below the meter frame rate. */
const READOUT_HZ = 10;

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

function createAppState(): AppState {
  const backend = getBackend();
  const meters = new MeterBus();
  const playhead = new Playhead();

  const [kernelState, setKernelState] = createSignal<KernelState>('connecting');
  const [kernelError, setKernelError] = createSignal<string | null>(null);
  const [plugins, setPlugins] = createSignal<PluginStatusDto[]>([]);
  const [panels, setPanels] = createSignal<PanelManifestDto[]>([]);
  const [playing, setPlaying] = createSignal(false);
  const [positionSeconds, setPositionSeconds] = createSignal(0);
  const [octave, setOctave] = createSignal(DEFAULT_OCTAVE);
  const [heldNotes, setHeldNotes] = createSignal<number[]>([]);
  const [sources, setSources] = createSignal<SourceDto[]>([]);
  const [pattern, setPattern] = createSignal<PatternDto | undefined>();
  const [bpm, setBpmSignal] = createSignal(130);

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

  const refresh = () => {
    setKernelState('connecting');
    void backend.getSources().then(setSources).catch(reportError);
    void backend.getPattern().then(adoptSequencerState).catch(reportError);
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

  const handleKeyDown = (event: KeyboardEvent) => {
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

  const readoutTimer = setInterval(() => {
    setPositionSeconds(playhead.positionSeconds());
  }, 1000 / READOUT_HZ);

  onCleanup(() => {
    disposed = true;
    detach?.();
    clearInterval(readoutTimer);
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

  const stop = () => {
    setPlaying(false);
    playhead.reset(0, false);
    setPositionSeconds(0);
    void backend.transportStop().catch((err: unknown) => {
      setKernelError(err instanceof Error ? err.message : String(err));
    });
  };

  /**
   * Switch instrument. Everything held is released first: the note-offs have to
   * reach the *old* instrument, or a key still down when the source changes
   * would leave a voice sounding on a node that no longer receives its release.
   */
  const selectSource = (id: string) => {
    if (currentSource()?.id === id) return;
    releaseAll();
    void backend
      .setSource(id)
      .then((listed) => {
        setSources(listed);
        setKernelError(null);
      })
      .catch(reportError);
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
    positionSeconds,
    octave,
    heldNotes,
    sources,
    currentSource,
    drumPadsActive,
    pattern,
    bpm,
    play,
    stop,
    refresh,
    selectSource,
    toggleStep,
    clearPattern,
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
