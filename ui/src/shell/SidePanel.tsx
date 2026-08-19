/**
 * The right-hand panel: what the playlist paints with, and what is selected.
 *
 * Two halves that never overlap. The **picker** is a source of brushes -- a
 * pattern or a loaded audio file, dragged onto a lane or clicked to load the
 * brush. The **inspector** is one strip: its instrument, its insert chain, and
 * whatever meters that chain publishes.
 *
 * One deliberate departure from `.design/wave-studio.html`: the mock's inspector
 * draws four knobs for the instrument. **There are no per-instrument parameters
 * on the wire.** `SourceDto` carries a label, a description and a `takesNotes`
 * flag; the internal instruments expose nothing else, and a hosted plugin's
 * parameters live behind its own editor window rather than in a DTO. So the card
 * says so in a sentence instead of drawing four knobs that would move nothing.
 */

import {
  createEffect,
  createMemo,
  createSignal,
  For,
  Index,
  onCleanup,
  Show,
  type JSX,
} from 'solid-js';

import type {
  ClapPluginInfoDto,
  InsertDto,
  PatternDto,
  PatternSummaryDto,
  TrackDto,
  Vst3PluginInfoDto,
} from '../api/types';
import { formatValue, shortParamName } from '../lib/param';
import { insertTargetFor, useAppState, type StripRef } from '../state';
import { trackColor } from '../theme/tokens';
import { IconChevron, IconClose, IconPopOut } from '../ui/icons';
import { Knob } from '../ui/Knob';
import { formatDuration, formatRate } from './Browser';
import { useView, type PickerTab, type SideTab } from './view';

import '../styles/side.css';

/** As many marks as a 62 px preview can show before it is a smear. */
const MAX_MARKS = 96;

const SIDE_TABS: readonly { id: SideTab; label: string }[] = [
  { id: 'picker', label: 'Picker' },
  { id: 'inspector', label: 'Inspector' },
];

const PICKER_TABS: readonly { id: PickerTab; label: string }[] = [
  { id: 'patterns', label: 'Patterns' },
  { id: 'audio', label: 'Audio' },
];

export function SidePanel(): JSX.Element {
  const view = useView();
  return (
    <Show when={view.sideOpen()} fallback={<SideRail />}>
      <SideBody />
    </Show>
  );
}

function SideRail(): JSX.Element {
  const view = useView();
  return (
    <button
      type="button"
      class="wv-rail wv-rail--right"
      title="Show the picker and inspector"
      aria-label="Show the picker and inspector"
      onClick={() => view.setSideOpen(true)}
    >
      <IconChevron direction="left" />
      <span class="wv-rail__label">PICKER · INSPECTOR</span>
    </button>
  );
}

function SideBody(): JSX.Element {
  const view = useView();
  return (
    <aside class="wv-side">
      <div class="wv-side__head">
        <For each={SIDE_TABS}>
          {(tab) => (
            <button
              type="button"
              role="tab"
              class="wv-utab"
              classList={{ 'is-active': view.sideTab() === tab.id }}
              aria-selected={view.sideTab() === tab.id}
              onClick={() => view.setSideTab(tab.id)}
            >
              {tab.label}
            </button>
          )}
        </For>
        <span class="wv-spacer" />
        <button
          type="button"
          class="wv-side__collapse"
          title="Collapse the panel"
          aria-label="Collapse the panel"
          onClick={() => view.setSideOpen(false)}
        >
          <IconChevron direction="right" />
        </button>
      </div>

      <Show when={view.sideTab() === 'picker'}>
        <Picker />
      </Show>
      <Show when={view.sideTab() === 'inspector'}>
        <Inspector />
      </Show>
    </aside>
  );
}

// ---------------------------------------------------------------------------
// The picker
// ---------------------------------------------------------------------------

/** One bar of a pattern preview, in fractions of the box. */
interface Mark {
  left: number;
  width: number;
  bottom: number;
}

/**
 * The active pattern's content, as marks.
 *
 * Only ever called for the **active** pattern: `PatternSummaryDto` carries
 * counts and no content, so every other row would be a drawing of a pattern
 * rather than a picture of one.
 */
function previewMarks(pattern: PatternDto, lengthTicks: number): Mark[] {
  const marks: Mark[] = [];
  const total = Math.max(1, lengthTicks);
  const lanes = Math.max(1, pattern.channels.length);
  const keys = pattern.channels.flatMap((clip) => clip.notes.map((note) => note.key));
  const lowest = keys.length > 0 ? Math.min(...keys) : 0;
  const span = Math.max(1, (keys.length > 0 ? Math.max(...keys) : 0) - lowest);

  pattern.channels.forEach((clip, index) => {
    const lane = (lanes - index - 0.5) / lanes;
    const steps = clip.steps;
    if (steps) {
      const width = 1 / Math.max(1, pattern.steps);
      steps.steps.forEach((step, cell) => {
        if (!step.on) return;
        marks.push({ left: cell * width, width, bottom: lane });
      });
    }
    for (const note of clip.notes) {
      marks.push({
        left: note.start / total,
        width: Math.max(0.02, note.length / total),
        bottom: keys.length > 0 ? (note.key - lowest) / span : lane,
      });
    }
  });

  return marks.slice(0, MAX_MARKS);
}

function Picker(): JSX.Element {
  const view = useView();
  return (
    <div class="wv-picker">
      <div class="wv-picker__tabs wv-tabs" role="tablist" aria-label="What to paint with">
        <For each={PICKER_TABS}>
          {(tab) => (
            <button
              type="button"
              role="tab"
              class="wv-tab"
              classList={{ 'is-active': view.pickerTab() === tab.id }}
              aria-selected={view.pickerTab() === tab.id}
              onClick={() => view.setPickerTab(tab.id)}
            >
              {tab.label}
            </button>
          )}
        </For>
      </div>

      <div class="wv-picker__list">
        <Show when={view.pickerTab() === 'patterns'}>
          <PatternRows />
        </Show>
        <Show when={view.pickerTab() === 'audio'}>
          <AudioRows />
        </Show>
      </div>

      <div class="wv-picker__foot">
        Drag onto a playlist track — or click to load the brush, then click a lane.
      </div>
    </div>
  );
}

/** Load a brush, arm the paint tool, and go where painting happens. */
function useBrushClick(): (brush: { kind: 'pattern' | 'audio'; id: number }) => void {
  const view = useView();
  return (brush) => {
    view.setBrush(brush);
    view.setPlaylistTool('paint');
    // The rack cannot paint, so a click there means "take me to the playlist".
    if (view.view() === 'rack') view.go('playlist');
  };
}

function PatternRows(): JSX.Element {
  const app = useAppState();
  const view = useView();
  const pick = useBrushClick();

  const brushId = () => {
    const brush = view.brush();
    return brush?.kind === 'pattern' ? brush.id : null;
  };

  const activeId = () => app.sequencer()?.patternSet.activeId ?? -1;
  const maxPatterns = () => app.sequencer()?.patternSet.maxPatterns ?? 0;
  const full = () => maxPatterns() > 0 && app.patterns().length >= maxPatterns();

  const marks = createMemo<Mark[]>(() => {
    const pattern = app.pattern();
    if (!pattern) return [];
    const summary = app.patterns().find((row) => row.id === pattern.id);
    return previewMarks(pattern, summary?.lengthTicks ?? pattern.steps * 240);
  });

  const meta = (pattern: PatternSummaryDto) =>
    `${pattern.hitCount + pattern.noteCount} events · ${pattern.steps} steps`;

  return (
    <>
      <Show
        when={app.patterns().length > 0}
        fallback={
          <div class="wv-empty">
            No patterns.
            <span class="wv-empty__hint">add one below</span>
          </div>
        }
      >
        <Index each={app.patterns()}>
          {(pattern) => {
            const color = () => pattern().color ?? trackColor(pattern().id);
            const isActive = () => pattern().id === activeId();
            return (
              <div
                class="wv-pick"
                classList={{ 'is-active': brushId() === pattern().id }}
                role="button"
                tabindex={0}
                draggable={true}
                title={
                  isActive()
                    ? `${pattern().name} — the pattern being edited, so its content is drawn.`
                    : `${pattern().name} — only the pattern being edited has its content on the wire, so this preview is a plain bar.`
                }
                onClick={() => pick({ kind: 'pattern', id: pattern().id })}
                onKeyDown={(event) => {
                  if (event.key !== 'Enter' && event.key !== ' ') return;
                  event.preventDefault();
                  pick({ kind: 'pattern', id: pattern().id });
                }}
                onDragStart={(event) => {
                  event.dataTransfer?.setData('text/plain', `pattern::${pattern().id}`);
                  if (event.dataTransfer) event.dataTransfer.effectAllowed = 'copy';
                  view.setBrush({ kind: 'pattern', id: pattern().id });
                }}
              >
                <span class="wv-swatch" style={{ background: color() }} />
                <div class="wv-pick__text">
                  <span class="wv-pick__name">{pattern().name}</span>
                  <span class="wv-pick__meta">{meta(pattern())}</span>
                </div>
                <div class="wv-pick__art">
                  <Show
                    when={isActive()}
                    fallback={<span class="wv-pick__bar" style={{ background: color() }} />}
                  >
                    <Index each={marks()}>
                      {(mark) => (
                        <span
                          class="wv-pick__mark"
                          style={{
                            left: `${mark().left * 100}%`,
                            width: `${mark().width * 100}%`,
                            bottom: `${6 + mark().bottom * 76}%`,
                            background: color(),
                          }}
                        />
                      )}
                    </Index>
                  </Show>
                </div>
              </div>
            );
          }}
        </Index>
      </Show>

      <button
        type="button"
        class="wv-dashed wv-pick__add wv-can-disable"
        disabled={full()}
        title={full() ? `The engine holds ${maxPatterns()} patterns` : 'Add an empty pattern'}
        onClick={() => void app.sequencerCommand((backend) => backend.addPattern())}
      >
        + New pattern
      </button>
    </>
  );
}

function AudioRows(): JSX.Element {
  const app = useAppState();
  const view = useView();
  const pick = useBrushClick();

  const brushId = () => {
    const brush = view.brush();
    return brush?.kind === 'audio' ? brush.id : null;
  };

  return (
    <Show
      when={app.audioSources().length > 0}
      fallback={
        <div class="wv-empty">
          No audio loaded.
          <span class="wv-empty__hint">load a file from the browser's Audio tab</span>
        </div>
      }
    >
      <Index each={app.audioSources()}>
        {(source) => {
          const color = () => (source().missing ? 'var(--rec)' : trackColor(source().id));
          return (
            <div
              class="wv-pick"
              classList={{ 'is-active': brushId() === source().id }}
              role="button"
              tabindex={0}
              draggable={true}
              title={`${source().path}${
                source().missing ? '\nThe file has moved: its clips stay and are silent.' : ''
              }\nThe waveform is drawn on the clip, where there is room for it.`}
              onClick={() => pick({ kind: 'audio', id: source().id })}
              onKeyDown={(event) => {
                if (event.key !== 'Enter' && event.key !== ' ') return;
                event.preventDefault();
                pick({ kind: 'audio', id: source().id });
              }}
              onDragStart={(event) => {
                event.dataTransfer?.setData('text/plain', `audio::${source().id}`);
                if (event.dataTransfer) event.dataTransfer.effectAllowed = 'copy';
                view.setBrush({ kind: 'audio', id: source().id });
              }}
            >
              <span class="wv-swatch" style={{ background: color() }} />
              <div class="wv-pick__text">
                <span class="wv-pick__name">{source().name}</span>
                <span class="wv-pick__meta">
                  {source().missing
                    ? 'file missing'
                    : `${formatDuration(source().durationSeconds)} · ${formatRate(source().sampleRate)}`}
                </span>
              </div>
              <div class="wv-pick__art">
                <span class="wv-pick__bar" style={{ background: color() }} />
              </div>
            </div>
          );
        }}
      </Index>
    </Show>
  );
}

// ---------------------------------------------------------------------------
// The inspector
// ---------------------------------------------------------------------------

/** How much of a meter bar a readout fills, by the unit the engine declared. */
function readoutFraction(unit: string, value: number): number {
  const size = Math.abs(value);
  switch (unit) {
    case 'dB':
      return Math.min(1, size / 24);
    case '%':
      return Math.min(1, size / 100);
    case 'samples':
      return Math.min(1, size / 512);
    default:
      return Math.min(1, size);
  }
}

function Inspector(): JSX.Element {
  const app = useAppState();
  const view = useView();

  const track = createMemo(() => {
    const strip = view.strip();
    return strip.kind === 'track' ? app.mixer()?.tracks[strip.index] : undefined;
  });
  const bus = createMemo(() => {
    const strip = view.strip();
    return strip.kind === 'bus' ? app.mixer()?.buses[strip.index] : undefined;
  });
  const master = createMemo(() =>
    view.strip().kind === 'master' ? app.mixer()?.master : undefined,
  );

  const chain = createMemo<InsertDto[]>(
    () => track()?.inserts ?? bus()?.inserts ?? master()?.inserts ?? [],
  );

  const name = () => track()?.name ?? bus()?.name ?? (master() ? 'Master' : 'nothing selected');
  const kind = () => (track() ? 'instrument' : bus() ? 'bus' : master() ? 'master' : '');
  const exists = () => track() !== undefined || bus() !== undefined || master() !== undefined;

  const color = () => {
    const current = track();
    if (current) return trackColor(current.id);
    const strip = bus();
    if (strip) return strip.color ?? trackColor(strip.id);
    return 'var(--txt2)';
  };

  /** What the pattern being edited puts on this channel. */
  const patternMeta = () => {
    const current = track();
    const pattern = app.pattern();
    if (!current || !pattern) return '';
    const clip = pattern.channels.find((entry) => entry.channel === current.index);
    if (clip && clip.notes.length > 0) return `${clip.notes.length} notes in ${pattern.name}`;
    if (clip?.steps?.steps.some((step) => step.on) === true) return 'step sequenced';
    return `silent in ${pattern.name}`;
  };

  const inserts = () => `${chain().length} insert${chain().length === 1 ? '' : 's'}`;

  const meta = () => {
    const current = track();
    if (current) return [current.sourceLabel, patternMeta(), inserts()].join(' · ');
    if (bus()) return `return bus · ${inserts()}`;
    if (master()) return `sum of every strip · ${inserts()}`;
    return 'select a strip in the mixer or the rack';
  };

  /**
   * Poll the chain's meters while this pane is up, and only then.
   *
   * `watchReadouts` is what makes the gain-reduction bar move; nothing polls
   * unless a chain that actually publishes meters is on screen, which is the
   * whole reason the engine splits readouts out of the parameter values.
   */
  createEffect(() => {
    const strip = view.strip();
    const catalogue = app.effects();
    const metered = chain().some((insert) => {
      const info = catalogue.find((entry) => entry.typeId === insert.typeId);
      return (info?.readouts.length ?? 0) > 0;
    });
    app.watchReadouts(metered ? insertTargetFor(strip) : null);
  });

  onCleanup(() => app.watchReadouts(null));

  const maxInserts = () => app.mixer()?.maxInserts ?? 0;
  const chainFull = () => maxInserts() > 0 && chain().length >= maxInserts();

  return (
    <div class="wv-insp">
      <div class="wv-insp__head">
        <div class="wv-insp__title">
          <span class="wv-swatch wv-insp__swatch" style={{ background: color() }} />
          <span class="wv-insp__name" title={name()}>
            {name()}
          </span>
          <span class="wv-insp__kind">{kind()}</span>
        </div>
        <span class="wv-insp__meta">{meta()}</span>
      </div>

      <Show
        when={exists()}
        fallback={
          <div class="wv-empty">
            Nothing selected.
            <span class="wv-empty__hint">pick a strip in the mixer</span>
          </div>
        }
      >
        <div class="wv-insp__body">
          <Show when={track()}>{(current) => <InstrumentCard track={current()} />}</Show>

          <Index each={chain()}>
            {(insert) => (
              <InsertCard strip={view.strip()} stripName={name()} insert={insert()} chain={chain()} />
            )}
          </Index>

          <div class="wv-insp__chips">
            <Index each={app.effects()}>
              {(effect) => (
                <button
                  type="button"
                  class="wv-dashed wv-can-disable"
                  disabled={chainFull()}
                  title={
                    chainFull()
                      ? `This chain holds ${maxInserts()} inserts`
                      : effect().description
                  }
                  onClick={() =>
                    void app.mixerCommand((backend) =>
                      backend.addInsert(insertTargetFor(view.strip()), effect().typeId),
                    )
                  }
                >
                  + {effect().name}
                </button>
              )}
            </Index>
          </div>

          <Show when={view.strip().kind === 'master'}>
            <HostedMasterEffect />
          </Show>
        </div>
      </Show>
    </div>
  );
}

/**
 * The one hosted effect slot the engine has.
 *
 * A scanned CLAP or VST3 *effect* can only go in one place -- the master
 * insert, after the internal chain -- because that is the only hosted effect
 * slot `app/src/state.rs` offers. It is here rather than on a chip row with the
 * internal effects because it does not behave like them: there is one of it, it
 * replaces whatever was there, and it comes out with its own button.
 */
function HostedMasterEffect(): JSX.Element {
  const app = useAppState();
  const [open, setOpen] = createSignal(false);
  const [clap, setClap] = createSignal<ClapPluginInfoDto[]>([]);
  const [vst3, setVst3] = createSignal<Vst3PluginInfoDto[]>([]);

  const master = () => app.mixer()?.master;
  const placed = () => master()?.clap ?? master()?.vst3 ?? null;

  function browse(): void {
    setOpen(true);
    void app.backend.getMasterEffects().then(setClap).catch(app.report);
    void app.backend.getVst3MasterEffects().then(setVst3).catch(app.report);
  }

  return (
    <div class="wv-card">
      <div class="wv-card__head">
        <span class="wv-card__tag">HOSTED EFFECT</span>
        <span class="wv-spacer" />
        <Show when={placed()}>
          <button
            type="button"
            class="wv-card__icon"
            title="Take the hosted effect out of the master insert"
            aria-label="Clear the hosted master effect"
            onClick={() => void app.mixerCommand((backend) => backend.clearMasterEffect())}
          >
            <IconClose />
          </button>
        </Show>
      </div>
      <div class="wv-insp__card-body">
        <Show
          when={placed()}
          fallback={
            <span class="wv-insp__line">
              The master insert can host one scanned CLAP or VST3 effect, after the internal chain.
            </span>
          }
        >
          {(slot) => (
            <div class="wv-insp__hosted">
              <span class="wv-insp__line">{slot().name}</span>
              <Show when={slot().hasEditor}>
                <button
                  type="button"
                  class="wv-chip"
                  title="Open this plugin's own window"
                  onClick={() => app.toggleGui({ slot: 'master' }, !slot().guiOpen)}
                >
                  {slot().guiOpen ? 'Close editor' : 'Open editor'}
                </button>
              </Show>
            </div>
          )}
        </Show>
        <Show
          when={open()}
          fallback={
            <button type="button" class="wv-dashed" onClick={browse}>
              {placed() ? 'Replace…' : '+ Hosted effect…'}
            </button>
          }
        >
          <div class="wv-insp__hostlist">
            <Index each={clap()}>
              {(plugin) => (
                <button
                  type="button"
                  class="wv-row"
                  title={`${plugin().vendor || 'unknown vendor'} — CLAP`}
                  onClick={() => {
                    setOpen(false);
                    void app.mixerCommand((backend) =>
                      backend.setMasterEffectClap(plugin().path, plugin().id),
                    );
                  }}
                >
                  <span class="wv-row__name">{plugin().name || plugin().id}</span>
                  <span class="wv-spacer" />
                  <span class="wv-row__meta">CLAP</span>
                </button>
              )}
            </Index>
            <Index each={vst3()}>
              {(plugin) => (
                <button
                  type="button"
                  class="wv-row"
                  title={`${plugin().vendor || 'unknown vendor'} — VST3`}
                  onClick={() => {
                    setOpen(false);
                    void app.mixerCommand((backend) =>
                      backend.setMasterEffectVst3(plugin().path, plugin().id),
                    );
                  }}
                >
                  <span class="wv-row__name">{plugin().name || plugin().id}</span>
                  <span class="wv-spacer" />
                  <span class="wv-row__meta">VST3</span>
                </button>
              )}
            </Index>
            <Show when={clap().length === 0 && vst3().length === 0}>
              <span class="wv-insp__line">No scanned effects. Open the browser's Plugins tab to scan.</span>
            </Show>
          </div>
        </Show>
      </div>
    </div>
  );
}

/**
 * What is at the head of the selected track.
 *
 * No knobs, and that is the honest answer: the engine publishes no parameters
 * for an instrument -- the internal ones have none to publish, and a hosted
 * plugin's are behind its own editor. The one control here is the button that
 * opens that editor, and it is drawn only when the slot says it has one.
 */
function InstrumentCard(props: { track: TrackDto }): JSX.Element {
  const app = useAppState();

  const slot = () => props.track.clap ?? props.track.vst3;
  const hasEditor = () => slot()?.hasEditor === true;
  const guiOpen = () => slot()?.guiOpen === true;
  const format = () => (props.track.clap ? 'CLAP' : props.track.vst3 ? 'VST3' : 'built-in');
  const description = () =>
    app.sources().find((source) => source.id === props.track.sourceId)?.description ?? '';

  return (
    <div class="wv-card">
      <div class="wv-card__head">
        <span class="wv-card__tag">INSTRUMENT</span>
        <span class="wv-spacer" />
        <span class="wv-card__tag">{format()}</span>
        <Show when={hasEditor()}>
          <button
            type="button"
            class="wv-card__icon wv-card__icon--accent"
            title={guiOpen() ? 'Close the plugin window' : 'Open the plugin window'}
            aria-label={guiOpen() ? 'Close the plugin window' : 'Open the plugin window'}
            onClick={() =>
              app.toggleGui({ slot: 'track', index: props.track.index }, !guiOpen())
            }
          >
            <IconPopOut />
          </button>
        </Show>
      </div>
      <div class="wv-insp__card-body">
        <span class="wv-insp__source">{props.track.sourceLabel}</span>
        <Show
          when={slot()}
          fallback={
            <span class="wv-insp__line">
              {description()}
              {description().length > 0 ? ' ' : ''}
              This instrument exposes no parameters to the host.
            </span>
          }
        >
          <span class="wv-insp__line">
            {hasEditor()
              ? 'This plugin exposes no parameters to the host — its controls live in its own window.'
              : 'This plugin exposes no parameters to the host, and draws no editor.'}
          </span>
        </Show>
        <Show when={props.track.missing}>
          {(missing) => (
            <span class="wv-insp__warn" title={`${missing().path}\n${missing().reason}`}>
              {missing().name} ({missing().format}) could not be loaded
            </span>
          )}
        </Show>
      </div>
    </div>
  );
}

/**
 * One insert: its switches at the head, its parameters in the grid, its meters
 * under them.
 *
 * The `bypass` parameter is deliberately not in the grid -- it is the dot in the
 * head, and a knob that does the same thing two rows down is two controls that
 * can disagree about what is switched on.
 */
function InsertCard(props: {
  strip: StripRef;
  stripName: string;
  insert: InsertDto;
  chain: InsertDto[];
}): JSX.Element {
  const app = useAppState();

  /**
   * The parameter being dragged right now, and where the pointer has put it.
   *
   * `set_insert_param` is the per-frame command and answers with a single
   * number; the whole `MixerState` -- which is what `props.insert.params` comes
   * from -- is only refetched when the gesture ends. Without this the knob
   * would not move under the pointer.
   */
  const [dragging, setDragging] = createSignal<{ id: string; value: number } | null>(null);

  const target = () => insertTargetFor(props.strip);
  const info = () => app.effects().find((entry) => entry.typeId === props.insert.typeId);
  const params = () => (info()?.params ?? []).filter((def) => def.id !== 'bypass');
  const readoutDefs = () => info()?.readouts ?? [];
  const live = () =>
    app.readouts().find((entry) => entry.id === props.insert.id)?.readouts ?? [];

  const first = () => props.insert.index === 0;
  const last = () => props.insert.index === props.chain.length - 1;

  const move = (to: number) =>
    void app.mixerCommand((backend) => backend.moveInsert(target(), props.insert.index, to));

  return (
    <div class="wv-card">
      <div class="wv-card__head">
        <button
          type="button"
          class="wv-dot"
          classList={{ 'is-off': props.insert.bypassed }}
          title={props.insert.bypassed ? 'Bypassed — switch it in' : 'Active — bypass it'}
          aria-label={props.insert.bypassed ? 'Switch this effect in' : 'Bypass this effect'}
          aria-pressed={!props.insert.bypassed}
          onClick={() =>
            void app.mixerCommand((backend) =>
              backend.setInsertBypass(target(), props.insert.index, !props.insert.bypassed),
            )
          }
        />
        <span class="wv-card__name" classList={{ 'is-off': props.insert.bypassed }}>
          {props.insert.name}
        </span>
        <span class="wv-spacer" />
        <button
          type="button"
          class="wv-card__icon wv-can-disable"
          disabled={first()}
          title={first() ? 'Already first in the chain' : 'Move earlier in the chain'}
          aria-label="Move earlier in the chain"
          onClick={() => move(props.insert.index - 1)}
        >
          ↑
        </button>
        <button
          type="button"
          class="wv-card__icon wv-can-disable"
          disabled={last()}
          title={last() ? 'Already last in the chain' : 'Move later in the chain'}
          aria-label="Move later in the chain"
          onClick={() => move(props.insert.index + 1)}
        >
          ↓
        </button>
        <button
          type="button"
          class="wv-card__icon"
          title={`Remove ${props.insert.name}`}
          aria-label={`Remove ${props.insert.name}`}
          onClick={() =>
            void app.mixerCommand((backend) =>
              backend.removeInsert(target(), props.insert.index),
            )
          }
        >
          <IconClose />
        </button>
      </div>

      <Show
        when={params().length > 0}
        fallback={
          <div class="wv-insp__card-body">
            <span class="wv-insp__line">
              This build's catalogue has no parameters for {props.insert.typeId}.
            </span>
          </div>
        }
      >
        <div class="wv-card__grid">
          <Index each={params()}>
            {(def) => (
              <Knob
                def={def()}
                typeId={props.insert.typeId}
                label={shortParamName(def().name)}
                owner={`${props.stripName} · ${props.insert.name}`}
                size={28}
                color={props.insert.bypassed ? 'var(--dim)' : 'var(--live)'}
                value={() =>
                  // While a drag is in flight the engine's copy is a frame or
                  // more behind -- `set_insert_param` answers with the stored
                  // value but the *whole mixer* is only refetched on commit, so
                  // reading `props.insert.params` here would leave the pointer
                  // pinned where the drag started. The local value is dropped
                  // the moment the fresh mixer arrives.
                  dragging()?.id === def().id
                    ? dragging()!.value
                    : (props.insert.params.find((value) => value.id === def().id)?.value ??
                      def().defaultValue)
                }
                // Per frame, and one atomic store: the engine answers with what
                // it actually kept, which the next whole-mixer reply carries.
                onInput={(value) => {
                  setDragging({ id: def().id, value });
                  void app.backend
                    .setInsertParam(target(), props.insert.index, def().id, value)
                    .catch(app.report);
                }}
                onCommit={() => {
                  void app
                    .mixerCommand((backend) => backend.getTracks())
                    .finally(() => setDragging(null));
                }}
              />
            )}
          </Index>
        </div>
      </Show>

      <Show when={readoutDefs().length > 0}>
        <div class="wv-meters">
          <Index each={readoutDefs()}>
            {(def) => {
              const value = () =>
                live().find((entry) => entry.id === def().id)?.value ?? 0;
              return (
                <div class="wv-meterbar" title={`${def().name}, live from the engine`}>
                  <span class="wv-meterbar__name">{def().name}</span>
                  <span class="wv-meterbar__track">
                    <span
                      class="wv-meterbar__fill"
                      style={{ width: `${readoutFraction(def().unit, value()) * 100}%` }}
                    />
                  </span>
                  <span class="wv-meterbar__value">{formatValue(def().unit, value())}</span>
                </div>
              );
            }}
          </Index>
        </div>
      </Show>
    </div>
  );
}
