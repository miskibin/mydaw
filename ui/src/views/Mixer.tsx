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
import { faderFraction, faderToDb, formatDb, panLabel } from '../panels/mixer/scale';
import { useView } from '../shell/view';
import { insertTargetFor, useAppState, type StripRef } from '../state';
import { trackColor } from '../theme/tokens';
import { FADER_TRAVEL_PX, startDrag } from '../ui/drag';
import { onFrame } from '../ui/frameLoop';
import { Knob, spec } from '../ui/Knob';
import { Meter } from '../ui/Meter';

/** Pan is the same parameter on every strip, so its spec is built once. */
const PAN_DEF = spec('pan', 'Pan', '', -1, 1, 0);

/** How far the pointer may travel and still count as a click on a send cell. */
const SEND_CLICK_SLOP_PX = 3;

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
              <div class="wv-mixer__strips">
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
      return track ? trackColor(track.id) : 'var(--dim)';
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
  const gainDb = () => track()?.gainDb ?? bus()?.gainDb ?? master()?.gainDb ?? 0;
  const pan = () => track()?.pan ?? bus()?.pan;
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
    if (asTrack) return trackColor(asTrack.id);
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

  function setGain(db: number): void {
    app.say(`${name()} · Volume`, `${formatDb(db)} dB`);
    const asTrack = track();
    const asBus = bus();
    if (asTrack) void app.mixerCommand((b) => b.setTrackGain(asTrack.index, db));
    else if (asBus) void app.mixerCommand((b) => b.setBusGain(asBus.index, db));
    else void app.mixerCommand((b) => b.setMasterGain(db));
  }

  function onFaderDown(event: PointerEvent): void {
    if (event.button !== 0) return;
    event.stopPropagation();
    startDrag(event, {
      // In *travel* rather than decibels, so `scale.ts` stays the one thing
      // that decides how the dB are spaced along the throw.
      from: faderFraction(gainDb()),
      perPixel: 1 / FADER_TRAVEL_PX,
      min: 0,
      max: 1,
      onChange: (fraction) => setGain(faderToDb(fraction)),
    });
  }

  function setPan(value: number): void {
    const next = Math.round(value * 100) / 100;
    const asTrack = track();
    const asBus = bus();
    if (asTrack) void app.mixerCommand((b) => b.setTrackPan(asTrack.index, next));
    else if (asBus) void app.mixerCommand((b) => b.setBusPan(asBus.index, next));
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
          title: `${fx.name} — click opens it in the inspector · right-click removes it · the dot bypasses it`,
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
    if (row.kind === 'add') {
      props.onPickEffect(target(), name(), event);
      return;
    }
    select();
    if (row.kind === 'blank') return;
    view.setSideTab('inspector');
    view.setSideOpen(true);
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

  function onSendLevel(db: number): void {
    const send = existing();
    if (!send) return;
    setDragDb(db);
    const state = app.mixer();
    const floor = state?.minGainDb ?? -60;
    const ceiling = state?.maxSendLevel ?? 1.25;
    // The knob works in decibels because that is the scale a level belongs on;
    // the engine stores the linear level, and holds the one conversion.
    const level = db <= floor ? 0 : Math.min(ceiling, 10 ** (db / 20));
    void app.backend.setSendLevel(send.id, level).catch(app.report);
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
   * Click versus drag on the send cell, measured rather than guessed.
   *
   * A press that goes nowhere toggles the route; a press that moves is the
   * knob's own level drag. Listening in the **capture** phase because the knob
   * stops the event propagating once it has started its drag, and a bubble
   * listener would therefore only ever see the "no route yet" case.
   */
  let cell: HTMLDivElement | undefined;

  function onCellDown(event: PointerEvent): void {
    if (event.button !== 0) return;
    const from = event.target as HTMLElement | null;
    if (from?.closest('.wv-mixer__pre')) return;
    if (!routable()) return;
    const x0 = event.clientX;
    const y0 = event.clientY;
    let moved = false;
    const move = (moveEvent: PointerEvent) => {
      if (
        Math.abs(moveEvent.clientX - x0) > SEND_CLICK_SLOP_PX ||
        Math.abs(moveEvent.clientY - y0) > SEND_CLICK_SLOP_PX
      ) {
        moved = true;
      }
    };
    const up = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
      window.removeEventListener('pointercancel', up);
      if (!moved) toggleRoute();
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
          <span class="wv-mixer__name" title={name()}>
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
              }}
              title={row().title}
              onClick={(event) => onSlot(row(), event)}
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
            owner={name()}
            size={22}
            color={selected() ? 'var(--acc)' : 'var(--dim)'}
            title={`Pan ${panLabel(pan() ?? 0)} — drag, shift for fine, double-click to centre`}
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
          class="wv-fader"
          role="slider"
          tabindex="0"
          aria-label={`${name()} level`}
          aria-valuemin={app.mixer()?.minGainDb ?? -60}
          aria-valuemax={app.mixer()?.maxGainDb ?? 6}
          aria-valuenow={Number(gainDb().toFixed(1))}
          aria-valuetext={`${formatDb(gainDb())} dB`}
          title="Drag to set the level, double-click for 0 dB"
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
              top: `calc((1 - ${faderFraction(gainDb()).toFixed(4)}) * (100% - 12px))`,
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
              owner={sendOwner()}
              label="Send"
              size={20}
              disabled={existing() === undefined}
              title={
                existing()
                  ? 'Drag for the send level · click to unroute'
                  : 'Click to route this pair'
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
                void app.mixerCommand((b) => b.addInsert(props.menu.target, effect().typeId));
                app.say(`${props.menu.owner} · Insert`, `${effect().name} added`);
                props.close();
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
