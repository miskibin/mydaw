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
export interface SourceDto {
  /**
   * Stable id: `poly-synth`, `drum-kit`, `test-tone`, or `clap:<pluginId>`.
   *
   * This is what `setTrackSource` takes for a built-in, and what a `TrackDto`
   * reports as its `sourceId` either way. A CLAP source is *placed* with
   * `setTrackSourceClap` instead, because the host has to load a bundle from
   * disk rather than look a name up in a table.
   */
  id: string;
  label: string;
  description: string;
  /** Which of the two ways this source is placed. */
  kind: 'internal' | 'clap';
  /** Plugin vendor; empty for the built-ins, and possibly empty for a CLAP one. */
  vendor: string;
  /** Absolute `.clap` bundle path. Set exactly when `kind` is `clap`. */
  bundlePath: string | null;
  /** CLAP id. Set exactly when `kind` is `clap`. */
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

/** The whole grid. Mirrors `PatternDto` in `app/src/dto.rs`. */
export interface PatternDto {
  /** Steps per row. 16 today; a different number means this UI is stale. */
  steps: number;
  rows: PatternRowDto[];
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
}

/** The master strip. Mirrors `MasterDto` in `app/src/project.rs`. */
export interface MasterDto {
  gainDb: number;
  /** The CLAP audio effect on the master insert, or `null` when there is none. */
  clap: ClapSlot | null;
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

/** Unsubscribe handle returned by streaming APIs. */
export type Unsubscribe = () => void;
