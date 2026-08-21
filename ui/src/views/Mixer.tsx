/**
 * The MIXER view.
 *
 * One strip per track, then one per bus, then the master pinned on the right.
 * Everything on a strip is wired to a real engine command; the two controls the
 * mock draws that the engine has no parameter for are **not drawn at all**
 * rather than drawn dead:
 *
 * - the mock's per-strip "stereo separation" slider. There is no such parameter
 *   on a strip -- `Utility` has a `width`, and that is an *insert*, not part of
 *   the channel. The space it occupied carries the strip's dB readout instead.
 * - the master's LUFS readout. `MeterFrame` carries peak and RMS; loudness is
 *   not measured anywhere in the engine, so the master shows the peak dBFS it
 *   actually has, held and decayed, rather than a number nobody computed.
 *
 * The one control that *is* drawn disabled is R (record arm), because a strip
 * without it looks like a strip with a missing button; it carries a `title`
 * saying the engine cannot record yet.
 *
 * Two rules run through the whole file:
 *
 * - **`Index`, never `For`.** Every mixer command answers with a whole fresh
 *   `MixerState`, so every DTO is a new object on every pointer move of a fader
 *   drag. A keyed `For` would tear down and rebuild every strip at that rate.
 * - **Meters never touch a signal.** They paint from the shared animation frame
 *   straight onto the DOM (`ui/Meter.tsx`, `ui/frameLoop.ts`), because a 30-60 Hz
 *   packet stream routed through Solid re-runs every dependent computation.
 */

import {
  createMemo,
  createSignal,
  Index,
  onCleanup,
  onMount,
  Show,
  type JSX,
} from 'solid-js';

import type {
  BusDto,
  InsertDto,
  InsertTarget,
  MasterDto,
  MixerState,
  SendDto,
  TrackDto,
} from '../api/types';
import { channelColor } from '../lib/channel';
import { mixerSlotOpensEditor, mixerStripDestination } from '../lib/navigate';
import { faderFraction, faderToDb, formatDb, panLabel } from '../panels/mixer/scale';
import { addedInsertId, insertsOnStrip } from '../lib/inserts';
import { useView } from '../shell/view';
import { insertTargetFor, useAppState, type StripRef } from '../state';
import { trackColor } from '../theme/tokens';
import {
  bindWheel,
  debounce,
  FADER_HANDLE_PX,
  faderThrowPx,
  FINE_FACTOR,
  startDrag,
  WHEEL_COMMIT_MS,
} from '../ui/drag';
import { onFrame } from '../ui/frameLoop';
import { Knob, spec } from '../ui/Knob';
import { Meter } from '../ui/Meter';

/** Pan is the same parameter on every strip, so its spec is built once. */
const PAN_DEF = spec('pan', 'Pan', '', -1, 1, 0);

/** How far the pointer may travel and still count as a click on a send cell. */
const SEND_CLICK_SLOP_PX = 3;

/**
 * How long after a release a second press is still the same double-click.
 *
 * The send cell's click is a *toggle*, and a toggle double-clicked twitches:
 * route, unroute, and the level and the PRE flag the send was carrying are gone
 * without the second click ever having been meant. So the cell swallows the
 * second press and a double-click does exactly what a click does. 500 ms is the
 * window Windows uses by default.
 *
 * Nothing in the DOM tells a `pointerup` that a `dblclick` is coming, so this
 * has to be measured rather than asked for.
 */
const DOUBLE_CLICK_MS = 500;

/**
 * Decibels per notch of the wheel on a fader, and with shift held.
 *
 * A fader is read in dB, so its notch is a round number of dB rather than a
 * fraction of the throw -- half a dB is the smallest move that is worth making
 * on purpose, and a tenth is the smallest anyone can hear on a single source.
 */
const FADER_WHEEL_DB = 0.5;

/**
 * Pan units per notch. Pan runs -1..1 and is read out in hundredths (`L42`), so
 * a notch is two of those: fifty notches from hard left to hard right.
 */
const PAN_WHEEL_STEP = 0.02;

/**
 * Pan units per notch with shift held.
 *
 * A hundredth rather than a fifth of a notch, because a hundredth is the whole
 * resolution the parameter has -- `setPan` rounds there and the readout counts
 * there. A truer fifth would round straight back to where it started, which is
 * a modifier that visibly does nothing.
 */
const PAN_FINE_STEP = 0.01;

/**
 * Send level per notch, in the **linear** level the engine stores rather than
 * the dB the knob shows.
 *
 * A send is a proportion of a signal, and 2% of it is the unit that proportion
 * comes in. It also lands the resolution where it is needed: fine near unity,
 * where a send is actually balanced, and coarse down in the -40s, which is a
 * region you pass through rather than work in.
 */
const SEND_WHEEL_LEVEL = 0.02;

/**
 * A fine notch on a send, in the dB it is read in rather than the level it is
 * stored in.
 *
 * A fifth of 2% of the signal is about a thirtieth of a decibel: real, sent to
 * the engine, and invisible on a readout with one decimal place -- which is a
 * modifier that looks broken. A tenth of a dB is the smallest move the readout
 * can show, so that is what fine means here.
 */
const SEND_FINE_DB = 0.1;

/**
 * One strip's worth of horizontal travel: the 90 px strip of `mixer.css` plus
 * the 5 px gap between them. Mirrored here rather than measured, because a
 * scroll step that changed with the DOM would be a scroll step nobody could
 * predict; if the strip width in `mixer.css` moves, this moves with it.
 */
const STRIP_PITCH_PX = 95;

/** Insert rows a strip draws before it collapses the rest into "+N more". */
const SLOT_ROWS = 4;

/** How long the master's peak readout holds its maximum, in frames. */
const PEAK_HOLD_FRAMES = 45;

// ---------------------------------------------------------------------------
// Routing arithmetic
// ---------------------------------------------------------------------------

/** Whether two references name the same strip. */
function sameStrip(a: StripRef, b: StripRef): boolean {
  if (a.kind !== b.kind) return false;
  if (a.kind === 'master' || b.kind === 'master') return true;
  return a.index === b.index;
}

/** What a strip is called, for a readout or a route sentence. */
function stripName(mix: MixerState, ref: StripRef): string {
  if (ref.kind === 'master') return 'Master';
  if (ref.kind === 'track') return mix.tracks[ref.index]?.name ?? `Track ${ref.index + 1}`;
  return mix.buses[ref.index]?.name ?? `Bus ${ref.index + 1}`;
}

/**
 * Whether bus `from` already reaches bus `target` through the send graph.
 *
 * The UI's copy of the engine's cycle rule (§4 of the routing spec): adding
 * `target -> from` would close a loop. The engine refuses it with the whole path
 * written out; this exists so the cell says "no path" *before* the click rather
 * than turning the status bar red after it.
 */
function busReaches(sends: readonly SendDto[], from: number, target: number): boolean {
  const seen = new Set<number>();
  const stack = [from];
  while (stack.length > 0) {
    const bus = stack.pop();
    if (bus === undefined) break;
    if (bus === target) return true;
    if (seen.has(bus)) continue;
    seen.add(bus);
    for (const send of sends) {
      if (send.sourceKind === 'bus' && send.sourceIndex === bus) stack.push(send.dest);
    }
  }
  return false;
}

/** The buses one strip sends to, by name. */
function sendTargets(mix: MixerState, ref: StripRef): string[] {
  if (ref.kind === 'master') return [];
  return mix.sends
    .filter((send) => send.sourceKind === ref.kind && send.sourceIndex === ref.index)
    .map((send) => mix.buses[send.dest]?.name ?? `Bus ${send.dest + 1}`);
}

/** The strips feeding one bus, by name. */
function sendSources(mix: MixerState, ref: StripRef): string[] {
  if (ref.kind !== 'bus') return [];
  return mix.sends
    .filter((send) => send.dest === ref.index)
    .map((send) =>
      stripName(
        mix,
        send.sourceKind === 'track'
          ? { kind: 'track', index: send.sourceIndex }
          : { kind: 'bus', index: send.sourceIndex },
      ),
    );
}

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

/** What the "+ effect" menu is offering to put where. */
interface FxMenu {
  target: InsertTarget;
  owner: string;
  x: number;
  y: number;
}

export function Mixer(): JSX.Element {
  const app = useAppState();
  const [menu, setMenu] = createSignal<FxMenu | null>(null);

  /**
   * The strip row scrolls itself sideways on the wheel.
   *
   * Chrome turns a vertical wheel into horizontal scrolling for a box that only
   * overflows sideways on macOS and not on Windows, so on the platform this is
   * built for a mixer wider than its window could only be reached by finding
   * the scrollbar. The row therefore does it itself, one strip per notch, which
   * also keeps the strips aligned rather than landing mid-fader.
   *
   * It only ever sees the *gaps*: every control on a strip stops its own wheel
   * event, and this declines the notch when there is nothing to scroll, so a
   * mixer that fits its window leaves the wheel exactly as it found it.
   *
   * Bound from the ref rather than from `onMount`, because the row lives inside
   * the `Show` waiting for the engine's first `MixerState` and does not exist
   * yet when the view mounts.
   */
  let unbindRow: (() => void) | undefined;
  const bindStripRow = (row: HTMLDivElement): void => {
    unbindRow?.();
    unbindRow = bindWheel(row, (notches) => {
      if (row.scrollWidth <= row.clientWidth) return false;
      row.scrollLeft -= notches * STRIP_PITCH_PX;
      return true;
    });
  };
  onCleanup(() => unbindRow?.());

  function openMenu(target: InsertTarget, owner: string, event: MouseEvent): void {
    setMenu({
      target,
      owner,
      x: Math.max(8, Math.min(event.clientX, window.innerWidth - 206)),
      y: Math.max(8, Math.min(event.clientY, window.innerHeight - 336)),
    });
  }

  return (
    <div class="wv-mixer">
      <MixerBar />
      <div class="wv-mixer__body">
        <Show
          when={app.mixer()}
          fallback={
            <div class="wv-empty">
              No mixer yet.
              <span class="wv-empty__hint">waiting for the engine's first answer</span>
            </div>
          }
        >
          {(state) => (
            <>
              <div class="wv-mixer__strips" ref={bindStripRow}>
                <Index each={state().tracks}>
                  {(track, i) => (
                    <Strip
                      source={{ kind: 'track', track: track() }}
                      num={String(i + 1)}
                      onPickEffect={openMenu}
                    />
                  )}
                </Index>
                <Index each={state().buses}>
                  {(bus, i) => (
                    <Strip
                      source={{ kind: 'bus', bus: bus() }}
                      num={`B${i + 1}`}
                      onPickEffect={openMenu}
                    />
                  )}
                </Index>
              </div>
              <div class="wv-mixer__pin">
                <Strip
                  source={{ kind: 'master', master: state().master }}
                  num="M"
                  onPickEffect={openMenu}
                />
              </div>
            </>
          )}
        </Show>
      </div>
      <Show when={menu()}>
        {(open) => <EffectMenu menu={open()} close={() => setMenu(null)} />}
      </Show>
    </div>
  );
}

// ---------------------------------------------------------------------------
// The header
// ---------------------------------------------------------------------------

/**
 * How a track's routing is worded, and why it is worded that way.
 *
 * The mock says "not routed — nothing is heard" for a strip with no sends. In
 * this engine that would be a lie: every track and every bus sums into the
 * master directly, always. A send is an *extra* tap, so the chip lists the
 * master first and the send destinations after it.
 */
const ROUTE_TITLE =
  'Every track and every bus reaches the master directly. A send is an extra tap ' +
  'off the strip, not its output — removing every send never silences anything.';

function MixerBar(): JSX.Element {
  const app = useAppState();
  const view = useView();

  const mix = () => app.mixer();
  const maxBuses = () => mix()?.maxBuses ?? 8;
  const full = () => (mix()?.buses.length ?? 0) >= maxBuses();

  const selName = () => {
    const state = mix();
    return state ? stripName(state, view.strip()) : '—';
  };

  const selColor = () => {
    const state = mix();
    const ref = view.strip();
    if (!state || ref.kind === 'master') return 'var(--txt2)';
    if (ref.kind === 'track') {
      const track = state.tracks[ref.index];
      return track ? channelColor(track) : 'var(--dim)';
    }
    const bus = state.buses[ref.index];
    return bus ? (bus.color ?? trackColor(bus.id)) : 'var(--dim)';
  };

  const route = () => {
    const state = mix();
    if (!state) return '';
    const ref = view.strip();
    if (ref.kind === 'master') return 'final output — every strip lands here';
    if (view.sendMode() === 'from') {
      return `out → ${[...sendTargets(state, ref), 'Master'].join(' + ')}`;
    }
    if (ref.kind !== 'bus') {
      return 'a track is not a send destination — only a bus can be sent into';
    }
    const feeders = sendSources(state, ref);
    return feeders.length > 0
      ? `in ← ${feeders.join(' + ')}`
      : 'nothing feeding it — only its own inserts';
  };

  const meta = () => {
    const state = mix();
    if (!state) return '';
    return (
      `${state.tracks.length} tracks · ${state.buses.length}/${state.maxBuses} buses · ` +
      `${state.sends.length} sends · click a send cell to route it, drag it for level`
    );
  };

  return (
    <div class="wv-viewbar">
      <span class="wv-viewbar__title">MIXER</span>

      <div class="wv-seg">
        <button
          type="button"
          class="wv-seg__btn wv-mixer__mode"
          classList={{ 'is-active': view.sendMode() === 'from' }}
          title="Each cell shows the send from the selected strip into that strip"
          onClick={() => view.setSendMode('from')}
        >
          SENDS FROM
        </button>
        <button
          type="button"
          class="wv-seg__btn wv-mixer__mode"
          classList={{ 'is-active': view.sendMode() === 'into' }}
          title="Each cell shows the send from that strip into the selected strip"
          onClick={() => view.setSendMode('into')}
        >
          SENDS INTO
        </button>
      </div>

      <button
        type="button"
        class="wv-dashed wv-can-disable"
        disabled={full()}
        title={
          full()
            ? `A project may have at most ${maxBuses()} buses`
            : 'Add a bus. Sends land on buses; a bus sums into the master.'
        }
        onClick={() => void app.mixerCommand((b) => b.addBus())}
      >
        + Bus
      </button>

      <div class="wv-chip wv-mixer__selchip" title={ROUTE_TITLE}>
        <span class="wv-swatch wv-mixer__selswatch" style={{ background: selColor() }} />
        <span class="wv-mixer__selname">{selName()}</span>
        <span class="wv-mixer__route">{route()}</span>
      </div>

      <span class="wv-viewbar__meta">{meta()}</span>
    </div>
  );
}

// ---------------------------------------------------------------------------
// One strip
// ---------------------------------------------------------------------------

/** Which of the three kinds of strip this is, with its DTO. */
type StripSource =
  | { kind: 'track'; track: TrackDto }
  | { kind: 'bus'; bus: BusDto }
  | { kind: 'master'; master: MasterDto };

/** One row of the insert list: an effect, the adder, a gap, or the overflow. */
interface SlotRow {
  kind: 'fx' | 'add' | 'blank' | 'more';
  fx?: InsertDto;
  label: string;
  title: string;
}

interface StripProps {
  source: StripSource;
  /** `1`, `B2`, `M`. */
  num: string;
  onPickEffect: (target: InsertTarget, owner: string, event: MouseEvent) => void;
}

function Strip(props: StripProps): JSX.Element {
  const app = useAppState();
  const view = useView();

  // --- what this strip is --------------------------------------------------

  const track = () => (props.source.kind === 'track' ? props.source.track : undefined);
  const bus = () => (props.source.kind === 'bus' ? props.source.bus : undefined);
  const master = () => (props.source.kind === 'master' ? props.source.master : undefined);
  const isMaster = () => props.source.kind === 'master';

  const stripRef = (): StripRef => {
    const asTrack = track();
    if (asTrack) return { kind: 'track', index: asTrack.index };
    const asBus = bus();
    if (asBus) return { kind: 'bus', index: asBus.index };
    return { kind: 'master' };
  };

  const name = () => track()?.name ?? bus()?.name ?? 'Master';
  const kindTag = () => (isMaster() ? 'OUT' : track() ? 'INS' : 'BUS');

  const gainDb = () =>
    isMaster()
      ? app.masterGainDb()
      : (localDb() ?? track()?.gainDb ?? bus()?.gainDb ?? 0);
  const pan = () => localPan() ?? track()?.pan ?? bus()?.pan;
  const muted = () => track()?.muted ?? bus()?.muted ?? false;
  const soloed = () => track()?.soloed ?? bus()?.soloed ?? false;
  const silenced = () => track()?.silenced ?? bus()?.silenced ?? false;
  const inserts = (): InsertDto[] =>
    track()?.inserts ?? bus()?.inserts ?? master()?.inserts ?? [];
  const missing = () => track()?.missing ?? master()?.missing ?? null;
  const selected = () => sameStrip(view.strip(), stripRef());
  const target = () => insertTargetFor(stripRef());

  /** The colour the engine has no opinion about. Buses may carry their own. */
  const color = () => {
    const asTrack = track();
    if (asTrack) return channelColor(asTrack);
    const asBus = bus();
    if (asBus) return asBus.color ?? trackColor(asBus.id);
    return 'var(--txt2)';
  };

  function select(): void {
    const ref = stripRef();
    view.setStrip(ref);
    // The keyboard piano and the source picker follow the *track* selection,
    // which is engine state rather than view state -- so a click on a track
    // strip has to move both or the two disagree about what is selected.
    const asTrack = track();
    if (asTrack) app.selectTrack(asTrack.index);
  }

  // --- mute, solo, arm -----------------------------------------------------

  function toggleMute(event: MouseEvent): void {
    event.stopPropagation();
    const asTrack = track();
    const asBus = bus();
    if (asTrack) {
      void app.mixerCommand((b) => b.setTrackMute(asTrack.index, !asTrack.muted));
      app.say(`${asTrack.name} · Mute`, asTrack.muted ? 'on' : 'muted');
    } else if (asBus) {
      void app.mixerCommand((b) => b.setBusMute(asBus.index, !asBus.muted));
      app.say(`${asBus.name} · Mute`, asBus.muted ? 'on' : 'muted');
    }
  }

  function toggleSolo(event: MouseEvent): void {
    event.stopPropagation();
    const asTrack = track();
    const asBus = bus();
    if (asTrack) {
      void app.mixerCommand((b) => b.setTrackSolo(asTrack.index, !asTrack.soloed));
      app.say(`${asTrack.name} · Solo`, asTrack.soloed ? 'off' : 'soloed');
    } else if (asBus) {
      void app.mixerCommand((b) => b.setBusSolo(asBus.index, !asBus.soloed));
      app.say(`${asBus.name} · Solo`, asBus.soloed ? 'off' : 'soloed');
    }
  }

  // --- level and pan -------------------------------------------------------
  //
  // Both go straight to the backend and draw from `local()` until a debounced
  // `get_tracks` reconciles them, rather than routing every step through
  // `mixerCommand` and reading the answer back.
  //
  // A gesture is a stream of small changes with an *end*, and the shell already
  // knows the value at every point along it. Waiting for a whole `MixerState`
  // per step means a control that only moves as fast as the round trip -- which
  // is the defect the inspector's knobs already had to be cured of -- and on
  // the wheel it means one refetch per click of a notched wheel. So the value
  // moves now, the engine is told now, and the authoritative answer is asked
  // for once the gesture goes quiet.

  /** What the strip draws while the engine's copy is still catching up. */
  const [localDb, setLocalDb] = createSignal<number | null>(null);
  const [localPan, setLocalPan] = createSignal<number | null>(null);

  const settle = debounce(WHEEL_COMMIT_MS, () => {
    void app.mixerCommand((b) => b.getTracks()).finally(() => {
      setLocalDb(null);
      setLocalPan(null);
    });
  });
  onCleanup(() => settle.cancel());

  function setGain(db: number): void {
    if (isMaster()) {
      app.setMasterGain(db);
      return;
    }
    setLocalDb(db);
    app.say(`${name()} · Volume`, `${formatDb(db)} dB`);
    const asTrack = track();
    const asBus = bus();
    if (asTrack) void app.backend.setTrackGain(asTrack.index, db).catch(app.report);
    else if (asBus) void app.backend.setBusGain(asBus.index, db).catch(app.report);
    settle();
  }

  function onFaderDown(event: PointerEvent): void {
    if (event.button !== 0) return;
    event.stopPropagation();
    startDrag(event, {
      // In *travel* rather than decibels, so `scale.ts` stays the one thing
      // that decides how the dB are spaced along the throw. Anchored on the
      // value the fader already has, never on the pixel that was clicked: a
      // fader that jumped to the pointer would be +6 dB into the monitors for
      // the sake of a click three pixels too high.
      from: faderFraction(gainDb()),
      // The strip is as tall as the window; measuring it keeps the handle
      // under the pointer instead of running ahead by (track / 180).
      perPixel: 1 / faderThrowPx(faderEl?.getBoundingClientRect().height ?? 0),
      min: 0,
      max: 1,
      onChange: (fraction) => setGain(faderToDb(fraction)),
    });
  }

  function onFaderWheel(notches: number, event: WheelEvent): void {
    const state = app.mixer();
    const floor = state?.minGainDb ?? -60;
    const ceiling = state?.maxGainDb ?? 6;
    const step = FADER_WHEEL_DB * (event.shiftKey ? FINE_FACTOR : 1);
    // Rounded to a tenth for the same reason `faderToDb` is: the readout has
    // one decimal, and a fader reporting -6.03 while showing -6.0 reads as a bug.
    const next = Math.round(Math.min(ceiling, Math.max(floor, gainDb() + notches * step)) * 10) / 10;
    if (next === gainDb()) return;
    setGain(next);
  }

  let faderEl: HTMLDivElement | undefined;
  let unbindFader: (() => void) | undefined;
  onMount(() => {
    if (faderEl) unbindFader = bindWheel(faderEl, onFaderWheel);
  });
  onCleanup(() => unbindFader?.());

  function setPan(value: number): void {
    const next = Math.round(value * 100) / 100;
    setLocalPan(next);
    const asTrack = track();
    const asBus = bus();
    if (asTrack) void app.backend.setTrackPan(asTrack.index, next).catch(app.report);
    else if (asBus) void app.backend.setBusPan(asBus.index, next).catch(app.report);
    settle();
  }

  // --- the meter -----------------------------------------------------------

  const peak = () => {
    const frame = app.meters.latest;
    if (!frame) return 0;
    const asTrack = track();
    const asBus = bus();
    const cell = asTrack
      ? frame.tracks[asTrack.index]
      : asBus
        ? frame.buses[asBus.index]
        : undefined;
    return cell ? Math.max(cell.peakL, cell.peakR) : 0;
  };

  // --- the insert list -----------------------------------------------------

  const slots = (): SlotRow[] => {
    const chain = inserts();
    const overflow = chain.length > SLOT_ROWS;
    const shown = overflow ? SLOT_ROWS - 1 : SLOT_ROWS;
    const rows: SlotRow[] = [];
    for (let i = 0; i < shown; i++) {
      const fx = chain[i];
      if (fx) {
        rows.push({
          kind: 'fx',
          fx,
          label: fx.name,
          title: `${fx.name} — drag to reorder the chain · click opens it in the inspector · double-click opens the editor · right-click removes it · the dot bypasses it`,
        });
      } else if (i === chain.length) {
        rows.push({ kind: 'add', label: '+ effect', title: 'Put an internal effect on this chain' });
      } else {
        rows.push({ kind: 'blank', label: '', title: `empty slot ${i + 1}` });
      }
    }
    if (overflow) {
      const extra = chain.length - shown;
      rows.push({
        kind: 'more',
        label: `+${extra} more`,
        title: `${extra} more effect${extra > 1 ? 's' : ''} on this chain — the inspector shows all of them`,
      });
    }
    return rows;
  };

  function onSlot(row: SlotRow, event: MouseEvent): void {
    event.stopPropagation();
    // The release that ended a reorder is also a click. It must not also open
    // the inspector on whatever the effect landed next to.
    if (dragged) {
      dragged = false;
      return;
    }
    if (row.kind === 'add') {
      props.onPickEffect(target(), name(), event);
      return;
    }
    select();
    if (row.kind === 'blank') return;
    view.setSideTab('inspector');
    view.setSideOpen(true);
  }

  // --- reordering the chain ------------------------------------------------
  //
  // The chain *is* the signal path: an EQ before a compressor and one after it
  // are different sounds, and the strip is where you look at that order. The
  // engine has had `move_insert` since the routing step and the inspector's
  // arrows have used it; the strip could only add and remove, so rearranging
  // meant deleting an effect and building it again from scratch.

  /** The slot being dragged, and the slot it would land in. */
  const [dragIndex, setDragIndex] = createSignal<number | null>(null);
  const [dropIndex, setDropIndex] = createSignal<number | null>(null);
  /** Whether the gesture that is ending was a drag rather than a click. */
  let dragged = false;

  function onSlotPointerDown(event: PointerEvent, row: SlotRow): void {
    const fx = row.fx;
    if (event.button !== 0 || row.kind !== 'fx' || !fx) return;
    const list = (event.currentTarget as HTMLElement).closest<HTMLElement>('.wv-mixer__slots');
    if (!list || inserts().length < 2) return;
    const startY = event.clientY;
    let moved = false;

    /** Which chain position the pointer is over, by each row's own box. */
    const indexAt = (clientY: number): number => {
      const last = inserts().length - 1;
      const rows = Array.from(list.querySelectorAll<HTMLElement>('.wv-mixer__slot'));
      for (let i = 0; i <= last && i < rows.length; i++) {
        const box = rows[i]!.getBoundingClientRect();
        if (clientY < box.top + box.height / 2) return i;
      }
      return last;
    };

    const onMove = (move: PointerEvent) => {
      if (!moved && Math.abs(move.clientY - startY) < 4) return;
      if (!moved) {
        moved = true;
        setDragIndex(fx.index);
        document.body.style.setProperty('cursor', 'grabbing');
      }
      setDropIndex(indexAt(move.clientY));
    };

    const onUp = (up: PointerEvent) => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      window.removeEventListener('pointercancel', onUp);
      document.body.style.removeProperty('cursor');
      setDragIndex(null);
      setDropIndex(null);
      if (!moved) return;
      dragged = true;
      const to = indexAt(up.clientY);
      if (to === fx.index) return;
      void app.mixerCommand((b) => b.moveInsert(target(), fx.index, to));
      app.say(`${name()} · ${fx.name}`, `slot ${to + 1} of ${inserts().length}`);
    };

    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
    window.addEventListener('pointercancel', onUp);
  }

  function onSlotContext(row: SlotRow, event: MouseEvent): void {
    event.preventDefault();
    event.stopPropagation();
    const fx = row.fx;
    if (!fx) return;
    void app.mixerCommand((b) => b.removeInsert(target(), fx.index));
    app.say(`${name()} · ${fx.name}`, 'removed');
  }

  function toggleBypass(fx: InsertDto, event: MouseEvent): void {
    event.stopPropagation();
    void app.mixerCommand((b) => b.setInsertBypass(target(), fx.index, !fx.bypassed));
    app.say(`${name()} · ${fx.name}`, fx.bypassed ? 'active' : 'bypassed');
  }

  // --- the send cell -------------------------------------------------------
  //
  // In `from` mode this cell is "the selected strip -> this strip"; in `into`
  // mode it is "this strip -> the selected strip". One matrix read two ways,
  // which is why there is one `sends` list rather than a per-strip copy.

  const sends = (): SendDto[] => app.mixer()?.sends ?? [];
  const isSelf = () => sameStrip(view.strip(), stripRef());

  const pair = (): { source: StripRef; dest: StripRef } => {
    const self = stripRef();
    const selection = view.strip();
    return view.sendMode() === 'from'
      ? { source: selection, dest: self }
      : { source: self, dest: selection };
  };

  const existing = (): SendDto | undefined => {
    const { source, dest } = pair();
    if (source.kind === 'master' || dest.kind !== 'bus') return undefined;
    return sends().find(
      (send) =>
        send.sourceKind === source.kind &&
        send.sourceIndex === source.index &&
        send.dest === dest.index,
    );
  };

  /** Why this pair cannot be a send, or `null` when it can. */
  const blocked = (): string | null => {
    if (isSelf()) return null;
    const { source, dest } = pair();
    if (source.kind === 'master') {
      return 'no path — the master is the end of the chain, not a send source';
    }
    if (dest.kind !== 'bus') {
      return 'no path — a send lands on a bus, and this is not a bus';
    }
    if (source.kind === 'bus' && busReaches(sends(), dest.index, source.index)) {
      return 'no path — that would feed a bus into itself';
    }
    return null;
  };

  const routable = () => !isSelf() && blocked() === null;

  const sendDef = createMemo(() =>
    spec(
      'send',
      'Send',
      'dB',
      app.mixer()?.minGainDb ?? -60,
      app.mixer()?.maxSendDb ?? 1.94,
      0,
      'db',
    ),
  );

  // While the knob is being dragged the engine's `levelDb` is stale --
  // `set_send_level` answers with nothing, by design -- so the drawn position
  // comes from here until the gesture commits and the whole mixer is refetched.
  const [dragDb, setDragDb] = createSignal<number | null>(null);
  const sendDb = () => dragDb() ?? existing()?.levelDb ?? (app.mixer()?.minGainDb ?? -60);

  /** The linear level a send's dB reading stands for, and back again. */
  const sendLevelOf = (db: number): number => {
    const state = app.mixer();
    const floor = state?.minGainDb ?? -60;
    const ceiling = state?.maxSendLevel ?? 1.25;
    return db <= floor ? 0 : Math.min(ceiling, 10 ** (db / 20));
  };

  const sendDbOf = (level: number): number => {
    const floor = app.mixer()?.minGainDb ?? -60;
    return level <= 0 ? floor : Math.max(floor, 20 * Math.log10(level));
  };

  function onSendLevel(db: number): void {
    const send = existing();
    if (!send) return;
    setDragDb(db);
    // The knob works in decibels because that is the scale a level belongs on;
    // the engine stores the linear level, and holds the one conversion.
    void app.backend.setSendLevel(send.id, sendLevelOf(db)).catch(app.report);
  }

  function onSendCommit(): void {
    // Only when the knob actually moved: a plain click is the route toggle, and
    // it refetches the mixer by itself.
    if (dragDb() === null) return;
    void app.mixerCommand((b) => b.getTracks()).finally(() => setDragDb(null));
  }

  function togglePre(send: SendDto, event: MouseEvent): void {
    event.stopPropagation();
    const state = app.mixer();
    const label = state
      ? `${stripName(state, pair().source)} → ${stripName(state, pair().dest)} · Send tap`
      : 'Send tap';
    void app.backend
      .setSendPre(send.id, !send.pre)
      .then(() => app.mixerCommand((b) => b.getTracks()))
      .catch(app.report);
    app.say(label, send.pre ? 'post-fader' : 'pre-fader');
  }

  function toggleRoute(): void {
    const { source, dest } = pair();
    if (source.kind === 'master' || dest.kind !== 'bus') return;
    const send = existing();
    const state = app.mixer();
    const label = state
      ? `${stripName(state, source)} → ${stripName(state, dest)} · Send`
      : 'Send';
    if (send) {
      void app.mixerCommand((b) => b.removeSend(send.id));
      app.say(label, 'removed');
    } else {
      void app.mixerCommand((b) => b.addSend(source.kind, source.index, dest.index));
      app.say(label, '0.0 dB, post-fader');
    }
  }

  /**
   * Click versus drag versus double-click on the send cell, measured rather
   * than guessed.
   *
   * A press that goes nowhere toggles the route; a press that moves more than
   * `SEND_CLICK_SLOP_PX` is the knob's own level drag, so a hand that wobbles
   * two pixels on the way to a click still routes; a press that follows a
   * release closely enough is the second half of a double-click and does
   * nothing at all, so the pair reads as the single toggle it was meant to be.
   *
   * Listening in the **capture** phase because the knob stops the event
   * propagating once it has started its drag, and a bubble listener would
   * therefore only ever see the "no route yet" case.
   */
  let cell: HTMLDivElement | undefined;
  let lastRelease = Number.NEGATIVE_INFINITY;

  function onCellDown(event: PointerEvent): void {
    if (event.button !== 0) return;
    const from = event.target as HTMLElement | null;
    if (from?.closest('.wv-mixer__pre')) return;
    if (!routable()) return;
    const x0 = event.clientX;
    const y0 = event.clientY;
    const redoubled = event.timeStamp - lastRelease < DOUBLE_CLICK_MS;
    let moved = false;
    const move = (moveEvent: PointerEvent) => {
      if (
        Math.abs(moveEvent.clientX - x0) > SEND_CLICK_SLOP_PX ||
        Math.abs(moveEvent.clientY - y0) > SEND_CLICK_SLOP_PX
      ) {
        moved = true;
      }
    };
    const up = (upEvent: PointerEvent) => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
      window.removeEventListener('pointercancel', up);
      // A drag ends nowhere near where it began, so it cannot be the first half
      // of anything: only a still press arms the double-click window.
      lastRelease = moved ? Number.NEGATIVE_INFINITY : upEvent.timeStamp;
      if (!moved && !redoubled) toggleRoute();
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
    window.addEventListener('pointercancel', up);
  }

  onMount(() => cell?.addEventListener('pointerdown', onCellDown, true));
  onCleanup(() => cell?.removeEventListener('pointerdown', onCellDown, true));

  const arrow = () => (isSelf() ? '▼' : routable() ? '▲' : '·');

  const outLabel = () => {
    if (isMaster()) return 'final out';
    const state = app.mixer();
    if (!state) return '→ Master';
    return `→ ${['Master', ...sendTargets(state, stripRef())].join(' + ')}`;
  };

  const sendOwner = () => {
    const state = app.mixer();
    if (!state) return name();
    const { source, dest } = pair();
    return `${stripName(state, source)} → ${stripName(state, dest)}`;
  };

  // --- render --------------------------------------------------------------

  return (
    <div
      class="wv-mixer__strip"
      classList={{
        'wv-mixer__strip--master': isMaster(),
        'is-active': selected(),
        'is-silenced': silenced(),
      }}
      role="group"
      aria-label={`${name()} strip`}
      onClick={select}
    >
      <div class="wv-mixer__head">
        <span class="wv-mixer__num">{props.num}</span>
        <Show
          when={!isMaster()}
          fallback={<span class="wv-mixer__mastername">MASTER</span>}
        >
          <span class="wv-swatch" style={{ background: color() }} />
          <span
            class="wv-mixer__name"
            title={
              track()
                ? `${name()} — double-click to open this channel in the rack`
                : name()
            }
            onDblClick={(event) => {
              event.stopPropagation();
              if (!track() || mixerStripDestination('track') === null) return;
              select();
              view.go('rack');
            }}
          >
            {name()}
          </span>
        </Show>
        <span class="wv-spacer" />
        <Show when={missing()}>
          {(gone) => (
            <span
              class="wv-pill wv-pill--rec is-on"
              title={`${gone().name} (${gone().format}) could not be loaded — ${gone().reason}\n${gone().path}`}
            >
              !
            </span>
          )}
        </Show>
      </div>

      <div class="wv-mixer__btns">
        <Show when={!isMaster()}>
          {/*
            No master mute: there is no `set_master_mute` command, and a button
            that answers nothing is worse than no button. The master fader
            reaches −60 dB, which is the honest way to silence the output.
          */}
          <button
            type="button"
            class="wv-led wv-led--dot"
            classList={{ 'is-muted': muted() }}
            title={muted() ? `Unmute ${name()}` : `Mute ${name()}`}
            aria-label={`Mute ${name()}`}
            aria-pressed={muted()}
            onClick={toggleMute}
          />
          <button
            type="button"
            class="wv-pill wv-pill--solo"
            classList={{ 'is-on': soloed() }}
            title={`Solo ${name()} — solo is one scope across tracks and buses`}
            aria-pressed={soloed()}
            onClick={toggleSolo}
          >
            S
          </button>
          <button
            type="button"
            class="wv-pill wv-pill--rec wv-can-disable"
            disabled
            title="Recording is not implemented yet"
          >
            R
          </button>
        </Show>
        <span class="wv-spacer" />
        <span class="wv-mixer__kind">{kindTag()}</span>
      </div>

      <div class="wv-mixer__slots">
        <Index each={slots()}>
          {(row) => (
            <div
              class="wv-mixer__slot"
              classList={{
                'is-filled': row().kind === 'fx',
                'is-bypassed': row().fx?.bypassed === true,
                'is-add': row().kind === 'add',
                'is-blank': row().kind === 'blank',
                'is-more': row().kind === 'more',
                'is-dragging': dragIndex() !== null && dragIndex() === row().fx?.index,
                'is-drop':
                  dragIndex() !== null &&
                  dropIndex() === row().fx?.index &&
                  dropIndex() !== dragIndex(),
              }}
              title={row().title}
              onPointerDown={(event) => onSlotPointerDown(event, row())}
              onClick={(event) => onSlot(row(), event)}
              onDblClick={(event) => {
                event.stopPropagation();
                const current = row();
                if (!mixerSlotOpensEditor(current.kind) || !current.fx) return;
                select();
                view.openInsertEditor({ strip: stripRef(), insertId: current.fx.id });
              }}
              onContextMenu={(event) => onSlotContext(row(), event)}
            >
              <Show when={row().fx}>
                {(fx) => (
                  <button
                    type="button"
                    class="wv-dot wv-mixer__bypass"
                    classList={{ 'is-off': fx().bypassed }}
                    title={fx().bypassed ? `Enable ${fx().name}` : `Bypass ${fx().name}`}
                    aria-label={`Bypass ${fx().name}`}
                    aria-pressed={fx().bypassed}
                    onClick={(event) => toggleBypass(fx(), event)}
                  />
                )}
              </Show>
              <span class="wv-mixer__slotname">{row().label}</span>
            </div>
          )}
        </Index>
      </div>

      <Show when={master()}>
        {(state) => (
          <Show when={state().clap ?? state().vst3}>
            {(slot) => (
              <div class="wv-mixer__plugin" title={`${slot().name} — hosted on the master insert, after the internal chain`}>
                <span class="wv-dot wv-mixer__bypass" />
                <span class="wv-mixer__slotname">{slot().name}</span>
                <span class="wv-spacer" />
                <Show when={slot().hasEditor}>
                  <button
                    type="button"
                    class="wv-mixer__gui"
                    classList={{ 'is-open': slot().guiOpen }}
                    title={
                      slot().guiOpen
                        ? 'Close the plugin editor window'
                        : 'Open the plugin editor — a native window beside MYDAW, not a panel in it'
                    }
                    aria-label="Open the plugin editor"
                    onClick={(event) => {
                      event.stopPropagation();
                      app.toggleGui({ slot: 'master' }, !slot().guiOpen);
                    }}
                  >
                    ⧉
                  </button>
                </Show>
              </div>
            )}
          </Show>
        )}
      </Show>

      <div class="wv-mixer__pan">
        <Show when={pan() !== undefined}>
          <Knob
            def={PAN_DEF}
            value={() => pan() ?? 0}
            onInput={setPan}
            // Pan's own unit, not a fraction of the throw: the readout counts
            // in hundredths and the wheel should move it in hundredths too.
            wheelStep={(value, notches) =>
              value +
              Math.sign(notches) * (Math.abs(notches) < 1 ? PAN_FINE_STEP : PAN_WHEEL_STEP)
            }
            owner={name()}
            size={22}
            color={selected() ? 'var(--acc)' : 'var(--dim)'}
            title={`Pan ${panLabel(pan() ?? 0)} — drag or scroll, shift for fine, double-click to centre`}
          />
        </Show>
        <div class="wv-mixer__reads">
          <Show when={pan() !== undefined}>
            <span class="wv-mixer__panread">{panLabel(pan() ?? 0)}</span>
          </Show>
          {/*
            Where the mock draws a "stereo separation" slider. No strip in this
            engine has such a parameter, so the gain readout lives here instead
            of a control that would do nothing.
          */}
          <span class="wv-mixer__gain">{formatDb(gainDb())} dB</span>
        </div>
      </div>

      <div class="wv-mixer__faders">
        <div
          ref={faderEl}
          class="wv-fader"
          role="slider"
          tabindex="0"
          aria-label={`${name()} level`}
          aria-valuemin={app.mixer()?.minGainDb ?? -60}
          aria-valuemax={app.mixer()?.maxGainDb ?? 6}
          aria-valuenow={Number(gainDb().toFixed(1))}
          aria-valuetext={`${formatDb(gainDb())} dB`}
          title={`Drag to set the level · scroll for ${FADER_WHEEL_DB} dB a notch, shift for ${
            FADER_WHEEL_DB * FINE_FACTOR
          } · double-click for 0 dB`}
          onPointerDown={onFaderDown}
          onDblClick={(event) => {
            event.stopPropagation();
            setGain(0);
          }}
          onKeyDown={(event) => {
            const step = event.shiftKey ? 0.1 : 1;
            if (event.key === 'ArrowUp') setGain(Math.min(6, gainDb() + step));
            else if (event.key === 'ArrowDown') setGain(Math.max(-60, gainDb() - step));
            else return;
            event.preventDefault();
          }}
        >
          <span
            class="wv-fader__handle"
            style={{
              top: `calc((1 - ${faderFraction(gainDb()).toFixed(4)}) * (100% - ${FADER_HANDLE_PX}px))`,
            }}
          />
        </div>
        <Show
          when={isMaster()}
          fallback={<Meter read={() => (silenced() ? 0 : peak())} />}
        >
          <Meter read={() => app.meters.latest?.peakL ?? 0} />
          <Meter read={() => app.meters.latest?.peakR ?? 0} />
        </Show>
      </div>

      <div class="wv-mixer__send" onClick={(event) => event.stopPropagation()}>
        <span
          class="wv-mixer__arrow"
          classList={{ 'is-self': isSelf(), 'is-live': existing() !== undefined }}
        >
          {arrow()}
        </span>

        <div class="wv-mixer__sendrow" ref={cell}>
          <Show
            when={routable()}
            fallback={
              <span
                class="wv-mixer__sendnote"
                classList={{ 'is-self': isSelf() }}
                title={
                  isSelf()
                    ? 'This is the selected strip: the other cells are its sends'
                    : (blocked() ?? '')
                }
              >
                {isSelf()
                  ? view.sendMode() === 'from'
                    ? 'SENDS'
                    : 'FED BY'
                  : 'no path'}
              </span>
            }
          >
            <Knob
              def={sendDef()}
              value={sendDb}
              onInput={onSendLevel}
              onCommit={onSendCommit}
              // A notch is 2% of the signal, in the linear level the engine
              // stores rather than the dB the cap shows: that is the unit a
              // send is actually thought about in, and it puts the fine end of
              // the resolution up at unity where the balancing happens. Shift
              // crosses back into dB, because that is where the readout can
              // show the difference.
              wheelStep={(db, notches) =>
                Math.abs(notches) < 1
                  ? db + Math.sign(notches) * SEND_FINE_DB
                  : sendDbOf(sendLevelOf(db) + notches * SEND_WHEEL_LEVEL)
              }
              owner={sendOwner()}
              label="Send"
              size={20}
              disabled={existing() === undefined}
              // No "double-click to reset" here, unlike every other knob in the
              // shell: on this cell the click is the route toggle, and the
              // first half of a double-click would have removed the send before
              // there was anything left to reset. Unrouting and routing again
              // *is* the reset -- a new send starts at 0 dB, post-fader.
              title={
                existing()
                  ? 'Drag or scroll for the send level · click to unroute'
                  : 'Click to route this pair · then drag or scroll for the level'
              }
            />
            <Show
              when={existing()}
              fallback={
                <span class="wv-mixer__pre is-off" title="Not routed — click to route">
                  off
                </span>
              }
            >
              {(send) => (
                <button
                  type="button"
                  class="wv-mixer__pre"
                  classList={{ 'is-pre': send().pre }}
                  title={
                    send().pre
                      ? 'Pre-fader: tapped before this strip’s fader, pan and mute. Click for post-fader.'
                      : 'Post-fader: tapped after the fader, pan and mute. Click for pre-fader.'
                  }
                  onClick={(event) => togglePre(send(), event)}
                >
                  {send().pre ? 'PRE' : 'post'}
                </button>
              )}
            </Show>
          </Show>
        </div>

        <Show
          when={isMaster()}
          fallback={
            <span class="wv-mixer__out" title={ROUTE_TITLE}>
              {outLabel()}
            </span>
          }
        >
          <span class="wv-mixer__out">final out</span>
          {/*
            The mock's LUFS readout. The engine measures peak and RMS and no
            loudness at all, so this is the peak it really has -- held for a
            moment and decayed, the way a peak readout is read.
          */}
          <PeakReadout read={() => Math.max(app.meters.latest?.peakL ?? 0, app.meters.latest?.peakR ?? 0)} />
        </Show>
      </div>
    </div>
  );
}

/**
 * The master's peak readout, in dBFS.
 *
 * Painted from the shared frame loop rather than a signal, for the same reason
 * the meters are: this updates thirty to sixty times a second.
 */
function PeakReadout(props: { read: () => number }): JSX.Element {
  let el: HTMLSpanElement | undefined;
  let held = 0;
  let frames = 0;

  onFrame(() => {
    if (!el) return;
    const now = props.read();
    if (now >= held) {
      held = now;
      frames = 0;
    } else if (++frames > PEAK_HOLD_FRAMES) {
      held = Math.max(now, held * 0.92);
    }
    const db = held > 0 ? 20 * Math.log10(held) : -Infinity;
    el.textContent = `${formatDb(db)} dBFS`;
  });

  return (
    <span
      class="wv-mixer__peak"
      ref={el}
      title="Peak, held for a moment. The engine reports peak, not LUFS."
    />
  );
}

// ---------------------------------------------------------------------------
// The effect picker
// ---------------------------------------------------------------------------

function EffectMenu(props: { menu: FxMenu; close: () => void }): JSX.Element {
  const app = useAppState();
  const view = useView();

  return (
    <div
      class="wv-scrim"
      onClick={props.close}
      onContextMenu={(event) => {
        event.preventDefault();
        props.close();
      }}
    >
      <div
        class="wv-menu wv-mixer__fxmenu"
        style={{ left: `${props.menu.x}px`, top: `${props.menu.y}px` }}
      >
        <Index each={app.effects()}>
          {(effect) => (
            <button
              type="button"
              class="wv-menu__row"
              title={effect().description}
              onClick={() => {
                const target = props.menu.target;
                const before = new Set(insertsOnStrip(app.mixer(), target).map((insert) => insert.id));
                const strip: StripRef =
                  target.kind === 'master'
                    ? { kind: 'master' }
                    : { kind: target.kind, index: target.index };
                void app
                  .mixerCommand((b) => b.addInsert(target, effect().typeId))
                  .then(() => {
                    const id = addedInsertId(before, insertsOnStrip(app.mixer(), target));
                    if (id !== undefined) view.openInsertEditor({ strip, insertId: id });
                    app.say(`${props.menu.owner} · Insert`, `${effect().name} added`);
                    props.close();
                  });
              }}
            >
              <span>{effect().name}</span>
              <span class="wv-spacer" />
              <span class="wv-menu__meta">{effect().params.length}</span>
            </button>
          )}
        </Index>
        <Show when={app.effects().length === 0}>
          <span
            class="wv-menu__row wv-disabled"
            title="The engine has not sent its effect catalogue yet"
          >
            no effects
          </span>
        </Show>
      </div>
    </div>
  );
}
