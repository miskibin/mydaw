// The standard library's file list (the samples that ship with the program), loaded once so
// that menus can offer them without opening the browser.
import { createSignal } from 'solid-js';
import { be } from '../api/backend';
import type { LibFile } from '../api/types';
import { t, type TranslationKey } from '../lib/i18n';

/** Kit id of the standard library (crates/engine/src/factory.rs `KIT`). */
export const STD_KIT = 'Standard';
const [files, setFiles] = createSignal<LibFile[]>([]);
let loading: Promise<void> | null = null;

/** The standard library, grouped as the browser shows it (empty until loaded). */
export const stdLib = files;
export function stdLibGroups(): [string, LibFile[]][] {
  const m = new Map<string, LibFile[]>();
  for (const f of files()) { const g = m.get(f.group) ?? []; g.push(f); m.set(f.group, g); }
  return [...m.entries()];
}
/** Loads the list if it is not there yet; a failure is retried on the next call. */
export function loadStdLib(): Promise<void> {
  if (files().length) return Promise.resolve();
  return loading ??= be().libraryFiles({ kit: STD_KIT }).then(f => { setFiles(f); }, () => {}).finally(() => { loading = null; });
}

const GROUPS: Record<string, TranslationKey> = {
  Kicks: 'b.lib.kicks', Snares: 'b.lib.snares', Claps: 'b.lib.claps', Hats: 'b.lib.hats',
  Cymbals: 'b.lib.cymbals', Toms: 'b.lib.toms', Percussion: 'b.lib.percussion', Tonal: 'b.lib.tonal', Orchestra: 'b.lib.orchestra', FX: 'b.lib.fx',
};
/** A standard-library group name in the UI language (other kits' and folders' group names pass through). */
export const libGroup = (g: string) => (Object.prototype.hasOwnProperty.call(GROUPS, g) ? t(GROUPS[g]) : g);
