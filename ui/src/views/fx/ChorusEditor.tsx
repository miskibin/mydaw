import { For, createMemo, type JSX } from 'solid-js';

import type { EffectInfoDto } from '../../api/types';
import { lfoSine } from '../../lib/modulation';
import { formatValue } from '../../lib/param';
import { Segmented } from '../../ui/Segmented';
import type { InsertKnobs } from '../../ui/insertKnob';
import { Advanced, PrimaryRow } from './chrome';
import { createLfoPhase } from './lfoClock';

/** `chorus::VOICE_COUNTS`. */
const VOICE_COUNTS = [2, 3] as const;

/**
 * `chorus.rs`: a quarter of a cycle between the sides at full spread, an eighth
 * each way.
 */
const SPREAD_TURNS = 0.125;

/** Points along the drawn LFO cycle. */
const CYCLE_POINTS = 96;

interface Voice {
  /** Which side of the stereo image this tap is on. */
  channel: 0 | 1;
  index: number;
  /** Its position in the LFO cycle, in turns. */
  turn: number;
  /** The delay it is reading at, in milliseconds. */
  ms: number;
}

export function ChorusEditor(props: {
  info: EffectInfoDto;
  knobs: InsertKnobs;
  values: Map<string, number>;
  primary: EffectInfoDto['params'];
  advanced: EffectInfoDto['params'];
  muted: boolean;
}): JSX.Element {
  const at = (id: string, fallback: number) => props.values.get(id) ?? fallback;
  const phase = createLfoPhase({
    rate: () => at('rate', 0.6),
    running: () => !props.muted,
  });

  const voicesDef = () => props.info.params.find((def) => def.id === 'voices');
  const count = () => VOICE_COUNTS[Math.min(1, Math.max(0, Math.round(at('voices', 1))))] ?? 3;
  const base = () => at('delay', 9);
  const depth = () => at('depth', 2.5);
  const spread = () => at('spread', 50) / 100;

  /** Every tap, both sides, where it is in the cycle and how late it is. */
  const voices = createMemo<Voice[]>(() => {
    const taps: Voice[] = [];
    for (const channel of [0, 1] as const) {
      const side = (channel === 0 ? 1 : -1) * spread() * SPREAD_TURNS;
      for (let index = 0; index < count(); index += 1) {
        const offset = side + index / count();
        const turn = (((phase() + offset) % 1) + 1) % 1;
        taps.push({ channel, index, turn, ms: base() + depth() * lfoSine(turn) });
      }
    }
    return taps;
  });

  /** The axis the delays are drawn against, in milliseconds. */
  const axisMs = () => Math.max(4, base() + depth() * 1.35);
  const wave = createMemo(() =>
    Array.from({ length: CYCLE_POINTS + 1 }, (_, index) => {
      const x = (index / CYCLE_POINTS) * 200;
      const y = 50 - 38 * lfoSine(index / CYCLE_POINTS);
      return `${index === 0 ? 'M' : 'L'}${x.toFixed(2)} ${y.toFixed(2)}`;
    }).join(' '),
  );

  return (
    <>
      <div class="wv-fx__display wv-fx__display--chorus">
        <div
          class="wv-fx__vizpanel"
          classList={{ 'is-off': props.muted }}
          title="One oscillator, read at a different point of its cycle by each voice. When one voice is sharp another is flat — that is what makes the sum an ensemble rather than one detuned copy."
        >
          <div class="wv-fx__vizhead">
            <span>ONE LFO, {count()} TAPS</span>
            <strong>{at('rate', 0.6).toFixed(2)} Hz</strong>
          </div>
          <div class="wv-chorus__stage">
            <svg class="wv-chorus__svg" viewBox="0 0 200 100" preserveAspectRatio="none" aria-hidden="true">
              <path class="wv-resp__zero" d="M0 50 H200" vector-effect="non-scaling-stroke" />
              <path class="wv-chorus__wave" d={wave()} vector-effect="non-scaling-stroke" />
            </svg>
            <For each={voices()}>
              {(voice) => (
                <span
                  class="wv-chorus__dot"
                  classList={{ 'is-right': voice.channel === 1 }}
                  style={{
                    left: `${voice.turn * 100}%`,
                    top: `${50 - 38 * lfoSine(voice.turn)}%`,
                  }}
                >
                  {voice.index + 1}
                </span>
              )}
            </For>
          </div>
          <div class="wv-fx__vizlegend">
            <span>
              <i /> left
            </span>
            <span>
              <i class="is-right" /> right
            </span>
            <span>{spread() > 0 ? `${(spread() * 90).toFixed(0)}° apart` : 'in phase'}</span>
          </div>
        </div>

        <div
          class="wv-fx__vizpanel"
          classList={{ 'is-off': props.muted }}
          title="Where each voice is reading the delay line right now. Below about 30 ms the ear fuses the copies with the source instead of hearing echoes, which is why a chorus lives here."
        >
          <div class="wv-fx__vizhead">
            <span>VOICE DELAYS</span>
            <strong>{formatValue('ms', base())} ± {formatValue('ms', depth())}</strong>
          </div>
          <div class="wv-chorus__axis">
            <i class="wv-chorus__base" style={{ left: `${(base() / axisMs()) * 100}%` }} />
            <For each={voices()}>
              {(voice) => (
                <i
                  class="wv-chorus__tap"
                  classList={{ 'is-right': voice.channel === 1 }}
                  style={{ left: `${Math.max(0, Math.min(100, (voice.ms / axisMs()) * 100))}%` }}
                >
                  <b>{voice.ms.toFixed(1)}</b>
                </i>
              )}
            </For>
          </div>
          <div class="wv-fx__vizlegend">
            <span>0 ms</span>
            <span>dry ◀ · ▶ later</span>
            <span>{formatValue('ms', axisMs())}</span>
          </div>
        </div>

        <div class="wv-fx__satmeta">
          {voicesDef() && (
            <Segmented
              def={voicesDef()!}
              typeId={props.info.typeId}
              value={at('voices', 1)}
              onChange={(value) => {
                props.knobs.setParam('voices', value);
                props.knobs.commit();
              }}
            />
          )}
          <span class="wv-fx__topo">
            in → {count()} modulated taps ÷ √{count()} → mix {at('mix', 50).toFixed(0)} %
          </span>
        </div>
      </div>
      <PrimaryRow params={props.primary} knobs={props.knobs} oversized="depth" />
      <Advanced params={props.advanced} knobs={props.knobs} />
    </>
  );
}
