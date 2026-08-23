/**
 * A magnitude response on the shell's standard 20 Hz…20 kHz logarithmic axis.
 *
 * The flanger's comb, the phaser's notches and the auto-filter's corner are
 * three different filters drawn the same way, so they are drawn by one
 * component: same axis, same grid, same live spectrum behind, same "this is a
 * drawing of the settings" contract as {@link ../lib/eq.ts}. What differs is
 * the curve each editor hands in, the shaded band showing where its modulation
 * takes it, and whatever it overlays on top.
 *
 * The SVG is `position: absolute; inset: 0` inside a bounded container -- an
 * in-flow SVG with `height: 100%` resolves against its own viewBox aspect ratio
 * and inflates the whole sheet past the window. See `.wv-resp__svg`.
 */

import { For, Show, type JSX } from 'solid-js';

import { analyserLine, analyserPath, eqCurvePath, eqX, EQ_MAX_HZ, EQ_MIN_HZ } from '../lib/eq';

const VIEW_W = 1000;
const VIEW_H = 100;

/** Decade marks, the same set the EQ's curve labels. */
const DECADES = [
  { hz: 100, label: '100' },
  { hz: 1_000, label: '1k' },
  { hz: 10_000, label: '10k' },
];

const GRID_HZ = [50, 200, 500, 2_000, 5_000];

export interface ResponseBand {
  /** The response at one end of the modulation's travel, in dB. */
  lower: readonly number[];
  /** The response at the other end. */
  upper: readonly number[];
}

export function ResponseCurve(props: {
  /** dB at log-spaced points from 20 Hz to 20 kHz. */
  curve: readonly number[];
  /** Where the modulation takes the curve, shaded behind it. */
  band?: ResponseBand | null;
  /**
   * A second, thinner curve: where the modulation has the filter *right now*,
   * against the settled one the handle is dragged on.
   */
  ghost?: readonly number[] | null;
  /** Half-height of the box, in dB. */
  rangeDb: number;
  /** Post-effect spectrum, drawn behind everything as a measurement. */
  spectrum?: readonly number[] | null;
  /** Left of the header: what the picture is. */
  label: string;
  /** Right of the header: what it currently says. */
  value?: string;
  title: string;
  muted?: boolean;
  /** Vertical guides, e.g. where the notches are. */
  marks?: readonly number[];
  /** The container, for an editor that makes the curve draggable. */
  ref?: (element: HTMLDivElement) => void;
  /** Overlaid HTML: handles, dots, extra labels. */
  children?: JSX.Element;
}): JSX.Element {
  const line = () => eqCurvePath(props.curve, VIEW_W, VIEW_H, props.rangeDb);
  const area = () => `${line()} L${VIEW_W} ${VIEW_H / 2} L0 ${VIEW_H / 2} Z`;
  const bandPath = () => {
    const band = props.band;
    if (!band || band.lower.length < 2 || band.upper.length !== band.lower.length) return '';
    const upper = eqCurvePath(band.upper, VIEW_W, VIEW_H, props.rangeDb);
    const lower = [...band.lower]
      .reverse()
      .map((db, index) => {
        const span = props.rangeDb > 0 ? props.rangeDb : 1;
        const x = VIEW_W - (index * VIEW_W) / (band.lower.length - 1);
        const y = VIEW_H / 2 - (Math.min(span, Math.max(-span, db)) / span) * (VIEW_H / 2);
        return `L${x.toFixed(2)} ${y.toFixed(2)}`;
      })
      .join(' ');
    return `${upper} ${lower} Z`;
  };
  const analyser = () => {
    const bins = props.spectrum;
    if (!bins || bins.length < 2) return null;
    return { fill: analyserPath(bins, VIEW_W, VIEW_H), ridge: analyserLine(bins, VIEW_W, VIEW_H) };
  };

  return (
    <div class="wv-resp" classList={{ 'is-off': props.muted }} title={props.title}>
      <div class="wv-fx__vizhead">
        <span>{props.label}</span>
        <Show when={props.value}>{(text) => <strong>{text()}</strong>}</Show>
      </div>
      <div class="wv-resp__box" ref={props.ref}>
        <svg
          class="wv-resp__svg"
          viewBox={`0 0 ${VIEW_W} ${VIEW_H}`}
          preserveAspectRatio="none"
          aria-hidden="true"
        >
          <For each={GRID_HZ}>
            {(hz) => (
              <path
                class="wv-resp__grid"
                d={`M${eqX(hz, VIEW_W).toFixed(1)} 0 V${VIEW_H}`}
                vector-effect="non-scaling-stroke"
              />
            )}
          </For>
          <For each={DECADES}>
            {(decade) => (
              <path
                class="wv-resp__grid is-decade"
                d={`M${eqX(decade.hz, VIEW_W).toFixed(1)} 0 V${VIEW_H}`}
                vector-effect="non-scaling-stroke"
              />
            )}
          </For>
          <Show when={analyser()}>
            {(spectrum) => (
              <>
                <path class="wv-eqc__spec" d={spectrum().fill} />
                <path class="wv-eqc__specline" d={spectrum().ridge} vector-effect="non-scaling-stroke" />
              </>
            )}
          </Show>
          <path
            class="wv-resp__zero"
            d={`M0 ${VIEW_H / 2} H${VIEW_W}`}
            vector-effect="non-scaling-stroke"
          />
          <Show when={bandPath()}>{(d) => <path class="wv-resp__band" d={d()} />}</Show>
          <For each={props.marks ?? []}>
            {(hz) => (
              <path
                class="wv-resp__mark"
                d={`M${eqX(hz, VIEW_W).toFixed(1)} 0 V${VIEW_H}`}
                vector-effect="non-scaling-stroke"
              />
            )}
          </For>
          <Show when={props.ghost && props.ghost.length > 1}>
            <path
              class="wv-resp__ghost"
              d={eqCurvePath(props.ghost ?? [], VIEW_W, VIEW_H, props.rangeDb)}
              vector-effect="non-scaling-stroke"
            />
          </Show>
          <path class="wv-resp__area" d={area()} />
          <path class="wv-resp__line" d={line()} vector-effect="non-scaling-stroke" />
        </svg>
        {props.children}
        <For each={DECADES}>
          {(decade) => (
            <span class="wv-resp__hz" style={{ left: `${(eqX(decade.hz, VIEW_W) / VIEW_W) * 100}%` }}>
              {decade.label}
            </span>
          )}
        </For>
        <span class="wv-resp__hz wv-resp__hz--start">{EQ_MIN_HZ}</span>
        <span class="wv-resp__hz wv-resp__hz--end">{EQ_MAX_HZ / 1000}k</span>
        <span class="wv-resp__db">+{props.rangeDb.toFixed(0)}</span>
        <span class="wv-resp__db wv-resp__db--bottom">&minus;{props.rangeDb.toFixed(0)} dB</span>
      </div>
    </div>
  );
}

/** Sample a response at the same log-spaced points {@link ResponseCurve} draws. */
export function sampleResponse(points: number, at: (hz: number) => number): number[] {
  const count = Math.max(2, Math.trunc(points));
  const ratio = EQ_MAX_HZ / EQ_MIN_HZ;
  return Array.from({ length: count }, (_, index) =>
    at(EQ_MIN_HZ * ratio ** (index / (count - 1))),
  );
}
