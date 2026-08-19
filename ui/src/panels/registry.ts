import { lazy } from 'solid-js';
import { ClapBrowserPanel } from './clap-browser/ClapBrowserPanel';
import { KernelStatusPanel } from './kernel-status/KernelStatusPanel';
import { MetersPanel } from './meters/MetersPanel';
import { MixerPanel } from './mixer/MixerPanel';
import { PianoRollPanel } from './piano-roll/PianoRollPanel';
import { SequencerPanel } from './sequencer/SequencerPanel';
import { Vst3BrowserPanel } from './vst3-browser/Vst3BrowserPanel';
import type { PanelComponent } from './types';

/**
 * Mirror of the panel contributions declared on the Rust side.
 *
 * A plugin's `PanelManifestDto.component` is a plain string key looked up here.
 * Keys are part of the plugin<->UI contract: renaming one is a breaking change,
 * and a key with no entry renders as an "unknown panel" card rather than
 * failing the whole shell.
 */
export const PANEL_REGISTRY: Record<string, PanelComponent> = {
  'kernel-status': KernelStatusPanel,
  // Contributed by the shell, not by a plugin: a strip is a view of the
  // project, and the project lives in `app`. See `MIXER_PANEL` in
  // `app/src/state.rs`.
  mixer: MixerPanel,
  meters: MetersPanel,
  'step-sequencer': SequencerPanel,
  // Contributed by the sequencer plugin, but rendered in the workspace rather
  // than the dock -- see `WORKSPACE_PANELS`.
  'piano-roll': PianoRollPanel,
  'clap-browser': ClapBrowserPanel,
  'vst3-browser': Vst3BrowserPanel,
  // Dev instrument (spike 4), contributed only under MYDAW_DEV_PANELS /
  // MYDAW_IPC_BENCH. Lazy so a normal session never downloads it: Vite splits it
  // into its own chunk, and a manifest that is never sent is a chunk that is
  // never fetched.
  'ipc-bench': lazy(() => import('../dev/IpcBenchPanel')),
};

/**
 * Panels the shell renders in the **workspace** rather than in the left dock.
 *
 * A plugin contributes a panel without saying where it goes, which is right --
 * it knows what it draws, not what the shell's layout is. The shell decides,
 * and the rule is size: the dock is a 380 px column of controls, and a piano
 * roll drawn 380 px wide would be a piano roll nobody could work in. So this
 * one key is routed to the tabbed main area, where it gets the window.
 *
 * The dock skips these, so a workspace panel is not also drawn as a card.
 */
export const WORKSPACE_PANELS: ReadonlySet<string> = new Set(['piano-roll']);

/** Whether `component` belongs in the workspace instead of the dock. */
export function isWorkspacePanel(component: string): boolean {
  return WORKSPACE_PANELS.has(component);
}

/** Resolves a manifest key, or `undefined` when this build does not know it. */
export function resolvePanel(component: string): PanelComponent | undefined {
  return PANEL_REGISTRY[component];
}

/** Keys this build can render. Useful for diagnostics and future tests. */
export function registeredPanelKeys(): string[] {
  return Object.keys(PANEL_REGISTRY);
}
