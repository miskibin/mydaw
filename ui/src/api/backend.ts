import type {
  ClapGuiStatus,
  ClapPluginInfoDto,
  ClapSlot,
  ClapTarget,
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
   * Open one placed plugin's editor in its own native window.
   *
   * Every placed plugin has its own window and they are independent, so opening
   * a second one does not close the first. Rejects with a sentence meant for the
   * status bar -- most often "This plugin has no editor.", which is a normal
   * answer rather than a failure, and which the mixer avoids provoking by only
   * offering the button when the slot's `hasEditor` is true.
   */
  openClapGui(target: ClapTarget): Promise<ClapGuiStatus>;
  /** Close one plugin's editor. Resolves even when nothing was open. */
  closeClapGui(target: ClapTarget): Promise<ClapGuiStatus>;
  /**
   * The placed plugin's own opaque state, base64-encoded.
   *
   * An empty string is a real answer: the plugin does not implement the CLAP
   * state extension, so there is nothing to save and nothing was lost.
   */
  saveClapState(target: ClapTarget): Promise<string>;
  /** Restore what `saveClapState` returned. Rejects if the plugin refuses it. */
  loadClapState(target: ClapTarget, stateBase64: string): Promise<void>;
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
  /** Put a different built-in instrument at the head of one track, by index. */
  setTrackSource(track: number, source: string): Promise<MixerState>;
  /**
   * Put a hosted CLAP instrument at the head of one track.
   *
   * Separate from `setTrackSource` because placing one means loading a bundle
   * from disk and instantiating it, which can fail in ways a built-in cannot:
   * the file may have moved, or the bundle may no longer export that id. The
   * bundle path *and* the plugin id are both sent because one `.clap` file can
   * export several plugins.
   */
  setTrackSourceClap(
    track: number,
    bundlePath: string,
    pluginId: string,
  ): Promise<MixerState>;
  /**
   * The scanned CLAP plugins eligible for the master insert, i.e. the audio
   * effects. A separate call from `getSources` because the two lists are drawn
   * from the same scan but never overlap.
   */
  getMasterEffects(): Promise<ClapPluginInfoDto[]>;
  /** Put a CLAP audio effect on the master insert, replacing whatever was there. */
  setMasterEffectClap(bundlePath: string, pluginId: string): Promise<MixerState>;
  /** Take the master insert out. Resolves even when the slot was already empty. */
  clearMasterEffect(): Promise<MixerState>;
  /** Add a track. Rejects past the engine's limit. */
  addTrack(source: string, name?: string): Promise<MixerState>;
  /** Remove a track. Rejects for the last one. */
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

  // Tauri v2 rewrites camelCase argument keys to the Rust parameter's
  // snake_case name, so `bundlePath` here is `bundle_path` there. Sending
  // snake_case from this side is what would *not* arrive.

  async openClapGui(target: ClapTarget): Promise<ClapGuiStatus> {
    const { invoke } = await this.core();
    return invoke<ClapGuiStatus>('open_clap_gui', { target });
  }

  async closeClapGui(target: ClapTarget): Promise<ClapGuiStatus> {
    const { invoke } = await this.core();
    return invoke<ClapGuiStatus>('close_clap_gui', { target });
  }

  async saveClapState(target: ClapTarget): Promise<string> {
    const { invoke } = await this.core();
    return invoke<string>('save_clap_state', { target });
  }

  async loadClapState(target: ClapTarget, stateBase64: string): Promise<void> {
    const { invoke } = await this.core();
    await invoke('load_clap_state', { target, stateBase64 });
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

  async setTrackSourceClap(
    track: number,
    bundlePath: string,
    pluginId: string,
  ): Promise<MixerState> {
    const { invoke } = await this.core();
    return invoke<MixerState>('set_track_source_clap', { track, bundlePath, pluginId });
  }

  async getMasterEffects(): Promise<ClapPluginInfoDto[]> {
    const { invoke } = await this.core();
    return invoke<ClapPluginInfoDto[]>('get_master_effects');
  }

  async setMasterEffectClap(bundlePath: string, pluginId: string): Promise<MixerState> {
    const { invoke } = await this.core();
    return invoke<MixerState>('set_master_effect_clap', { bundlePath, pluginId });
  }

  async clearMasterEffect(): Promise<MixerState> {
    const { invoke } = await this.core();
    return invoke<MixerState>('clear_master_effect');
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
      clap: null,
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
      clap: null,
    },
  ];
}

/**
 * Four plausible plugins: two instruments and two audio effects, so both
 * pickers have something to show and neither is exercised only in its empty
 * state.
 *
 * The fixture gain declares no vendor, no version and one bare feature, because
 * the CLAP format allows all of that and the browser has to render it without
 * collapsing. It is also the one with no editor, so the mixer's "this plugin
 * has no editor button" path is reachable in a plain browser.
 */
const MOCK_FIXTURE_GAIN_ID = 'dev.mydaw.fixture.gain';

const MOCK_CLAP_PLUGINS: ClapPluginInfoDto[] = [
  {
    id: 'dev.mydaw.fixture.synth',
    name: 'MYDAW Fixture Synth',
    vendor: 'MYDAW',
    version: '0.1.0',
    path: 'D:\\MYDAW\\target\\debug\\mydaw-synth.clap',
    kind: 'instrument',
    features: ['instrument', 'synthesizer', 'stereo'],
  },
  {
    id: 'com.example.polybox',
    name: 'Example PolyBox',
    vendor: 'Example Audio',
    version: '3.0.2',
    path: 'C:\\Program Files\\Common Files\\CLAP\\ExamplePolyBox.clap',
    kind: 'instrument',
    features: ['instrument', 'sampler'],
  },
  {
    id: 'com.example.reverb',
    name: 'Example Reverb',
    vendor: 'Example Audio',
    version: '2.1.0',
    path: 'C:\\Program Files\\Common Files\\CLAP\\ExampleReverb.clap',
    kind: 'audio-effect',
    features: ['audio-effect', 'reverb'],
  },
  {
    id: MOCK_FIXTURE_GAIN_ID,
    name: 'MYDAW Fixture Gain',
    vendor: '',
    version: '',
    path: 'D:\\MYDAW\\target\\debug\\mydaw-gain.clap',
    kind: 'audio-effect',
    features: ['audio-effect'],
  },
];

/**
 * Whether a mock plugin draws an editor. Only the fixture gain does not -- see
 * `MOCK_CLAP_PLUGINS`.
 */
function mockHasEditor(plugin: ClapPluginInfoDto): boolean {
  return plugin.id !== MOCK_FIXTURE_GAIN_ID;
}

/**
 * The id a placed CLAP plugin reports as a track's `sourceId`. Mirrors the
 * `clap:` prefix Rust builds in `app/src/source.rs`; the UI compares these
 * strings, so the two spellings have to agree exactly.
 */
function mockClapSourceId(pluginId: string): string {
  return `clap:${pluginId}`;
}

/** One scanned CLAP instrument, as `get_sources` would offer it. */
function mockClapSource(plugin: ClapPluginInfoDto): SourceDto {
  return {
    id: mockClapSourceId(plugin.id),
    label: plugin.name || plugin.id,
    description: plugin.vendor
      ? `Hosted CLAP instrument by ${plugin.vendor}`
      : 'Hosted CLAP instrument',
    kind: 'clap',
    vendor: plugin.vendor,
    bundlePath: plugin.path,
    pluginId: plugin.id,
    selected: false,
    takesNotes: true,
  };
}

const MOCK_SAMPLE_RATE = 48_000;
const MOCK_FRAME_HZ = 30;

/**
 * The three built-in sources Rust offers, in the same fixed order it lists them
 * in. The scanned CLAP instruments are appended after these at call time, which
 * is exactly what `get_sources` does.
 *
 * Mirrors `Source::BUILT_IN` in `app/src/source.rs`.
 */
const MOCK_INTERNAL_SOURCES: readonly SourceDto[] = [
  {
    id: 'poly-synth',
    label: 'Poly Synth',
    description: 'Eight-voice saw + sub synth, played from the keyboard',
    kind: 'internal',
    vendor: '',
    bundlePath: null,
    pluginId: null,
    selected: true,
    takesNotes: true,
  },
  {
    id: 'drum-kit',
    label: 'Drum Kit',
    description: 'Synthesised kick, snare and hi-hats on GM keys 36/38/42/46',
    kind: 'internal',
    vendor: '',
    bundlePath: null,
    pluginId: null,
    selected: false,
    takesNotes: true,
  },
  {
    id: 'test-tone',
    label: 'Test Tone',
    description: '440 Hz sine, gated on the transport',
    kind: 'internal',
    vendor: '',
    bundlePath: null,
    pluginId: null,
    selected: false,
    takesNotes: false,
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
  private bpm = MOCK_DEFAULT_BPM;
  private pattern: PatternDto = mockPattern();
  private tracks: TrackDto[] = mockTracks();
  private masterGainDb = 0;
  private masterClap: ClapSlot | null = null;
  private selected = 0;
  private nextTrackId = 2;
  /**
   * Instance handles, handed out the way the Rust host does: never reused, so a
   * stale handle is recognisably stale rather than silently addressing whatever
   * took the slot next.
   */
  private nextInstance = 1;
  /**
   * Which editors are up, keyed by target. A *set*, not a single flag: several
   * plugins can have their windows open at once, which is the case the mixer's
   * per-strip buttons exist to reach and therefore the case the mock has to be
   * able to rehearse.
   */
  private openEditors = new Set<string>();
  /** The last state `loadClapState` was handed, per target. */
  private clapStates = new Map<string, string>();

  async getUiSnapshot(): Promise<UiSnapshot> {
    // Structured clone so panels cannot mutate the shared fixture.
    return structuredClone(MOCK_SNAPSHOT);
  }

  async getClapPlugins(_rescan = false): Promise<ClapPluginInfoDto[]> {
    // Structured clone so panels cannot mutate the shared fixture.
    return structuredClone(MOCK_CLAP_PLUGINS);
  }

  /**
   * A stable string per target, used to key the open-editor set and the saved
   * states. Targets are compared by value, and two objects describing the same
   * slot must not count as two different editors.
   */
  private static targetKey(target: ClapTarget): string {
    return target.slot === 'master' ? 'master' : `track:${target.index}`;
  }

  /** The slot a target names, rejecting an empty one the way Rust does. */
  private requireSlot(target: ClapTarget): ClapSlot {
    const slot = target.slot === 'master' ? this.masterClap : this.track(target.index).clap;
    if (!slot) {
      throw new Error(
        target.slot === 'master'
          ? 'there is no plugin on the master insert'
          : `track ${target.index} has no hosted plugin`,
      );
    }
    return slot;
  }

  /**
   * There is no native window in a browser, so this only records the state and
   * says so in the console. The panel's own logic is what is being developed
   * against the mock; the window is the Rust side's business.
   */
  async openClapGui(target: ClapTarget): Promise<ClapGuiStatus> {
    const slot = this.requireSlot(target);
    if (!slot.hasEditor) throw new Error(`${slot.name} has no editor.`);
    const key = MockBackend.targetKey(target);
    this.openEditors.add(key);
    console.info(`[mock] open_clap_gui ${key}: no native window outside Tauri`);
    return { open: true, hasEditor: true, pluginId: slot.pluginId, width: 400, height: 250 };
  }

  async closeClapGui(target: ClapTarget): Promise<ClapGuiStatus> {
    const slot = this.requireSlot(target);
    const key = MockBackend.targetKey(target);
    this.openEditors.delete(key);
    console.info(`[mock] close_clap_gui ${key}`);
    return {
      open: false,
      hasEditor: slot.hasEditor,
      pluginId: slot.pluginId,
      width: 0,
      height: 0,
    };
  }

  /**
   * Obviously a stand-in: a real plugin's state is an opaque blob only it can
   * read, so all that can be rehearsed here is that the same target keeps
   * answering with the same string.
   */
  async saveClapState(target: ClapTarget): Promise<string> {
    const slot = this.requireSlot(target);
    return btoa(`mock-clap-state:${slot.pluginId}`);
  }

  async loadClapState(target: ClapTarget, stateBase64: string): Promise<void> {
    const key = MockBackend.targetKey(target);
    this.requireSlot(target);
    this.clapStates.set(key, stateBase64);
    console.info(`[mock] load_clap_state ${key} (${stateBase64.length} chars)`);
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

  /** The built-ins, then every scanned instrument -- the order Rust lists them in. */
  private sourceList(): SourceDto[] {
    const current = this.tracks[this.selected]?.sourceId;
    const scanned = MOCK_CLAP_PLUGINS.filter((plugin) => plugin.kind === 'instrument').map(
      mockClapSource,
    );
    return [...MOCK_INTERNAL_SOURCES, ...scanned].map((source) => ({
      ...source,
      selected: source.id === current,
    }));
  }

  async getSources(): Promise<SourceDto[]> {
    return this.sourceList();
  }

  /**
   * There is no engine in a browser, so this only records the choice and says
   * so. A CLAP id is routed to the placement command, because that is the only
   * way a bundle ever reaches a track -- the same dispatch the UI does, kept
   * here too so `set_source` stays usable with any id `get_sources` offered.
   */
  async setSource(id: string): Promise<SourceDto[]> {
    const wanted = this.sourceList().find((source) => source.id === id);
    if (!wanted) throw new Error(`unknown source '${id}'`);
    if (wanted.kind === 'clap' && wanted.bundlePath && wanted.pluginId) {
      await this.setTrackSourceClap(this.selected, wanted.bundlePath, wanted.pluginId);
    } else {
      await this.setTrackSource(this.selected, id);
    }
    return this.getSources();
  }

  // --- the mixer -----------------------------------------------------------

  /**
   * The current mixer, cloned so a panel cannot mutate the mock behind its back.
   *
   * `guiOpen` is *derived* from the open-editor set rather than stored on the
   * slot, so there is exactly one answer to "is that window up" and no pair of
   * copies to keep in step.
   */
  private mixerState(): MixerState {
    const anySoloed = this.tracks.some((track) => track.soloed);
    const withGui = (slot: ClapSlot | null, target: ClapTarget): ClapSlot | null =>
      slot ? { ...slot, guiOpen: this.openEditors.has(MockBackend.targetKey(target)) } : null;
    return {
      tracks: this.tracks.map((track, index) => ({
        ...track,
        index,
        silenced: anySoloed ? !track.soloed : track.muted,
        selected: index === this.selected,
        clap: withGui(track.clap, { slot: 'track', index }),
      })),
      master: {
        gainDb: this.masterGainDb,
        clap: withGui(this.masterClap, { slot: 'master' }),
      },
      selected: this.selected,
      maxTracks: MOCK_MAX_TRACKS,
      minGainDb: MOCK_MIN_GAIN_DB,
      maxGainDb: MOCK_MAX_GAIN_DB,
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
    const wanted = MOCK_INTERNAL_SOURCES.find((entry) => entry.id === source);
    if (!wanted) throw new Error(`unknown source '${source}'`);
    const def = this.track(track);
    def.sourceId = wanted.id;
    def.sourceLabel = wanted.label;
    // A built-in replaces whatever plugin was there, and a window belonging to
    // an instance that no longer exists must not be left claiming to be open.
    def.clap = null;
    this.openEditors.delete(MockBackend.targetKey({ slot: 'track', index: track }));
    console.info(`[mock] set_track_source ${track} ${source}`);
    return this.mixerState();
  }

  /**
   * Find a scanned bundle, rejecting the way Rust does when the file has moved
   * or the id is not one the bundle exports.
   */
  private findPlugin(
    bundlePath: string,
    pluginId: string,
    kind: ClapPluginInfoDto['kind'],
  ): ClapPluginInfoDto {
    const plugin = MOCK_CLAP_PLUGINS.find(
      (entry) => entry.path === bundlePath && entry.id === pluginId,
    );
    if (!plugin) throw new Error(`no plugin '${pluginId}' in bundle ${bundlePath}`);
    if (plugin.kind !== kind) {
      throw new Error(`${plugin.name || plugin.id} is a ${plugin.kind}, not a ${kind}`);
    }
    return plugin;
  }

  private newSlot(plugin: ClapPluginInfoDto): ClapSlot {
    return {
      instance: this.nextInstance++,
      pluginId: plugin.id,
      name: plugin.name || plugin.id,
      bundlePath: plugin.path,
      hasEditor: mockHasEditor(plugin),
      guiOpen: false,
    };
  }

  async setTrackSourceClap(
    track: number,
    bundlePath: string,
    pluginId: string,
  ): Promise<MixerState> {
    const plugin = this.findPlugin(bundlePath, pluginId, 'instrument');
    const def = this.track(track);
    def.clap = this.newSlot(plugin);
    def.sourceId = mockClapSourceId(plugin.id);
    def.sourceLabel = def.clap.name;
    // A fresh instance, so any window the previous one had is gone with it.
    this.openEditors.delete(MockBackend.targetKey({ slot: 'track', index: track }));
    console.info(`[mock] set_track_source_clap ${track} ${pluginId}`);
    return this.mixerState();
  }

  async getMasterEffects(): Promise<ClapPluginInfoDto[]> {
    // Structured clone so panels cannot mutate the shared fixture.
    return structuredClone(
      MOCK_CLAP_PLUGINS.filter((plugin) => plugin.kind === 'audio-effect'),
    );
  }

  async setMasterEffectClap(bundlePath: string, pluginId: string): Promise<MixerState> {
    const plugin = this.findPlugin(bundlePath, pluginId, 'audio-effect');
    this.masterClap = this.newSlot(plugin);
    this.openEditors.delete(MockBackend.targetKey({ slot: 'master' }));
    console.info(`[mock] set_master_effect_clap ${pluginId}`);
    return this.mixerState();
  }

  async clearMasterEffect(): Promise<MixerState> {
    this.masterClap = null;
    this.openEditors.delete(MockBackend.targetKey({ slot: 'master' }));
    console.info('[mock] clear_master_effect');
    return this.mixerState();
  }

  async addTrack(source: string, name?: string): Promise<MixerState> {
    if (this.tracks.length >= MOCK_MAX_TRACKS) {
      throw new Error(`a project may have at most ${MOCK_MAX_TRACKS} tracks`);
    }
    const wanted = MOCK_INTERNAL_SOURCES.find((entry) => entry.id === source);
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
      clap: null,
    });
    return this.mixerState();
  }

  async removeTrack(track: number): Promise<MixerState> {
    this.track(track);
    if (this.tracks.length === 1) throw new Error('a project must keep at least one track');
    this.tracks.splice(track, 1);
    // Editor keys name a track by *index*, and removing one renumbers every
    // track after it. Re-keying rather than leaving the set alone: otherwise the
    // window that belonged to the removed track would appear to belong to
    // whichever track slid into its place.
    this.openEditors = MockBackend.reindexEditors(this.openEditors, track);
    if (this.selected >= this.tracks.length) this.selected = this.tracks.length - 1;
    return this.mixerState();
  }

  /** Drop `removed`'s editor key and shift every key above it down by one. */
  private static reindexEditors(open: ReadonlySet<string>, removed: number): Set<string> {
    const next = new Set<string>();
    for (const key of open) {
      if (key === 'master') {
        next.add(key);
        continue;
      }
      const index = Number.parseInt(key.slice('track:'.length), 10);
      if (index === removed) continue;
      next.add(MockBackend.targetKey({ slot: 'track', index: index > removed ? index - 1 : index }));
    }
    return next;
  }

  /** Exposed so a panel test can assert the mock's view of one editor. */
  isGuiOpen(target: ClapTarget): boolean {
    return this.openEditors.has(MockBackend.targetKey(target));
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
