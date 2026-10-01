import { SessionClient, type SessionChange, type SharedHistory } from './session';
import type {
  AssetStatus,
  AssetStatusDto,
  AudioSourceDto,
  ClapGuiStatus,
  ClapPluginInfoDto,
  ClapTarget,
  EffectInfoDto,
  InsertReadoutsDto,
  InsertTarget,
  LibraryEntryDto,
  LibraryRootDto,
  LibrarySearchDto,
  BrowserPreviewDto,
  MeterFrame,
  MixerState,
  PatternDto,
  PitchAnalysis,
  PlayMode,
  ProjectInfo,
  RenderToSampleDto,
  RootMode,
  SampleEditDto,
  SampleOp,
  SequencerState,
  SliceAnalysis,
  SliceMode,
  SliceState,
  SourceDto,
  SpectrumDto,
  StreamInfo,
  StripRef,
  UiSnapshot,
  Unsubscribe,
  Vst3GuiStatus,
  Vst3PluginInfoDto,
  WaveformDto,
  SourcePreviewDto,
  MidiPortDto,
  MidiStatusDto,
  PreviewSettings,
  AudioOutput,
  InputDeviceDto,
  InputStatusDto,
  RecordedTakeDto,
} from './types';

/**
 * The only surface the UI is allowed to use to reach the engine.
 *
 * Two implementations exist: `TauriBackend` (real IPC) and `MockBackend`
 * (pure-browser dev). Views must never import `@tauri-apps/api` directly --
 * everything funnels through here so `pnpm dev` in a plain browser keeps
 * working, and so the mock has something to stand in for.
 *
 * Three shapes recur, and knowing them is most of the contract:
 *
 * - **Every mixer command resolves with the whole `MixerState`.** A strip is a
 *   handful of numbers, the IPC spike measured a message this size as free, and
 *   one shape back means a view never merges a partial update into what it
 *   already had -- which is where optimistic UIs go wrong. The two exceptions
 *   are `setSendLevel` and `setInsertParam`, which a knob drag sends per frame.
 * - **Every sequencer command resolves with the whole `SequencerState`**, for
 *   the same reason, and every one of them succeeds with no audio device open:
 *   a pattern is project data.
 * - **A command that shows a native dialog resolves with `null` when the user
 *   cancelled.** That is an answer, not a failure, and nothing belongs in the
 *   status bar.
 */
export interface Backend {
  /** Human-readable kind, shown in the status bar. */
  readonly kind: 'tauri' | 'mock';
  readonly sharedHistory: boolean;
  setHistoryGroup(label?: string): void;
  getSessionHistory(): Promise<SharedHistory>;
  replaySessionHistory(direction: 'undo' | 'redo'): Promise<SharedHistory>;
  subscribeSession(onChange: (change: SessionChange) => void): Promise<Unsubscribe>;
  /** One-shot fetch of plugins + contributed panels. */
  getUiSnapshot(): Promise<UiSnapshot>;

  // --- hosted plugins ------------------------------------------------------

  /**
   * Installed CLAP plugins. The first call makes the backend scan the standard
   * plugin directories; afterwards it is served from a cache.
   */
  getClapPlugins(rescan?: boolean): Promise<ClapPluginInfoDto[]>;
  /**
   * Installed VST3 plugins. Same contract, one format over.
   *
   * The first call opens every installed `.vst3` module in a *child process*, so
   * a plugin that hangs costs one row rather than the DAW.
   */
  getVst3Plugins(rescan?: boolean): Promise<Vst3PluginInfoDto[]>;
  /** The scanned CLAP plugins eligible for the master insert. */
  getMasterEffects(): Promise<ClapPluginInfoDto[]>;
  /** The scanned VST3 classes eligible for the master insert. */
  getVst3MasterEffects(): Promise<Vst3PluginInfoDto[]>;
  /**
   * Open one placed plugin's editor in its own native window.
   *
   * Every placed plugin has its own window and they are independent. Rejects
   * with a sentence meant for the status bar -- most often "This plugin has no
   * editor.", which the UI avoids provoking by only offering the button when
   * the slot's `hasEditor` is true.
   */
  openClapGui(target: ClapTarget): Promise<ClapGuiStatus>;
  closeClapGui(target: ClapTarget): Promise<ClapGuiStatus>;
  openVst3Gui(target: ClapTarget): Promise<Vst3GuiStatus>;
  closeVst3Gui(target: ClapTarget): Promise<Vst3GuiStatus>;
  /** The plugin's own opaque state, base64. An empty string is a real answer. */
  saveClapState(target: ClapTarget): Promise<string>;
  loadClapState(target: ClapTarget, stateBase64: string): Promise<void>;

  // --- the transport -------------------------------------------------------

  transportPlay(): Promise<void>;
  transportStop(): Promise<void>;
  /**
   * Move the playhead to an absolute **sample** position.
   *
   * Samples rather than ticks because that is what the transport counts and
   * what the meter stream reports back; converting in two places would let the
   * playhead the UI draws and the playhead the engine plays disagree.
   */
  transportSeek(samplePos: number): Promise<void>;
  /** Switch the transport between the active pattern and the arrangement. */
  setPlayMode(mode: PlayMode): Promise<SequencerState>;
  /** Repeat the arrangement, or stop at the end of the last clip. */
  setLoop(looping: boolean): Promise<SequencerState>;

  /**
   * Play a note on one track's instrument. `velocity` is `0..1`. Sounds whether
   * or not the transport is running -- this is live input.
   */
  noteOn(key: number, velocity: number): Promise<void>;
  noteOff(key: number): Promise<void>;

  // --- instruments ---------------------------------------------------------

  /** Every instrument the engine can play, with the selected track's marked. */
  getSources(): Promise<SourceDto[]>;
  /** Put a different instrument at the head of the *selected* track. */
  setSource(id: string): Promise<SourceDto[]>;
  /** Put a different built-in instrument at the head of one track, by index. */
  setTrackSource(track: number, source: string): Promise<MixerState>;
  /**
   * Put a hosted CLAP instrument at the head of one track.
   *
   * The bundle path *and* the plugin id both travel, because one `.clap` file
   * can export several plugins and two copies can be installed.
   */
  setTrackSourceClap(track: number, bundlePath: string, pluginId: string): Promise<MixerState>;
  setTrackSourceVst3(track: number, modulePath: string, cid: string): Promise<MixerState>;

  // --- the mixer -----------------------------------------------------------

  getTracks(): Promise<MixerState>;
  setTrackGain(track: number, db: number): Promise<MixerState>;
  setTrackPan(track: number, pan: number): Promise<MixerState>;
  /** Channel pitch in cents (`-1200..=1200`) for clip and one-shot tracks. */
  setTrackPitch(track: number, cents: number): Promise<MixerState>;
  /** Detect the root pitch of an audio-file channel's selected region. */
  analyzeTrackPitch(track: number): Promise<PitchAnalysis>;
  /**
   * Calibrate an audio-file channel's playback root without changing transpose.
   * `key` is MIDI 0..=127; `fineCents` is typically -50..=50.
   */
  setTrackSampleRoot(
    track: number,
    key: number,
    fineCents: number,
    mode: RootMode,
  ): Promise<MixerState>;
  /**
   * Where an audio-file channel's slices would fall — without installing them.
   *
   * Analysis and installation are two calls on purpose: the sensitivity slider
   * re-runs this on every step of a drag, and a chop the user is still shaping
   * should not be what the keyboard plays until they stop. The range analysed
   * is the channel's SMP region.
   */
  analyzeSlices(
    track: number,
    mode: SliceMode,
    sensitivity: number,
    division: number,
  ): Promise<SliceAnalysis>;
  /**
   * Install (or clear) an audio-file channel's chop.
   *
   * Answers with what was **stored**, not what was asked for: markers are
   * sorted, deduped, clamped to the file and capped at 128, so the window can
   * draw the table the engine is actually playing.
   */
  setTrackSlices(track: number, slices: SliceState): Promise<SliceState>;
  /** Channel fade in and out, in milliseconds. One call, one republish. */
  setTrackFades(track: number, fadeInMs: number, fadeOutMs: number): Promise<MixerState>;
  /**
   * Set one of a channel's **instrument** parameters. Answers with what was
   * actually stored, so a knob can snap to a clamped value.
   */
  setSourceParam(track: number, paramId: string, value: number): Promise<number>;
  /** SMP region in the file's frames. `endFrames` of `0` means the whole file. */
  setTrackSampleRegion(
    track: number,
    startFrames: number,
    endFrames: number,
  ): Promise<MixerState>;
  /**
   * Apply a **destructive** edit to the file behind a sample channel.
   *
   * Copy-on-write: a new file is written and this channel (with its playlist
   * clips) repointed at it, so another channel on the same take is unaffected.
   * The answer carries the old pointer too — replay it through
   * {@link Backend.setTrackAudioSource} and the edit is undone in one step.
   */
  editTrackSample(track: number, op: SampleOp): Promise<SampleEditDto>;
  /**
   * Point a sample channel at a different **already loaded** file and region.
   * The inverse of {@link Backend.editTrackSample}, and how undo works.
   */
  setTrackAudioSource(
    track: number,
    sourceId: number,
    startFrames: number,
    endFrames: number,
    clipOffsetShiftFrames: number,
  ): Promise<MixerState>;
  /**
   * Render one loop of the active pattern through the whole mixer, offline, and
   * add it as a new sample channel. "Resampling".
   */
  renderPatternToSample(): Promise<RenderToSampleDto>;
  setTrackMute(track: number, muted: boolean): Promise<MixerState>;
  setTrackSolo(track: number, soloed: boolean): Promise<MixerState>;
  setMasterGain(db: number): Promise<MixerState>;
  /** Choose the track the keyboard piano plays and the source picker targets. */
  setSelectedTrack(track: number): Promise<MixerState>;
  /** Add a track. Rejects past `maxTracks`. */
  addTrack(source: string, name?: string): Promise<MixerState>;
  /**
   * Decode an audio file as a oneshot sampler and add a rack channel that
   * plays it. The same file can still be dropped on a playlist lane.
   */
  addSampleChannel(path: string): Promise<MixerState>;
  /** Remove a track. Rejects for the last one, and renumbers every pattern. */
  removeTrack(track: number): Promise<MixerState>;
  setMasterEffectClap(bundlePath: string, pluginId: string): Promise<MixerState>;
  setMasterEffectVst3(modulePath: string, cid: string): Promise<MixerState>;
  clearMasterEffect(): Promise<MixerState>;

  // --- buses, sends and insert chains --------------------------------------

  /**
   * Every internal effect this build can insert, with the metadata a generic
   * knob panel renders from. Static: fetch once at startup.
   */
  getEffects(): Promise<EffectInfoDto[]>;
  addBus(name?: string): Promise<MixerState>;
  /** Remove a bus **and every send at either end of it**. */
  removeBus(bus: number): Promise<MixerState>;
  renameBus(bus: number, name: string): Promise<MixerState>;
  setBusColor(bus: number, color: string | null): Promise<MixerState>;
  setBusGain(bus: number, db: number): Promise<MixerState>;
  setBusPan(bus: number, pan: number): Promise<MixerState>;
  setBusMute(bus: number, muted: boolean): Promise<MixerState>;
  setBusSolo(bus: number, soloed: boolean): Promise<MixerState>;
  /**
   * Add a send tap from a strip into a bus, at unity and post-fader.
   *
   * Refused for a duplicate pair, for a bus sending to itself, and for anything
   * that would feed a bus back into itself through others -- the error names the
   * whole loop.
   */
  addSend(sourceKind: 'track' | 'bus', sourceIndex: number, dest: number): Promise<MixerState>;
  removeSend(sendId: number): Promise<MixerState>;
  /**
   * Set a send's level, linear, `0..=maxSendLevel`.
   *
   * One atomic store, so this is what a knob drag sends per frame -- which is
   * why it resolves with nothing rather than the whole mixer.
   */
  setSendLevel(sendId: number, level: number): Promise<void>;
  /**
   * Move a send between pre- and post-fader. Pre-fader is taken after the
   * source's inserts and **before** its fader, pan and mute.
   */
  setSendPre(sendId: number, pre: boolean): Promise<void>;
  /** Add an internal effect to a chain. `position` absent means the end. */
  addInsert(target: InsertTarget, typeId: string, position?: number): Promise<MixerState>;
  removeInsert(target: InsertTarget, index: number): Promise<MixerState>;
  moveInsert(target: InsertTarget, from: number, to: number): Promise<MixerState>;
  /**
   * Set one insert parameter, resolving with **what was actually stored** --
   * clamped into range and snapped to the parameter's step grid, so a knob can
   * put itself where the engine actually is.
   */
  setInsertParam(
    target: InsertTarget,
    index: number,
    paramId: string,
    value: number,
  ): Promise<number>;
  setInsertBypass(target: InsertTarget, index: number, bypassed: boolean): Promise<MixerState>;
  /**
   * Every meter on one chain, in chain order.
   *
   * Separate from the parameter values on purpose: a gain-reduction bar wants to
   * move at frame rate, and sending twenty knob positions sixty times a second
   * to animate one bar is the mistake this split exists to prevent. Only polled
   * while a chain with meters is actually on screen.
   */
  getChainReadouts(target: InsertTarget): Promise<InsertReadoutsDto[]>;
  /**
   * Arm the analysis tap. `index` is the insert; omit it (`null`) on a track
   * to listen to the instrument itself. `null`/`null` silences the tap.
   * Only one editor should hold this at a time.
   */
  watchSpectrum(target: InsertTarget | null, index: number | null): Promise<void>;
  /** Latest hop from the armed tap, or `null` when nothing is watched. */
  getInsertSpectrum(): Promise<SpectrumDto | null>;
  /** EQ band-solo of the source. `band` is a prefix, or `null` to stop. */
  setInsertListen(target: InsertTarget, index: number, band: string | null): Promise<void>;

  /**
   * Point one insert's detector at another strip, or unroute it with `null`.
   *
   * The compressor's external key. One atomic store in the engine and no graph
   * rebuild, so a source can be changed while the transport runs -- which is
   * how a user finds out which drum the bass should duck under. Returns the
   * whole mixer, because the route is part of the insert every strip draws.
   */
  setInsertSidechain(
    target: InsertTarget,
    index: number,
    source: StripRef | null,
  ): Promise<MixerState>;

  // --- patterns ------------------------------------------------------------

  /** The whole sequencer state. Called once at startup. */
  getPattern(): Promise<SequencerState>;
  /**
   * The content of one pattern, by id.
   *
   * `SequencerState` carries every pattern as a summary and only the active one
   * in full. The playlist draws a miniature of each clip's *own* pattern, most
   * of which are not the one being edited, so it asks for those one at a time
   * and caches them. **Rejects** an id the set does not have.
   */
  getPatternContent(patternId: number): Promise<PatternDto>;
  /**
   * Replace one pattern wholesale. `pattern.id` names which; `0` means the
   * active one.
   *
   * A whole-pattern replacement rather than a per-step edit: a rack of grids is
   * a few kilobytes of JSON and one message per edit is a protocol with no
   * ordering problems and no diff to get wrong.
   */
  setPattern(pattern: PatternDto, expected?: PatternDto): Promise<SequencerState>;
  /** Add an empty pattern. **Does not switch to it** -- that is `setActivePattern`. */
  addPattern(name?: string): Promise<SequencerState>;
  clonePattern(patternId: number, name?: string): Promise<SequencerState>;
  /**
   * Split a pattern into one pattern per occupied rack channel, and spread
   * its playlist clips onto consecutive lanes (adding lanes when needed).
   */
  splitPatternByChannel(patternId: number): Promise<SequencerState>;
  /** Delete a pattern **and every playlist clip that played it**. */
  removePattern(patternId: number): Promise<SequencerState>;
  renamePattern(patternId: number, name: string): Promise<SequencerState>;
  setPatternColor(patternId: number, color: string | null): Promise<SequencerState>;
  /** Set a pattern's loop length in steps: 16, 32 or 64. Shortening is not destructive. */
  setPatternLength(patternId: number, steps: number): Promise<SequencerState>;
  setActivePattern(patternId: number): Promise<SequencerState>;
  /** Set the tempo. **Rejects** out-of-range values rather than clamping. */
  setBpm(bpm: number): Promise<number>;

  // --- the playlist --------------------------------------------------------

  addPlaylistTrack(name?: string): Promise<SequencerState>;
  /** Remove a lane **and every clip on it**. Refused for the last lane. */
  removePlaylistTrack(track: number): Promise<SequencerState>;
  renamePlaylistTrack(track: number, name: string): Promise<SequencerState>;
  setPlaylistTrackMute(track: number, muted: boolean): Promise<SequencerState>;
  setPlaylistTrackSolo(track: number, soloed: boolean): Promise<SequencerState>;
  /** Place a pattern on the timeline. `start`/`length` are ticks. */
  addPatternClip(
    patternId: number,
    track: number,
    start: number,
    length?: number,
  ): Promise<SequencerState>;
  removeClip(clipId: number): Promise<SequencerState>;
  moveClip(clipId: number, track: number, start: number): Promise<SequencerState>;
  resizeClip(clipId: number, length: number): Promise<SequencerState>;

  // --- audio files ---------------------------------------------------------

  /**
   * Decode an audio file into memory and give it a source id.
   *
   * Loading a path that is already loaded returns the source that is already
   * there, so dragging one loop onto four lanes decodes once.
   */
  loadAudioSource(path: string): Promise<AudioSourceDto>;
  /**
   * Ask for an audio file with the native dialog and load it.
   *
   * Resolves with `null` when the user cancelled. The dialog lives in Rust for
   * the reason the project dialogs do: a view that reached for
   * `@tauri-apps/api` would be a blank screen in a plain browser.
   */
  openAudioFile(): Promise<AudioSourceDto | null>;
  getAudioSources(): Promise<AudioSourceDto[]>;
  /** Refused while a clip still plays the file, and the message says how many. */
  unloadAudioSource(sourceId: number): Promise<AudioSourceDto[]>;
  /** A source's waveform as min/max pairs. `targetBins` is a request, not a promise. */
  getAudioWaveform(sourceId: number, targetBins?: number): Promise<WaveformDto>;
  /** One hit of a built-in or sampled instrument, as min/max bins. */
  previewSource(sourceId: string, targetBins?: number): Promise<SourcePreviewDto>;
  /**
   * Write an OS-dropped file's bytes into the app's imports folder and answer
   * with the absolute path, so the drop can ride the normal file pipeline.
   * WebView2 gives a drop no path — see `lib/dnd.ts`. **Rejects** past 100 MB.
   */
  importDroppedAudio(fileName: string, bytes: Uint8Array): Promise<string>;
  getLibraryRoots(): Promise<LibraryRootDto[]>;
  /** Native folder dialog. `null` when cancelled. */
  addLibraryFolder(): Promise<LibraryRootDto[] | null>;
  removeLibraryFolder(id: string): Promise<LibraryRootDto[]>;
  listLibraryDir(rootId: string, relPath: string): Promise<LibraryEntryDto[]>;
  searchLibrary(query: string): Promise<LibrarySearchDto>;
  previewBrowserFile(path: string, audition: boolean): Promise<BrowserPreviewDto>;
  stopBrowserPreview(): Promise<void>;
  /** Audition a file without loading it into the project. */
  previewAudioFile(path: string): Promise<void>;
  /** Audition an audio-file channel with its current region, pitch and fades. */
  previewTrackSample(track: number): Promise<void>;
  /**
   * Audition a built-in or sampled instrument through the same preview slot.
   * Plays that source, not the selected rack channel.
   */
  previewInstrument(sourceId: string): Promise<void>;
  /** Browser preview level as a percent of master. Default 80. */
  getPreviewSettings(): Promise<PreviewSettings>;
  setPreviewVolume(percent: number): Promise<PreviewSettings>;
  /** WASAPI/ASIO preference and the stream they produced. */
  getAudioSettings(): Promise<AudioOutput>;
  setAudioSettings(preferAsio: boolean, bufferFrames: number): Promise<AudioOutput>;
  /** Place an audio file on the timeline. `length` absent means the whole file. */
  addAudioClip(
    sourceId: number,
    track: number,
    start: number,
    length?: number,
    offsetFrames?: number,
    gainDb?: number,
  ): Promise<SequencerState>;
  setClipGain(clipId: number, gainDb: number): Promise<SequencerState>;
  setClipOffset(clipId: number, offsetFrames: number): Promise<SequencerState>;

  // --- the project ---------------------------------------------------------

  getProjectInfo(): Promise<ProjectInfo>;
  newProject(): Promise<ProjectInfo>;
  /** Open a project, asking for one when no `path` is given. `null` = cancelled. */
  openProject(path?: string | null): Promise<ProjectInfo | null>;
  /**
   * Write the session out. With `saveAs` false and a project already on disk
   * this overwrites it with no dialog, which is the only thing Ctrl+S is ever
   * allowed to mean. `null` = cancelled.
   */
  saveProject(saveAs: boolean, path?: string | null): Promise<ProjectInfo | null>;

  /** What the open audio device is. Answers `running: false` rather than failing. */
  getStreamInfo(): Promise<StreamInfo>;

  // --- audio input and recording -------------------------------------------
  //
  // Seven commands, one state machine: idle -> armed -> recording -> armed.
  // `getInputStatus` is the only one worth polling, and only while the device
  // is open; the rest are gestures. `state.tsx` owns that poll.

  /**
   * Every capture device the default host can offer. An empty list is a real
   * answer — a headless machine or a disabled microphone produces one — and
   * Settings draws an empty state for it rather than an error.
   */
  getInputDevices(): Promise<InputDeviceDto[]>;
  /**
   * Choose the capture device and remember it. `null` follows the system
   * default. **Refused** while a take is running.
   */
  setInputDevice(deviceId: string | null): Promise<InputStatusDto>;
  /** What the REC button and the input meter draw. Cheap enough to poll. */
  getInputStatus(): Promise<InputStatusDto>;
  /**
   * Open the capture device and start metering, or release it.
   *
   * WAVE does not hold the microphone while idle: that is what stops a
   * conferencing app from working and lights the OS privacy indicator for
   * nothing. Disarming mid-take discards the take.
   */
  setInputArmed(armed: boolean): Promise<InputStatusDto>;
  /** Start a take, arming the device first if it is not open yet. */
  startRecording(): Promise<InputStatusDto>;
  /**
   * End the take, write it, and load it as an audio source.
   *
   * Placing it is `addSampleChannel`, on purpose: a recorded take and a dropped
   * WAV reach the rack by the same path.
   */
  stopRecording(): Promise<RecordedTakeDto>;
  /** Throw the take away. The device stays armed for another attempt. */
  cancelRecording(): Promise<InputStatusDto>;

  /** MIDI input ports for Settings. Reconciles with the OS on each call. */
  listMidiPorts(): Promise<MidiPortDto[]>;
  /** Pair or unpair one input. Returns the whole list. */
  setMidiPortEnabled(name: string, enabled: boolean): Promise<MidiPortDto[]>;
  /** Activity timestamp and paired count, for the status-bar lamp. */
  getMidiStatus(): Promise<MidiStatusDto>;

  /** Downloadable instrument assets. */
  getAssetStatus(): Promise<AssetStatus[]>;
  startAssetDownload(id: string): Promise<AssetStatus[]>;

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
 *
 * Tauri v2 rewrites camelCase argument keys to the Rust parameter's snake_case
 * name, so `bundlePath` here is `bundle_path` there. Sending snake_case from
 * this side is what would *not* arrive.
 */
class TauriBackend implements Backend {
  readonly kind = 'tauri' as const;
  readonly sharedHistory = true;
  private readonly session = new SessionClient(async <T,>(request: Parameters<import('./session').ControlInvoke>[0]) => {
    const { invoke } = await this.core();
    return invoke<{ revision: number; data: T; error?: { code: string; message: string } }>('control_call', { request });
  });
  setHistoryGroup(label?: string): void { this.session.setGroup(label); }
  async getSessionHistory(): Promise<SharedHistory> {
    return (await this.call<{ history: SharedHistory }>('history')).history;
  }
  replaySessionHistory(direction: 'undo' | 'redo'): Promise<SharedHistory> { return this.call(direction); }
  async subscribeSession(onChange: (change: SessionChange) => void): Promise<Unsubscribe> {
    const { listen } = await import('@tauri-apps/api/event');
    return listen<SessionChange>('mydaw:control-changed', (event) => onChange(event.payload));
  }

  private async core(): Promise<typeof import('@tauri-apps/api/core')> {
    return import('@tauri-apps/api/core');
  }

  /** One invoke, so every method below is a one-liner naming its command. */
  private async call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
    if (command.startsWith('bench_')) {
      const { invoke } = await this.core();
      return invoke<T>(command, args);
    }
    return this.session.call<T>(command, args);
  }

  getUiSnapshot(): Promise<UiSnapshot> {
    return this.call('get_ui_snapshot');
  }

  getClapPlugins(rescan = false): Promise<ClapPluginInfoDto[]> {
    return this.call('get_clap_plugins', { rescan });
  }

  getVst3Plugins(rescan = false): Promise<Vst3PluginInfoDto[]> {
    return this.call('get_vst3_plugins', { rescan });
  }

  getMasterEffects(): Promise<ClapPluginInfoDto[]> {
    return this.call('get_master_effects');
  }

  getVst3MasterEffects(): Promise<Vst3PluginInfoDto[]> {
    return this.call('get_vst3_master_effects');
  }

  openClapGui(target: ClapTarget): Promise<ClapGuiStatus> {
    return this.call('open_clap_gui', { target });
  }

  closeClapGui(target: ClapTarget): Promise<ClapGuiStatus> {
    return this.call('close_clap_gui', { target });
  }

  openVst3Gui(target: ClapTarget): Promise<Vst3GuiStatus> {
    return this.call('open_vst3_gui', { target });
  }

  closeVst3Gui(target: ClapTarget): Promise<Vst3GuiStatus> {
    return this.call('close_vst3_gui', { target });
  }

  saveClapState(target: ClapTarget): Promise<string> {
    return this.call('save_clap_state', { target });
  }

  async loadClapState(target: ClapTarget, stateBase64: string): Promise<void> {
    await this.call('load_clap_state', { target, stateBase64 });
  }

  async transportPlay(): Promise<void> {
    await this.call('transport_play');
  }

  async transportStop(): Promise<void> {
    await this.call('transport_stop');
  }

  async transportSeek(samplePos: number): Promise<void> {
    await this.call('transport_seek', { samplePos: Math.max(0, Math.round(samplePos)) });
  }

  setPlayMode(mode: PlayMode): Promise<SequencerState> {
    return this.call('set_play_mode', { mode });
  }

  setLoop(looping: boolean): Promise<SequencerState> {
    return this.call('set_loop', { looping });
  }

  async noteOn(key: number, velocity: number): Promise<void> {
    await this.call('note_on', { key, velocity });
  }

  async noteOff(key: number): Promise<void> {
    await this.call('note_off', { key });
  }

  getSources(): Promise<SourceDto[]> {
    return this.call('get_sources');
  }

  setSource(id: string): Promise<SourceDto[]> {
    return this.call('set_source', { source: id });
  }

  setTrackSource(track: number, source: string): Promise<MixerState> {
    return this.call('set_track_source', { track, source });
  }

  setTrackSourceClap(track: number, bundlePath: string, pluginId: string): Promise<MixerState> {
    return this.call('set_track_source_clap', { track, bundlePath, pluginId });
  }

  setTrackSourceVst3(track: number, modulePath: string, cid: string): Promise<MixerState> {
    return this.call('set_track_source_vst3', { track, modulePath, cid });
  }

  getTracks(): Promise<MixerState> {
    return this.call('get_tracks');
  }

  setTrackGain(track: number, db: number): Promise<MixerState> {
    return this.call('set_track_gain', { track, db });
  }

  setTrackPan(track: number, pan: number): Promise<MixerState> {
    return this.call('set_track_pan', { track, pan });
  }

  setTrackPitch(track: number, cents: number): Promise<MixerState> {
    return this.call('set_track_pitch', { track, cents });
  }

  analyzeTrackPitch(track: number): Promise<PitchAnalysis> {
    return this.call('analyze_track_pitch', { track });
  }

  setTrackSampleRoot(
    track: number,
    key: number,
    fineCents: number,
    mode: RootMode,
  ): Promise<MixerState> {
    return this.call('set_track_sample_root', { track, key, fineCents, mode });
  }

  analyzeSlices(
    track: number,
    mode: SliceMode,
    sensitivity: number,
    division: number,
  ): Promise<SliceAnalysis> {
    return this.call('analyze_slices', { track, mode, sensitivity, division });
  }

  setTrackSlices(track: number, slices: SliceState): Promise<SliceState> {
    return this.call('set_track_slices', { track, slices });
  }

  setTrackFades(track: number, fadeInMs: number, fadeOutMs: number): Promise<MixerState> {
    return this.call('set_track_fades', { track, fadeInMs, fadeOutMs });
  }

  setSourceParam(track: number, paramId: string, value: number): Promise<number> {
    return this.call('set_source_param', { track, paramId, value });
  }

  setTrackSampleRegion(
    track: number,
    startFrames: number,
    endFrames: number,
  ): Promise<MixerState> {
    return this.call('set_track_sample_region', { track, startFrames, endFrames });
  }

  editTrackSample(track: number, op: SampleOp): Promise<SampleEditDto> {
    return this.call('edit_track_sample', { track, op });
  }

  setTrackAudioSource(
    track: number,
    sourceId: number,
    startFrames: number,
    endFrames: number,
    clipOffsetShiftFrames: number,
  ): Promise<MixerState> {
    return this.call('set_track_audio_source', {
      track,
      sourceId,
      startFrames,
      endFrames,
      clipOffsetShiftFrames,
    });
  }

  renderPatternToSample(): Promise<RenderToSampleDto> {
    return this.call('render_pattern_to_sample');
  }

  setTrackMute(track: number, muted: boolean): Promise<MixerState> {
    return this.call('set_track_mute', { track, muted });
  }

  setTrackSolo(track: number, soloed: boolean): Promise<MixerState> {
    return this.call('set_track_solo', { track, soloed });
  }

  setMasterGain(db: number): Promise<MixerState> {
    return this.call('set_master_gain', { db });
  }

  setSelectedTrack(track: number): Promise<MixerState> {
    return this.call('set_selected_track', { track });
  }

  addTrack(source: string, name?: string): Promise<MixerState> {
    return this.call('add_track', { source, name: name ?? null });
  }

  addSampleChannel(path: string): Promise<MixerState> {
    return this.call('add_sample_channel', { path });
  }

  removeTrack(track: number): Promise<MixerState> {
    return this.call('remove_track', { track });
  }

  setMasterEffectClap(bundlePath: string, pluginId: string): Promise<MixerState> {
    return this.call('set_master_effect_clap', { bundlePath, pluginId });
  }

  setMasterEffectVst3(modulePath: string, cid: string): Promise<MixerState> {
    return this.call('set_master_effect_vst3', { modulePath, cid });
  }

  clearMasterEffect(): Promise<MixerState> {
    return this.call('clear_master_effect');
  }

  getEffects(): Promise<EffectInfoDto[]> {
    return this.call('get_effects');
  }

  addBus(name?: string): Promise<MixerState> {
    return this.call('add_bus', { name: name ?? null });
  }

  removeBus(bus: number): Promise<MixerState> {
    return this.call('remove_bus', { bus });
  }

  renameBus(bus: number, name: string): Promise<MixerState> {
    return this.call('rename_bus', { bus, name });
  }

  setBusColor(bus: number, color: string | null): Promise<MixerState> {
    return this.call('set_bus_color', { bus, color });
  }

  setBusGain(bus: number, db: number): Promise<MixerState> {
    return this.call('set_bus_gain', { bus, db });
  }

  setBusPan(bus: number, pan: number): Promise<MixerState> {
    return this.call('set_bus_pan', { bus, pan });
  }

  setBusMute(bus: number, muted: boolean): Promise<MixerState> {
    return this.call('set_bus_mute', { bus, muted });
  }

  setBusSolo(bus: number, soloed: boolean): Promise<MixerState> {
    return this.call('set_bus_solo', { bus, soloed });
  }

  addSend(
    sourceKind: 'track' | 'bus',
    sourceIndex: number,
    dest: number,
  ): Promise<MixerState> {
    return this.call('add_send', { sourceKind, sourceIndex, dest });
  }

  removeSend(sendId: number): Promise<MixerState> {
    return this.call('remove_send', { sendId });
  }

  async setSendLevel(sendId: number, level: number): Promise<void> {
    await this.call('set_send_level', { sendId, level });
  }

  async setSendPre(sendId: number, pre: boolean): Promise<void> {
    await this.call('set_send_pre', { sendId, pre });
  }

  addInsert(target: InsertTarget, typeId: string, position?: number): Promise<MixerState> {
    return this.call('add_insert', {
      target: target.kind,
      targetIndex: target.index,
      typeId,
      position: position ?? null,
    });
  }

  removeInsert(target: InsertTarget, index: number): Promise<MixerState> {
    return this.call('remove_insert', {
      target: target.kind,
      targetIndex: target.index,
      index,
    });
  }

  moveInsert(target: InsertTarget, from: number, to: number): Promise<MixerState> {
    return this.call('move_insert', {
      target: target.kind,
      targetIndex: target.index,
      from,
      to,
    });
  }

  setInsertParam(
    target: InsertTarget,
    index: number,
    paramId: string,
    value: number,
  ): Promise<number> {
    return this.call('set_insert_param', {
      target: target.kind,
      targetIndex: target.index,
      index,
      paramId,
      value,
    });
  }

  setInsertBypass(
    target: InsertTarget,
    index: number,
    bypassed: boolean,
  ): Promise<MixerState> {
    return this.call('set_insert_bypass', {
      target: target.kind,
      targetIndex: target.index,
      index,
      bypassed,
    });
  }

  getChainReadouts(target: InsertTarget): Promise<InsertReadoutsDto[]> {
    return this.call('get_chain_readouts', {
      target: target.kind,
      targetIndex: target.index,
    });
  }

  watchSpectrum(target: InsertTarget | null, index: number | null): Promise<void> {
    return this.call('watch_spectrum', {
      target: target?.kind ?? null,
      targetIndex: target?.index ?? 0,
      index,
    });
  }

  getInsertSpectrum(): Promise<SpectrumDto | null> {
    return this.call('get_insert_spectrum');
  }

  setInsertListen(target: InsertTarget, index: number, band: string | null): Promise<void> {
    return this.call('set_insert_listen', {
      target: target.kind,
      targetIndex: target.index,
      index,
      band,
    });
  }

  setInsertSidechain(
    target: InsertTarget,
    index: number,
    source: StripRef | null,
  ): Promise<MixerState> {
    return this.call('set_insert_sidechain', {
      target: target.kind,
      targetIndex: target.index,
      index,
      sourceKind: source?.kind ?? null,
      sourceIndex: source?.index ?? 0,
    });
  }

  async getPattern(): Promise<SequencerState> {
    const result = await this.call<SequencerState>('get_pattern');
    this.session.observe(result.patternSet.active);
    return result;
  }

  async getPatternContent(patternId: number): Promise<PatternDto> {
    const result = await this.call<PatternDto>('get_pattern_content', { patternId });
    this.session.observe(result);
    return result;
  }

  setPattern(pattern: PatternDto, expected?: PatternDto): Promise<SequencerState> {
    return this.session.call('set_pattern', { pattern }, expected);
  }

  addPattern(name?: string): Promise<SequencerState> {
    return this.call('add_pattern', { name: name ?? null });
  }

  clonePattern(patternId: number, name?: string): Promise<SequencerState> {
    return this.call('clone_pattern', { patternId, name: name ?? null });
  }

  splitPatternByChannel(patternId: number): Promise<SequencerState> {
    return this.call('split_pattern_by_channel', { patternId });
  }

  removePattern(patternId: number): Promise<SequencerState> {
    return this.call('remove_pattern', { patternId });
  }

  renamePattern(patternId: number, name: string): Promise<SequencerState> {
    return this.call('rename_pattern', { patternId, name });
  }

  setPatternColor(patternId: number, color: string | null): Promise<SequencerState> {
    return this.call('set_pattern_color', { patternId, color });
  }

  setPatternLength(patternId: number, steps: number): Promise<SequencerState> {
    return this.call('set_pattern_length', { patternId, steps });
  }

  setActivePattern(patternId: number): Promise<SequencerState> {
    return this.call('set_active_pattern', { patternId });
  }

  setBpm(bpm: number): Promise<number> {
    return this.call('set_bpm', { bpm });
  }

  addPlaylistTrack(name?: string): Promise<SequencerState> {
    return this.call('add_playlist_track', { name: name ?? null });
  }

  removePlaylistTrack(track: number): Promise<SequencerState> {
    return this.call('remove_playlist_track', { track });
  }

  renamePlaylistTrack(track: number, name: string): Promise<SequencerState> {
    return this.call('rename_playlist_track', { track, name });
  }

  setPlaylistTrackMute(track: number, muted: boolean): Promise<SequencerState> {
    return this.call('set_playlist_track_mute', { track, muted });
  }

  setPlaylistTrackSolo(track: number, soloed: boolean): Promise<SequencerState> {
    return this.call('set_playlist_track_solo', { track, soloed });
  }

  addPatternClip(
    patternId: number,
    track: number,
    start: number,
    length?: number,
  ): Promise<SequencerState> {
    return this.call('add_pattern_clip', {
      patternId,
      track,
      start,
      length: length ?? null,
    });
  }

  removeClip(clipId: number): Promise<SequencerState> {
    return this.call('remove_clip', { clipId });
  }

  moveClip(clipId: number, track: number, start: number): Promise<SequencerState> {
    return this.call('move_clip', { clipId, track, start });
  }

  resizeClip(clipId: number, length: number): Promise<SequencerState> {
    return this.call('resize_clip', { clipId, length });
  }

  loadAudioSource(path: string): Promise<AudioSourceDto> {
    return this.call('load_audio_source', { path });
  }

  openAudioFile(): Promise<AudioSourceDto | null> {
    return this.call('open_audio_file');
  }

  getAudioSources(): Promise<AudioSourceDto[]> {
    return this.call('get_audio_sources');
  }

  unloadAudioSource(sourceId: number): Promise<AudioSourceDto[]> {
    return this.call('unload_audio_source', { sourceId });
  }

  getAudioWaveform(sourceId: number, targetBins?: number): Promise<WaveformDto> {
    return this.call('get_audio_waveform', { sourceId, targetBins: targetBins ?? null });
  }

  previewSource(sourceId: string, targetBins?: number): Promise<SourcePreviewDto> {
    return this.call('preview_source', { sourceId, targetBins: targetBins ?? null });
  }

  importDroppedAudio(fileName: string, bytes: Uint8Array): Promise<string> {
    // A plain array: Tauri's JSON arg layer would turn a Uint8Array into an
    // `{ "0": … }` object, which Rust's `Vec<u8>` does not deserialise.
    return this.call('import_dropped_audio', { fileName, bytes: Array.from(bytes) });
  }

  getLibraryRoots(): Promise<LibraryRootDto[]> {
    return this.call('get_library_roots');
  }

  addLibraryFolder(): Promise<LibraryRootDto[] | null> {
    return this.call('add_library_folder');
  }

  removeLibraryFolder(id: string): Promise<LibraryRootDto[]> {
    return this.call('remove_library_folder', { id });
  }

  listLibraryDir(rootId: string, relPath: string): Promise<LibraryEntryDto[]> {
    return this.call('list_library_dir', { rootId, relPath });
  }

  searchLibrary(query: string): Promise<LibrarySearchDto> {
    return this.call('search_library', { query });
  }

  previewBrowserFile(path: string, audition: boolean): Promise<BrowserPreviewDto> {
    return this.call('preview_browser_file', { path, audition });
  }

  stopBrowserPreview(): Promise<void> {
    return this.call('stop_browser_preview');
  }

  previewAudioFile(path: string): Promise<void> {
    return this.call('preview_audio_file', { path });
  }

  previewTrackSample(track: number): Promise<void> {
    return this.call('preview_track_sample', { track });
  }

  previewInstrument(sourceId: string): Promise<void> {
    return this.call('preview_instrument', { sourceId });
  }

  getPreviewSettings(): Promise<PreviewSettings> {
    return this.call('get_preview_settings');
  }

  setPreviewVolume(percent: number): Promise<PreviewSettings> {
    return this.call('set_preview_volume', { percent });
  }

  getAudioSettings(): Promise<AudioOutput> {
    return this.call('get_audio_settings');
  }

  setAudioSettings(preferAsio: boolean, bufferFrames: number): Promise<AudioOutput> {
    return this.call('set_audio_settings', { preferAsio, bufferFrames });
  }

  addAudioClip(
    sourceId: number,
    track: number,
    start: number,
    length?: number,
    offsetFrames?: number,
    gainDb?: number,
  ): Promise<SequencerState> {
    return this.call('add_audio_clip', {
      sourceId,
      track,
      start,
      length: length ?? null,
      offsetFrames: offsetFrames ?? null,
      gainDb: gainDb ?? null,
    });
  }

  setClipGain(clipId: number, gainDb: number): Promise<SequencerState> {
    return this.call('set_clip_gain', { clipId, gainDb });
  }

  setClipOffset(clipId: number, offsetFrames: number): Promise<SequencerState> {
    return this.call('set_clip_offset', { clipId, offsetFrames });
  }

  getProjectInfo(): Promise<ProjectInfo> {
    return this.call('get_project_info');
  }

  newProject(): Promise<ProjectInfo> {
    return this.call('new_project');
  }

  // `path: null` rather than an omitted key, because that is what asks Rust for
  // the dialog. An absent argument would be a `None` too, but only by accident
  // of how Tauri deserialises a missing field.
  openProject(path?: string | null): Promise<ProjectInfo | null> {
    return this.call('open_project', { path: path ?? null });
  }

  saveProject(saveAs: boolean, path?: string | null): Promise<ProjectInfo | null> {
    return this.call('save_project', { saveAs, path: path ?? null });
  }

  getStreamInfo(): Promise<StreamInfo> {
    return this.call('get_stream_info');
  }

  getInputDevices(): Promise<InputDeviceDto[]> {
    return this.call('get_input_devices');
  }

  // `deviceId: null` rather than an omitted key, for the reason `openProject`
  // passes `path: null`: that is what asks Rust for the system default.
  setInputDevice(deviceId: string | null): Promise<InputStatusDto> {
    return this.call('set_input_device', { deviceId });
  }

  getInputStatus(): Promise<InputStatusDto> {
    return this.call('get_input_status');
  }

  setInputArmed(armed: boolean): Promise<InputStatusDto> {
    return this.call('set_input_armed', { armed });
  }

  startRecording(): Promise<InputStatusDto> {
    return this.call('start_recording');
  }

  stopRecording(): Promise<RecordedTakeDto> {
    return this.call('stop_recording');
  }

  cancelRecording(): Promise<InputStatusDto> {
    return this.call('cancel_recording');
  }

  listMidiPorts(): Promise<MidiPortDto[]> {
    return this.call('list_midi_ports');
  }

  setMidiPortEnabled(name: string, enabled: boolean): Promise<MidiPortDto[]> {
    return this.call('set_midi_port_enabled', { name, enabled });
  }

  getMidiStatus(): Promise<MidiStatusDto> {
    return this.call('get_midi_status');
  }

  /** Map one downloader's DTO onto the UI's asset row. */
  private static assetRow(dto: AssetStatusDto): AssetStatus {
    const downloading =
      dto.state === 'downloading' ||
      dto.state === 'verifying' ||
      // Files on disk but still decoding in the background: not ready yet, so
      // keep the row busy rather than offering a dead piano.
      (dto.installed && !dto.instrumentReady && dto.state !== 'error');
    const state: AssetStatus['state'] =
      dto.state === 'error'
        ? 'failed'
        : dto.instrumentReady
          ? 'ready'
          : downloading
            ? 'downloading'
            : 'absent';
    return {
      id: dto.instrumentId,
      name: dto.instrumentName,
      state,
      progress:
        state === 'ready' ? 1 : dto.totalBytes > 0 ? dto.downloadedBytes / dto.totalBytes : 0,
      bytesTotal: dto.totalBytes,
      bytesDone: dto.downloadedBytes,
      error: dto.error.length > 0 ? dto.error : null,
    };
  }

  async getAssetStatus(): Promise<AssetStatus[]> {
    return (await this.call<AssetStatusDto[]>('get_asset_status')).map(TauriBackend.assetRow);
  }

  async startAssetDownload(id: string): Promise<AssetStatus[]> {
    return (await this.call<AssetStatusDto[]>('start_asset_download', { id })).map(
      TauriBackend.assetRow,
    );
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
// Selection
// ---------------------------------------------------------------------------

let cached: Backend | undefined;

/**
 * The process-wide backend: the real one when the Tauri internals are injected,
 * the mock in a plain browser.
 *
 * The mock is imported lazily *and* synchronously-registered through
 * `setMockFactory` so that `backend.ts` does not pull the whole fixture file
 * into the app bundle. See `main.tsx`.
 */
export function getBackend(): Backend {
  if (!cached) {
    if (isTauri()) {
      cached = new TauriBackend();
    } else {
      if (!mockFactory) {
        throw new Error(
          'no Tauri internals and no mock registered: call setMockFactory() before getBackend()',
        );
      }
      cached = mockFactory();
    }
  }
  return cached;
}

let mockFactory: (() => Backend) | undefined;

/** Register the browser-only backend. Called from `main.tsx` before mount. */
export function setMockFactory(factory: () => Backend): void {
  mockFactory = factory;
}

export { TauriBackend };
