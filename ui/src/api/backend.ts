import type {
  ClapGuiStatus,
  ClapPluginInfoDto,
  ClapSlot,
  ClapTarget,
  MeterFrame,
  MixerState,
  PatternDto,
  ProjectInfo,
  SequencerState,
  SourceDto,
  TrackDto,
  UiSnapshot,
  Unsubscribe,
  Vst3GuiStatus,
  Vst3PluginInfoDto,
  Vst3Slot,
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
   * Installed VST3 plugins. Same contract as `getClapPlugins`, one format over.
   *
   * The first call opens every installed `.vst3` module. A module that hangs --
   * on Windows, almost always a licensing dialog -- is remembered and skipped by
   * later scans, so the slow first call really is once per machine rather than
   * once per launch.
   */
  getVst3Plugins(rescan?: boolean): Promise<Vst3PluginInfoDto[]>;
  /** The scanned VST3 plugins eligible for the master insert. */
  getVst3MasterEffects(): Promise<Vst3PluginInfoDto[]>;
  /**
   * Put a hosted VST3 instrument at the head of a track.
   *
   * The module path *and* the class id both travel, because one `.vst3` module
   * can export several plugin classes.
   */
  setTrackSourceVst3(track: number, modulePath: string, cid: string): Promise<MixerState>;
  /**
   * Put a VST3 audio effect on the master insert.
   *
   * The insert holds **one** plugin of either format, so this replaces a CLAP
   * effect that was there. `clearMasterEffect` empties it whichever it holds.
   */
  setMasterEffectVst3(modulePath: string, cid: string): Promise<MixerState>;
  /** Open one placed VST3 plugin's editor in its own native window. */
  openVst3Gui(target: ClapTarget): Promise<Vst3GuiStatus>;
  /** Close it. Resolves even when nothing was open. */
  closeVst3Gui(target: ClapTarget): Promise<Vst3GuiStatus>;
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

  // --- the project ---------------------------------------------------------
  //
  // What crosses this boundary is an *intention* -- open a project -- rather
  // than a path, because the file dialog is Rust's and not the webview's.
  //
  // That is the whole reason these four commands look the way they do. The
  // dialog plugin only exists inside the app, so a panel that reached for
  // `@tauri-apps/api` to raise one would be a blank screen in a plain browser
  // and the mock would have nothing to stand in for -- which is exactly the
  // situation this interface exists to prevent. Keeping the dialog behind the
  // command also leaves "which extension is ours" with one spelling instead of
  // two, and gives every command an optional explicit `path` for the caller
  // that already knows: a test, a headless run, and one day a command line.
  //
  // The cost is one shape to remember: a command that shows a dialog resolves
  // with `null` when the user closed it. That is an answer, not a failure --
  // nothing changed, and nothing belongs in the status bar.

  /** Which project is open, for the window title and the status bar. */
  getProjectInfo(): Promise<ProjectInfo>;
  /** Throw the session away and start again on a fresh default project. */
  newProject(): Promise<ProjectInfo>;
  /**
   * Open a project, asking for one when no `path` is given.
   *
   * Replaces the whole session -- mixer, sources, pattern and tempo -- so a
   * caller that keeps any of those has to re-read all of them. Resolves with
   * `null` when the user cancelled the dialog.
   */
  openProject(path?: string | null): Promise<ProjectInfo | null>;
  /**
   * Write the session out, resolving with `null` when the user cancelled.
   *
   * `saveAs` is what decides whether a dialog is shown at all: without it, a
   * project that already has a path is overwritten where it stands, which is
   * the only thing Ctrl+S is ever allowed to mean. A project that has never
   * been saved has nowhere to go, so that one asks either way.
   */
  saveProject(saveAs: boolean, path?: string | null): Promise<ProjectInfo | null>;

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

  async getVst3Plugins(rescan = false): Promise<Vst3PluginInfoDto[]> {
    const { invoke } = await this.core();
    return invoke<Vst3PluginInfoDto[]>('get_vst3_plugins', { rescan });
  }

  async getVst3MasterEffects(): Promise<Vst3PluginInfoDto[]> {
    const { invoke } = await this.core();
    return invoke<Vst3PluginInfoDto[]>('get_vst3_master_effects');
  }

  async setTrackSourceVst3(
    track: number,
    modulePath: string,
    cid: string,
  ): Promise<MixerState> {
    const { invoke } = await this.core();
    return invoke<MixerState>('set_track_source_vst3', { track, modulePath, cid });
  }

  async setMasterEffectVst3(modulePath: string, cid: string): Promise<MixerState> {
    const { invoke } = await this.core();
    return invoke<MixerState>('set_master_effect_vst3', { modulePath, cid });
  }

  async openVst3Gui(target: ClapTarget): Promise<Vst3GuiStatus> {
    const { invoke } = await this.core();
    return invoke<Vst3GuiStatus>('open_vst3_gui', { target });
  }

  async closeVst3Gui(target: ClapTarget): Promise<Vst3GuiStatus> {
    const { invoke } = await this.core();
    return invoke<Vst3GuiStatus>('close_vst3_gui', { target });
  }

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

  // --- the project ---------------------------------------------------------

  // The only synchronous one of the four: it reads a value the main thread has
  // already published, where the other three wait on it.
  async getProjectInfo(): Promise<ProjectInfo> {
    const { invoke } = await this.core();
    return invoke<ProjectInfo>('get_project_info');
  }

  async newProject(): Promise<ProjectInfo> {
    const { invoke } = await this.core();
    return invoke<ProjectInfo>('new_project');
  }

  // `path: null` rather than an omitted key, because that is what asks Rust for
  // the dialog. An absent argument would be a `None` too, but only by accident
  // of how Tauri deserialises a missing field, and the intention here is not
  // accidental.
  async openProject(path?: string | null): Promise<ProjectInfo | null> {
    const { invoke } = await this.core();
    return invoke<ProjectInfo | null>('open_project', { path: path ?? null });
  }

  async saveProject(saveAs: boolean, path?: string | null): Promise<ProjectInfo | null> {
    const { invoke } = await this.core();
    return invoke<ProjectInfo | null>('save_project', { saveAs, path: path ?? null });
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
    { id: 'host-vst3', name: 'VST3 Host', state: 'registered' },
  ],
  panels: [
    { id: 'kernel-status', title: 'Kernel Status', component: 'kernel-status' },
    { id: 'mixer', title: 'Mixer', component: 'mixer' },
    { id: 'meters', title: 'Master Meters', component: 'meters' },
    { id: 'step-sequencer', title: 'Step Sequencer', component: 'step-sequencer' },
    { id: 'clap-browser', title: 'CLAP Plugins', component: 'clap-browser' },
    { id: 'vst3-browser', title: 'VST3 Plugins', component: 'vst3-browser' },
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
      vst3: null,
      missing: null,
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
      vst3: null,
      // Never set by the mock. Every plugin it knows about is a fixture in this
      // file, and a fixture cannot be uninstalled between a save and a load --
      // so the badge is deliberately not reachable here rather than faked, and
      // a strip that shows one in a plain browser is a bug in the panel.
      missing: null,
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

/**
 * Stand-ins for what a real machine has installed, chosen to cover the states
 * the panel has to render: an instrument, an effect, and a class whose module
 * declares no subcategories at all (`unknown`), which is the VST3-only case.
 *
 * The class ids are the real shape -- 32 hex digits -- because the mock is also
 * what proves the UI never assumes a CLAP-style reverse-DNS id.
 */
const MOCK_VST3_PLUGINS: Vst3PluginInfoDto[] = [
  {
    id: '58545356736673586572756D00000000',
    name: 'Example Serial Synth',
    vendor: 'Example Records',
    version: '1.4.0',
    path: 'C:\\Program Files\\Common Files\\VST3\\ExampleSerial.vst3',
    kind: 'instrument',
    subcategories: ['Instrument', 'Synth'],
  },
  {
    id: '41545356746E4148564F582041535049',
    name: 'Example Vocal Air',
    vendor: 'Example Audio',
    version: '4.2.1',
    path: 'C:\\Program Files\\Common Files\\VST3\\ExampleVocalAir.vst3',
    kind: 'audio-effect',
    subcategories: ['Fx', 'Pitch Shift'],
  },
  {
    id: '0123456789ABCDEF0123456789ABCDEF',
    name: 'Example Legacy Comp',
    vendor: '',
    version: '',
    path: 'C:\\Program Files\\Common Files\\VST3\\ExampleLegacy.vst3',
    // No `IPluginFactory2`, so the module declares nothing. MYDAW still offers
    // it as an effect and lets the bus check at load time decide.
    kind: 'unknown',
    subcategories: [],
  },
];

/** Wire id of a VST3 source, mirroring `Vst3SourceRef::wire_id` in Rust. */
function mockVst3SourceId(cid: string): string {
  return `vst3:${cid}`;
}

/** One scanned VST3 instrument, in the shape the source picker reads. */
function mockVst3Source(plugin: Vst3PluginInfoDto): SourceDto {
  return {
    id: mockVst3SourceId(plugin.id),
    label: plugin.name || plugin.id,
    description: plugin.vendor ? `${plugin.vendor} — VST3` : `VST3 — ${plugin.id}`,
    kind: 'vst3',
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

/** What a never-saved session is called. Mirrors `UNTITLED` in `app/src/project_file.rs`. */
const MOCK_UNTITLED = 'Untitled';

/** Our extension. Mirrors `PROJECT_EXTENSION` in `app/src/project_file.rs`. */
const MOCK_PROJECT_EXTENSION = 'mydaw';

/**
 * The folder the mock's invented paths sit in.
 *
 * A Windows path, because MYDAW's are: the mock is also where the UI proves it
 * never tries to parse one, and a POSIX path here would let a `split('/')`
 * survive review.
 */
const MOCK_PROJECT_DIR = 'D:\\Songs';

/**
 * Everything the mock writes when it "saves" a project, and everything it puts
 * back when it opens one.
 *
 * The map of these is what makes save-then-open a real round trip rather than a
 * new name over an untouched session. Without it the mock would agree with a UI
 * that forgot to refresh after a load, which is the single mistake this feature
 * is most likely to make.
 */
interface MockSession {
  tracks: TrackDto[];
  masterGainDb: number;
  masterClap: ClapSlot | null;
  masterVst3: Vst3Slot | null;
  pattern: PatternDto;
  bpm: number;
  selected: number;
  nextTrackId: number;
}

/**
 * The name a path goes by: its file stem. Mirrors `project_name` in
 * `app/src/project_file.rs`, separators included -- the two spellings have to
 * agree, or the mock would teach the status bar a different answer than Rust
 * gives it.
 */
function mockProjectName(path: string): string {
  const file = path.split(/[\\/]/).pop() ?? path;
  const dot = file.lastIndexOf('.');
  // `> 0`, not `>= 0`: a leading dot is a hidden file's name, not an extension.
  return dot > 0 ? file.slice(0, dot) : file;
}

/** The project information for a session living at `path`, or for no path at all. */
function mockProjectInfo(path: string | null): ProjectInfo {
  return {
    path,
    name: path === null ? MOCK_UNTITLED : mockProjectName(path),
    // Always zero -- see the note on `mockTracks`.
    missingPlugins: 0,
    warnings: [],
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
  /**
   * The VST3 effect on the master insert.
   *
   * Separate from `masterClap` and mutually exclusive with it: the insert holds
   * one plugin, so placing either format clears the other. Modelling it as two
   * fields rather than one union is what makes that rule visible at every site
   * that has to enforce it.
   */
  private masterVst3: Vst3Slot | null = null;
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
  /** Which project is open. A browser session starts on a never-saved one. */
  private project: ProjectInfo = mockProjectInfo(null);
  /**
   * The mock's "disk": every session it has written, by path.
   *
   * In memory and therefore gone on reload, which is honest -- a browser tab is
   * not a filesystem, and pretending otherwise (localStorage, say) would let a
   * stale session from last week's UI outlive the code that wrote it.
   */
  private saved = new Map<string, MockSession>();
  /** The path of the most recent save; what the open dialog would have offered. */
  private lastSavedPath: string | null = null;

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
    const scannedVst3 = MOCK_VST3_PLUGINS.filter(
      (plugin) => plugin.kind === 'instrument',
    ).map(mockVst3Source);
    const scanned = MOCK_CLAP_PLUGINS.filter((plugin) => plugin.kind === 'instrument').map(
      mockClapSource,
    );
    return [...MOCK_INTERNAL_SOURCES, ...scanned, ...scannedVst3].map((source) => ({
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
    const withGui3 = (slot: Vst3Slot | null, target: ClapTarget): Vst3Slot | null =>
      slot ? { ...slot, guiOpen: this.openEditors.has(MockBackend.targetKey(target)) } : null;
    return {
      tracks: this.tracks.map((track, index) => ({
        ...track,
        index,
        silenced: anySoloed ? !track.soloed : track.muted,
        selected: index === this.selected,
        clap: withGui(track.clap, { slot: 'track', index }),
        vst3: withGui3(track.vst3, { slot: 'track', index }),
      })),
      master: {
        gainDb: this.masterGainDb,
        clap: withGui(this.masterClap, { slot: 'master' }),
        vst3: withGui3(this.masterVst3, { slot: 'master' }),
        missing: null,
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
    // A built-in replaces whatever plugin was there, in either format, and a
    // window belonging to an instance that no longer exists must not be left
    // claiming to be open.
    def.clap = null;
    def.vst3 = null;
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

  // -------------------------------------------------------------- VST3
  //
  // The same surface one format over. It shares `openEditors` and the master
  // slot with the CLAP half deliberately: the insert holds one plugin of either
  // kind, and a mock that let both sit there at once would let the panel be
  // developed against a state Rust can never produce.

  async getVst3Plugins(_rescan = false): Promise<Vst3PluginInfoDto[]> {
    // Structured clone so panels cannot mutate the shared fixture.
    return structuredClone(MOCK_VST3_PLUGINS);
  }

  async getVst3MasterEffects(): Promise<Vst3PluginInfoDto[]> {
    return structuredClone(
      MOCK_VST3_PLUGINS.filter(
        (plugin) => plugin.kind === 'audio-effect' || plugin.kind === 'unknown',
      ),
    );
  }

  /** Find a scanned class, rejecting the way Rust does. */
  private findVst3(
    modulePath: string,
    cid: string,
    wanted: 'instrument' | 'audio-effect',
  ): Vst3PluginInfoDto {
    const plugin = MOCK_VST3_PLUGINS.find(
      (entry) => entry.path === modulePath && entry.id === cid,
    );
    if (!plugin) throw new Error(`no plugin class ${cid} in module ${modulePath}`);
    // `unknown` is accepted as an effect: the module declares no subcategories,
    // and Rust's bus-layout check is what actually decides. Mirroring that here
    // keeps the mock from teaching the panel a stricter rule than exists.
    const usable =
      plugin.kind === wanted || (wanted === 'audio-effect' && plugin.kind === 'unknown');
    if (!usable) {
      throw new Error(`${plugin.name || plugin.id} is a ${plugin.kind}, not a ${wanted}`);
    }
    return plugin;
  }

  private newVst3Slot(plugin: Vst3PluginInfoDto): Vst3Slot {
    return {
      instance: this.nextInstance++,
      cid: plugin.id,
      name: plugin.name || plugin.id,
      vendor: plugin.vendor,
      modulePath: plugin.path,
      // The legacy class stands in for a plugin with no editor, so the mixer's
      // "no editor button" path stays reachable in a plain browser.
      hasEditor: plugin.kind !== 'unknown',
      guiOpen: false,
    };
  }

  async setTrackSourceVst3(
    track: number,
    modulePath: string,
    cid: string,
  ): Promise<MixerState> {
    const plugin = this.findVst3(modulePath, cid, 'instrument');
    const def = this.track(track);
    def.vst3 = this.newVst3Slot(plugin);
    def.clap = null;
    def.sourceId = mockVst3SourceId(plugin.id);
    def.sourceLabel = def.vst3.name;
    this.openEditors.delete(MockBackend.targetKey({ slot: 'track', index: track }));
    console.info(`[mock] set_track_source_vst3 ${track} ${cid}`);
    return this.mixerState();
  }

  async setMasterEffectVst3(modulePath: string, cid: string): Promise<MixerState> {
    const plugin = this.findVst3(modulePath, cid, 'audio-effect');
    this.masterVst3 = this.newVst3Slot(plugin);
    // One insert, one plugin: taking it evicts the other format's, exactly as
    // `Vst3Host::place_master_effect` does.
    this.masterClap = null;
    this.openEditors.delete(MockBackend.targetKey({ slot: 'master' }));
    console.info(`[mock] set_master_effect_vst3 ${cid}`);
    return this.mixerState();
  }

  /** The VST3 slot a target names, rejecting an empty one the way Rust does. */
  private requireVst3Slot(target: ClapTarget): Vst3Slot {
    const slot = target.slot === 'master' ? this.masterVst3 : this.track(target.index).vst3;
    if (!slot) {
      throw new Error(
        target.slot === 'master'
          ? 'there is no VST3 plugin on the master insert'
          : `track ${target.index} has no hosted VST3 plugin`,
      );
    }
    return slot;
  }

  async openVst3Gui(target: ClapTarget): Promise<Vst3GuiStatus> {
    const slot = this.requireVst3Slot(target);
    if (!slot.hasEditor) throw new Error('This plugin has no editor.');
    const key = MockBackend.targetKey(target);
    this.openEditors.add(key);
    console.info(`[mock] open_vst3_gui ${key}: no native window outside Tauri`);
    return { open: true, hasEditor: true, cid: slot.cid, width: 800, height: 600 };
  }

  async closeVst3Gui(target: ClapTarget): Promise<Vst3GuiStatus> {
    const slot = this.requireVst3Slot(target);
    this.openEditors.delete(MockBackend.targetKey(target));
    return {
      open: false,
      hasEditor: slot.hasEditor,
      cid: slot.cid,
      width: 0,
      height: 0,
    };
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
    // One insert: emptying it empties it, whichever format was in it.
    this.masterClap = null;
    this.masterVst3 = null;
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
      vst3: null,
      missing: null,
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

  // --- the project ---------------------------------------------------------

  async getProjectInfo(): Promise<ProjectInfo> {
    return structuredClone(this.project);
  }

  /**
   * Everything a save writes down, cloned on the way out so that edits made
   * after the save cannot reach back into the file it wrote.
   */
  private session(): MockSession {
    return structuredClone({
      tracks: this.tracks,
      masterGainDb: this.masterGainDb,
      masterClap: this.masterClap,
      masterVst3: this.masterVst3,
      pattern: this.pattern,
      bpm: this.bpm,
      selected: this.selected,
      nextTrackId: this.nextTrackId,
    });
  }

  /** Put a written session back, cloned so the file survives the next edit. */
  private restore(session: MockSession): void {
    const copy = structuredClone(session);
    this.tracks = copy.tracks;
    this.masterGainDb = copy.masterGainDb;
    this.masterClap = copy.masterClap;
    this.masterVst3 = copy.masterVst3;
    this.pattern = copy.pattern;
    this.bpm = copy.bpm;
    this.selected = copy.selected;
    this.nextTrackId = copy.nextTrackId;
    this.reinstantiate();
  }

  /**
   * Everything that belonged to the session being replaced and cannot outlive
   * it. Both of the ways a session is replaced -- new, and open -- destroy every
   * plugin instance and build fresh ones, so no window may still claim to be
   * open, no state blob may be attributed to an instance that no longer exists,
   * and a saved handle must not be handed out a second time. That last one is
   * the reason the slots are renumbered rather than restored as written: the
   * mock never reuses an instance handle, and a file is full of old ones.
   */
  private reinstantiate(): void {
    this.openEditors.clear();
    this.clapStates.clear();
    for (const track of this.tracks) {
      if (track.clap) track.clap.instance = this.nextInstance++;
      if (track.vst3) track.vst3.instance = this.nextInstance++;
    }
    if (this.masterClap) this.masterClap.instance = this.nextInstance++;
    if (this.masterVst3) this.masterVst3.instance = this.nextInstance++;
  }

  /** Back to the two fixture tracks, an empty grid and 130 bpm. */
  async newProject(): Promise<ProjectInfo> {
    this.tracks = mockTracks();
    this.pattern = mockPattern();
    this.bpm = MOCK_DEFAULT_BPM;
    this.masterGainDb = 0;
    this.masterClap = null;
    this.masterVst3 = null;
    this.selected = 0;
    // Two fixture tracks own ids 0 and 1, the same arithmetic this field's
    // initialiser does -- a fresh project is a fresh mock, ids included.
    this.nextTrackId = 2;
    this.reinstantiate();
    this.project = mockProjectInfo(null);
    console.info('[mock] new_project');
    return this.getProjectInfo();
  }

  /**
   * Open a written session, or -- with no path -- whatever the open dialog would
   * have come back with.
   *
   * There is no dialog outside Tauri, so the most recent save stands in for one,
   * and `null` comes back when nothing has been written yet: a dialog with
   * nothing to offer is a dialog the user closes, and that is the same `null`
   * the real backend answers a cancel with. Rehearsing the cancel path is worth
   * more than inventing a file nobody saved, which would put the panel in a
   * state Rust can never produce. Same principle as `openClapGui`, which says
   * what it cannot do rather than pretending it did it.
   */
  async openProject(path?: string | null): Promise<ProjectInfo | null> {
    const chosen = path ?? this.lastSavedPath;
    if (chosen === null) {
      console.info('[mock] open_project: no dialog outside Tauri, and nothing saved to offer');
      return null;
    }
    const session = this.saved.get(chosen);
    if (!session) throw new Error(`there is no project at ${chosen}`);
    this.restore(session);
    this.project = mockProjectInfo(chosen);
    console.info(`[mock] open_project ${chosen}`);
    return this.getProjectInfo();
  }

  async saveProject(saveAs: boolean, path?: string | null): Promise<ProjectInfo | null> {
    // Save As always asks; a plain Save asks only when the project has nowhere
    // to go yet. The same two branches Rust's `save_project` has, mirrored here
    // so Ctrl+S can be developed against the mock and still mean "overwrite".
    const chosen = path ?? (saveAs ? null : this.project.path) ?? this.invent();
    this.saved.set(chosen, this.session());
    this.lastSavedPath = chosen;
    this.project = mockProjectInfo(chosen);
    console.info(`[mock] save_project ${chosen}`);
    return this.getProjectInfo();
  }

  /**
   * What the save dialog would have come back with.
   *
   * Invented rather than refused: a Save As that could never succeed would
   * leave the round trip -- and half the panel's logic -- unreachable in a
   * plain browser, which is the one thing the mock exists to prevent. Numbered
   * past a name already taken, because silently overwriting a file is the one
   * thing a real dialog would never do without asking first.
   */
  private invent(): string {
    const stem =
      this.project.path === null ? MOCK_UNTITLED : mockProjectName(this.project.path);
    let candidate = `${MOCK_PROJECT_DIR}\\${stem}.${MOCK_PROJECT_EXTENSION}`;
    for (let n = 2; this.saved.has(candidate); n += 1) {
      candidate = `${MOCK_PROJECT_DIR}\\${stem} ${n}.${MOCK_PROJECT_EXTENSION}`;
    }
    console.info(`[mock] no save dialog outside Tauri, using ${candidate}`);
    return candidate;
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
