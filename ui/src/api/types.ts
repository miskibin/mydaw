/**
 * Wire types mirrored from the Rust side.
 *
 * These are hand-mirrored on purpose: the Rust side is the source of truth and
 * any drift should show up as a compile error in the view that consumes them.
 * The authoritative definitions are `app/src/dto.rs` (sequencer, meters,
 * plugins), `app/src/project.rs` (mixer), `app/src/effects.rs` (the effect
 * catalogue) and `app/src/source.rs` (instrument sources).
 *
 * Two conventions run through all of it:
 *
 * - **camelCase.** serde renames on the way out; the consumer is TypeScript.
 * - **Ids, not indices, for anything a user or a file can name.** A pattern, a
 *   playlist lane, a clip, a send and an insert each carry a stable number.
 *   Positions change under insertion and deletion.
 */

/** Lifecycle state of a kernel plugin, as reported by the registry. */
export type PluginState = 'registered' | 'started' | 'stopped';

/** One row of the kernel plugin registry. */
export interface PluginStatusDto {
  id: string;
  name: string;
  state: PluginState;
}

/**
 * A UI panel contributed by a plugin via `ui.contribute_panel(...)`.
 *
 * The panel dock is gone, but the manifest list is still how the dev-only IPC
 * bench announces itself (`ipc-bench-auto`), which is the only thing that reads
 * this now.
 */
export interface PanelManifestDto {
  id: string;
  title: string;
  component: string;
}

/** Full UI state pushed/pulled in one shot at startup. */
export interface UiSnapshot {
  plugins: PluginStatusDto[];
  panels: PanelManifestDto[];
}

/**
 * One strip's level for a single block. Mirrors `TrackMeterDto` in
 * `app/src/dto.rs`.
 *
 * Measured after that strip's fader/pan/mute and **before** the master fader,
 * which is what a channel meter in a mixer shows.
 */
export interface TrackMeter {
  peakL: number;
  peakR: number;
}

/**
 * A single metering packet from the RT graph, throttled to 30-60 Hz by the
 * bridge. Peak/RMS are linear amplitudes in `[0, 1+]` (they exceed 1 when
 * clipping). `samplePos` is the transport position in frames.
 */
export interface MeterFrame {
  /** Master bus, post master fader. */
  peakL: number;
  peakR: number;
  rmsL: number;
  rmsR: number;
  samplePos: number;
  playing: boolean;
  /** One entry per track, in track order. Empty before the first graph. */
  tracks: TrackMeter[];
  /**
   * One entry per bus, in bus order. A separate list rather than more entries
   * in `tracks`, because a bus is a different strip in a different index space:
   * `buses[0]` is bus 0, not "the track after the last one".
   */
  buses: TrackMeter[];
  /**
   * When the pump handed this frame to the webview, on the Rust bench clock.
   * Absent in every normal run -- only the IPC benchmark asks for it.
   */
  pumpMs?: number;
}

/**
 * What a CLAP plugin is *for*, decided by the host from the bundle's `features`
 * list. This is what decides which picker a plugin turns up in.
 */
export type ClapPluginKind = 'instrument' | 'audio-effect' | 'note-effect' | 'other';

/** Which plugin format a row in a unified list came from. */
export type PluginFormat = 'CLAP' | 'VST3';

/**
 * One installed CLAP plugin, as reported by the host's scan.
 *
 * `vendor` and `version` may be empty strings: the CLAP format only requires an
 * id, and a bundle that omits the rest is still perfectly loadable.
 */
export interface ClapPluginInfoDto {
  /** Stable CLAP id, e.g. `com.u-he.diva`. Unique across the whole scan. */
  id: string;
  name: string;
  vendor: string;
  version: string;
  /** Absolute path of the `.clap` bundle the plugin came from. */
  path: string;
  /** Which picker this plugin belongs in. Derived from `features` by Rust. */
  kind: ClapPluginKind;
  /** The bundle's raw CLAP feature strings, carried verbatim as evidence. */
  features: string[];
}

/**
 * What a VST3 plugin is *for*, from the class's declared `subCategories`.
 *
 * `unknown` has no CLAP equivalent: a module that exposes only the older
 * `IPluginFactory` declares no subcategories at all, which is a real state
 * rather than a failure.
 */
export type Vst3PluginKind = 'instrument' | 'audio-effect' | 'other' | 'unknown';

/** One installed VST3 plugin class, as reported by the host's scan. */
export interface Vst3PluginInfoDto {
  /** The class id as 32 uppercase hex digits. Unique within a module. */
  id: string;
  name: string;
  vendor: string;
  version: string;
  /** Absolute path of the `.vst3` module: a bundle directory or a flat DLL. */
  path: string;
  kind: Vst3PluginKind;
  /** The class's declared subcategories, split for display (`['Fx', 'Reverb']`). */
  subcategories: string[];
}

/** State of one placed VST3 plugin's editor window. */
export interface Vst3GuiStatus {
  open: boolean;
  /** Whether the plugin draws an editor at all. False is normal, not an error. */
  hasEditor: boolean;
  cid: string | null;
  width: number;
  height: number;
}

/** A hosted VST3 plugin in a track's source slot or the master's insert. */
export interface Vst3Slot {
  instance: number;
  cid: string;
  name: string;
  vendor: string;
  modulePath: string;
  hasEditor: boolean;
  guiOpen: boolean;
}

/**
 * Which placed plugin a hosted-plugin command addresses.
 *
 * A tagged union rather than "a track index, or -1 for the master": several
 * editors can be open at once, so a target is a lasting identity rather than a
 * momentary selection.
 */
export type ClapTarget = { slot: 'track'; index: number } | { slot: 'master' };

/**
 * State of one placed plugin's editor window.
 *
 * The editor is a *native* window next to MYDAW's, not a region of this webview:
 * a plugin draws with its own toolkit into an OS window handle, which cannot be
 * composited into WebView2. All the UI can do is ask for it to be opened or
 * closed, and show what came back.
 */
export interface ClapGuiStatus {
  open: boolean;
  hasEditor: boolean;
  pluginId: string | null;
  width: number;
  height: number;
}

/** A hosted CLAP plugin in a track's source slot or the master's insert. */
export interface ClapSlot {
  instance: number;
  pluginId: string;
  name: string;
  bundlePath: string;
  /**
   * Whether this plugin draws an editor. When false the UI shows no editor
   * button at all -- a plugin with no GUI is an ordinary plugin, and a button
   * that always answers "there is no editor" is worse than no button.
   */
  hasEditor: boolean;
  guiOpen: boolean;
}

/** The sampler's asset-download progress, exactly as Rust reports it. */
export interface AssetStatusDto {
  installed: boolean;
  /**
   * Whether the instrument is loaded and playable, which lags `installed`:
   * decoding is a background job, so there is a window in which the files are
   * all there and the source picker still has nothing to offer.
   */
  instrumentReady: boolean;
  state: 'idle' | 'downloading' | 'verifying' | 'done' | 'error';
  totalBytes: number;
  downloadedBytes: number;
  filesDone: number;
  fileCount: number;
  instrumentId: string;
  instrumentName: string;
  /** Empty unless `state` is `error`. */
  error: string;
}

/** One thing the engine can put at the head of a track. */
export interface SourceDto {
  /**
   * Stable id: `poly-synth`, `kick`, `sampler:<instrumentId>`,
   * `clap:<pluginId>` or `vst3:<cid>`.
   */
  id: string;
  label: string;
  description: string;
  kind: 'internal' | 'clap' | 'vst3';
  vendor: string;
  bundlePath: string | null;
  pluginId: string | null;
  /** Whether this is the selected track's source. Exactly one is true. */
  selected: boolean;
  /** Whether the computer-keyboard piano does anything for this source. */
  takesNotes: boolean;
}

// ---------------------------------------------------------------------------
// The sequencer: patterns, the playlist, the transport mode
// ---------------------------------------------------------------------------

/**
 * One cell of the step grid.
 *
 * `velocity` is carried even when `on` is false, so toggling a step off and on
 * again does not silently discard an accent.
 */
export interface StepDto {
  on: boolean;
  /** Normalised velocity, `0..1`. */
  velocity: number;
}

/**
 * One channel's step grid.
 *
 * `label` is derived by Rust from `key` (the General MIDI drum map) and ignored
 * on the way back in: the row's identity is its key, and the name is a
 * rendering of it. The UI must not keep its own copy of that map.
 */
export interface StepRowDto {
  /** MIDI key every switched-on cell triggers. */
  key: number;
  label: string;
  /** Exactly `PatternDto.steps` entries -- the cells that play. */
  steps: StepDto[];
}

/**
 * One note of the piano roll.
 *
 * `start` and `length` are in **ticks**: `PPQ` (960) to the quarter note, so a
 * sixteenth is `TICKS_PER_STEP` (240). Ticks rather than steps because a note's
 * whole point is that it need not sit on the grid.
 *
 * **No channel.** The note lives inside a `ChannelClipDto`, which knows which
 * channel it is; carrying it twice was two facts that could disagree.
 */
export interface MelodicNoteDto {
  /** MIDI key, `0..127`. 60 is middle C. */
  key: number;
  start: number;
  length: number;
  /** Normalised velocity, `0..1`. */
  velocity: number;
}

/**
 * What one pattern says to one channel.
 *
 * `mode` is derived from whether there are notes and is ignored on the way in:
 * the content decides. A grid cannot show a note length, so notes win.
 */
export interface ChannelClipDto {
  /** The rack channel, the mixer strip and the note channel: one index. */
  channel: number;
  mode: 'steps' | 'roll';
  /** The step grid, when this channel has one. Absent otherwise. */
  steps?: StepRowDto | null | undefined;
  notes: MelodicNoteDto[];
}

/** A whole pattern, as the rack and the roll read and write it. */
export interface PatternDto {
  /** Stable across a save and load. Playlist clips reference it. */
  id: number;
  name: string;
  /** Absent when the engine has none -- the engine has no palette. */
  color?: string | null;
  /** Steps in the loop: 16, 32 or 64. Every step row carries this many cells. */
  steps: number;
  channels: ChannelClipDto[];
}

/** One row of the pattern picker: everything a list needs, nothing a grid does. */
export interface PatternSummaryDto {
  id: number;
  name: string;
  color?: string | null;
  steps: number;
  /** Ticks in one time round the loop. `steps * 240`. */
  lengthTicks: number;
  channelCount: number;
  hitCount: number;
  noteCount: number;
}

/** The pattern picker's whole state, plus the pattern being edited. */
export interface PatternSetDto {
  patterns: PatternSummaryDto[];
  activeId: number;
  active: PatternDto;
  maxPatterns: number;
}

/** One lane of the arrangement. */
export interface PlaylistTrackDto {
  id: number;
  name: string;
  muted: boolean;
  soloed: boolean;
}

/**
 * One clip on the timeline.
 *
 * One shape for both kinds rather than a tagged union, because the UI draws
 * them with the same geometry and only the fill differs: `kind` says which, and
 * the fields the other kind does not have are simply absent from the JSON.
 */
export interface ClipDto {
  id: number;
  kind: 'pattern' | 'audio';
  /** Index of the lane it is drawn on. */
  track: number;
  /** Where it starts, in absolute song ticks. */
  start: number;
  /** How long it plays for, in song ticks. */
  length: number;
  /** The pattern it plays. Absent for an audio clip. */
  patternId?: number;
  /** The loaded audio file it plays. Absent for a pattern clip. */
  sourceId?: number;
  /** How far into that file playback begins, in the file's own frames. */
  offsetFrames?: number;
  /** Per-clip trim in dB. Absent for a pattern clip. */
  gainDb?: number;
}

/** The arrangement. */
export interface PlaylistDto {
  tracks: PlaylistTrackDto[];
  clips: ClipDto[];
  /** One past the last tick any clip occupies, and where SONG mode stops. */
  endTicks: number;
  maxTracks: number;
  maxClips: number;
  maxTicks: number;
}

/**
 * One loaded audio file.
 *
 * Everything here is a property of the *file*, not of any clip that plays it.
 * The waveform is deliberately not in it -- see `WaveformDto`.
 */
export interface AudioSourceDto {
  id: number;
  path: string;
  name: string;
  /** Playable frames, in the file's own rate. `0` for a file that has moved. */
  frames: number;
  sampleRate: number;
  durationSeconds: number;
  /**
   * How long the file is at the session's **current** tempo, in ticks -- which
   * is what a clip drawn to cover the whole take should be. It moves with the
   * tempo, because a tick does; the audio does not stretch.
   */
  durationTicks: number;
  /** Whether the file could not be found. Its clips stay, and are silent. */
  missing: boolean;
  bytes: number;
}

/** A root in the sample library: bundled Packs, or a user extra-search folder. */
export interface LibraryRootDto {
  id: string;
  name: string;
  kind: 'packs' | 'extra';
  path: string;
}

/** One child of a listed library directory. */
export interface LibraryEntryDto {
  name: string;
  kind: 'dir' | 'file';
  relPath: string;
  path: string;
}

/**
 * A source's waveform at the resolution that was asked for: one `[min, max]`
 * pair per bin, over **both** channels, so the outer bound never hides a
 * transient that is on one channel only.
 */
export interface WaveformDto {
  sourceId: number;
  frames: number;
  /** Pairs actually returned, which may be fewer than asked for. */
  bins: number;
  maxBins: number;
  peaks: [number, number][];
}

/**
 * One hit of a built-in or sampled instrument, as min/max pairs.
 *
 * Distinct from {@link WaveformDto}: that is a loaded audio *file* addressed by
 * a numeric id, and this is an instrument addressed by its source string.
 */
export interface SourcePreviewDto {
  sourceId: string;
  engine: 'synth' | 'sampler' | 'clap' | 'vst3' | 'tone' | 'clip' | 'sample';
  frames: number;
  sampleRate: number;
  durationMs: number;
  bins: number;
  peaks: [number, number][];
  /** Largest absolute sample. Near zero means the instrument produced silence. */
  peak: number;
}

/** Which of the two timelines the transport plays. */
export type PlayMode = 'pat' | 'song';

/**
 * Everything the rack, the roll, the playlist and the transport need in one
 * message. Every sequencer command returns this whole shape.
 */
export interface SequencerState {
  bpm: number;
  /**
   * The open device's sample rate, or `0` when no stream is running.
   *
   * The views need it to turn the transport position they get in *frames* into
   * a tick. Without it the UI would have to assume 48 kHz and the playhead
   * would sit in the wrong place on a 44.1 kHz interface.
   */
  sampleRate: number;
  playMode: PlayMode;
  /**
   * Whether SONG repeats the arrangement instead of stopping at the last clip.
   *
   * PAT loops whatever this says -- a pattern is a loop by definition. The
   * engine implements the repeat as an offset in *ticks* rather than a seek, so
   * the transport position keeps rising past the end of the song and a view
   * that draws a playhead has to fold it back (`loopedTicks`).
   */
  looping: boolean;
  /** Ticks per quarter note: 960. Sent so there is one definition of the grid. */
  ppq: number;
  /** Ticks in one step of the grid (a sixteenth): 240. */
  ticksPerStep: number;
  /** Ticks in one bar of 4/4: 3840. */
  ticksPerBar: number;
  patternSet: PatternSetDto;
  playlist: PlaylistDto;
}

// ---------------------------------------------------------------------------
// The mixer: strips, buses, sends, insert chains
// ---------------------------------------------------------------------------

/** One plugin a project asked for and did not get. */
export interface MissingPlugin {
  name: string;
  /** `clap` or `vst3`, so a warning can say which format is absent. */
  format: string;
  path: string;
  reason: string;
}

/** One parameter's or one meter's current value. */
export interface ParamValueDto {
  id: string;
  value: number;
}

/** One internal effect placed in a chain. */
export interface InsertDto {
  /** Position in the chain, which is also the index every command takes. */
  index: number;
  /** Stable identity across reordering. Key a UI list on this. */
  id: number;
  typeId: string;
  name: string;
  /** The bypass switch's position, lifted out because every strip draws it. */
  bypassed: boolean;
  /** Every parameter's current value, in declaration order. */
  params: ParamValueDto[];
  /** Every meter's current value. Empty for the effects that publish none. */
  readouts: ParamValueDto[];
  /**
   * Which strip feeds this insert's detector, or `null`.
   *
   * A **route**, not a parameter: only the compressor reads it, it names a
   * strip by position, and it is set with `setInsertSidechain` rather than by
   * moving a knob. Every other effect reports `null`.
   */
  sidechain: StripRef | null;
}

/** A strip named by position. The same pair a send's source uses. */
export interface StripRef {
  kind: 'track' | 'bus';
  index: number;
}

/** Every meter on one insert, as the polling call returns them. */
export interface InsertReadoutsDto {
  index: number;
  id: number;
  readouts: ParamValueDto[];
}

/**
 * How a knob should lay one parameter's range out. The stored value is always
 * the real value in the parameter's own unit.
 */
export type ParamCurve = 'linear' | 'log' | 'db';

/** The metadata a generic knob panel renders one parameter from. */
export interface ParamDefDto {
  id: string;
  name: string;
  /** `"dB"`, `"Hz"`, `"ms"`, `"%"`, `":1"`, or empty. */
  unit: string;
  min: number;
  max: number;
  /** Named this because `default` is a reserved word in too many languages. */
  defaultValue: number;
  curve: ParamCurve;
  /** Quantisation in the parameter's own unit, or `null` for continuous. */
  step: number | null;
  /**
   * Ramp length in milliseconds. `0` marks a **switch**: the UI draws it as one
   * and must not animate it.
   */
  smoothingMs: number;
}

/** One live meter an effect publishes. */
export interface ReadoutDefDto {
  id: string;
  name: string;
  unit: string;
}

/**
 * One hop of analysis for the focused insert editor.
 *
 * `bins*` are log-spaced dB from 20 Hz to 20 kHz (same axis as the EQ).
 * `scope*` are downsampled L/R pairs for the goniometer.
 */
export interface SpectrumDto {
  binsPre: number[];
  binsPost: number[];
  scopeL: number[];
  scopeR: number[];
  correlation: number;
}

/** One entry of the internal effect catalogue. Static: fetched once. */
export interface EffectInfoDto {
  typeId: string;
  name: string;
  description: string;
  params: ParamDefDto[];
  readouts: ReadoutDefDto[];
}

/** Which chain an insert command addresses. */
export type InsertTargetKind = 'track' | 'bus' | 'master';

/**
 * A chain, as the UI addresses one. `index` is ignored for the master, and sent
 * as `0` so the argument is never absent.
 */
export interface InsertTarget {
  kind: InsertTargetKind;
  index: number;
}

/** How a sample channel's playback root was last set. */
export type RootMode = 'default' | 'detected' | 'manual';

/** Where a detected root pitch came from. */
export type PitchSource = 'metadata' | 'filename' | 'analysis';

/** Why analysis could not name a root. */
export type NoPitchReason = 'silent' | 'tooShort' | 'unvoiced' | 'unstable' | 'polyphonic';

/** A named root: MIDI note plus remaining cents. */
export interface DetectedPitch {
  midiNote: number;
  cents: number;
  confidence: number;
  voicedCoverage: number;
  source: PitchSource;
}

/** Result of `analyzeTrackPitch`. Never invents C for unpitched material. */
export type PitchAnalysis =
  | ({ kind: 'detected' } & DetectedPitch)
  | { kind: 'noStablePitch'; reason: NoPitchReason };

/** One mixer track. `index` is the track's position *and* its note channel. */
export interface TrackDto {
  index: number;
  /** Stable identity across renumbering. Key a UI list on this. */
  id: number;
  name: string;
  sourceId: string;
  sourceLabel: string;
  /** What actually makes the sound: `synth`, `sampler`, `clap`, `vst3`, `tone`, `clip`, `sample`. */
  engine: 'synth' | 'sampler' | 'clap' | 'vst3' | 'tone' | 'clip' | 'sample';
  gainDb: number;
  /** `-1` hard left, `0` centre, `1` hard right. */
  pan: number;
  /** The button state, not the outcome. */
  muted: boolean;
  soloed: boolean;
  /** Whether this track is actually silent right now, solo resolved. */
  silenced: boolean;
  selected: boolean;
  clap: ClapSlot | null;
  vst3: Vst3Slot | null;
  missing: MissingPlugin | null;
  /** Internal effects on this track, in signal order. */
  inserts: InsertDto[];
  /** Channel pitch in cents. Meaningful for `clip` and `sample` engines. */
  pitchCents: number;
  /**
   * Channel fade in, in milliseconds, applied to every clip (or note) of it.
   *
   * Milliseconds and not frames, unlike the region: a region is a place in a
   * file and a fade is a length of time, so the same number has to mean the
   * same thing whatever the file's own rate is.
   */
  fadeInMs: number;
  /** Channel fade out, in milliseconds. */
  fadeOutMs: number;
  /**
   * What the instrument at the head of this channel exposes as knobs.
   *
   * Empty for an instrument with none -- every drum, the test tone, a sampled
   * instrument, and a hosted plugin, which has its own editor window. The defs
   * travel with the track rather than in a catalogue because they are what the
   * generic knob panel draws from, and a panel that had to join two messages
   * to draw one window is a panel that can draw half of one.
   */
  sourceParamDefs: ParamDefDto[];
  /** Their current values, in declaration order. */
  sourceParams: ParamValueDto[];
  /** SMP region start, in the file's own frames. */
  sampleStartFrames: number;
  /** SMP region end, exclusive. `0` means the whole file. */
  sampleEndFrames: number;
  /** MIDI note at which the file plays at recorded pitch. Default 60 (C4). */
  rootKey: number;
  /** Fine offset of that root, in cents. */
  rootFineCents: number;
  /** How the root was last set. */
  rootMode: RootMode;
  /** Last successful detection, when one has been stored. */
  detectedPitch?: DetectedPitch | null;
  /** Numeric audio-file id when this channel is `clip:` or `oneshot:`. */
  audioSourceId?: number | null;
}

/** One bus. `index` is what a send's `dest` names. */
export interface BusDto {
  index: number;
  id: number;
  name: string;
  /** Absent when the engine has none. */
  color?: string | null;
  gainDb: number;
  pan: number;
  muted: boolean;
  soloed: boolean;
  /** Solo is resolved across tracks *and* buses: they share one scope. */
  silenced: boolean;
  inserts: InsertDto[];
}

/** One send tap. Every send command addresses `id`, never a pair of indices. */
export interface SendDto {
  id: number;
  sourceKind: 'track' | 'bus';
  sourceIndex: number;
  /** Index of the destination bus. */
  dest: number;
  /** Linear level, `0..=maxSendLevel`. `1.0` is unity. */
  level: number;
  /**
   * The same level in decibels, so a knob can lay its range out logarithmically
   * without the UI keeping its own copy of the conversion.
   */
  levelDb: number;
  /** Whether the tap is taken before the source's fader, pan and mute. */
  pre: boolean;
}

/** The master strip. */
export interface MasterDto {
  gainDb: number;
  clap: ClapSlot | null;
  vst3: Vst3Slot | null;
  missing: MissingPlugin | null;
  /** Internal effects on the master bus, **ahead of** the hosted insert. */
  inserts: InsertDto[];
}

/**
 * Everything the mixer needs in one message. Every mixer command returns this
 * whole shape rather than an acknowledgement.
 */
export interface MixerState {
  tracks: TrackDto[];
  buses: BusDto[];
  /**
   * Every send tap, from both kinds of source. The UI draws "sends from this
   * strip" and "what feeds this bus" by filtering this one list -- two copies
   * of a relationship can disagree.
   */
  sends: SendDto[];
  master: MasterDto;
  selected: number;
  maxTracks: number;
  maxBuses: number;
  maxInserts: number;
  minGainDb: number;
  maxGainDb: number;
  maxSendLevel: number;
  maxSendDb: number;
}

/** Which project is open. Every project command answers with this. */
export interface ProjectInfo {
  path: string | null;
  name: string;
  missingPlugins: number;
  /** Sentences rather than codes, because they go straight into the status bar. */
  warnings: string[];
}

/**
 * What the open audio device is.
 *
 * `running: false` with every number at zero is the ordinary "audio has not
 * started" state, not a failure -- the status bar draws a dash for it.
 */
export interface StreamInfo {
  deviceName: string;
  sampleRate: number;
  channels: number;
  /** Frames the last callback was handed: the BUFFER readout. */
  bufferFrames: number;
  /** Engine allocation ceiling. Not the BUFFER readout. */
  maxBufferFrames: number;
  /** What Settings requested. `0` = host default. */
  requestedBufferFrames: number;
  latencyMs: number;
  /** `"wasapi"` or `"asio"`. */
  backend: 'wasapi' | 'asio';
  /** Why the opened backend is not what was asked for, or empty. */
  note: string;
  running: boolean;
}

/** WASAPI/ASIO preference plus the stream currently open. */
export interface AudioOutput {
  version: number;
  preferAsio: boolean;
  bufferFrames: number;
  asioCompiled: boolean;
  stream: StreamInfo;
}

/** One MIDI input port, as Settings draws it. */
export type MidiPortStatus = 'connected' | 'offline' | 'error' | 'available';

export interface MidiPortDto {
  name: string;
  /** Currently open (paired and present). */
  connected: boolean;
  /** The user has paired it. */
  enabled: boolean;
  status: MidiPortStatus;
  lastError?: string;
}

/** Status-bar MIDI lamp. `activityAtMs` is Unix ms of the last event, 0 = never. */
export interface MidiStatusDto {
  activityAtMs: number;
  enabledCount: number;
  activeName: string | null;
}

/** Browser / Sounds audition level, as a percent of the current master volume. */
export interface PreviewSettings {
  version: number;
  volumePercent: number;
}

/** One downloadable instrument asset, as the browser's download row reads it. */
export interface AssetStatus {
  id: string;
  name: string;
  state: 'absent' | 'downloading' | 'ready' | 'failed';
  /** `0..1` while downloading; `1` when ready. */
  progress: number;
  bytesTotal: number;
  bytesDone: number;
  error: string | null;
}

/** Unsubscribe handle returned by streaming APIs. */
export type Unsubscribe = () => void;
