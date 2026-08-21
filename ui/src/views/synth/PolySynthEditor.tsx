/**
 * The poly synth's own window: three oscillators stacked on one picture, then
 * the shared filter and envelope.
 */

import { For, Show, createEffect, createMemo, createSignal, on, onCleanup, type JSX } from 'solid-js';

import type { ParamDefDto, SpectrumDto } from '../../api/types';
import { eqCurvePath, eqX, eqY } from '../../lib/eq';
import { formatParam, snapToStep } from '../../lib/param';
import {
  FILTER_RANGE_DB,
  adsrFromDrag,
  adsrLayout,
  envCutoffHz,
  envSweepActive,
  filterFromPointer,
  hitAdsrHandle,
  oscillatorPath,
  oscillatorStack,
  oscSample,
  signalPath,
  svfLowpassCurveDb,
  svfLowpassMagDb,
  type AdsrHandle,
  type OscSlot,
} from '../../lib/polySynth';
import { useView } from '../../shell/view';
import { useAppState } from '../../state';
import { Knob } from '../../ui/Knob';
import { Segmented } from '../../ui/Segmented';

import '../../styles/synth.css';

const VIEW_W = 1000;
const VIEW_H = 100;
const OSC_SAMPLES = 160;
const CURVE_POINTS = 160;
const ADSR_BOX = 100;
const HANDLE_PX = 14;
const WAVE_LABELS = ['Saw', 'Sqr', 'Sin', 'Tri', 'Nse'] as const;

const DECADES = [
  { hz: 100, label: '100' },
  { hz: 1_000, label: '1k' },
  { hz: 10_000, label: '10k' },
] as const;

const OSCS: readonly {
  title: string;
  wave: string;
  coarse: string;
  fine: string;
  mix: string | null;
}[] = [
  { title: 'OSC 1', wave: 'wave', coarse: 'osc1Coarse', fine: 'osc1Fine', mix: null },
  { title: 'OSC 2', wave: 'osc2Wave', coarse: 'osc2Coarse', fine: 'osc2Fine', mix: 'osc2Mix' },
  { title: 'OSC 3', wave: 'osc3Wave', coarse: 'osc3Coarse', fine: 'osc3Fine', mix: 'osc3Mix' },
];

const FILT_IDS = ['cutoff', 'resonance', 'envAmount'] as const;
const ENV_IDS = ['attack', 'decay', 'sustain', 'release'] as const;

type Pane = 'mix' | 'osc0' | 'osc1' | 'osc2' | 'filt' | 'env';

export function PolySynthEditor(props: {
  defs: ParamDefDto[];
  owner: string;
  color: string;
  paramValue: (def: ParamDefDto) => number;
  setParam: (def: ParamDefDto, value: number) => void;
  sampleRate: number;
  trackIndex: number;
  onPreview: () => void;
}): JSX.Element {
  const app = useAppState();
  const view = useView();
  const [hot, setHot] = createSignal<Pane | null>(null);
  const [live, setLive] = createSignal<SpectrumDto | null>(null);

  const defOf = (id: string): ParamDefDto | undefined => props.defs.find((entry) => entry.id === id);

  const at = (id: string, fallback: number): number => {
    const found = defOf(id);
    return found ? props.paramValue(found) : fallback;
  };

  function write(id: string, value: number): void {
    const found = defOf(id);
    if (!found) return;
    const next = snapToStep(found, Math.min(found.max, Math.max(found.min, value)));
    props.setParam(found, next);
    app.say(`${props.owner} · ${found.name}`, formatParam('poly-synth', found, next));
  }

  const filtKnobs = createMemo(() =>
    FILT_IDS.map((id) => defOf(id)).filter((entry): entry is ParamDefDto => !!entry),
  );
  const envKnobs = createMemo(() =>
    ENV_IDS.map((id) => defOf(id)).filter((entry): entry is ParamDefDto => !!entry),
  );

  const slots = createMemo((): OscSlot[] =>
    OSCS.map((osc, index) => ({
      wave: at(osc.wave, 0),
      coarse: at(osc.coarse, index === 2 ? -12 : 0),
      fine: at(osc.fine, 0),
      mix: osc.mix ? at(osc.mix, index === 2 ? 0.5 : 0) : 1,
    })),
  );

  const stacked = createMemo(() => oscillatorStack(slots(), OSC_SAMPLES));

  const cutoff = () => at('cutoff', 3_000);
  const resonance = () => at('resonance', 0.8);
  const envAmount = () => at('envAmount', 0);
  const rate = () => props.sampleRate;

  const restCurve = createMemo(() => svfLowpassCurveDb(cutoff(), resonance(), rate(), CURVE_POINTS));
  const envHz = createMemo(() => envCutoffHz(cutoff(), envAmount()));
  const envCurve = createMemo(() =>
    envSweepActive(envAmount()) ? svfLowpassCurveDb(envHz(), resonance(), rate(), CURVE_POINTS) : null,
  );
  const restPath = createMemo(() => eqCurvePath(restCurve(), VIEW_W, VIEW_H, FILTER_RANGE_DB));
  const restArea = createMemo(() => `${restPath()} L${VIEW_W} ${VIEW_H / 2} L0 ${VIEW_H / 2} Z`);
  const envPath = createMemo(() => {
    const curve = envCurve();
    return curve ? eqCurvePath(curve, VIEW_W, VIEW_H, FILTER_RANGE_DB) : '';
  });
  const sweepPath = createMemo(() => {
    const peak = envCurve();
    if (!peak) return '';
    return bandPath(restCurve(), peak, VIEW_W, VIEW_H, FILTER_RANGE_DB);
  });
  const handleX = createMemo(() => eqX(cutoff(), 100));
  const handleY = createMemo(() =>
    eqY(svfLowpassMagDb(cutoff(), cutoff(), resonance(), rate()), 100, FILTER_RANGE_DB),
  );

  const envelope = createMemo(() => ({
    attackMs: at('attack', 5),
    decayMs: at('decay', 80),
    sustain: at('sustain', 0.6),
    releaseMs: at('release', 120),
  }));
  const ads = createMemo(() => adsrLayout(envelope(), ADSR_BOX, ADSR_BOX));

  const liveScope = createMemo(() => {
    const frame = live();
    if (!frame) return null;
    const left = frame.scopeL;
    if (left.length < 2) return null;
    const peak = left.reduce((max, value) => Math.max(max, Math.abs(value)), 0);
    if (peak < 0.012) return null;
    return left.map((value) => value / peak);
  });

  const insertOwnsTap = () => {
    const focused = view.focusedInsertId();
    return focused !== null && view.insertEditors().some((ref) => ref.insertId === focused);
  };

  createEffect(
    on(
      () => [props.trackIndex, insertOwnsTap()] as const,
      ([index, stolen]) => {
        if (stolen) {
          setLive(null);
          return;
        }
        void app.backend.watchSpectrum({ kind: 'track', index }, null).catch(() => {});
        const timer = setInterval(() => {
          void app.backend
            .getInsertSpectrum()
            .then((frame) => setLive(frame))
            .catch(() => {});
        }, 40);
        onCleanup(() => {
          clearInterval(timer);
          setLive(null);
          if (!insertOwnsTap()) {
            void app.backend.watchSpectrum(null, null).catch(() => {});
          }
        });
      },
    ),
  );

  function onFilterPointer(event: PointerEvent, stage: HTMLElement): void {
    if (event.button !== 0) return;
    event.preventDefault();
    event.stopPropagation();
    setHot('filt');
    const apply = (move: PointerEvent) => {
      const rect = stage.getBoundingClientRect();
      if (rect.width <= 0 || rect.height <= 0) return;
      const mapped = filterFromPointer(
        move.clientX - rect.left,
        move.clientY - rect.top,
        rect.width,
        rect.height,
        FILTER_RANGE_DB,
      );
      write('cutoff', mapped.cutoffHz);
      write('resonance', mapped.resonance);
    };
    apply(event);
    const up = () => {
      window.removeEventListener('pointermove', apply);
      window.removeEventListener('pointerup', up);
    };
    window.addEventListener('pointermove', apply);
    window.addEventListener('pointerup', up);
  }

  function onEnvelopePointer(event: PointerEvent, stage: HTMLElement): void {
    if (event.button !== 0) return;
    const rect = stage.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return;
    const x = ((event.clientX - rect.left) / rect.width) * ADSR_BOX;
    const y = ((event.clientY - rect.top) / rect.height) * ADSR_BOX;
    const hit = hitAdsrHandle(ads(), x, y, HANDLE_PX);
    if (!hit) return;
    event.preventDefault();
    event.stopPropagation();
    setHot('env');
    const move = (next: PointerEvent) => {
      const box = stage.getBoundingClientRect();
      const nx = ((next.clientX - box.left) / box.width) * ADSR_BOX;
      const ny = ((next.clientY - box.top) / box.height) * ADSR_BOX;
      applyAdsr(hit, nx, ny);
    };
    const up = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  }

  function applyAdsr(handle: AdsrHandle, x: number, y: number): void {
    const next = adsrFromDrag(envelope(), handle, x, y, ADSR_BOX, ADSR_BOX);
    if (handle === 'attack') write('attack', next.attackMs);
    if (handle === 'decay') {
      write('decay', next.decayMs);
      write('sustain', next.sustain);
    }
    if (handle === 'sustain') write('sustain', next.sustain);
    if (handle === 'release') write('release', next.releaseMs);
  }

  return (
    <div class="wv-synth" style={{ color: props.color }} onPointerLeave={() => setHot(null)}>
      <section
        class="wv-synth__pane wv-synth__pane--stack"
        classList={{ 'is-hot': hot() === 'mix' }}
        onPointerEnter={() => setHot('mix')}
      >
        <div class="wv-synth__head">
          <span>STACK</span>
          <strong>OSC 1 + 2 + 3</strong>
        </div>
        <div
          class="wv-synth__stage wv-synth__stage--signal"
          role="img"
          aria-label="Three oscillators stacked on one cycle. Click to preview."
          title="Click to preview"
          onPointerDown={(event) => {
            if (event.button !== 0) return;
            props.onPreview();
          }}
        >
          <svg class="wv-synth__svg" viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
            <path class="wv-synth__grid" d="M0 50H100 M25 0V100 M50 0V100 M75 0V100" />
            <For each={stacked().layers}>
              {(layer) => (
                <path
                  class={`wv-synth__trace wv-synth__trace--osc${layer.id}`}
                  d={oscillatorPath(layer.samples, 100, 100)}
                />
              )}
            </For>
            <path class="wv-synth__wave" d={oscillatorPath(stacked().mix, 100, 100)} />
            <Show when={liveScope()}>
              {(scope) => <path class="wv-synth__trace wv-synth__trace--live" d={signalPath(scope(), 100, 100)} />}
            </Show>
          </svg>
        </div>
      </section>

      <div class="wv-synth__display">
        <For each={OSCS}>
          {(osc, index) => {
            const pane = (): Pane => `osc${index()}` as Pane;
            const waveDef = () => defOf(osc.wave);
            const knobs = () =>
              [osc.coarse, osc.fine, osc.mix].flatMap((id) => {
                if (!id) return [];
                const found = defOf(id);
                return found ? [found] : [];
              });
            const slot = () => slots()[index()]!;
            return (
              <section
                class="wv-synth__pane"
                classList={{ 'is-hot': hot() === pane(), 'is-muted': index() > 0 && slot().mix < 0.02 }}
                onPointerEnter={() => setHot(pane())}
              >
                <div class="wv-synth__head">
                  <span>{osc.title}</span>
                  <strong>{index() === 0 ? 'BED' : 'MIX IN'}</strong>
                </div>
                <div class="wv-synth__stage wv-synth__stage--mini" role="img" aria-label={`${osc.title} waveform`}>
                  <svg class="wv-synth__svg" viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
                    <path class="wv-synth__grid" d="M0 50H100 M50 0V100" />
                    <path
                      class={`wv-synth__trace wv-synth__trace--osc${index()}`}
                      d={oscillatorPath(
                        Array.from({ length: 64 }, (_, sample) => oscSample(slot().wave, sample / 64)),
                        100,
                        100,
                      )}
                    />
                  </svg>
                </div>
                <div class="wv-synth__knobs">
                  <Show when={waveDef()}>
                    {(def) => (
                      <div class="wv-synth__switch">
                        <Segmented
                          def={def()}
                          typeId="poly-synth"
                          value={props.paramValue(def())}
                          labels={WAVE_LABELS}
                          onChange={(value) => {
                            setHot(pane());
                            write(osc.wave, value);
                          }}
                        />
                      </div>
                    )}
                  </Show>
                  <For each={knobs()}>
                    {(def) => (
                      <Knob
                        def={def}
                        typeId="poly-synth"
                        value={() => props.paramValue(def)}
                        onInput={(value) => {
                          setHot(pane());
                          props.setParam(def, value);
                        }}
                        owner={props.owner}
                        size={32}
                        color={props.color}
                      />
                    )}
                  </For>
                </div>
              </section>
            );
          }}
        </For>
      </div>

      <div class="wv-synth__display wv-synth__display--tail">
        <section
          class="wv-synth__pane"
          classList={{ 'is-hot': hot() === 'filt' }}
          onPointerEnter={() => setHot('filt')}
        >
          <div class="wv-synth__head">
            <span>FILTER</span>
            <strong>LOWPASS</strong>
          </div>
          <div
            class="wv-synth__stage wv-synth__stage--live"
            role="img"
            aria-label="Lowpass frequency response. Drag horizontally for cutoff, vertically for resonance."
            onPointerDown={(event) => onFilterPointer(event, event.currentTarget)}
          >
            <svg class="wv-synth__svg" viewBox={`0 0 ${VIEW_W} ${VIEW_H}`} preserveAspectRatio="none" aria-hidden="true">
              <For each={DECADES}>
                {(decade) => (
                  <path class="wv-synth__grid" d={`M${eqX(decade.hz, VIEW_W).toFixed(1)} 0 V${VIEW_H}`} />
                )}
              </For>
              <path class="wv-synth__zero" d={`M0 ${VIEW_H / 2} H${VIEW_W}`} />
              <Show when={sweepPath()}>{(d) => <path class="wv-synth__sweep" d={d()} />}</Show>
              <Show when={envPath()}>{(d) => <path class="wv-synth__ghost" d={d()} />}</Show>
              <path class="wv-synth__area" d={restArea()} />
              <path class="wv-synth__wave" d={restPath()} />
            </svg>
            <For each={DECADES}>
              {(decade) => (
                <span class="wv-synth__hz" style={{ left: `${eqX(decade.hz, 100)}%` }}>
                  {decade.label}
                </span>
              )}
            </For>
            <span class="wv-synth__dot" style={{ left: `${handleX()}%`, top: `${handleY()}%` }} />
          </div>
          <div class="wv-synth__knobs">
            <For each={filtKnobs()}>
              {(def) => (
                <Knob
                  def={def}
                  typeId="poly-synth"
                  value={() => props.paramValue(def)}
                  onInput={(value) => {
                    setHot('filt');
                    props.setParam(def, value);
                  }}
                  owner={props.owner}
                  size={32}
                  color={props.color}
                />
              )}
            </For>
          </div>
        </section>

        <section
          class="wv-synth__pane"
          classList={{ 'is-hot': hot() === 'env' }}
          onPointerEnter={() => setHot('env')}
        >
          <div class="wv-synth__head">
            <span>ENVELOPE</span>
            <strong>AMPLITUDE</strong>
          </div>
          <div
            class="wv-synth__stage wv-synth__stage--live"
            role="img"
            aria-label="Amplitude envelope. Drag a point to set attack, decay, sustain or release."
            onPointerDown={(event) => onEnvelopePointer(event, event.currentTarget)}
          >
            <svg class="wv-synth__svg" viewBox={`0 0 ${ADSR_BOX} ${ADSR_BOX}`} preserveAspectRatio="none" aria-hidden="true">
              <path class="wv-synth__grid" d="M0 50H100 M25 0V100 M50 0V100 M75 0V100" />
              <path class="wv-synth__noteoff" d={`M${ads().noteOffX.toFixed(2)} 8 V92`} />
              <path class="wv-synth__area" d={`${ads().path} Z`} />
              <path class="wv-synth__wave" d={ads().path} />
            </svg>
            <For each={(['attack', 'decay', 'sustain', 'release'] as const)}>
              {(handle) => (
                <span
                  class={`wv-synth__dot wv-synth__dot--${handle}`}
                  style={{
                    left: `${ads().points[handle].x}%`,
                    top: `${ads().points[handle].y}%`,
                  }}
                >
                  {handle[0]!.toUpperCase()}
                </span>
              )}
            </For>
          </div>
          <div class="wv-synth__knobs">
            <For each={envKnobs()}>
              {(def) => (
                <Knob
                  def={def}
                  typeId="poly-synth"
                  value={() => props.paramValue(def)}
                  onInput={(value) => {
                    setHot('env');
                    props.setParam(def, value);
                  }}
                  owner={props.owner}
                  size={32}
                  color={props.color}
                />
              )}
            </For>
          </div>
        </section>
      </div>
    </div>
  );
}

function bandPath(
  rest: readonly number[],
  peak: readonly number[],
  width: number,
  height: number,
  rangeDb: number,
): string {
  if (rest.length < 2 || rest.length !== peak.length) return '';
  const step = width / (rest.length - 1);
  const yAt = (db: number) => eqY(db, height, rangeDb);
  const up = rest.map(
    (db, index) => `${index === 0 ? 'M' : 'L'}${(index * step).toFixed(2)} ${yAt(db).toFixed(2)}`,
  );
  const down = [];
  for (let index = peak.length - 1; index >= 0; index -= 1) {
    down.push(`L${(index * step).toFixed(2)} ${yAt(peak[index]!).toFixed(2)}`);
  }
  return `${up.join(' ')} ${down.join(' ')} Z`;
}
