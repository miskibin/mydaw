/**
 * How an effect is laid out -- compact in the inspector, complete in the editor.
 *
 * Both tables here are **the UI's decisions**, in the same spirit as
 * `param.ts`'s switch names: the engine publishes a flat list of `ParamDef`s in
 * declaration order and deliberately says nothing about which of them matter
 * most or which belong together, because that is a design opinion and would end
 * up in a file format. So the opinion is ours, it is written down in one place,
 * and it degrades honestly for an effect it has never heard of.
 */

import type { EffectInfoDto, ParamDefDto } from '../api/types';

/** Knobs a compact card has room for before it stops being compact. */
export const COMPACT_KNOBS = 4;

/** What fills a compact card's body. */
export type CompactBody =
  | { kind: 'curve' }
  | { kind: 'knobs'; params: ParamDefDto[]; badge: string | null };

/**
 * The four (or fewer) controls each effect is worth showing in a 306 px panel,
 * and the one effect that is better drawn than knobbed.
 *
 * The picks are the controls `2026-08-19-effects-ux-research.md` found people
 * reach for first, band by band:
 *
 * - **EQ** -- no knobs at all. The research's interaction spec opens with the
 *   canvas ("log-frequency on X, linear-dB on Y"), and it is right: four of the
 *   EQ's twenty parameters is an arbitrary quarter of an EQ, whereas the curve
 *   is *all* of it at a glance. `curveParam` is what the card watches.
 * - **Compressor** -- threshold, ratio, attack, release: the four that define the
 *   static curve and the ballistics. Knee, detector, sidechain, makeup and mix
 *   are all refinements of a decision these four have already made. The GR bar
 *   rides underneath because the research asks for it "large and always
 *   visible".
 * - **Delay** -- division, feedback, mix, and a badge saying what the division
 *   resolved to. The research's own summary of the category is that time,
 *   feedback and mix are the three controls, and that the sync state must never
 *   be ambiguous.
 * - **Saturator** -- drive first ("one oversized Drive knob... usable by touching
 *   exactly one control"), then type, because tape/tube/clip changes what drive
 *   *means*, then mix, which is how saturation is actually used on a bus.
 * - **Utility** -- gain, pan, width, mono. The polarity switches and the DC
 *   blocker are repair tools: reached for once, deliberately, with the full
 *   editor open.
 *
 * An effect that is not in this table gets its first four non-`bypass`
 * parameters, which is a sensible-looking card for something that lands after
 * this file was last read.
 */
export const COMPACT_CARDS: Readonly<
  Record<string, { curve?: true; params?: readonly string[] }>
> = {
  'effects.eq': { curve: true },
  'effects.compressor': { params: ['threshold', 'ratio', 'attack', 'release'] },
  'effects.delay': { params: ['division', 'feedback', 'mix'] },
  'effects.saturator': { params: ['drive', 'type', 'mix'] },
  'effects.utility': { params: ['gain', 'pan', 'width', 'mono'] },
};

/** Every parameter except `bypass`, which is the dot in the card's head. */
export function editableParams(info: EffectInfoDto | undefined): ParamDefDto[] {
  return (info?.params ?? []).filter((def) => def.id !== 'bypass');
}

/**
 * What one effect's compact card draws.
 *
 * A curated id the catalogue does not carry is skipped rather than faked, so a
 * parameter renamed in the engine costs a knob and not a crash.
 */
export function compactBody(info: EffectInfoDto | undefined): CompactBody {
  if (!info) return { kind: 'knobs', params: [], badge: null };
  const entry = COMPACT_CARDS[info.typeId];
  if (entry?.curve) return { kind: 'curve' };
  const wanted = entry?.params;
  const params = wanted
    ? wanted
        .map((id) => info.params.find((def) => def.id === id))
        .filter((def): def is ParamDefDto => def !== undefined)
    : editableParams(info).slice(0, COMPACT_KNOBS);
  return { kind: 'knobs', params, badge: null };
}

/**
 * How tall a magnitude curve's dB axis should be for this effect.
 *
 * Taken from a band-gain parameter's own range rather than hard-coded, so the
 * box is exactly as tall as the knobs can reach and a change to the engine's
 * `MAX_BAND_GAIN_DB` moves the display with it.
 */
export function bandGainRangeDb(info: EffectInfoDto | undefined): number {
  const gain = info?.params.find((def) => def.id.endsWith('.gain'));
  return gain ? Math.max(Math.abs(gain.min), Math.abs(gain.max)) : 18;
}

/**
 * What a band's id prefix is called.
 *
 * The engine's prefixes are terse because they are persisted; a heading is not,
 * so it can say "High shelf" where the id says `high`. Anything absent falls
 * back to the prefix itself, so a new band is headed rather than unheaded.
 */
const GROUP_LABELS: Readonly<Record<string, string>> = {
  hp: 'Highpass',
  low: 'Low shelf',
  bell1: 'Bell 1',
  bell2: 'Bell 2',
  high: 'High shelf',
  lp: 'Lowpass',
};

/** One heading and the knobs under it. */
export interface ParamGroup {
  key: string;
  /** Empty for the bucket that gets no heading, i.e. an effect with no groups. */
  label: string;
  params: ParamDefDto[];
}

/**
 * The full editor's grid, grouped by the prefix the engine puts on an id.
 *
 * `low.freq`, `low.gain` and `low.q` are one band and belong in one row; the
 * compressor's ids carry no prefix at all and are one grid. Groups come out in
 * the order the engine declared them, which for the EQ is signal order -- so the
 * editor reads top to bottom the way the audio flows.
 */
export function paramGroups(info: EffectInfoDto | undefined): ParamGroup[] {
  const groups: ParamGroup[] = [];
  for (const def of editableParams(info)) {
    const key = def.id.includes('.') ? (def.id.split('.')[0] ?? '') : '';
    const existing = groups.find((group) => group.key === key);
    if (existing) existing.params.push(def);
    else groups.push({ key, label: GROUP_LABELS[key] ?? key, params: [def] });
  }
  // One bucket and no prefixes anywhere: a heading saying nothing is worse than
  // no heading, so the effect gets a single unlabelled grid.
  if (groups.length === 1 && groups[0]!.key === '') groups[0]!.label = '';
  else for (const group of groups) if (group.key === '') group.label = 'General';
  return groups;
}
