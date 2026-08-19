/**
 * The EQ's magnitude response. Compact: a picture. Detailed: the control.
 *
 * Settings curve is still the webview's RBJ arithmetic (see {@link EQ_CURVE_NOTE}).
 * When `spectrum` is present it is drawn *behind* the curve as a measurement.
 */

import { For, Show, createMemo, onCleanup, onMount, type JSX } from 'solid-js';

import {
  EQ_BANDS,
  EQ_MAX_HZ,
  EQ_MIN_HZ,
  analyserLine,
  analyserPath,
  bandHandle,
  eqCurveDb,
  eqCurvePath,
  eqDbFromY,
  eqHzFromX,
  eqX,
  eqY,
  hitBand,
  inferBand,
  type EqValues,
} from '../lib/eq';
import { formatValue } from '../lib/param';
import { useAppState } from '../state';
import { bindWheel, FINE_FACTOR } from './drag';

export const EQ_CURVE_NOTE =
  'The shell computes this curve from the EQ’s declared filters (the same RBJ ' +
  'biquad formulas the engine runs) at the settings shown. It is a drawing of ' +
  'what the EQ is set to do — not a measurement of the audio.';

const VIEW_W = 1000;
const VIEW_H = 100;
const POINTS = 220;
const HIT_PX = 16;
const DECADES = [
  { hz: 50, label: '50' },
  { hz: 100, label: '100' },
  { hz: 200, label: '200' },
  { hz: 500, label: '500' },
  { hz: 1_000, label: '1k' },
  { hz: 2_000, label: '2k' },
  { hz: 5_000, label: '5k' },
  { hz: 10_000, label: '10k' },
];
/** Horizontal EQ-gain guides, in dB. 0 dB is the midline, drawn separately. */
const DB_GRID = [12, 6, -6, -12];
const FREQ_MAX: Record<string, number> = { hp: 1_000 };

export type EqParamFn = (id: string, value: number) => void;

export function EqCurve(props: {
  values: EqValues;
  rangeDb: number;
  height: number;
  detailed?: boolean;
  muted?: boolean;
  /** Log-spaced dB bins behind the curve. Same 20 Hz…20 kHz span. */
  spectrum?: readonly number[] | null;
  onParam?: EqParamFn;
  onCommit?: () => void;
  onListen?: (bandId: string | null) => void;
  onAnnounce?: (name: string, value: string) => void;
}): JSX.Element {
  const app = useAppState();
  let root: HTMLDivElement | undefined;

  const sampleRate = () => {
    const fromSequencer = app.sequencer()?.sampleRate ?? 0;
    return fromSequencer > 0 ? fromSequencer : (app.streamInfo()?.sampleRate ?? 0);
  };

  const interactive = () => Boolean(props.detailed && props.onParam);
  const curve = createMemo(() => eqCurveDb(props.values, sampleRate(), POINTS));
  const line = createMemo(() => eqCurvePath(curve(), VIEW_W, VIEW_H, props.rangeDb));
  const area = () => `${line()} L${VIEW_W} ${VIEW_H / 2} L0 ${VIEW_H / 2} Z`;
  const analyser = createMemo(() => {
    const bins = props.spectrum;
    if (!bins || bins.length < 2) return { fill: '', ridge: '' };
    return {
      fill: analyserPath(bins, VIEW_W, VIEW_H),
      ridge: analyserLine(bins, VIEW_W, VIEW_H),
    };
  });

  function local(event: { clientX: number; clientY: number }): { x: number; y: number; hz: number; db: number } | null {
    if (!root) return null;
    const rect = root.getBoundingClientRect();
    const x = event.clientX - rect.left;
    const y = event.clientY - rect.top;
    return {
      x,
      y,
      hz: eqHzFromX(x, rect.width),
      db: eqDbFromY(y, rect.height, props.rangeDb),
    };
  }

  function clampFreq(bandId: string, hz: number): number {
    return Math.min(FREQ_MAX[bandId] ?? EQ_MAX_HZ, Math.max(EQ_MIN_HZ, hz));
  }

  function setBand(bandId: string, hz: number, db: number, enable = true): void {
    const onParam = props.onParam;
    if (!onParam) return;
    if (enable) onParam(`${bandId}.enabled`, 1);
    onParam(`${bandId}.freq`, clampFreq(bandId, hz));
    if (bandId !== 'hp') onParam(`${bandId}.gain`, db);
    props.onAnnounce?.(
      bandLabel(bandId),
      `${formatValue('Hz', clampFreq(bandId, hz))}  ${bandId === 'hp' ? '' : formatValue('dB', db)}`.trim(),
    );
  }

  onMount(() => {
    const el = root;
    if (!el || !interactive()) return;

    const down = (event: PointerEvent) => {
      if (event.button !== 0) return;
      const pos = local(event);
      if (!pos) return;
      const hit = hitBand(props.values, pos.x, pos.y, el.clientWidth, el.clientHeight, props.rangeDb, HIT_PX);

      if (event.altKey && hit) {
        event.preventDefault();
        event.stopPropagation();
        const on = (props.values.get(`${hit}.enabled`) ?? 0) >= 0.5 ? 0 : 1;
        props.onParam?.(`${hit}.enabled`, on);
        props.onCommit?.();
        return;
      }

      if (!hit) return;
      event.preventDefault();
      event.stopPropagation();
      const startX = pos.x;
      const startY = pos.y;
      const startHz = props.values.get(`${hit}.freq`) ?? pos.hz;
      const startDb = props.values.get(`${hit}.gain`) ?? 0;
      props.onListen?.(hit);

      const move = (moveEvent: PointerEvent) => {
        const next = local(moveEvent);
        if (!next) return;
        const lock = moveEvent.altKey;
        const dx = next.x - startX;
        const dy = next.y - startY;
        const useX = !lock || Math.abs(dx) >= Math.abs(dy);
        const useY = !lock || Math.abs(dy) > Math.abs(dx);
        const hz = useX ? next.hz : startHz;
        const db = useY ? next.db : startDb;
        setBand(hit, hz, db);
      };
      const up = () => {
        window.removeEventListener('pointermove', move);
        window.removeEventListener('pointerup', up);
        props.onListen?.(null);
        props.onCommit?.();
      };
      window.addEventListener('pointermove', move);
      window.addEventListener('pointerup', up);
    };

    const dbl = (event: MouseEvent) => {
      const pos = local(event);
      if (!pos) return;
      event.preventDefault();
      const hit = hitBand(props.values, pos.x, pos.y, el.clientWidth, el.clientHeight, props.rangeDb, HIT_PX);
      const band = hit ?? inferBand(pos.hz, pos.x, el.clientWidth, props.values);
      setBand(band, pos.hz, hit ? (props.values.get(`${band}.gain`) ?? 0) : 0);
      props.onCommit?.();
    };

    el.addEventListener('pointerdown', down);
    el.addEventListener('dblclick', dbl);
    const unbind = bindWheel(el, (notches, event) => {
      const pos = local(event);
      if (!pos) return false;
      const hit = hitBand(props.values, pos.x, pos.y, el.clientWidth, el.clientHeight, props.rangeDb, HIT_PX);
      if (!hit) return false;
      const scale = event.shiftKey ? FINE_FACTOR : 1;
      if (hit === 'hp') {
        const slope = Math.round(props.values.get('hp.slope') ?? 1);
        const next = Math.min(1, Math.max(0, slope + (notches > 0 ? 1 : -1)));
        props.onParam?.('hp.slope', next);
        props.onAnnounce?.('Highpass', next ? '24 dB/oct' : '12 dB/oct');
        props.onCommit?.();
        return;
      }
      const q = props.values.get(`${hit}.q`) ?? 1;
      const next = Math.min(10, Math.max(0.1, q * 1.08 ** (notches * scale)));
      props.onParam?.(`${hit}.q`, next);
      props.onAnnounce?.(bandLabel(hit), `Q ${next.toFixed(2)}`);
      // Commit like the hp-slope branch above, or the knobs' dragging
      // override never clears and the mixer keeps the stale Q.
      props.onCommit?.();
    });

    onCleanup(() => {
      el.removeEventListener('pointerdown', down);
      el.removeEventListener('dblclick', dbl);
      unbind();
    });
  });

  return (
    <div
      ref={root}
      class="wv-eqc"
      classList={{
        'is-muted': props.muted,
        'wv-eqc--detailed': props.detailed,
        'wv-eqc--live': interactive(),
      }}
      style={props.detailed ? undefined : { height: `${props.height}px` }}
      title={
        interactive()
          ? 'Drag a band. Wheel: Q. Alt-click: enable. Double-click empty: add. Hold: solo the source.'
          : EQ_CURVE_NOTE
      }
    >
      <svg
        class="wv-eqc__svg"
        viewBox={`0 0 ${VIEW_W} ${VIEW_H}`}
        preserveAspectRatio="none"
        aria-hidden="true"
      >
        <Show when={props.detailed}>
          <For each={DECADES}>
            {(decade) => (
              <path
                class="wv-eqc__grid"
                d={`M${eqX(decade.hz, VIEW_W).toFixed(1)} 0 V${VIEW_H}`}
                vector-effect="non-scaling-stroke"
              />
            )}
          </For>
          <For each={DB_GRID}>
            {(db) => (
              <path
                class="wv-eqc__grid"
                d={`M0 ${eqY(db, VIEW_H, props.rangeDb).toFixed(2)} H${VIEW_W}`}
                vector-effect="non-scaling-stroke"
              />
            )}
          </For>
        </Show>
        <Show when={analyser().fill}>
          {(d) => <path class="wv-eqc__spec" d={d()} />}
        </Show>
        <Show when={analyser().ridge}>
          {(d) => <path class="wv-eqc__specline" d={d()} vector-effect="non-scaling-stroke" />}
        </Show>
        <path
          class="wv-eqc__zero"
          d={`M0 ${VIEW_H / 2} H${VIEW_W}`}
          vector-effect="non-scaling-stroke"
        />
        <path class="wv-eqc__area" d={area()} />
        <path class="wv-eqc__line" d={line()} vector-effect="non-scaling-stroke" />
      </svg>

      <Show when={props.detailed}>
        <For each={EQ_BANDS}>
          {(band, index) => {
            const handle = () => bandHandle(props.values, band.id, 100, 100, props.rangeDb);
            return (
              <Show when={handle()}>
                {(pt) => (
                  <span
                    class={`wv-eqc__dot wv-eqc__dot--${band.id}`}
                    classList={{ 'is-off': !pt().enabled }}
                    style={{ left: `${pt().x}%`, top: `${pt().y}%` }}
                  >
                    {index() + 1}
                  </span>
                )}
              </Show>
            );
          }}
        </For>
        <For each={DECADES}>
          {(decade) => (
            <span
              class="wv-eqc__hz"
              style={{ left: `${(eqX(decade.hz, VIEW_W) / VIEW_W) * 100}%` }}
            >
              {decade.label}
            </span>
          )}
        </For>
        <span class="wv-eqc__db wv-eqc__db--top">+{props.rangeDb.toFixed(0)}</span>
        <span class="wv-eqc__db wv-eqc__db--zero">0</span>
        <span class="wv-eqc__db wv-eqc__db--bottom">&minus;{props.rangeDb.toFixed(0)}</span>
        <span class="wv-eqc__hz wv-eqc__hz--end" style={{ left: '100%' }}>
          {EQ_MAX_HZ / 1000}k
        </span>
        <span class="wv-eqc__hz wv-eqc__hz--start" style={{ left: '0%' }}>
          {EQ_MIN_HZ}
        </span>
      </Show>
    </div>
  );
}

function bandLabel(id: string): string {
  switch (id) {
    case 'hp':
      return 'Highpass';
    case 'low':
      return 'Low shelf';
    case 'bell1':
      return 'Bell 1';
    case 'bell2':
      return 'Bell 2';
    case 'high':
      return 'High shelf';
    default:
      return id;
  }
}
