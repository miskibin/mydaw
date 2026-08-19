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
 * A single metering packet from the RT graph, throttled to 30-60 Hz by the
 * bridge. Peak/RMS are linear amplitudes in [0, 1+] (may exceed 1 when
 * clipping). `samplePos` is the transport position in frames.
 */
export interface MeterFrame {
  peakL: number;
  peakR: number;
  rmsL: number;
  rmsR: number;
  samplePos: number;
  playing: boolean;
  /**
   * When the pump handed this frame to the webview, on the Rust bench clock.
   *
   * Absent in every normal run: only the spike-4 IPC benchmark asks the pump to
   * stamp frames, and it is the only thing that reads this.
   */
  pumpMs?: number;
}

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
}

/**
 * State of the hosted plugin's editor window. Mirrors `ClapGuiStatusDto` in
 * `app/src/gui_bridge.rs`.
 *
 * The editor is a *native* window next to MYDAW's, not a region of this webview:
 * a plugin draws with its own toolkit into an OS window handle, which cannot be
 * composited into WebView2. All the UI can do is ask for it to be opened or
 * closed, and show what came back.
 */
export interface ClapGuiStatus {
  /** Whether an editor window is currently open. */
  open: boolean;
  /** CLAP id of the plugin the editor belongs to, if one is loaded. */
  pluginId: string | null;
  /** Editor size in physical pixels; `0` when closed. */
  width: number;
  height: number;
}

/**
 * One thing the engine can put at the head of its chain. Mirrors `SourceDto` in
 * `app/src/source.rs`.
 *
 * The list is fixed (two built-in instruments, the test tone, and a hosted CLAP
 * instrument) and comes from Rust rather than being hard-coded here, so adding
 * an instrument is a change in one language rather than two.
 */
export interface SourceDto {
  /** Stable id, e.g. `poly-synth`. This is what `setSource` takes. */
  id: string;
  label: string;
  description: string;
  /**
   * Whether it can be selected right now. Only `clap` is ever false, and only
   * because no bundle was named by `MYDAW_CLAP_INSTRUMENT` at startup.
   */
  available: boolean;
  /** Whether this is the source currently playing. Exactly one is true. */
  selected: boolean;
  /** Whether the computer-keyboard piano does anything for this source. */
  takesNotes: boolean;
}

/** Unsubscribe handle returned by streaming APIs. */
export type Unsubscribe = () => void;
