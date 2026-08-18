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
};

/** Resolves a manifest key, or `undefined` when this build does not know it. */
export function resolvePanel(component: string): PanelComponent | undefined {
  return PANEL_REGISTRY[component];
}

/** Keys this build can render. Useful for diagnostics and future tests. */
export function registeredPanelKeys(): string[] {
  return Object.keys(PANEL_REGISTRY);
}
