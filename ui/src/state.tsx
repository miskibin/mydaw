import {
  createContext,
  createSignal,
  onCleanup,
  useContext,
  type Accessor,
  type JSX,
} from 'solid-js';
import { getBackend, type Backend } from './api/backend';
import type { MeterFrame, PanelManifestDto, PluginStatusDto } from './api/types';
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
  play(): void;
  stop(): void;
  refresh(): void;
}

const AppStateContext = createContext<AppState>();

/** Text readout refresh rate. Deliberately far below the meter frame rate. */
const READOUT_HZ = 10;

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

  const refresh = () => {
    setKernelState('connecting');
    backend
      .getUiSnapshot()
      .then((snapshot) => {
        setPlugins(snapshot.plugins);
        setPanels(snapshot.panels);
        setKernelError(null);
        setKernelState('ready');
      })
      .catch((err: unknown) => {
        setKernelError(err instanceof Error ? err.message : String(err));
        setKernelState('error');
      });
  };

  refresh();

  // Meter subscription: feeds the bus (for canvases) and the playhead anchor.
  let detach: (() => void) | undefined;
  let disposed = false;
  backend
    .subscribeMeters((frame) => {
      meters.push(frame);
      playhead.update(frame.samplePos, frame.playing);
      if (frame.playing !== playing()) setPlaying(frame.playing);
    })
    .then((unsubscribe) => {
      if (disposed) unsubscribe();
      else detach = unsubscribe;
    })
    .catch((err: unknown) => {
      setKernelError(err instanceof Error ? err.message : String(err));
    });

  const readoutTimer = setInterval(() => {
    setPositionSeconds(playhead.positionSeconds());
  }, 1000 / READOUT_HZ);

  onCleanup(() => {
    disposed = true;
    detach?.();
    clearInterval(readoutTimer);
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
    play,
    stop,
    refresh,
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
