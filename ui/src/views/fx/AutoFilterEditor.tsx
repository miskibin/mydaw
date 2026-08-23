import { Show, createMemo, onCleanup, onMount, type JSX } from 'solid-js';

import type { EffectInfoDto, ParamValueDto } from '../../api/types';
import { EQ_MAX_HZ, EQ_MIN_HZ, eqHzFromX, eqX } from '../../lib/eq';
import {
  filterMode,
  modulatedCutoffHz,
  resonanceQ,
  svfMagDb,
  syncRateHz,
} from '../../lib/modulation';
import { formatValue } from '../../lib/param';
import { Segmented } from '../../ui/Segmented';
import { ResponseCurve, sampleResponse } from '../../ui/ResponseCurve';
import { bindWheel, FINE_FACTOR } from '../../ui/drag';
import type { InsertKnobs } from '../../ui/insertKnob';
import { Advanced, PrimaryRow } from './chrome';
import { createLfoPhase } from './lfoClock';

const POINTS = 260;

/** Half-height of the response box, in dB. A filter falls further than an EQ. */
const RANGE_DB = 24;

/** `autofilter::MAX_LFO_OCTAVES` and `MAX_ENV_OCTAVES`. */
const MAX_LFO_OCTAVES = 3;
const MAX_ENV_OCTAVES = 4;

/** A whole box height of vertical drag is this much resonance. */
const RESONANCE_PER_BOX = 100;

export function AutoFilterEditor(props: {
  info: EffectInfoDto;
  knobs: InsertKnobs;
  values: Map<string, number>;
  live: ParamValueDto[];
  primary: EffectInfoDto['params'];
  advanced: EffectInfoDto['params'];
  muted: boolean;
  bpm: number;
  sampleRate: number;
  spectrum: readonly number[] | null;
  onAnnounce: (name: string, value: string) => void;
}): JSX.Element {
  let box: HTMLDivElement | undefined;

  const at = (id: string, fallback: number) => props.values.get(id) ?? fallback;
  const readout = (id: string) => props.live.find((entry) => entry.id === id)?.value;

  const mode = () => filterMode(at('mode', 0));
  const cutoff = () => at('cutoff', 20_000);
  const q = () => resonanceQ(at('res', 0));
  const mix = () => at('mix', 100) / 100;
  const lfoAmount = () => at('lfo_amount', 0);
  const envAmount = () => at('env_amount', 0);
  const synced = () => at('lfo_sync', 0) >= 0.5;
  const rateHz = () => (synced() ? syncRateHz(at('lfo_div', 2), props.bpm) : at('lfo_rate', 1));

  const phase = createLfoPhase({
    rate: rateHz,
    running: () => !props.muted && lfoAmount() > 0,
  });

  /**
   * The envelope the engine reports, or nothing.
   *
   * The follower's output depends on the audio, so unlike everything else on
   * this display it cannot be drawn from the settings -- when the engine is not
   * publishing (no transport, a bypassed insert) the display says so rather
   * than animating an envelope that is not happening.
   */
  const envelope = () => readout('env') ?? 0;

  /** Where the filter actually is, from the engine's own readout when it has one. */
  const liveCutoff = () => {
    const published = readout('cutoff');
    if (published !== undefined && published > EQ_MIN_HZ) return published;
    return modulatedCutoffHz({
      cutoffHz: cutoff(),
      lfoAmount: lfoAmount(),
      phase: phase(),
      envAmount: envAmount(),
      envelope: envelope(),
      maxLfoOctaves: MAX_LFO_OCTAVES,
      maxEnvOctaves: MAX_ENV_OCTAVES,
    });
  };

  const response = (hz: number) =>
    sampleResponse(POINTS, (f) => svfMagDb(f, hz, q(), mode(), props.sampleRate, mix()));
  const curve = createMemo(() => response(cutoff()));
  const ghost = createMemo(() => {
    const moving = lfoAmount() > 0 || envAmount() !== 0;
    return moving ? response(liveCutoff()) : null;
  });

  /** How far the modulation can take the cutoff, in octaves either way. */
  const travel = () => {
    const lfo = (lfoAmount() / 100) * MAX_LFO_OCTAVES;
    const env = (envAmount() / 100) * MAX_ENV_OCTAVES;
    return { down: -lfo + Math.min(0, env), up: lfo + Math.max(0, env) };
  };
  const band = createMemo(() => {
    const { down, up } = travel();
    if (up - down < 0.01) return null;
    return { lower: response(cutoff() * 2 ** down), upper: response(cutoff() * 2 ** up) };
  });

  /** The handle sits on the settled curve, at the cutoff the knob holds. */
  const handle = () => {
    const db = svfMagDb(cutoff(), cutoff(), q(), mode(), props.sampleRate, mix());
    const clamped = Math.min(RANGE_DB, Math.max(-RANGE_DB, db));
    return {
      left: (eqX(cutoff(), 1000) / 1000) * 100,
      top: 50 - (clamped / RANGE_DB) * 50,
    };
  };

  function announce(): void {
    props.onAnnounce(
      'Filter',
      `${formatValue('Hz', cutoff())} · resonance ${at('res', 0).toFixed(0)} %`,
    );
  }

  onMount(() => {
    const el = box;
    if (!el) return;

    const down = (event: PointerEvent) => {
      if (event.button !== 0) return;
      const rect = el.getBoundingClientRect();
      event.preventDefault();
      event.stopPropagation();
      const startY = event.clientY;
      const startRes = at('res', 0);

      const apply = (moveEvent: PointerEvent) => {
        const hz = eqHzFromX(moveEvent.clientX - rect.left, rect.width);
        props.knobs.setParam('cutoff', Math.min(EQ_MAX_HZ, Math.max(30, hz)));
        // Vertical drag is *relative*: a resonance has no position on a dB
        // axis, so the handle would otherwise jump to the pointer the moment a
        // drag started.
        const delta = ((startY - moveEvent.clientY) / Math.max(1, rect.height)) * RESONANCE_PER_BOX;
        props.knobs.setParam('res', Math.min(100, Math.max(0, startRes + delta)));
        announce();
      };
      apply(event);

      const up = () => {
        window.removeEventListener('pointermove', apply);
        window.removeEventListener('pointerup', up);
        props.knobs.commit();
      };
      window.addEventListener('pointermove', apply);
      window.addEventListener('pointerup', up);
    };

    el.addEventListener('pointerdown', down);
    const unbind = bindWheel(el, (notches, event) => {
      const scale = event.shiftKey ? FINE_FACTOR : 1;
      const next = Math.min(100, Math.max(0, at('res', 0) + notches * 4 * scale));
      props.knobs.setParam('res', next);
      props.knobs.commit();
      announce();
    });
    onCleanup(() => {
      el.removeEventListener('pointerdown', down);
      unbind();
    });
  });

  /**
   * The rate knob is only on the row when it is the rate in force.
   *
   * A synced LFO reads its rate from the transport, so leaving a knob saying
   * "1.0 Hz" next to a display saying 2.17 Hz would be a control that lies.
   * The division row beside the sync switch is the rate control while sync is
   * on, and the knob comes back -- at the setting it was left at -- the moment
   * it goes off. The same rule the delay applies to its `time_ms` knob.
   */
  const primaryKnobs = () =>
    synced() ? props.primary.filter((def) => def.id !== 'lfo_rate') : props.primary;

  const divisionDef = () => props.info.params.find((def) => def.id === 'lfo_div');
  const modeDef = () => props.info.params.find((def) => def.id === 'mode');

  return (
    <>
      <div class="wv-fx__display wv-fx__display--filter">
        <div class="wv-fx__satmeta">
          {modeDef() && (
            <Segmented
              def={modeDef()!}
              typeId={props.info.typeId}
              value={at('mode', 0)}
              labels={['LP', 'HP', 'BP']}
              onChange={(value) => {
                props.knobs.setParam('mode', value);
                props.knobs.commit();
              }}
            />
          )}
          <button
            type="button"
            class="wv-chip"
            classList={{ 'is-active': synced() }}
            aria-pressed={synced()}
            title={
              synced()
                ? 'The LFO is locked to the transport — click for a rate in hertz'
                : 'The LFO runs in hertz — click to lock it to the transport'
            }
            onClick={() => {
              props.knobs.setParam('lfo_sync', synced() ? 0 : 1);
              props.knobs.commit();
            }}
          >
            {synced() ? 'Synced' : 'Free'}
          </button>
          <Show when={synced() && divisionDef()}>
            <Segmented
              def={divisionDef()!}
              typeId={props.info.typeId}
              value={at('lfo_div', 2)}
              onChange={(value) => {
                props.knobs.setParam('lfo_div', value);
                props.knobs.commit();
              }}
            />
          </Show>
          <span class="wv-fx__time">
            {rateHz() >= 1 ? `${rateHz().toFixed(2)} Hz` : `${(1 / Math.max(1e-6, rateHz())).toFixed(1)} s`}
          </span>
        </div>

        <ResponseCurve
          curve={curve()}
          ghost={ghost()}
          band={band()}
          rangeDb={RANGE_DB}
          spectrum={props.spectrum}
          muted={props.muted}
          label={`${mode().toUpperCase()} · ${formatValue('Hz', cutoff())} · Q ${q().toFixed(2)}`}
          value={
            ghost()
              ? `now ${formatValue('Hz', liveCutoff())}`
              : `${at('res', 0).toFixed(0)} % resonance`
          }
          title={
            'Drag the handle: sideways for cutoff, up and down for resonance. Wheel: resonance. ' +
            'The shaded band is everywhere the LFO and the envelope can take the filter; the ' +
            'dashed curve is where the engine reports it right now; the fill behind is the ' +
            'live spectrum leaving this insert.'
          }
          ref={(element) => {
            box = element;
          }}
        >
          <span
            class="wv-resp__handle"
            classList={{ 'is-off': props.muted }}
            style={{ left: `${handle().left}%`, top: `${handle().top}%` }}
          />
          <Show when={ghost()}>
            <span
              class="wv-resp__live"
              style={{ left: `${(eqX(liveCutoff(), 1000) / 1000) * 100}%` }}
            />
          </Show>
        </ResponseCurve>

        <p class="wv-fx__topo">
          {lfoAmount() > 0
            ? `LFO ±${((lfoAmount() / 100) * MAX_LFO_OCTAVES).toFixed(1)} oct at ${rateHz().toFixed(2)} Hz`
            : 'LFO off'}
          {' · '}
          {envAmount() !== 0
            ? `envelope ${envAmount() > 0 ? '+' : '−'}${Math.abs((envAmount() / 100) * MAX_ENV_OCTAVES).toFixed(1)} oct, now ${(envelope() * 100).toFixed(0)} %`
            : 'envelope off'}
        </p>
      </div>
      <PrimaryRow params={primaryKnobs()} knobs={props.knobs} oversized="lfo_amount" />
      <Advanced params={props.advanced} knobs={props.knobs} />
    </>
  );
}
