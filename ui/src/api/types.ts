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
}

/** Unsubscribe handle returned by streaming APIs. */
export type Unsubscribe = () => void;
