/**
 * One effect, whole.
 *
 * The inspector's card is a preview -- four controls out of twenty, or a curve
 * instead of them -- and this is where the other sixteen live. It is a
 * fullscreen layer over the three main views, built the way the piano roll is:
 * a `wv-viewbar` with a back button that names where it came from, Escape to
 * leave, and the status bar left visible underneath so a drag still reports
 * itself.
 *
 * Two things it does *not* copy from the roll. It sits inside `.wv-body` rather
 * than replacing it, which keeps the inspector mounted behind it -- and the
 * inspector is what polls `get_chain_readouts`, so a compressor's gain reduction
 * keeps moving in the editor without a second poller and without the two of them
 * fighting over one interval. And it addresses its insert by **id**, because the
 * header can reorder the chain out from under itself.
 */

import { For, Show, createEffect, createMemo, onCleanup, type JSX } from 'solid-js';

import type { EffectInfoDto, InsertDto } from '../api/types';
import { bandGainRangeDb, compactBody, paramGroups } from '../lib/effectLayout';
import { useView } from '../shell/view';
import { insertTargetFor, useAppState, type StripRef } from '../state';
import { trackColor } from '../theme/tokens';
import { EqCurve } from '../ui/EqCurve';
import { IconBack, IconClose } from '../ui/icons';
import { createInsertKnobs } from '../ui/insertKnob';
import { ReadoutBars } from '../ui/ReadoutBars';

import '../styles/effect.css';

/** The strip an editor with nothing selected would point at. Never rendered. */
const NO_STRIP: StripRef = { kind: 'master' };

/** How big a knob is when there is a whole screen for it. */
const EDITOR_KNOB_PX = 44;

/** How tall the EQ's curve is at the top of its editor. */
const EDITOR_CURVE_PX = 208;

export function EffectEditor(): JSX.Element {
  const app = useAppState();
  const view = useView();

  /** The strip being edited, resolved through the mixer each time it changes. */
  const strip = createMemo(() => {
    const ref = view.insertEditor();
    const mixer = app.mixer();
    if (!ref || !mixer) return undefined;
    if (ref.strip.kind === 'track') {
      const track = mixer.tracks[ref.strip.index];
      return track && { name: track.name, color: trackColor(track.id), chain: track.inserts };
    }
    if (ref.strip.kind === 'bus') {
      const bus = mixer.buses[ref.strip.index];
      return bus && { name: bus.name, color: bus.color ?? trackColor(bus.id), chain: bus.inserts };
    }
    return { name: 'Master', color: 'var(--txt2)', chain: mixer.master.inserts };
  });

  const insert = createMemo<InsertDto | undefined>(() =>
    strip()?.chain.find((entry) => entry.id === view.insertEditor()?.insertId),
  );

  /**
   * An insert that has gone takes its editor with it.
   *
   * Reachable from the editor's own remove button, and from anywhere else that
   * edits the chain. Leaving a header naming an effect that is no longer in the
   * signal path would be the most direct kind of lie this screen could tell.
   */
  createEffect(() => {
    if (app.mixer() && view.insertEditor() && !insert()) view.closeInsertEditor();
  });

  const info = (): EffectInfoDto | undefined =>
    app.effects().find((entry) => entry.typeId === insert()?.typeId);

  const owner = () => `${strip()?.name ?? ''} · ${insert()?.name ?? ''}`;
  const knobs = createInsertKnobs({
    strip: () => view.insertEditor()?.strip ?? NO_STRIP,
    owner,
    insert: () => insert()!,
  });

  /** Every parameter's value by id, live while one is being dragged. */
  const values = createMemo(() => {
    const map = new Map<string, number>();
    for (const def of info()?.params ?? []) map.set(def.id, knobs.valueOf(def));
    return map;
  });

  const drawsCurve = () => compactBody(info()).kind === 'curve';
  const groups = createMemo(() => paramGroups(info()));
  const live = () =>
    app.readouts().find((entry) => entry.id === insert()?.id)?.readouts ?? [];

  const target = () => insertTargetFor(view.insertEditor()?.strip ?? NO_STRIP);
  const index = () => insert()?.index ?? 0;
  const first = () => index() === 0;
  const last = () => index() === (strip()?.chain.length ?? 1) - 1;
  const move = (to: number) =>
    void app.mixerCommand((backend) => backend.moveInsert(target(), index(), to));

  /**
   * Escape leaves the editor, and only the editor.
   *
   * There is never a roll underneath -- `openInsertEditor` closes it and
   * `openRoll` closes this -- so the roll's own listener is not mounted and
   * cannot also fire. The guard is the same one every other listener in the
   * shell uses: a key typed into a field belongs to the field.
   */
  function onKeyDown(event: KeyboardEvent): void {
    if (event.key !== 'Escape') return;
    const target_ = event.target as HTMLElement | null;
    if (target_ && (target_.tagName === 'INPUT' || target_.isContentEditable)) return;
    event.preventDefault();
    event.stopPropagation();
    view.closeInsertEditor();
  }

  window.addEventListener('keydown', onKeyDown);
  onCleanup(() => window.removeEventListener('keydown', onKeyDown));

  return (
    <Show when={insert()}>
      {(current) => (
        <div class="wv-fx">
          <div class="wv-viewbar">
            <button
              type="button"
              class="wv-chip"
              title="Back (Escape)"
              onClick={() => view.closeInsertEditor()}
            >
              <IconBack />
              Inspector
            </button>

            <span class="wv-swatch" style={{ background: strip()?.color }} />
            <span class="wv-fx__name" classList={{ 'is-off': current().bypassed }}>
              {current().name}
            </span>
            <span class="wv-fx__owner">on {strip()?.name}</span>
            <span class="wv-fx__desc">{info()?.description}</span>

            <span class="wv-spacer" />

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
              {current().bypassed ? 'Bypassed' : 'Active'}
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
              <IconClose />
              Remove
            </button>
          </div>

          <div class="wv-fx__body">
            <div class="wv-fx__sheet">
              <Show when={drawsCurve()}>
                <div class="wv-fx__curve">
                  <EqCurve
                    values={values()}
                    rangeDb={bandGainRangeDb(info())}
                    height={EDITOR_CURVE_PX}
                    detailed
                    muted={current().bypassed}
                  />
                </div>
              </Show>

              <For each={groups()}>
                {(group) => (
                  <section class="wv-fx__group">
                    <Show when={group.label !== ''}>
                      <span class="wv-fx__grouphead">{group.label}</span>
                    </Show>
                    <div class="wv-fx__grid">
                      <For each={group.params}>
                        {(def) => <knobs.Param def={def} size={EDITOR_KNOB_PX} />}
                      </For>
                    </div>
                  </section>
                )}
              </For>

              <Show when={(info()?.readouts.length ?? 0) > 0}>
                <section class="wv-fx__group">
                  <span class="wv-fx__grouphead">Meters</span>
                  <ReadoutBars defs={info()?.readouts ?? []} live={live()} large />
                </section>
              </Show>

              <Show when={groups().length === 0}>
                <span class="wv-fx__none">
                  This build&rsquo;s catalogue has no parameters for {current().typeId}.
                </span>
              </Show>
            </div>
          </div>
        </div>
      )}
    </Show>
  );
}
