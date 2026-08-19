import type {
  ClapGuiStatus,
  ClapPluginInfoDto,
  MeterFrame,
  MixerState,
  PatternDto,
  SequencerState,
  SourceDto,
  TrackDto,
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
  /**
   * Every instrument the engine can play, with the current one marked.
   *
   * Fetched once at startup; `setSource` returns the same list, so the UI never
   * has to guess what the engine did.
   */
  getSources(): Promise<SourceDto[]>;
  /**
   * Put a different instrument at the head of the chain.
   *
   * Resolves with the refreshed list. Rejects with a status-bar sentence when
   * the id is unknown, no audio device is open, or `clap` was asked for on a
   * session with no CLAP instrument loaded.
   */
  setSource(id: string): Promise<SourceDto[]>;

  // --- the mixer -----------------------------------------------------------
  //
  // Every one of these resolves with the whole `MixerState`, so the panel never
  // merges a partial update into what it already had. A strip is a handful of
  // numbers and the IPC spike measured a message this size as free.

  /** The project's tracks, the master strip and the current selection. */
  getTracks(): Promise<MixerState>;
  /**
   * Set a track's fader, in dB (`minGainDb..=maxGainDb`).
   *
   * Succeeds with no audio device open -- a fader is project data. The engine
   * ramps the change over 10 ms, so dragging is as cheap as clicking.
   */
  setTrackGain(track: number, db: number): Promise<MixerState>;
  /** Set a track's pan, `-1` hard left to `1` hard right. */
  setTrackPan(track: number, pan: number): Promise<MixerState>;
  /** Mute or unmute a track. A mute is a 10 ms fade, not a cut. */
  setTrackMute(track: number, muted: boolean): Promise<MixerState>;
  /** Solo or un-solo a track. Anything soloed silences everything that is not. */
  setTrackSolo(track: number, soloed: boolean): Promise<MixerState>;
  /** Set the master fader, in dB. */
  setMasterGain(db: number): Promise<MixerState>;
  /** Choose the track the keyboard piano plays and the source selector targets. */
  setSelectedTrack(track: number): Promise<MixerState>;
  /** Put a different instrument at the head of one track, by index. */
  setTrackSource(track: number, source: string): Promise<MixerState>;
  /**
   * Add a track. Rejects past the engine's limit, and while a hosted CLAP
   * plugin is in the graph (`canEditLayout` is false then).
   */
  addTrack(source: string, name?: string): Promise<MixerState>;
  /** Remove a track. Rejects for the last one, and while `canEditLayout` is false. */
  removeTrack(track: number): Promise<MixerState>;

  /**
   * The step grid and the session tempo, in one message.
   *
   * Fetched once at startup. `setPattern` and `setBpm` return the same shape (or
   * the tempo alone), so the UI never has to guess what the engine did.
   */
  getPattern(): Promise<SequencerState>;
  /**
   * Replace the whole step grid.
   *
   * A whole-pattern replacement rather than a per-step edit: the grid is about a
   * kilobyte of JSON, and one message per edit has no ordering problems and no
   * diff to get wrong. Succeeds with no audio device open -- a pattern is
   * project data. Rejects with a status-bar sentence naming the offending row.
   */
  setPattern(pattern: PatternDto): Promise<SequencerState>;
  /**
   * Set the session tempo, resolving with the value that was stored.
   *
   * Rejects rather than clamping when the value is outside 20..999, so a typo
   * is reported instead of silently rounded.
   */
  setBpm(bpm: number): Promise<number>;
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

  async getSources(): Promise<SourceDto[]> {
    const { invoke } = await this.core();
    return invoke<SourceDto[]>('get_sources');
  }

  async setSource(id: string): Promise<SourceDto[]> {
    const { invoke } = await this.core();
    return invoke<SourceDto[]>('set_source', { source: id });
  }

  async getTracks(): Promise<MixerState> {
    const { invoke } = await this.core();
    return invoke<MixerState>('get_tracks');
  }

  async setTrackGain(track: number, db: number): Promise<MixerState> {
    const { invoke } = await this.core();
    return invoke<MixerState>('set_track_gain', { track, db });
  }

  async setTrackPan(track: number, pan: number): Promise<MixerState> {
    const { invoke } = await this.core();
    return invoke<MixerState>('set_track_pan', { track, pan });
  }

  async setTrackMute(track: number, muted: boolean): Promise<MixerState> {
    const { invoke } = await this.core();
    return invoke<MixerState>('set_track_mute', { track, muted });
  }

  async setTrackSolo(track: number, soloed: boolean): Promise<MixerState> {
    const { invoke } = await this.core();
    return invoke<MixerState>('set_track_solo', { track, soloed });
  }

  async setMasterGain(db: number): Promise<MixerState> {
    const { invoke } = await this.core();
    return invoke<MixerState>('set_master_gain', { db });
  }

  async setSelectedTrack(track: number): Promise<MixerState> {
    const { invoke } = await this.core();
    return invoke<MixerState>('set_selected_track', { track });
  }

  async setTrackSource(track: number, source: string): Promise<MixerState> {
    const { invoke } = await this.core();
    return invoke<MixerState>('set_track_source', { track, source });
  }

  async addTrack(source: string, name?: string): Promise<MixerState> {
    const { invoke } = await this.core();
    return invoke<MixerState>('add_track', { source, name: name ?? null });
  }

  async removeTrack(track: number): Promise<MixerState> {
    const { invoke } = await this.core();
    return invoke<MixerState>('remove_track', { track });
  }

  async getPattern(): Promise<SequencerState> {
    const { invoke } = await this.core();
    return invoke<SequencerState>('get_pattern');
  }

  async setPattern(pattern: PatternDto): Promise<SequencerState> {
    const { invoke } = await this.core();
    return invoke<SequencerState>('set_pattern', { pattern });
  }

  async setBpm(bpm: number): Promise<number> {
    const { invoke } = await this.core();
    return invoke<number>('set_bpm', { bpm });
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
    { id: 'sequencer', name: 'Step Sequencer', state: 'started' },
    { id: 'host-clap', name: 'CLAP Host', state: 'registered' },
    { id: 'host-vst3', name: 'VST3 Host', state: 'stopped' },
  ],
  panels: [
    { id: 'kernel-status', title: 'Kernel Status', component: 'kernel-status' },
    { id: 'mixer', title: 'Mixer', component: 'mixer' },
    { id: 'meters', title: 'Master Meters', component: 'meters' },
    { id: 'step-sequencer', title: 'Step Sequencer', component: 'step-sequencer' },
    { id: 'clap-browser', title: 'CLAP Plugins', component: 'clap-browser' },
  ],
};

/**
 * The mixer's own limits, mirroring `MAX_TRACKS`, `SILENCE_DB` and
 * `MAX_GAIN_DB` in `crates/rt-core/src/mixer.rs`.
 */
const MOCK_MAX_TRACKS = 16;
const MOCK_MIN_GAIN_DB = -60;
const MOCK_MAX_GAIN_DB = 6;

/**
 * The same two tracks Rust starts with. Mirrors `Project::default` in
 * `app/src/project.rs` -- a mock that greeted you with a different project
 * would be a worse rehearsal than no mock at all.
 */
function mockTracks(): TrackDto[] {
  return [
    {
      index: 0,
      id: 0,
      name: 'Drums',
      sourceId: 'drum-kit',
      sourceLabel: 'Drum Kit',
      gainDb: 0,
      pan: 0,
      muted: false,
      soloed: false,
      silenced: false,
      selected: true,
    },
    {
      index: 1,
      id: 1,
      name: 'Synth',
      sourceId: 'poly-synth',
      sourceLabel: 'Poly Synth',
      gainDb: 0,
      pan: 0,
      muted: false,
      soloed: false,
      silenced: false,
      selected: false,
    },
  ];
}

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
 * The same four sources Rust offers, in the same order, with `clap`
 * unavailable -- a browser session has no `MYDAW_CLAP_INSTRUMENT`, so this is
 * also the state the selector's disabled path has to look right in.
 *
 * Mirrors `Source::ALL` in `app/src/source.rs`.
 */
const MOCK_SOURCES: readonly SourceDto[] = [
  {
    id: 'poly-synth',
    label: 'Poly Synth',
    description: 'Eight-voice saw + sub synth, played from the keyboard',
    available: true,
    selected: true,
    takesNotes: true,
  },
  {
    id: 'drum-kit',
    label: 'Drum Kit',
    description: 'Synthesised kick, snare and hi-hats on GM keys 36/38/42/46',
    available: true,
    selected: false,
    takesNotes: true,
  },
  {
    id: 'test-tone',
    label: 'Test Tone',
    description: '440 Hz sine, gated on the transport',
    available: true,
    selected: false,
    takesNotes: false,
  },
  {
    id: 'clap',
    label: 'CLAP',
    description: 'The CLAP instrument named by MYDAW_CLAP_INSTRUMENT',
    available: false,
    selected: false,
    takesNotes: true,
  },
];

/** Steps per pattern. Mirrors `STEPS` in `plugins/sequencer/src/pattern.rs`. */
const MOCK_STEPS = 16;

/** Tempo bounds and default, mirroring `crates/rt-core/src/transport.rs`. */
const MOCK_DEFAULT_BPM = 130;
const MOCK_MIN_BPM = 20;
const MOCK_MAX_BPM = 999;

/**
 * The same four empty drum rows Rust starts with, labels included -- the labels
 * come from Rust in a real session, and hard-coding a *different* set here would
 * make the mock a worse rehearsal than no mock at all.
 *
 * Mirrors `Pattern::default` in `plugins/sequencer/src/pattern.rs`.
 */
function mockPattern(): PatternDto {
  const rows: [number, string][] = [
    [36, 'Kick'],
    [38, 'Snare'],
    [42, 'CH'],
    [46, 'OH'],
  ];
  return {
    steps: MOCK_STEPS,
    rows: rows.map(([key, label]) => ({
      key,
      channel: 0,
      label,
      steps: Array.from({ length: MOCK_STEPS }, () => ({ on: false, velocity: 0.78 })),
    })),
  };
}

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
  private bpm = MOCK_DEFAULT_BPM;
  private pattern: PatternDto = mockPattern();
  private tracks: TrackDto[] = mockTracks();
  private masterGainDb = 0;
  private selected = 0;
  private nextTrackId = 2;

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

  async getSources(): Promise<SourceDto[]> {
    const current = this.tracks[this.selected]?.sourceId;
    return MOCK_SOURCES.map((source) => ({ ...source, selected: source.id === current }));
  }

  /**
   * There is no engine in a browser, so this only records the choice and says
   * so. The selector's own logic -- disabled options, the keyboard hint that
   * follows the selection -- is what is being developed against the mock.
   */
  async setSource(id: string): Promise<SourceDto[]> {
    await this.setTrackSource(this.selected, id);
    return this.getSources();
  }

  // --- the mixer -----------------------------------------------------------

  /** The current mixer, cloned so a panel cannot mutate the mock behind its back. */
  private mixerState(): MixerState {
    const anySoloed = this.tracks.some((track) => track.soloed);
    return {
      tracks: this.tracks.map((track, index) => ({
        ...track,
        index,
        silenced: anySoloed ? !track.soloed : track.muted,
        selected: index === this.selected,
      })),
      master: { gainDb: this.masterGainDb },
      selected: this.selected,
      maxTracks: MOCK_MAX_TRACKS,
      minGainDb: MOCK_MIN_GAIN_DB,
      maxGainDb: MOCK_MAX_GAIN_DB,
      canEditLayout: true,
    };
  }

  /** Reject an index the panel should never have sent, the way Rust does. */
  private track(index: number): TrackDto {
    const track = this.tracks[index];
    if (!track) throw new Error(`there is no track ${index}`);
    return track;
  }

  private checkGain(db: number): number {
    if (!Number.isFinite(db) || db < MOCK_MIN_GAIN_DB || db > MOCK_MAX_GAIN_DB) {
      throw new Error(
        `gain ${db} dB is not a number between ${MOCK_MIN_GAIN_DB} and ${MOCK_MAX_GAIN_DB}`,
      );
    }
    return db;
  }

  async getTracks(): Promise<MixerState> {
    return this.mixerState();
  }

  async setTrackGain(track: number, db: number): Promise<MixerState> {
    this.track(track).gainDb = this.checkGain(db);
    return this.mixerState();
  }

  async setTrackPan(track: number, pan: number): Promise<MixerState> {
    if (!Number.isFinite(pan) || pan < -1 || pan > 1) {
      throw new Error(`pan ${pan} is not a number between -1.0 and 1.0`);
    }
    this.track(track).pan = pan;
    return this.mixerState();
  }

  async setTrackMute(track: number, muted: boolean): Promise<MixerState> {
    this.track(track).muted = muted;
    return this.mixerState();
  }

  async setTrackSolo(track: number, soloed: boolean): Promise<MixerState> {
    this.track(track).soloed = soloed;
    return this.mixerState();
  }

  async setMasterGain(db: number): Promise<MixerState> {
    this.masterGainDb = this.checkGain(db);
    return this.mixerState();
  }

  async setSelectedTrack(track: number): Promise<MixerState> {
    this.track(track);
    this.selected = track;
    return this.mixerState();
  }

  async setTrackSource(track: number, source: string): Promise<MixerState> {
    const wanted = MOCK_SOURCES.find((entry) => entry.id === source);
    if (!wanted) throw new Error(`unknown source '${source}'`);
    if (!wanted.available) {
      throw new Error(
        'no CLAP instrument is loaded; set MYDAW_CLAP_INSTRUMENT before starting MYDAW',
      );
    }
    const def = this.track(track);
    def.sourceId = wanted.id;
    def.sourceLabel = wanted.label;
    console.info(`[mock] set_track_source ${track} ${source}`);
    return this.mixerState();
  }

  async addTrack(source: string, name?: string): Promise<MixerState> {
    if (this.tracks.length >= MOCK_MAX_TRACKS) {
      throw new Error(`a project may have at most ${MOCK_MAX_TRACKS} tracks`);
    }
    const wanted = MOCK_SOURCES.find((entry) => entry.id === source);
    if (!wanted) throw new Error(`unknown source '${source}'`);
    this.tracks.push({
      index: this.tracks.length,
      id: this.nextTrackId++,
      name: name ?? wanted.label,
      sourceId: wanted.id,
      sourceLabel: wanted.label,
      gainDb: 0,
      pan: 0,
      muted: false,
      soloed: false,
      silenced: false,
      selected: false,
    });
    return this.mixerState();
  }

  async removeTrack(track: number): Promise<MixerState> {
    this.track(track);
    if (this.tracks.length === 1) throw new Error('a project must keep at least one track');
    this.tracks.splice(track, 1);
    if (this.selected >= this.tracks.length) this.selected = this.tracks.length - 1;
    return this.mixerState();
  }

  /** Exposed so a panel test can assert the mock's view of the editor. */
  isGuiOpen(): boolean {
    return this.guiOpen;
  }

  async getPattern(): Promise<SequencerState> {
    // Structured clone so the panel cannot mutate the mock's own state behind
    // its back -- the real backend gives out a fresh object every call.
    return {
      bpm: this.bpm,
      sampleRate: MOCK_SAMPLE_RATE,
      pattern: structuredClone(this.pattern),
    };
  }

  /**
   * Stores the grid in memory and validates it the way Rust does, so the
   * panel's error path can be exercised in a plain browser.
   */
  async setPattern(pattern: PatternDto): Promise<SequencerState> {
    if (pattern.steps !== MOCK_STEPS) {
      throw new Error(`a pattern must have exactly ${MOCK_STEPS} steps, got ${pattern.steps}`);
    }
    const bad = pattern.rows.findIndex(
      (row) =>
        row.steps.length !== MOCK_STEPS ||
        row.steps.some((step) => !Number.isFinite(step.velocity) || step.velocity < 0 || step.velocity > 1),
    );
    if (bad >= 0) throw new Error(`row ${bad} is not a valid pattern row`);

    this.pattern = structuredClone(pattern);
    return this.getPattern();
  }

  async setBpm(bpm: number): Promise<number> {
    if (!Number.isFinite(bpm) || bpm < MOCK_MIN_BPM || bpm > MOCK_MAX_BPM) {
      throw new Error(`tempo ${bpm} is not a number between ${MOCK_MIN_BPM} and ${MOCK_MAX_BPM}`);
    }
    this.bpm = bpm;
    return this.bpm;
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
      // One meter per track, each on its own LFO and honouring that track's
      // fader, pan and effective mute -- so the mixer's strips move
      // independently and a mute visibly empties one.
      const state = this.mixerState();
      const tracks = state.tracks.map((track, index) => {
        if (track.silenced) return { peakL: 0, peakR: 0 };
        const gain = track.gainDb <= MOCK_MIN_GAIN_DB ? 0 : 10 ** (track.gainDb / 20);
        const level = env * gain * (0.5 + 0.45 * Math.abs(Math.sin(t * (0.7 + index * 0.31))));
        const theta = ((track.pan + 1) * Math.PI) / 4;
        return {
          peakL: level * Math.SQRT2 * Math.cos(theta),
          peakR: level * Math.SQRT2 * Math.sin(theta),
        };
      });
      onFrame({
        peakL,
        peakR,
        rmsL: peakL * 0.62,
        rmsR: peakR * 0.62,
        samplePos: this.samplePos,
        playing: this.playing,
        tracks,
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
