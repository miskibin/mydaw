import type {
  ClapGuiStatus,
  ClapPluginInfoDto,
  MeterFrame,
  UiSnapshot,
  Unsubscribe,
} from './types';

/**
 * The only surface the UI is allowed to use to reach the kernel.
 *
 * Two implementations exist: `TauriBackend` (real IPC) and `MockBackend`
 * (pure-browser dev + tests). Panels must never import `@tauri-apps/api`
 * directly -- everything funnels through here so `pnpm dev` in a plain browser
 * keeps working.
 */
export interface Backend {
  /** Human-readable name, shown in the status bar. */
  readonly kind: 'tauri' | 'mock';
  /** One-shot fetch of plugins + contributed panels. */
  getUiSnapshot(): Promise<UiSnapshot>;
  /**
   * Installed CLAP plugins. The first call makes the backend scan the standard
   * plugin directories, which can take a moment on a well-stocked machine;
   * afterwards it is served from a cache. Pass `rescan` to walk them again.
   */
  getClapPlugins(rescan?: boolean): Promise<ClapPluginInfoDto[]>;
  /**
   * Open the loaded CLAP effect's editor in its own native window.
   *
   * Rejects with a sentence meant for the status bar -- most often "This plugin
   * has no editor.", which is a normal answer rather than a failure.
   */
  openClapGui(): Promise<ClapGuiStatus>;
  /** Close the plugin editor. Resolves even when nothing was open. */
  closeClapGui(): Promise<ClapGuiStatus>;
  transportPlay(): Promise<void>;
  transportStop(): Promise<void>;
  /**
   * Play a note on the loaded instrument.
   *
   * `key` is a MIDI key number (60 = middle C) and `velocity` is `0..1`. Sounds
   * whether or not the transport is running -- this is live input, not
   * sequenced playback. Rejects with a status-bar sentence when no audio device
   * is open or the values are out of range.
   */
  noteOn(key: number, velocity: number): Promise<void>;
  /** Release a note. Harmless if that key was never pressed. */
  noteOff(key: number): Promise<void>;
  /** Subscribe to the throttled meter stream. Call the result to detach. */
  subscribeMeters(onFrame: (frame: MeterFrame) => void): Promise<Unsubscribe>;
}

/** True when running inside a Tauri v2 webview. */
export function isTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

// ---------------------------------------------------------------------------
// Tauri backend
// ---------------------------------------------------------------------------

/**
 * Real backend. `@tauri-apps/api` is imported dynamically so that a plain
 * browser session never evaluates it (its module init assumes the injected
 * internals object exists).
 */
class TauriBackend implements Backend {
  readonly kind = 'tauri' as const;

  private async core(): Promise<typeof import('@tauri-apps/api/core')> {
    return import('@tauri-apps/api/core');
  }

  async getUiSnapshot(): Promise<UiSnapshot> {
    const { invoke } = await this.core();
    return invoke<UiSnapshot>('get_ui_snapshot');
  }

  async getClapPlugins(rescan = false): Promise<ClapPluginInfoDto[]> {
    const { invoke } = await this.core();
    return invoke<ClapPluginInfoDto[]>('get_clap_plugins', { rescan });
  }

  async openClapGui(): Promise<ClapGuiStatus> {
    const { invoke } = await this.core();
    return invoke<ClapGuiStatus>('open_clap_gui');
  }

  async closeClapGui(): Promise<ClapGuiStatus> {
    const { invoke } = await this.core();
    return invoke<ClapGuiStatus>('close_clap_gui');
  }

  async transportPlay(): Promise<void> {
    const { invoke } = await this.core();
    await invoke('transport_play');
  }

  async transportStop(): Promise<void> {
    const { invoke } = await this.core();
    await invoke('transport_stop');
  }

  async noteOn(key: number, velocity: number): Promise<void> {
    const { invoke } = await this.core();
    await invoke('note_on', { key, velocity });
  }

  async noteOff(key: number): Promise<void> {
    const { invoke } = await this.core();
    await invoke('note_off', { key });
  }

  async subscribeMeters(onFrame: (frame: MeterFrame) => void): Promise<Unsubscribe> {
    const { Channel, invoke } = await this.core();
    const channel = new Channel<MeterFrame>();
    channel.onmessage = onFrame;
    // The Rust command holds the channel and feeds it from the bridge's
    // throttled meter queue until the webview drops it.
    await invoke('subscribe_meters', { channel });
    return () => {
      channel.onmessage = () => {};
    };
  }
}

// ---------------------------------------------------------------------------
// Mock backend
// ---------------------------------------------------------------------------

const MOCK_SNAPSHOT: UiSnapshot = {
  plugins: [
    { id: 'kernel', name: 'Microkernel', state: 'started' },
    { id: 'audio-io', name: 'Audio I/O (WASAPI)', state: 'started' },
    { id: 'metering', name: 'Metering', state: 'started' },
    { id: 'test-tone', name: 'Test Tone', state: 'registered' },
    { id: 'host-clap', name: 'CLAP Host', state: 'registered' },
    { id: 'host-vst3', name: 'VST3 Host', state: 'stopped' },
  ],
  panels: [
    { id: 'kernel-status', title: 'Kernel Status', component: 'kernel-status' },
    { id: 'meters', title: 'Master Meters', component: 'meters' },
    { id: 'clap-browser', title: 'CLAP Plugins', component: 'clap-browser' },
    { id: 'mixer', title: 'Mixer', component: 'mixer-view' },
  ],
};

/**
 * Two plausible plugins, one of them with no vendor or version, because the
 * CLAP format allows that and the panel has to render it without collapsing.
 */
const MOCK_FIXTURE_ID = 'dev.mydaw.fixture.gain';

const MOCK_CLAP_PLUGINS: ClapPluginInfoDto[] = [
  {
    id: 'com.example.reverb',
    name: 'Example Reverb',
    vendor: 'Example Audio',
    version: '2.1.0',
    path: 'C:\\Program Files\\Common Files\\CLAP\\ExampleReverb.clap',
  },
  {
    id: MOCK_FIXTURE_ID,
    name: 'MYDAW Fixture Gain',
    vendor: '',
    version: '',
    path: 'D:\\MYDAW\\target\\debug\\mydaw-gain.clap',
  },
];

const MOCK_SAMPLE_RATE = 48_000;
const MOCK_FRAME_HZ = 30;

/**
 * Browser-only stand-in. Emits a plausible 30 Hz meter stream driven by a pair
 * of detuned sines so the meter panel and playhead interpolation can be
 * developed without the Rust side running.
 */
class MockBackend implements Backend {
  readonly kind = 'mock' as const;
  private playing = false;
  private samplePos = 0;
  private guiOpen = false;

  async getUiSnapshot(): Promise<UiSnapshot> {
    // Structured clone so panels cannot mutate the shared fixture.
    return structuredClone(MOCK_SNAPSHOT);
  }

  async getClapPlugins(_rescan = false): Promise<ClapPluginInfoDto[]> {
    // Structured clone so panels cannot mutate the shared fixture.
    return structuredClone(MOCK_CLAP_PLUGINS);
  }

  /**
   * There is no native window in a browser, so this only records the state and
   * says so in the console. The panel's own logic is what is being developed
   * against the mock; the window is the Rust side's business.
   */
  async openClapGui(): Promise<ClapGuiStatus> {
    this.guiOpen = true;
    console.info('[mock] open_clap_gui: no native window outside Tauri');
    return { open: true, pluginId: MOCK_FIXTURE_ID, width: 400, height: 250 };
  }

  async closeClapGui(): Promise<ClapGuiStatus> {
    this.guiOpen = false;
    console.info('[mock] close_clap_gui');
    return { open: false, pluginId: MOCK_FIXTURE_ID, width: 0, height: 0 };
  }

  async transportPlay(): Promise<void> {
    this.playing = true;
  }

  async transportStop(): Promise<void> {
    this.playing = false;
    this.samplePos = 0;
  }

  /**
   * There is no engine in a browser, so this only logs. The piano's own logic
   * (mapping, held notes, octave shifting) is what is being developed against
   * the mock; making a sound is the Rust side's business.
   */
  async noteOn(key: number, velocity: number): Promise<void> {
    console.info(`[mock] note_on key=${key} velocity=${velocity.toFixed(2)}`);
  }

  async noteOff(key: number): Promise<void> {
    console.info(`[mock] note_off key=${key}`);
  }

  /** Exposed so a panel test can assert the mock's view of the editor. */
  isGuiOpen(): boolean {
    return this.guiOpen;
  }

  async subscribeMeters(onFrame: (frame: MeterFrame) => void): Promise<Unsubscribe> {
    const periodMs = 1000 / MOCK_FRAME_HZ;
    const framesPerTick = Math.round(MOCK_SAMPLE_RATE / MOCK_FRAME_HZ);
    const start = performance.now();

    const timer = setInterval(() => {
      if (this.playing) {
        this.samplePos += framesPerTick;
      }
      const t = (performance.now() - start) / 1000;
      const env = this.playing ? 1 : 0;
      // Two slow LFOs at different rates so L/R visibly diverge.
      const peakL = env * (0.55 + 0.4 * Math.abs(Math.sin(t * 1.1)));
      const peakR = env * (0.55 + 0.4 * Math.abs(Math.sin(t * 1.37 + 0.6)));
      onFrame({
        peakL,
        peakR,
        rmsL: peakL * 0.62,
        rmsR: peakR * 0.62,
        samplePos: this.samplePos,
        playing: this.playing,
      });
    }, periodMs);

    return () => clearInterval(timer);
  }
}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

let cached: Backend | undefined;

/**
 * Returns the process-wide backend, picking the real one when the Tauri
 * internals are injected and falling back to the mock in a plain browser.
 */
export function getBackend(): Backend {
  if (!cached) {
    cached = isTauri() ? new TauriBackend() : new MockBackend();
  }
  return cached;
}

export { MockBackend, TauriBackend };
