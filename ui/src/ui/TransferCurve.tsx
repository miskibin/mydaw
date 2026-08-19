/**
 * A transfer curve: input on X, output on Y, optional live operating point.
 *
 * Used by the compressor (dB in / dB out, shared scale) and the saturator
 * (linear sample in / sample out). The path is a picture of the *parameters*;
 * the dot is whatever the caller computed from a readout.
 */

import { Show, createMemo, type JSX } from 'solid-js';

const VIEW = 200;

export function TransferCurve(props: {
  /** Output for an input in 0..1 of the axis. */
  yAt: (t: number) => number;
  /** Axis min (left / bottom). */
  min: number;
  max: number;
  /** Optional operating point in the same units as min/max. */
  live?: { x: number; y: number } | null;
  /** Extra guides, as unit values, drawn as dashed lines. */
  marks?: readonly number[];
  muted?: boolean;
  height?: number;
  title?: string;
}): JSX.Element {
  const points = 80;
  const path = createMemo(() => {
    const span = props.max - props.min || 1;
    let d = '';
    for (let i = 0; i < points; i++) {
      const t = i / (points - 1);
      const x = t * VIEW;
      const yUnit = props.yAt(props.min + t * span);
      const y = VIEW - ((yUnit - props.min) / span) * VIEW;
      d += `${i === 0 ? 'M' : 'L'}${x.toFixed(2)} ${y.toFixed(2)} `;
    }
    return d.trim();
  });

  const live = () => {
    const point = props.live;
    if (!point) return null;
    const span = props.max - props.min || 1;
    return {
      x: ((point.x - props.min) / span) * VIEW,
      y: VIEW - ((point.y - props.min) / span) * VIEW,
    };
  };

  return (
    <div
      class="wv-xfer"
      classList={{ 'is-muted': props.muted }}
      style={props.height !== undefined ? { height: `${props.height}px` } : undefined}
      title={props.title}
    >
      <svg class="wv-xfer__svg" viewBox={`0 0 ${VIEW} ${VIEW}`} preserveAspectRatio="none">
        <path class="wv-xfer__diag" d={`M0 ${VIEW} L${VIEW} 0`} />
        {(props.marks ?? []).map((mark) => {
          const span = props.max - props.min || 1;
          const t = ((mark - props.min) / span) * VIEW;
          return (
            <path
              class="wv-xfer__mark"
              d={`M${t.toFixed(1)} 0 V${VIEW}`}
            />
          );
        })}
        <path class="wv-xfer__line" d={path()} />
        <Show when={live()}>
          {(pt) => (
            <circle class="wv-xfer__live" cx={pt().x} cy={pt().y} r="4" />
          )}
        </Show>
      </svg>
    </div>
  );
}
