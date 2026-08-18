import { lazy } from 'solid-js';
import { ClapBrowserPanel } from './clap-browser/ClapBrowserPanel';
import { KernelStatusPanel } from './kernel-status/KernelStatusPanel';
import { MetersPanel } from './meters/MetersPanel';
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
  meters: MetersPanel,
  'clap-browser': ClapBrowserPanel,
  // Dev instrument (spike 4), contributed only under MYDAW_DEV_PANELS /
  // MYDAW_IPC_BENCH. Lazy so a normal session never downloads it: Vite splits it
  // into its own chunk, and a manifest that is never sent is a chunk that is
  // never fetched.
  'ipc-bench': lazy(() => import('../dev/IpcBenchPanel')),
};

/** Resolves a manifest key, or `undefined` when this build does not know it. */
export function resolvePanel(component: string): PanelComponent | undefined {
  return PANEL_REGISTRY[component];
}

/** Keys this build can render. Useful for diagnostics and future tests. */
export function registeredPanelKeys(): string[] {
  return Object.keys(PANEL_REGISTRY);
}
