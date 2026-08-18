import type { MeterFrame, UiSnapshot, Unsubscribe } from './types';

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
  transportPlay(): Promise<void>;
  transportStop(): Promise<void>;
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

  async transportPlay(): Promise<void> {
    const { invoke } = await this.core();
    await invoke('transport_play');
  }

  async transportStop(): Promise<void> {
    const { invoke } = await this.core();
    await invoke('transport_stop');
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
    { id: 'mixer', title: 'Mixer', component: 'mixer-view' },
  ],
};

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

  async getUiSnapshot(): Promise<UiSnapshot> {
    // Structured clone so panels cannot mutate the shared fixture.
    return structuredClone(MOCK_SNAPSHOT);
  }

  async transportPlay(): Promise<void> {
    this.playing = true;
  }

  async transportStop(): Promise<void> {
    this.playing = false;
    this.samplePos = 0;
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
