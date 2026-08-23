/**
 * One list of everything a channel can play.
 *
 * The browser used to split this three ways -- internal sources in "Sounds",
 * scanned plugins in "Plugins" -- which sorted instruments by *who wrote them*
 * rather than by what they are. Poly Synth is a plugin; so is the sampler. What
 * a musician wants to know is "is this a synth or a kit", and the format is a
 * badge on the row, not a tab.
 *
 * Flat and searched rather than foldered: CLAP and VST3 expose no reliable
 * category, so musical-role folders would put every installed plugin in "Other"
 * -- the old split wearing a different hat.
 */

import type { ClapPluginInfoDto, SourceDto, Vst3PluginInfoDto } from '../api/types';

/** What makes the sound, shown as a badge on the row. */
export type InstrumentBadge = 'SYNTH' | 'SAMPLER' | 'TONE' | 'CLAP' | 'VST3';

/** One row of the instrument list. */
export interface InstrumentRow {
  /** Stable across renders; unique within the list. */
  key: string;
  label: string;
  badge: InstrumentBadge;
  /** Tooltip body: the source's description, or a plugin's vendor and path. */
  detail: string;
  /** Ships with MYDAW. Sorts ahead of anything installed on the machine. */
  builtIn: boolean;
  /** Payload for a drag onto the rack. */
  dragData: string;
  /** The selected channel is playing this. */
  selected: boolean;
  /** Placement: a built-in is an id the engine looks up, a plugin is a bundle. */
  place:
    | { via: 'source'; id: string }
    | { via: 'clap'; path: string; pluginId: string }
    | { via: 'vst3'; path: string; pluginId: string };
}

/** The badge for an engine source, from the id the engine gave it. */
export function badgeOf(source: SourceDto): InstrumentBadge {
  if (source.kind === 'clap') return 'CLAP';
  if (source.kind === 'vst3') return 'VST3';
  if (source.id.startsWith('sampler:')) return 'SAMPLER';
  if (source.id === 'test-tone') return 'TONE';
  return 'SYNTH';
}

/**
 * Every instrument, built-ins first and each group alphabetical.
 *
 * Hosted sources are taken from the *scan* rather than from `get_sources`, which
 * also offers them: the scan carries the bundle path and plugin id that
 * placement needs, and a plugin listed twice under two names is a browser you
 * cannot trust.
 *
 * Effects are not here. They are added from the mixer's insert picker, where you
 * are already looking at the strip you want one on.
 */
export function instrumentRows(
  sources: readonly SourceDto[],
  clap: readonly ClapPluginInfoDto[],
  vst3: readonly Vst3PluginInfoDto[],
): InstrumentRow[] {
  const builtIns: InstrumentRow[] = sources
    .filter((source) => source.kind === 'internal')
    .map((source) => ({
      key: `source:${source.id}`,
      label: source.label,
      badge: badgeOf(source),
      detail: source.description,
      builtIn: true,
      dragData: `inst::${source.id}`,
      selected: source.selected,
      place: { via: 'source', id: source.id },
    }));

  const hosted: InstrumentRow[] = [
    ...clap
      .filter((plugin) => plugin.kind === 'instrument')
      .map((plugin) => ({
        key: `clap:${plugin.path}:${plugin.id}`,
        label: plugin.name.length > 0 ? plugin.name : plugin.id,
        badge: 'CLAP' as const,
        detail: detailOf(plugin.vendor, plugin.path),
        builtIn: false,
        dragData: `inst::clap:${plugin.id}`,
        selected: false,
        place: { via: 'clap' as const, path: plugin.path, pluginId: plugin.id },
      })),
    ...vst3
      .filter((plugin) => plugin.kind === 'instrument')
      .map((plugin) => ({
        key: `vst3:${plugin.path}:${plugin.id}`,
        label: plugin.name.length > 0 ? plugin.name : plugin.id,
        badge: 'VST3' as const,
        detail: detailOf(plugin.vendor, plugin.path),
        builtIn: false,
        dragData: `inst::vst3:${plugin.id}`,
        selected: false,
        place: { via: 'vst3' as const, path: plugin.path, pluginId: plugin.id },
      })),
  ];

  return [...builtIns.sort(byLabel), ...hosted.sort(byLabel)];
}

/** Vendor and path, or just the path when the bundle declares no vendor. */
function detailOf(vendor: string, path: string): string {
  return vendor.length > 0 ? `${vendor}\n${path}` : path;
}

/** Case-insensitive by label, so the list does not reorder with locale. */
function byLabel(a: InstrumentRow, b: InstrumentRow): number {
  return a.label.toLowerCase().localeCompare(b.label.toLowerCase());
}

/**
 * The rows a search box shows.
 *
 * Matches the label, the badge and the detail -- so "clap" finds the format,
 * "u-he" finds a vendor, and "808" finds the kit. An empty query is not a
 * filter: it is the whole list.
 */
export function filterInstruments(
  rows: readonly InstrumentRow[],
  query: string,
): InstrumentRow[] {
  const needle = query.trim().toLowerCase();
  if (needle.length === 0) return [...rows];
  return rows.filter((row) =>
    `${row.label}\n${row.badge}\n${row.detail}`.toLowerCase().includes(needle),
  );
}
