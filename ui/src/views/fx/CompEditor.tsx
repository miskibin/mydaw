import { For, Show, createEffect, createMemo, createSignal, createUniqueId, type JSX } from 'solid-js';

import type { EffectInfoDto, ParamValueDto, StripRef } from '../../api/types';
import { compressorOutputDb, inputDbFromGr, reductionDb } from '../../lib/compressor';
import { normalisedScope, signalPath } from '../../lib/polySynth';
import { TransferCurve } from '../../ui/TransferCurve';
import type { InsertKnobs } from '../../ui/insertKnob';
import { Advanced, PrimaryRow } from './chrome';

/** One strip the key can come from, as the selector lists it. */
export interface KeyStrip extends StripRef {
  name: string;
}

/**
 * Samples the envelope display keeps. At the 66 ms readout poll that is a
 * little over eight seconds -- long enough that a duck lands several times
 * inside the picture at any usable tempo, short enough that the newest hit is
 * still the one your eye goes to.
 */
const HISTORY = 128;

/** Top of the plot, in the SVG's own units. 0 dBFS, and no reduction. */
const TOP = 8;
/** Bottom of the plot. -60 dBFS, and the full reduction the axis shows. */
const BOTTOM = 78;
/** Level axis: 0 dB at the top down to this many dB at the bottom. */
const LEVEL_RANGE_DB = 60;
/**
 * Reduction axis, hanging from the top. 24 dB because ducking a bass under a
 * kick routinely reaches the high teens, and an axis a real duck runs off the
 * end of would show a flat line at the worst moment.
 */
const GR_RANGE_DB = 24;

/** Where a level in dB sits vertically. Louder is higher, always. */
export function yForLevelDb(db: number): number {
  const clamped = Math.min(0, Math.max(-LEVEL_RANGE_DB, db));
  return TOP + (-clamped / LEVEL_RANGE_DB) * (BOTTOM - TOP);
}

/** Where a *reduction* in dB reaches, hanging down from 0 dB at the top. */
export function yForReductionDb(db: number): number {
  const clamped = Math.min(GR_RANGE_DB, Math.max(0, db));
  return TOP + (clamped / GR_RANGE_DB) * (BOTTOM - TOP);
}

/** Horizontal position of sample `at` in a history of `HISTORY` slots. */
function xAt(at: number, length: number): number {
  return 100 - (length - 1 - at) * (100 / (HISTORY - 1));
}

/** One scrolling trace as an SVG path, newest sample at the right edge. */
function tracePath(samples: readonly number[], y: (value: number) => number): string {
  if (samples.length < 2) return '';
  return samples
    .map((value, at) => `${at === 0 ? 'M' : 'L'}${xAt(at, samples.length).toFixed(2)} ${y(value).toFixed(2)}`)
    .join(' ');
}

/** The same trace closed against the top edge, so it can be filled. */
function areaPath(samples: readonly number[], y: (value: number) => number): string {
  const line = tracePath(samples, y);
  if (!line) return '';
  const first = xAt(0, samples.length).toFixed(2);
  const last = xAt(samples.length - 1, samples.length).toFixed(2);
  return `${line} L${last} ${TOP} L${first} ${TOP} Z`;
}

/** `"track:2"` / `"bus:0"` / `""`, which is what a `<select>` can hold. */
export function keyValue(source: StripRef | null): string {
  return source ? `${source.kind}:${source.index}` : '';
}

/** The inverse, tolerant of a value no strip answers to. */
export function keyFromValue(value: string, strips: readonly KeyStrip[]): StripRef | null {
  return strips.find((strip) => keyValue(strip) === value) ?? null;
}

export function CompEditor(props: {
  info: EffectInfoDto;
  knobs: InsertKnobs;
  values: Map<string, number>;
  live: ParamValueDto[];
  primary: EffectInfoDto['params'];
  advanced: EffectInfoDto['params'];
  muted: boolean;
  spectrum: readonly number[] | null;
  /** Every strip the key can come from, tracks first, in mixer order. */
  strips: KeyStrip[];
  /** Where the key comes from now, or `null`. */
  sidechain: StripRef | null;
  onSidechain: (source: StripRef | null) => void;
}): JSX.Element {
  const at = (id: string, fallback: number) => props.values.get(id) ?? fallback;
  const meter = (id: string, fallback: number) =>
    props.live.find((entry) => entry.id === id)?.value ?? fallback;
  const gr = () => Math.abs(meter('gr_max', 0));
  const threshold = () => at('threshold', -18);
  const ratio = () => at('ratio', 4);
  const knee = () => at('knee', 6);
  const makeup = () => at('makeup', 0) + (at('auto_makeup', 0) > 0.5 ? reductionDb(0, threshold(), ratio(), knee()) : 0);

  /**
   * The strip the engine says is wired in, resolved against the live mixer.
   *
   * A route to a strip that no longer exists reads as no route at all -- the
   * engine unroutes it on the next edit or load, and the selector must not sit
   * on a name it invented in the meantime.
   */
  const source = createMemo<KeyStrip | null>(() => {
    const wanted = props.sidechain;
    if (!wanted) return null;
    return (
      props.strips.find((strip) => strip.kind === wanted.kind && strip.index === wanted.index) ??
      null
    );
  });
  /** Whether a key that this mixer can actually name is wired in. */
  const keyed = () => source() !== null;

  /**
   * The level the detector is seeing, recovered from the reduction it produced.
   *
   * The engine publishes gain reduction, not detector level, and deliberately
   * so -- see `READOUTS` in `compressor.rs`. Inverting the static curve is exact
   * wherever the compressor is doing anything, which is exactly the part of a
   * duck worth drawing; below the knee it rests at the foot of the curve, which
   * reads as "the key is under the threshold and nothing is happening".
   *
   * With a key routed this *is* the key's level, because the key is what drove
   * the reduction. That is what makes the trace below honest rather than a
   * shape derived from the attack and release knobs.
   */
  const detectorDb = () => inputDbFromGr(gr(), threshold(), ratio(), knee());

  const [history, setHistory] = createSignal<{ level: number; gr: number }[]>([]);
  createEffect(() => {
    // Depends on `props.live`, so one sample lands per readout poll.
    const sample = { level: props.muted ? -120 : detectorDb(), gr: props.muted ? 0 : gr() };
    setHistory((previous) => {
      const kept = previous.length >= HISTORY ? previous.slice(previous.length - HISTORY + 1) : previous.slice();
      kept.push(sample);
      return kept;
    });
  });
  // A routing change makes everything before it a different measurement, so the
  // trace restarts rather than splicing two signals into one picture.
  createEffect((previous: string | undefined) => {
    const now = keyValue(props.sidechain);
    if (previous !== undefined && previous !== now) setHistory([]);
    return now;
  });

  const levels = () => history().map((sample) => sample.level);
  const reductions = () => history().map((sample) => sample.gr);

  const live = () => ({
    x: detectorDb(),
    y: compressorOutputDb(detectorDb(), threshold(), ratio(), knee(), makeup()),
  });

  const sourceLabel = () => source()?.name ?? '';
  const vizLabel = () =>
    keyed()
      ? `Compression over time: the key envelope from ${sourceLabel()}, and the gain reduction it causes`
      : 'Compression over time: input envelope and gain reduction';
  /**
   * Said out loud rather than implied: the level trace is recovered from the
   * reduction, so it is flat while the detector is under the threshold. A
   * display that let a reader believe otherwise would be the same lie as a
   * shape drawn from the knobs.
   */
  const vizTitle = () =>
    `${
      keyed()
        ? `The key from ${sourceLabel()} as the detector hears it, and the reduction it causes on this insert.`
        : "This insert's own level as the detector hears it, and the reduction it causes."
    } Recovered from the gain-reduction meter, so the level trace rests at the foot of the knee whenever nothing is over the threshold.`;

  // Unique per editor, not per effect type: two compressors can be open at
  // once, and a shared id would point both labels at the first select.
  const selectId = createUniqueId();

  const selectorTitle = () =>
    [
      'External key input. The detector listens to the strip you pick here, while the',
      "gain reduction still applies to this insert's own signal -- the way a bass is",
      'ducked under a kick.',
      '',
      'The key is taken after that strip’s inserts and before its fader, so riding or',
      'muting the source does not change how hard this compressor ducks. It arrives one',
      'audio block late, which is why an attack of 0-5 ms is the useful range here;',
      'release 20-150 ms lets the level back up between hits.',
    ].join(' ');

  return (
    <>
      <div class="wv-fx__display wv-fx__display--comp">
        <div
          class="wv-fx__timeview"
          classList={{ 'is-off': props.muted }}
          role="img"
          aria-label={vizLabel()}
          title={vizTitle()}
        >
          <div class="wv-fx__vizhead">
            <span>Envelope</span>
            <Show when={keyed()}>
              <em class="wv-fx__keybadge" title={`Keyed off ${sourceLabel()}`}>
                Key · {sourceLabel()}
              </em>
            </Show>
            <strong>Time →</strong>
          </div>
          <svg viewBox="0 0 100 86" preserveAspectRatio="none" aria-hidden="true">
            <path
              class="wv-fx__vgrid"
              d="M0 22H100 M0 36H100 M0 50H100 M0 64H100 M25 0V86 M50 0V86 M75 0V86"
            />
            <path class="wv-fx__threshold" d={`M0 ${yForLevelDb(threshold()).toFixed(2)}H100`} />
            <path class="wv-fx__grarea" d={areaPath(reductions(), yForReductionDb)} />
            <path
              class="wv-fx__envelope"
              classList={{ 'is-key': keyed() }}
              d={tracePath(levels(), yForLevelDb)}
            />
          </svg>
          <div class="wv-fx__vizlegend">
            <span>
              <i classList={{ 'is-input': !keyed(), 'is-key': keyed() }} />
              {keyed() ? `key · ${sourceLabel()}` : 'input envelope'}
            </span>
            <span>
              <i class="is-gr" /> gain reduction
            </span>
          </div>
        </div>
        <div class="wv-fx__compbody">
          <TransferCurve
            min={-60}
            max={0}
            yAt={(input) => compressorOutputDb(input, threshold(), ratio(), knee(), makeup())}
            live={live()}
            marks={[threshold()]}
            muted={props.muted}
            title={
              keyed()
                ? `Input dB → output dB. The dot is where ${sourceLabel()} is driving the detector right now.`
                : 'Input dB → output dB. The dot is the current gain reduction on this curve.'
            }
          />
          <div class="wv-fx__gr" title="Peak gain reduction this block. Click the number to note it.">
            <div
              class="wv-fx__grfill"
              style={{ height: `${Math.min(100, (gr() / Math.max(12, gr())) * 100)}%` }}
            />
            <span class="wv-fx__grnum">{gr() < 0.05 ? '0.0' : `−${gr().toFixed(1)}`} dB</span>
          </div>
        </div>
      </div>

      <div class="wv-fx__key" classList={{ 'is-routed': props.sidechain !== null }}>
        <label class="wv-fx__keylabel" for={selectId}>
          Sidechain
        </label>
        <select
          id={selectId}
          class="wv-fx__keypick"
          title={selectorTitle()}
          value={keyValue(props.sidechain)}
          onChange={(event) =>
            props.onSidechain(keyFromValue(event.currentTarget.value, props.strips))
          }
        >
          {/* `selected` on every option, not only `value` on the select: the
              option list is rebuilt whenever the mixer echoes a command, and a
              rebuilt list with no `selected` on it silently falls back to the
              first entry -- a selector that reads "None" over a live route. */}
          <option value="" selected={props.sidechain === null}>
            None — internal detector
          </option>
          <optgroup label="Tracks">
            <For each={props.strips.filter((strip) => strip.kind === 'track')}>
              {(strip) => (
                <option value={keyValue(strip)} selected={keyValue(props.sidechain) === keyValue(strip)}>
                  {strip.name}
                </option>
              )}
            </For>
          </optgroup>
          <Show when={props.strips.some((strip) => strip.kind === 'bus')}>
            <optgroup label="Buses">
              <For each={props.strips.filter((strip) => strip.kind === 'bus')}>
                {(strip) => (
                  <option value={keyValue(strip)} selected={keyValue(props.sidechain) === keyValue(strip)}>
                    {strip.name}
                  </option>
                )}
              </For>
            </optgroup>
          </Show>
        </select>
        <Show
          when={props.sidechain !== null}
          fallback={
            <span class="wv-fx__keyhint">Detector listens to this insert. Pick a strip to duck under it.</span>
          }
        >
          <Show
            when={source()}
            fallback={
              <span class="wv-fx__keyhint is-warn">
                That strip is gone — the detector is back on this insert's own input.
              </span>
            }
          >
            {(strip) => (
              <span class="wv-fx__keyhint">
                Detector follows <b>{strip().name}</b>, post-insert and pre-fader. For a duck: attack
                0–5 ms, release 20–150 ms.
              </span>
            )}
          </Show>
        </Show>
      </div>

      <PrimaryRow params={props.primary} knobs={props.knobs} />
      <Advanced params={props.advanced} knobs={props.knobs} />
    </>
  );
}
