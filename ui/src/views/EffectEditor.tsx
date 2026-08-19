/**
 * Built-in effect editors as floating plugin windows.
 *
 * Hosted CLAP/VST3 plugins already open a native HWND. These are that window
 * for the five internal effects: drag, resize, close, and a live analyser in
 * the display. The inspector stays mounted behind so readouts keep polling.
 */

import { For, Match, Show, Switch, createEffect, createMemo, createSignal, on, onCleanup, type JSX } from 'solid-js';

import type { EffectInfoDto, InsertDto, InsertTarget, SpectrumDto } from '../api/types';
import { editorLayout } from '../lib/effectLayout';
import { useView, type InsertEditorRef } from '../shell/view';
import { insertTargetFor, useAppState } from '../state';
import { trackColor } from '../theme/tokens';
import { PluginWindow } from '../ui/PluginWindow';
import { createInsertKnobs } from '../ui/insertKnob';
import { CompEditor } from './fx/CompEditor';
import { DelayEditor } from './fx/DelayEditor';
import { EqEditor } from './fx/EqEditor';
import { LimiterEditor } from './fx/LimiterEditor';
import { ReverbEditor } from './fx/ReverbEditor';
import { SatEditor } from './fx/SatEditor';
import { UtilityEditor } from './fx/UtilityEditor';
import { Advanced, PrimaryRow } from './fx/chrome';

import '../styles/effect.css';

const WINDOW_SIZE: Record<string, { w: number; h: number }> = {
  eq: { w: 860, h: 560 },
  compressor: { w: 620, h: 460 },
  saturator: { w: 560, h: 440 },
  delay: { w: 640, h: 460 },
  limiter: { w: 560, h: 420 },
  reverb: { w: 620, h: 460 },
  utility: { w: 520, h: 480 },
  generic: { w: 520, h: 400 },
};

export function EffectEditors(): JSX.Element {
  const app = useAppState();
  const view = useView();
  const [spectrum, setSpectrum] = createSignal<SpectrumDto | null>(null);

  // The tap's identity as a string, so the effect below only reruns when the
  // *target* changes. `app.mixer()` is a fresh object on every command echo;
  // keyed on it, each refetch would tear down the 40 ms poll and re-arm the
  // engine tap (watchSpectrum null + target) for nothing.
  const watchKey = createMemo((): string | null => {
    const focused = view.insertEditors().find((ref) => ref.insertId === view.focusedInsertId());
    if (!focused) return null;
    const mixer = app.mixer();
    const chain =
      focused.strip.kind === 'track'
        ? mixer?.tracks[focused.strip.index]?.inserts
        : focused.strip.kind === 'bus'
          ? mixer?.buses[focused.strip.index]?.inserts
          : mixer?.master.inserts;
    const insert = chain?.find((entry) => entry.id === focused.insertId);
    if (!insert) return null;
    const target = insertTargetFor(focused.strip);
    return `${target.kind}:${target.index}:${insert.index}`;
  });

  createEffect(
    on(watchKey, (key) => {
      if (key === null) {
        void app.backend.watchSpectrum(null, null).catch(() => {});
        setSpectrum(null);
        return;
      }
      const [kind, stripIndex, insertIndex] = key.split(':') as [InsertTarget['kind'], string, string];
      void app.backend
        .watchSpectrum({ kind, index: Number(stripIndex) }, Number(insertIndex))
        .catch(() => {});
      const timer = setInterval(() => {
        void app.backend
          .getInsertSpectrum()
          .then((frame) => setSpectrum(frame))
          .catch(() => {});
      }, 40);
      onCleanup(() => {
        clearInterval(timer);
        void app.backend.watchSpectrum(null, null).catch(() => {});
      });
    }),
  );

  return (
    <For each={view.insertEditors()}>
      {(ref, index) => (
        <EffectEditor
          editor={ref}
          offset={index()}
          spectrum={view.focusedInsertId() === ref.insertId ? spectrum() : null}
        />
      )}
    </For>
  );
}

function EffectEditor(props: {
  editor: InsertEditorRef;
  offset: number;
  spectrum: SpectrumDto | null;
}): JSX.Element {
  const app = useAppState();
  const view = useView();

  const strip = createMemo(() => {
    const mixer = app.mixer();
    if (!mixer) return undefined;
    const ref = props.editor.strip;
    if (ref.kind === 'track') {
      const track = mixer.tracks[ref.index];
      return track && { name: track.name, color: trackColor(track.id), chain: track.inserts };
    }
    if (ref.kind === 'bus') {
      const bus = mixer.buses[ref.index];
      return bus && { name: bus.name, color: bus.color ?? trackColor(bus.id), chain: bus.inserts };
    }
    return { name: 'Master', color: 'var(--txt2)', chain: mixer.master.inserts };
  });

  const insert = createMemo<InsertDto | undefined>(() =>
    strip()?.chain.find((entry) => entry.id === props.editor.insertId),
  );

  createEffect(() => {
    if (app.mixer() && !insert()) view.closeInsertEditor(props.editor.insertId);
  });

  const info = (): EffectInfoDto | undefined =>
    app.effects().find((entry) => entry.typeId === insert()?.typeId);

  const owner = () => `${strip()?.name ?? ''} · ${insert()?.name ?? ''}`;
  const knobs = createInsertKnobs({
    strip: () => props.editor.strip,
    owner,
    insert: () => insert()!,
  });

  const values = createMemo(() => {
    const map = new Map<string, number>();
    for (const def of info()?.params ?? []) map.set(def.id, knobs.valueOf(def));
    return map;
  });

  const layout = createMemo(() => editorLayout(info()));
  const live = () => app.readouts().find((entry) => entry.id === insert()?.id)?.readouts ?? [];
  const target = () => insertTargetFor(props.editor.strip);
  const index = () => insert()?.index ?? 0;
  const first = () => index() === 0;
  const last = () => index() === (strip()?.chain.length ?? 1) - 1;
  const move = (to: number) =>
    void app.mixerCommand((backend) => backend.moveInsert(target(), index(), to));
  const sampleRate = () => {
    const fromSequencer = app.sequencer()?.sampleRate ?? 0;
    return fromSequencer > 0 ? fromSequencer : (app.streamInfo()?.sampleRate ?? 48_000);
  };
  const focused = () => view.focusedInsertId() === props.editor.insertId;
  const windowSize = () => WINDOW_SIZE[layout().kind] ?? WINDOW_SIZE.generic!;

  function onListen(bandId: string | null): void {
    const current = insert();
    if (!current) return;
    void app.backend.setInsertListen(target(), current.index, bandId).catch(app.report);
  }

  return (
    <Show when={insert()}>
      {(current) => (
        <PluginWindow
          title={`${current().name} — ${strip()?.name ?? ''}`}
          color={strip()?.color}
          z={focused() ? 90 : 40 + props.offset}
          focused={focused()}
          defaultWidth={windowSize().w}
          defaultHeight={windowSize().h}
          offset={props.offset}
          onFocus={() => view.focusInsertEditor(props.editor.insertId)}
          onClose={() => view.closeInsertEditor(props.editor.insertId)}
        >
          <div class="wv-fx">
            <div class="wv-plug__tools">
              <button
                type="button"
                class="wv-chip"
                classList={{ 'is-active': !current().bypassed }}
                title={current().bypassed ? 'Bypassed — switch it in' : 'Active — bypass it'}
                aria-pressed={!current().bypassed}
                onClick={() =>
                  void app.mixerCommand((backend) =>
                    backend.setInsertBypass(target(), index(), !current().bypassed),
                  )
                }
              >
                <span class="wv-dot" classList={{ 'is-off': current().bypassed }} />
                {current().bypassed ? 'Bypassed' : 'In'}
              </button>
              <button
                type="button"
                class="wv-chip wv-can-disable"
                disabled={first()}
                title={first() ? 'Already first in the chain' : 'Move earlier in the chain'}
                onClick={() => move(index() - 1)}
              >
                ↑
              </button>
              <button
                type="button"
                class="wv-chip wv-can-disable"
                disabled={last()}
                title={last() ? 'Already last in the chain' : 'Move later in the chain'}
                onClick={() => move(index() + 1)}
              >
                ↓
              </button>
              <button
                type="button"
                class="wv-chip"
                title={`Remove ${current().name} from this chain`}
                onClick={() =>
                  void app.mixerCommand((backend) => backend.removeInsert(target(), index()))
                }
              >
                Remove
              </button>
            </div>

            <div class="wv-fx__sheet">
              <Switch
                fallback={
                  <>
                    <PrimaryRow params={layout().primary} knobs={knobs} />
                    <Advanced params={layout().advanced} knobs={knobs} />
                  </>
                }
              >
                <Match when={layout().kind === 'eq' && info()}>
                  {(catalogue) => (
                    <EqEditor
                      info={catalogue()}
                      knobs={knobs}
                      values={values()}
                      primary={layout().primary}
                      advanced={layout().advanced}
                      muted={current().bypassed}
                      spectrum={props.spectrum?.binsPost ?? null}
                      onListen={onListen}
                      onAnnounce={(name, value) => app.say(`${owner()} · ${name}`, value)}
                    />
                  )}
                </Match>
                <Match when={layout().kind === 'compressor' && info()}>
                  {(catalogue) => (
                    <CompEditor
                      info={catalogue()}
                      knobs={knobs}
                      values={values()}
                      live={live()}
                      primary={layout().primary}
                      advanced={layout().advanced}
                      muted={current().bypassed}
                      spectrum={props.spectrum?.binsPost ?? null}
                    />
                  )}
                </Match>
                <Match when={layout().kind === 'saturator' && info()}>
                  {(catalogue) => (
                    <SatEditor
                      info={catalogue()}
                      knobs={knobs}
                      values={values()}
                      live={live()}
                      primary={layout().primary}
                      advanced={layout().advanced}
                      muted={current().bypassed}
                      spectrum={props.spectrum?.binsPost ?? null}
                    />
                  )}
                </Match>
                <Match when={layout().kind === 'delay' && info()}>
                  {(catalogue) => (
                    <DelayEditor
                      info={catalogue()}
                      knobs={knobs}
                      values={values()}
                      primary={layout().primary}
                      advanced={layout().advanced}
                      muted={current().bypassed}
                      bpm={app.sequencer()?.bpm ?? 120}
                      sampleRate={sampleRate()}
                      spectrum={props.spectrum?.binsPost ?? null}
                    />
                  )}
                </Match>
                <Match when={layout().kind === 'limiter' && info()}>
                  {(catalogue) => (
                    <LimiterEditor
                      info={catalogue()}
                      knobs={knobs}
                      values={values()}
                      live={live()}
                      primary={layout().primary}
                      advanced={layout().advanced}
                      muted={current().bypassed}
                      spectrum={props.spectrum?.binsPost ?? null}
                    />
                  )}
                </Match>
                <Match when={layout().kind === 'reverb' && info()}>
                  {(catalogue) => (
                    <ReverbEditor
                      info={catalogue()}
                      knobs={knobs}
                      values={values()}
                      primary={layout().primary}
                      advanced={layout().advanced}
                      muted={current().bypassed}
                      spectrum={props.spectrum?.binsPost ?? null}
                    />
                  )}
                </Match>
                <Match when={layout().kind === 'utility' && info()}>
                  {(catalogue) => (
                    <UtilityEditor
                      info={catalogue()}
                      knobs={knobs}
                      values={values()}
                      live={live()}
                      primary={layout().primary}
                      advanced={layout().advanced}
                      muted={current().bypassed}
                      scopeL={props.spectrum?.scopeL ?? []}
                      scopeR={props.spectrum?.scopeR ?? []}
                    />
                  )}
                </Match>
              </Switch>
            </div>
          </div>
        </PluginWindow>
      )}
    </Show>
  );
}
