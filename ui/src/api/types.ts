/**
 * Wire types mirrored from the Rust `ipc-bridge` crate.
 *
 * These are hand-mirrored on purpose (Phase 0): the Rust side is the source of
 * truth and any drift should show up as a compile error in the panels that
 * consume them. If the surface grows past a handful of structs, generate this
 * file from the Rust definitions instead.
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
 * `component` is a registry key resolved against `src/panels/registry.ts`.
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
 * One track's level for a single block. Mirrors `TrackMeterDto` in
 * `app/src/dto.rs`.
 *
 * Measured after that track's fader/pan/mute and **before** the master fader,
 * which is what a channel meter in a mixer shows.
 */
export interface TrackMeter {
  peakL: number;
  peakR: number;
}

/**
 * A single metering packet from the RT graph, throttled to 30-60 Hz by the
 * bridge. Peak/RMS are linear amplitudes in [0, 1+] (may exceed 1 when
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
  /**
   * One entry per track in the project, in track order. Empty before the first
   * graph is installed.
   */
  tracks: TrackMeter[];
  /**
   * When the pump handed this frame to the webview, on the Rust bench clock.
   *
   * Absent in every normal run: only the spike-4 IPC benchmark asks the pump to
   * stamp frames, and it is the only thing that reads this.
   */
  pumpMs?: number;
}

/**
 * What a CLAP plugin is *for*, decided by the host from the bundle's `features`
 * list. Mirrors `ClapPluginKindDto` in `app/src/dto.rs`.
 *
 * This is what decides which picker a plugin turns up in: instruments are offered
 * as track sources, audio effects as the master insert. `other` is not a failure
 * -- a bundle may declare no recognisable feature at all -- it just means MYDAW
 * has nowhere to put it yet.
 */
export type ClapPluginKind = 'instrument' | 'audio-effect' | 'note-effect' | 'other';

/** Which plugin format a row in a unified list came from. */
export type PluginFormat = 'CLAP' | 'VST3';

/**
 * One installed CLAP plugin, as reported by the host's scan. Mirrors
 * `ClapPluginInfoDto` in `app/src/dto.rs`.
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
  /**
   * The bundle's raw CLAP feature strings, e.g. `['audio-effect', 'reverb']`.
   *
   * Carried verbatim rather than re-interpreted here: `kind` is the host's
   * reading of them, and this is the evidence, which the browser shows as
   * tooltip detail so a surprising classification can be explained rather than
   * argued with.
   */
  features: string[];
}

/**
 * What a VST3 plugin is *for*, decided by the host from the class's declared
 * `subCategories`. Mirrors `Vst3PluginInfoDto.kind` in `app/src/dto.rs`.
 *
 * Shares `instrument` and `audio-effect` with `ClapPluginKind` on purpose, so one
 * list can badge either format. `unknown` has no CLAP equivalent: a VST3 module
 * that exposes only the older `IPluginFactory` declares no subcategories at all,
 * which is a real state rather than a failure -- MYDAW offers such a plugin as an
 * effect and lets the bus-layout check at load time decide.
 */
export type Vst3PluginKind = 'instrument' | 'audio-effect' | 'other' | 'unknown';

/**
 * One installed VST3 plugin class, as reported by the host's scan. Mirrors
 * `Vst3PluginInfoDto` in `app/src/dto.rs`.
 */
export interface Vst3PluginInfoDto {
  /**
   * The class id as 32 uppercase hex digits, e.g. `58545356736673586572756D...`.
   *
   * Not a reverse-DNS string like a CLAP id: VST3 identifies a class by sixteen
   * raw bytes, and this is those bytes. Unique within a module.
   */
  id: string;
  name: string;
  vendor: string;
  version: string;
  /** Absolute path of the `.vst3` module: a bundle directory or a flat DLL. */
  path: string;
  /** Which picker this plugin belongs in. Derived from `subcategories` by Rust. */
  kind: Vst3PluginKind;
  /**
   * The class's declared subcategories, split for display (`['Fx', 'Reverb']`).
   *
   * VST3 carries these as one `|`-separated string; Rust splits it. Empty for a
   * module with no `IPluginFactory2`, which is exactly the `unknown` case.
   */
  subcategories: string[];
}

/**
 * State of one placed VST3 plugin's editor window. Mirrors `Vst3GuiStatusDto` in
 * `app/src/vst3_bridge.rs`.
 */
export interface Vst3GuiStatus {
  open: boolean;
  /** Whether the plugin draws an editor at all. False is normal, not an error. */
  hasEditor: boolean;
  /** Class id of the plugin the editor belongs to, if one is placed. */
  cid: string | null;
  width: number;
  height: number;
}

/**
 * A hosted VST3 plugin sitting in a track's source slot or the master's insert.
 * Mirrors `Vst3SlotDto` in `app/src/vst3_bridge.rs`.
 *
 * The sibling of `ClapSlot`. Two shapes rather than one because the identifying
 * field genuinely differs -- `cid` is 32 hex digits where CLAP has a `pluginId`
 * string -- and a shared type would have had to call one by the other's name.
 */
export interface Vst3Slot {
  /** Opaque handle for this instance, stable while the plugin stays placed. */
  instance: number;
  /** Class id, as hex. */
  cid: string;
  name: string;
  vendor: string;
  /** Absolute path of the `.vst3` module it was loaded from. */
  modulePath: string;
  hasEditor: boolean;
  guiOpen: boolean;
}

/**
 * Which placed plugin a CLAP command addresses. Mirrors `ClapTargetDto` in
 * `app/src/dto.rs`.
 *
 * A tagged union rather than "a track index, or -1 for the master": several
 * editors can be open at once, so a target is a lasting identity rather than a
 * momentary selection, and a sentinel index is exactly the kind of thing that
 * survives one refactor and then addresses the wrong strip.
 */
export type ClapTarget = { slot: 'track'; index: number } | { slot: 'master' };

/**
 * State of one placed plugin's editor window. Mirrors `ClapGuiStatusDto` in
 * `app/src/clap_bridge.rs`.
 *
 * The editor is a *native* window next to MYDAW's, not a region of this webview:
 * a plugin draws with its own toolkit into an OS window handle, which cannot be
 * composited into WebView2. All the UI can do is ask for it to be opened or
 * closed, and show what came back.
 */
export interface ClapGuiStatus {
  /** Whether an editor window is currently open. */
  open: boolean;
  /** Whether the plugin draws an editor at all. False is normal, not an error. */
  hasEditor: boolean;
  /** CLAP id of the plugin the editor belongs to, if one is loaded. */
  pluginId: string | null;
  /** Editor size in physical pixels; `0` when closed. */
  width: number;
  height: number;
}

/**
 * A hosted CLAP plugin sitting in a track's source slot or the master's insert.
 * Mirrors `ClapSlotDto` in `app/src/project.rs`.
 *
 * Present in `MixerState` rather than fetched separately because every strip
 * that draws a plugin also draws its editor button, and the two must never
 * disagree about whether the window is up.
 */
export interface ClapSlot {
  /**
   * Opaque handle for this instance, stable for as long as the plugin stays
   * placed. Only meaningful to Rust; the UI carries it and never reads into it.
   */
  instance: number;
  /** CLAP id of the plugin in the slot, e.g. `com.u-he.diva`. */
  pluginId: string;
  name: string;
  /** Absolute path of the `.clap` bundle it was loaded from. */
  bundlePath: string;
  /**
   * Whether this plugin draws an editor. When false the strip shows no editor
   * button at all -- a plugin with no GUI is an ordinary plugin, and a button
   * that always answers "there is no editor" is worse than no button.
   */
  hasEditor: boolean;
  /** Whether its editor window is up right now. */
  guiOpen: boolean;
}

/**
 * One thing the engine can put at the head of a track. Mirrors `SourceDto` in
 * `app/src/source.rs`.
 *
 * The list is the three built-ins in a fixed order followed by every scanned
 * CLAP *instrument*, and it comes from Rust rather than being hard-coded here,
 * so adding an instrument is a change in one language rather than two.
 */
/**
 * Whether the bundled sample content is installed, and what the downloader is
 * doing about it.
 *
 * The content (a CC-BY grand piano) is ~131 MB and is deliberately *not* part of
 * a checkout: it is fetched on demand, pinned by commit and verified by SHA-256.
 * Nothing downloads without a click -- see `startAssetDownload`.
 */
export interface AssetStatusDto {
  /** Whether every pinned file is present on disk. */
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
  /** The instrument this installs, e.g. `salamander-piano`. */
  instrumentId: string;
  instrumentName: string;
  /** Empty unless `state` is `error`. */
  error: string;
}

export interface SourceDto {
  /**
   * Stable id: `poly-synth`, `drum-kit`, `test-tone`, `sampler:<instrumentId>`,
   * or `clap:<pluginId>`.
   *
   * This is what `setTrackSource` takes for a built-in, and what a `TrackDto`
   * reports as its `sourceId` either way. A CLAP source is *placed* with
   * `setTrackSourceClap` instead, because the host has to load a bundle from
   * disk rather than look a name up in a table.
   */
  id: string;
  label: string;
  description: string;
  /** Which of the three ways this source is placed. */
  kind: 'internal' | 'clap' | 'vst3';
  /** Plugin vendor; empty for the built-ins, and possibly empty for a CLAP one. */
  vendor: string;
  /**
   * Absolute plugin path: a `.clap` bundle or a `.vst3` module. Set exactly when
   * `kind` is `clap` or `vst3`.
   */
  bundlePath: string | null;
  /**
   * The plugin's own identifier: a CLAP id, or a VST3 class id as hex. Set
   * exactly when `kind` is `clap` or `vst3`.
   */
  pluginId: string | null;
  /** Whether this is the selected track's source. Exactly one is true. */
  selected: boolean;
  /** Whether the computer-keyboard piano does anything for this source. */
  takesNotes: boolean;
}

/**
 * One cell of the step grid. Mirrors `StepDto` in `app/src/dto.rs`.
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
 * One row of the step grid. Mirrors `PatternRowDto` in `app/src/dto.rs`.
 *
 * `label` is derived by Rust from `key` (the General MIDI drum map) and ignored
 * on the way back in: the row's identity is its key, and the name is a rendering
 * of it. The UI must not keep its own copy of that map.
 */
export interface PatternRowDto {
  /** MIDI key this row triggers: 36 kick, 38 snare, 42 CH, 46 OH. */
  key: number;
  /** Note channel, `0..15`. */
  channel: number;
  label: string;
  /** Exactly `pattern.steps` entries. */
  steps: StepDto[];
}

/**
 * One note of the piano roll. Mirrors `MelodicNoteDto` in `app/src/dto.rs`.
 *
 * `start` and `length` are in **ticks**: `PPQ` (960) to the quarter note, so a
 * sixteenth is `TICKS_PER_STEP` (240). Ticks rather than steps because a note's
 * whole point is that it need not sit on the grid -- a triplet, a nudge, a note
 * held for three and a half beats. The units are not in the field names because
 * this shape is written into the `.mydaw` file too, and `startTicks` beside
 * `master_gain_db` would be a wart on the format for ever.
 */
export interface MelodicNoteDto {
  /** MIDI key, `0..127`. 60 is middle C. */
  key: number;
  /** Note channel, i.e. the index of the track this note plays. */
  channel: number;
  /** Offset from the start of the pattern, in ticks. */
  start: number;
  /** How long the note sounds, in ticks. At least 1. */
  length: number;
  /** Normalised velocity, `0..1`. */
  velocity: number;
}

/** The whole pattern. Mirrors `PatternDto` in `app/src/dto.rs`. */
export interface PatternDto {
  /** Steps in the loop: 16, 32 or 64. Every row has exactly this many entries. */
  steps: number;
  rows: PatternRowDto[];
  /**
   * The piano roll, in no particular order. Rust sorts on compile, so the index
   * of a note here is stable under an edit -- which is what lets the roll
   * address the note being dragged by its position in this array.
   */
  notes: MelodicNoteDto[];
}

/**
 * Tempo and pattern in one message. Mirrors `SequencerStateDto` in
 * `app/src/dto.rs`.
 *
 * They travel together because the panel needs both to draw itself: the grid
 * comes from the pattern, and the moving current-step highlight is computed from
 * the tempo.
 */
export interface SequencerState {
  /** Session tempo, already inside the transport's legal range. */
  bpm: number;
  /**
   * The open device's sample rate, or `0` when audio is not running.
   *
   * The panel needs it to turn a transport position in frames into a step
   * number; without it the UI would have to assume 48 kHz and the moving
   * highlight would sit in the wrong step on a 44.1 kHz interface.
   */
  sampleRate: number;
  pattern: PatternDto;
}

/**
 * One plugin a project asked for and did not get. Mirrors `MissingPluginDto` in
 * `app/src/project.rs`.
 *
 * A report rather than a loss: the reference stays in the project and is written
 * back out on the next save, so installing the plugin and reopening puts the
 * strip back together. That is why a load reports this instead of quietly
 * substituting a built-in, which would be an unasked-for edit to somebody's
 * song.
 */
export interface MissingPlugin {
  /**
   * What the project called it -- the display name from the scan that placed
   * it. A user recognises `Diva`; nobody recognises a path.
   */
  name: string;
  /** `clap` or `vst3`, so a warning can say which format is absent. */
  format: string;
  /** Where the project expected to find it. Often the whole explanation. */
  path: string;
  /** Why it would not load, in the loader's own words. */
  reason: string;
}

/**
 * One mixer strip. Mirrors `TrackDto` in `app/src/project.rs`.
 *
 * `index` is the track's position *and* its note channel: a sequencer row on
 * channel 1 plays track 1, and the keyboard piano sends on the selected track's
 * index. `id` is what a UI list should be keyed on -- removing a track
 * renumbers the ones after it, so `index` is not stable.
 */
export interface TrackDto {
  index: number;
  id: number;
  name: string;
  /** Source id, one of the ids from `getSources`. */
  sourceId: string;
  sourceLabel: string;
  /** Fader position, in dB. `minGainDb` means silence. */
  gainDb: number;
  /** `-1` hard left, `0` centre, `1` hard right. */
  pan: number;
  /** The mute button's own state. */
  muted: boolean;
  soloed: boolean;
  /**
   * Whether this track is actually silent right now, solo resolved. A track
   * another track's solo has silenced has `silenced` true and `muted` false.
   */
  silenced: boolean;
  /** Whether the keyboard piano and the source selector address this track. */
  selected: boolean;
  /**
   * The hosted CLAP instrument at the head of this track, when `sourceId` names
   * one. `null` for a built-in source.
   */
  clap: ClapSlot | null;
  /**
   * The hosted VST3 instrument at the head of this track.
   *
   * At most one of `clap` and `vst3` is ever set: a track has one source.
   */
  vst3: Vst3Slot | null;
  /**
   * Set when the project asked for a plugin on this track and it could not be
   * loaded. The track keeps everything else it had and renders silence.
   */
  missing: MissingPlugin | null;
}

/** The master strip. Mirrors `MasterDto` in `app/src/project.rs`. */
export interface MasterDto {
  gainDb: number;
  /** The CLAP audio effect on the master insert, or `null` when there is none. */
  clap: ClapSlot | null;
  /**
   * The VST3 audio effect on the master insert.
   *
   * The insert holds **one** plugin, so at most one of `clap` and `vst3` is set.
   * Placing either format evicts the other.
   */
  vst3: Vst3Slot | null;
  /** The master effect the project asked for and did not get. */
  missing: MissingPlugin | null;
}

/**
 * Everything the mixer panel needs in one message. Mirrors `MixerStateDto` in
 * `app/src/project.rs`.
 *
 * Every mixer command returns this whole shape rather than an acknowledgement,
 * so the panel never has to merge a partial update into what it already had.
 */
export interface MixerState {
  tracks: TrackDto[];
  master: MasterDto;
  /** Index of the track the keyboard and the source selector address. */
  selected: number;
  /** The engine's fixed track limit. */
  maxTracks: number;
  /** Fader bounds in dB, so the UI keeps no copy of the scale. */
  minGainDb: number;
  maxGainDb: number;
}

/**
 * Which project is open. Mirrors `ProjectInfoDto` in
 * `app/src/project_bridge.rs`.
 *
 * Every project command answers with this, not just `getProjectInfo`, so the
 * window title and the status bar are written from the reply to the command
 * that changed them rather than from a second question asked afterwards -- a
 * question whose answer could arrive out of order.
 */
export interface ProjectInfo {
  /**
   * Absolute path of the open project, `null` for one that has never been
   * saved.
   *
   * Read, never acted on: whether a plain Save needs a dialog is decided in
   * Rust, where the dialog is. This is here so the status bar can say which
   * file two projects of the same name are.
   */
  path: string | null;
  /** The file name without its extension, or `Untitled`. */
  name: string;
  /**
   * How many of the project's plugins could not be loaded. Zero in every
   * ordinary session; the status bar warns when it is not, and the strips say
   * which ones.
   */
  missingPlugins: number;
  /**
   * Whatever was worth saying that was not an outright failure -- a plugin that
   * would not load, a state blob a plugin refused, a load with no audio device
   * to attach anything to.
   *
   * Sentences rather than codes, because Rust writes them for the status bar
   * and the UI is not meant to interpret them.
   */
  warnings: string[];
}

/**
 * What the open audio device is. Mirrors `StreamInfoDto` in `app/src/dto.rs`.
 *
 * `running: false` with every number at zero is the ordinary "audio has not
 * started" state, not a failure -- the status bar draws a dash for it.
 */
export interface StreamInfo {
  deviceName: string;
  sampleRate: number;
  channels: number;
  /** Largest block the callback can be handed, in frames: the BUFFER readout. */
  bufferFrames: number;
  /** One block's worth of time in milliseconds: the LATENCY readout. */
  latencyMs: number;
  running: boolean;
}

/**
 * Progress of a downloadable instrument asset (the sampler's piano, today).
 *
 * **Provisional.** The commands behind it (`get_asset_status`,
 * `start_asset_download`) are being added on another branch; this shape is the
 * UI's half of that contract, written down here so the browser's download row is
 * a real component against a typed surface rather than a picture. `Backend`
 * declares the two methods as optional, and the shell hides the row when they
 * are absent -- see `browser/AssetRow.tsx`.
 */
export interface AssetStatus {
  /** Stable id, e.g. `piano`. */
  id: string;
  name: string;
  /** `absent` before a download, `ready` once it is installed and usable. */
  state: 'absent' | 'downloading' | 'ready' | 'failed';
  /** `0..1` while downloading; `1` when ready. */
  progress: number;
  /** Total download size in bytes, or `0` when it is not known yet. */
  bytesTotal: number;
  bytesDone: number;
  /** Why a `failed` download failed, in the backend's own words. */
  error: string | null;
}

/** Unsubscribe handle returned by streaming APIs. */
export type Unsubscribe = () => void;
