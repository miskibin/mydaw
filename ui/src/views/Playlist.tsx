/**
 * The arrangement: pattern clips and audio clips along a timeline.
 *
 * Geometry is one number -- {@link BAR_PX} pixels per bar at zoom 1 -- and
 * everything else is derived from the engine's own tick constants, so the
 * ruler, the clips and the playhead cannot disagree about where bar 5 is.
 *
 * ## What a lane is, and is not
 *
 * A playlist lane is **not** a mixer track. It has a name, a mute and an
 * ordering, and that is all the engine gives it. The mock's lane header also
 * carries a volume slider, a solo and a record-arm; none of those exist for a
 * lane, so rather than draw three dead controls this header shows what the lane
 * actually has plus its clip count. A clip's *sound* is the channel's, and its
 * fader is in the mixer.
 */

import { For, Index, Show, createSignal, onCleanup } from 'solid-js';

import type { ClipDto, WaveformDto } from '../api/types';
import { TICKS_PER_BAR, samplesToTicks, snapTicks, ticksToSamples } from '../lib/pattern';
import { useAppState } from '../state';
import {
  PLAYLIST_SNAPS,
  clampZoom,
  nextSnap,
  snapLabel,
  useView,
  type PlaylistTool,
} from '../shell/view';
import { alpha, trackColor } from '../theme/tokens';
import { onFrame } from '../ui/frameLoop';
import { IconEraseTool, IconPaintTool, IconSelectTool, IconX } from '../ui/icons';

/** Pixels one bar covers at zoom 1. */
const BAR_PX = 48;
/** How tall a lane is. */
const LANE_H = 48;
/** How wide a lane header is. */
const HEAD_W = 176;
/** How far past the last clip the timeline keeps going, in bars. */
const TAIL_BARS = 8;
/** The fewest bars ever drawn, so an empty arrangement still has a ruler. */
const MIN_BARS = 32;

const TOOLS: readonly [PlaylistTool, string, () => ReturnType<typeof IconPaintTool>][] = [
  ['paint', 'Paint clips — click a lane to place the brush', IconPaintTool],
  ['select', 'Select and move clips', IconSelectTool],
  ['delete', 'Delete clips', IconEraseTool],
];

export function Playlist(): ReturnType<typeof Show> {
  const app = useAppState();
  const view = useView();
  const [waveforms, setWaveforms] = createSignal<Record<number, WaveformDto>>({});
  const pending = new Set<number>();

  let playheadEl: HTMLDivElement | undefined;
  let scrollEl: HTMLDivElement | undefined;

  const pxPerTick = () => (BAR_PX * view.playlistZoom()) / TICKS_PER_BAR;
  const playlist = () => app.playlist();
  const bars = () => {
    const end = playlist()?.endTicks ?? 0;
    return Math.max(MIN_BARS, Math.ceil(end / TICKS_PER_BAR) + TAIL_BARS);
  };
  const laneWidth = () => bars() * BAR_PX * view.playlistZoom();

  /** The waveform for one source, fetched once and remembered. */
  function waveformFor(sourceId: number): WaveformDto | undefined {
    const have = waveforms()[sourceId];
    if (have) return have;
    if (!pending.has(sourceId)) {
      pending.add(sourceId);
      app.backend
        .getAudioWaveform(sourceId, 512)
        .then((wave) => setWaveforms((all) => ({ ...all, [sourceId]: wave })))
        .catch(() => {})
        .finally(() => pending.delete(sourceId));
    }
    return undefined;
  }

  // The playhead is written to the DOM on the frame loop: it moves every frame
  // and must not go through a signal. It is only meaningful in SONG mode --
  // in PAT mode the transport is looping a pattern, not running the timeline.
  onFrame(() => {
    if (!playheadEl) return;
    if (app.playMode() !== 'song') {
      playheadEl.style.display = 'none';
      return;
    }
    playheadEl.style.display = '';
    const ticks = samplesToTicks(
      app.playhead.positionSamples(),
      app.playhead.getSampleRate(),
      app.bpm(),
    );
    playheadEl.style.transform = `translateX(${ticks * pxPerTick()}px)`;
  });

  const onWheel = (event: WheelEvent) => {
    if (!event.shiftKey) return;
    event.preventDefault();
    view.setPlaylistZoom((zoom) => clampZoom(zoom * (event.deltaY < 0 ? 1.1 : 1 / 1.1)));
  };

  // --- placing -------------------------------------------------------------

  /** Where in the song a pointer at `clientX` is, snapped. */
  function tickAt(clientX: number, lane: HTMLElement): number {
    const rect = lane.getBoundingClientRect();
    const raw = (clientX - rect.left) / pxPerTick();
    return snapTicks(Math.max(0, raw), view.playlistSnap(), true);
  }

  function place(lane: number, start: number, brush = view.brush()): void {
    if (!brush) {
      app.say('Playlist', 'no brush — pick a pattern or a file in the Picker');
      return;
    }
    app.startPlaylistGesture(brush.kind === 'pattern' ? 'Paint pattern clip' : 'Paint audio clip');
    void app
      .sequencerCommand((backend) =>
        brush.kind === 'pattern'
          ? backend.addPatternClip(brush.id, lane, start)
          : backend.addAudioClip(brush.id, lane, start),
      )
      .then(() => app.commitPlaylistGesture());
  }

  function onLanePointerDown(event: PointerEvent, lane: number): void {
    if (event.button !== 0 || view.playlistTool() !== 'paint') return;
    place(lane, tickAt(event.clientX, event.currentTarget as HTMLElement));
  }

  function onLaneDrop(event: DragEvent, lane: number): void {
    event.preventDefault();
    const raw = event.dataTransfer?.getData('text/plain') ?? '';
    const [kind, ref] = raw.split('::');
    const id = Number(ref);
    if (!Number.isFinite(id)) return;
    const start = tickAt(event.clientX, event.currentTarget as HTMLElement);
    if (kind === 'pattern') place(lane, start, { kind: 'pattern', id });
    else if (kind === 'audio') place(lane, start, { kind: 'audio', id });
    else if (kind === 'inst') {
      app.report(new Error('An instrument goes on the rack, not a playlist lane'));
    }
  }

  // --- moving and resizing -------------------------------------------------

  function onClipPointerDown(event: PointerEvent, clip: ClipDto, mode: 'move' | 'resize'): void {
    event.stopPropagation();
    if (event.button !== 0) return;
    if (view.playlistTool() === 'delete') {
      removeClip(clip);
      return;
    }
    view.setSelectedClip(clip.id);
    const startX = event.clientX;
    const startY = event.clientY;
    const originStart = clip.start;
    const originLength = clip.length;
    const originLane = clip.track;
    const lanes = playlist()?.tracks.length ?? 1;
    let moved = false;
    let lastStart = originStart;
    let lastLane = originLane;
    let lastLength = originLength;

    app.startPlaylistGesture(mode === 'move' ? 'Move clip' : 'Resize clip');

    const onMove = (moveEvent: PointerEvent) => {
      const dx = (moveEvent.clientX - startX) / pxPerTick();
      if (!moved && Math.abs(moveEvent.clientX - startX) < 3 && Math.abs(moveEvent.clientY - startY) < 3) {
        return;
      }
      moved = true;
      if (mode === 'resize') {
        const snap = view.playlistSnap() || 1;
        lastLength = Math.max(snap, snapTicks(originLength + dx, view.playlistSnap()));
        app.say(clipName(clip), `${(lastLength / TICKS_PER_BAR).toFixed(2)} bars`);
        // Optimistic: the engine answers with the whole state, which would
        // otherwise arrive a frame late and make the edge stutter.
        (moveEvent.target as HTMLElement | null)?.closest<HTMLElement>('.wv-clip')?.style.setProperty(
          'width',
          `${Math.max(4, lastLength * pxPerTick() - 2)}px`,
        );
        return;
      }
      const dy = Math.round((moveEvent.clientY - startY) / LANE_H);
      lastLane = Math.min(lanes - 1, Math.max(0, originLane + dy));
      lastStart = snapTicks(Math.max(0, originStart + dx), view.playlistSnap());
      app.say(
        clipName(clip),
        `bar ${(lastStart / TICKS_PER_BAR + 1).toFixed(2)} · ${playlist()?.tracks[lastLane]?.name ?? ''}`,
      );
    };

    const onUp = () => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      if (!moved) {
        app.commitPlaylistGesture();
        return;
      }
      const run =
        mode === 'resize'
          ? app.sequencerCommand((backend) => backend.resizeClip(clip.id, lastLength))
          : app.sequencerCommand((backend) => backend.moveClip(clip.id, lastLane, lastStart));
      void run.then(() => app.commitPlaylistGesture());
    };

    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
  }

  function clipName(clip: ClipDto): string {
    if (clip.kind === 'pattern') {
      return app.patterns().find((pattern) => pattern.id === clip.patternId)?.name ?? 'Clip';
    }
    return app.audioSources().find((source) => source.id === clip.sourceId)?.name ?? 'Audio';
  }

  function removeClip(clip: ClipDto): void {
    app.startPlaylistGesture('Delete clip');
    void app
      .sequencerCommand((backend) => backend.removeClip(clip.id))
      .then(() => app.commitPlaylistGesture());
  }

  // --- lanes ---------------------------------------------------------------

  function removeLane(index: number, name: string): void {
    const lost = playlist()?.clips.filter((clip) => clip.track === index).length ?? 0;
    void app.sequencerCommand((backend) => backend.removePlaylistTrack(index));
    app.clearHistory(`removing ${name}`);
    app.say('Lane · Delete', lost > 0 ? `${name} and ${lost} clip${lost > 1 ? 's' : ''}` : name);
  }

  const brushLabel = () => {
    const brush = view.brush();
    if (!brush) return 'No brush — pick one on the right';
    if (brush.kind === 'pattern') {
      return `Brush · ${app.patterns().find((p) => p.id === brush.id)?.name ?? '?'}`;
    }
    return `Brush · ${app.audioSources().find((s) => s.id === brush.id)?.name ?? '?'}`;
  };

  const brushColour = () => {
    const brush = view.brush();
    if (!brush) return 'var(--dim)';
    if (brush.kind === 'pattern') {
      const pattern = app.patterns().find((p) => p.id === brush.id);
      return pattern?.color ?? trackColor(brush.id);
    }
    return 'var(--acc)';
  };

  onCleanup(() => pending.clear());

  return (
    <div class="wv-main">
      <div class="wv-viewbar">
        <span class="wv-viewbar__title">PLAYLIST</span>
        <div class="wv-seg wv-seg--plain">
          <For each={TOOLS}>
            {([id, title, Icon]) => (
              <button
                type="button"
                class="wv-tool"
                classList={{ 'is-active': view.playlistTool() === id }}
                title={title}
                onClick={() => view.setPlaylistTool(id)}
              >
                <Icon />
              </button>
            )}
          </For>
        </div>
        <button
          type="button"
          class="wv-chip wv-chip--sunk"
          title="What clip edges snap to"
          onClick={() => view.setPlaylistSnap(nextSnap(PLAYLIST_SNAPS, view.playlistSnap()))}
        >
          <span class="wv-chip__tag">SNAP</span>
          {snapLabel(PLAYLIST_SNAPS, view.playlistSnap())}
        </button>
        <div class="wv-zoom">
          <button
            type="button"
            class="wv-zoom__btn"
            title="Zoom out (shift-scroll)"
            onClick={() => view.setPlaylistZoom((zoom) => clampZoom(zoom / 1.25))}
          >
            −
          </button>
          <button
            type="button"
            class="wv-zoom__btn"
            title="Zoom in (shift-scroll)"
            onClick={() => view.setPlaylistZoom((zoom) => clampZoom(zoom * 1.25))}
          >
            +
          </button>
        </div>
        <div class="wv-spacer" />
        <span class="wv-brush" style={{ background: alpha(brushColour(), 0.14), color: brushColour() }}>
          <span class="wv-swatch" style={{ background: brushColour() }} />
          {brushLabel()}
        </span>
      </div>

      <div class="wv-pl" ref={scrollEl} onWheel={onWheel}>
        <div class="wv-pl__inner" style={{ width: `${HEAD_W + laneWidth()}px` }}>
          <div class="wv-pl__ruler">
            <div class="wv-pl__corner" style={{ width: `${HEAD_W}px` }}>
              {playlist()?.tracks.length ?? 0} lanes · {playlist()?.clips.length ?? 0} clips
            </div>
            <div
              class="wv-pl__ruler-track"
              style={{ width: `${laneWidth()}px` }}
              title="Click to move the playhead"
              onPointerDown={(event) => {
                const rect = event.currentTarget.getBoundingClientRect();
                const ticks = Math.max(0, (event.clientX - rect.left) / pxPerTick());
                app.seek(
                  ticksToSamples(ticks, app.playhead.getSampleRate(), app.bpm()),
                );
                app.say('Transport · Seek', `bar ${(ticks / TICKS_PER_BAR + 1).toFixed(2)}`);
              }}
            >
              <Index each={Array.from({ length: bars() })}>
                {(_, bar) => (
                  <span
                    class="wv-pl__mark"
                    classList={{ 'is-major': bar % 4 === 0 }}
                    style={{
                      left: `${bar * BAR_PX * view.playlistZoom()}px`,
                      width: `${BAR_PX * view.playlistZoom()}px`,
                    }}
                  >
                    {bar % 4 === 0 ? bar + 1 : ''}
                  </span>
                )}
              </Index>
            </div>
          </div>

          <Index each={playlist()?.tracks ?? []}>
            {(lane, index) => (
              <div class="wv-pl__row">
                <div class="wv-pl__head" style={{ width: `${HEAD_W}px`, height: `${LANE_H}px` }}>
                  <button
                    type="button"
                    class="wv-led"
                    classList={{ 'is-muted': lane().muted }}
                    title={lane().muted ? 'Unmute this lane' : 'Mute this lane'}
                    onClick={() => {
                      void app.sequencerCommand((backend) =>
                        backend.setPlaylistTrackMute(index, !lane().muted),
                      );
                      app.say(`${lane().name} · Lane`, lane().muted ? 'Active' : 'Muted');
                    }}
                  />
                  <LaneName
                    name={lane().name}
                    onRename={(name) =>
                      void app.sequencerCommand((backend) =>
                        backend.renamePlaylistTrack(index, name),
                      )
                    }
                  />
                  <span class="wv-pl__count">
                    {playlist()?.clips.filter((clip) => clip.track === index).length ?? 0}
                  </span>
                  <button
                    type="button"
                    class="wv-pl__del"
                    disabled={(playlist()?.tracks.length ?? 1) <= 1}
                    classList={{ 'wv-disabled': (playlist()?.tracks.length ?? 1) <= 1 }}
                    title={
                      (playlist()?.tracks.length ?? 1) <= 1
                        ? 'An arrangement must keep at least one lane'
                        : 'Remove this lane and every clip on it'
                    }
                    onClick={() => removeLane(index, lane().name)}
                  >
                    <IconX />
                  </button>
                </div>
                <div
                  class="wv-pl__lane"
                  classList={{ 'is-muted': lane().muted, 'is-painting': view.playlistTool() === 'paint' }}
                  style={{
                    width: `${laneWidth()}px`,
                    height: `${LANE_H}px`,
                    'background-size': `${BAR_PX * view.playlistZoom() * 4}px 100%, ${BAR_PX * view.playlistZoom()}px 100%`,
                  }}
                  onPointerDown={(event) => onLanePointerDown(event, index)}
                  onDragOver={(event) => event.preventDefault()}
                  onDrop={(event) => onLaneDrop(event, index)}
                  onContextMenu={(event) => event.preventDefault()}
                >
                  <For each={playlist()?.clips.filter((clip) => clip.track === index) ?? []}>
                    {(clip) => (
                      <Clip
                        clip={clip}
                        pxPerTick={pxPerTick()}
                        name={clipName(clip)}
                        colour={
                          clip.kind === 'pattern'
                            ? (app.patterns().find((p) => p.id === clip.patternId)?.color ??
                              trackColor(clip.patternId ?? 0))
                            : 'var(--acc)'
                        }
                        waveform={clip.kind === 'audio' ? waveformFor(clip.sourceId!) : undefined}
                        selected={view.selectedClip() === clip.id}
                        onDown={(event, mode) => onClipPointerDown(event, clip, mode)}
                        onDelete={() => removeClip(clip)}
                        onOpen={() => {
                          if (clip.kind !== 'pattern' || clip.patternId === undefined) return;
                          void app.sequencerCommand((backend) =>
                            backend.setActivePattern(clip.patternId!),
                          );
                          view.go('rack');
                        }}
                      />
                    )}
                  </For>
                </div>
              </div>
            )}
          </Index>

          <div class="wv-pl__row">
            <button
              type="button"
              class="wv-pl__add"
              style={{ width: `${HEAD_W}px` }}
              disabled={(playlist()?.tracks.length ?? 0) >= (playlist()?.maxTracks ?? 16)}
              classList={{
                'wv-disabled': (playlist()?.tracks.length ?? 0) >= (playlist()?.maxTracks ?? 16),
              }}
              title={
                (playlist()?.tracks.length ?? 0) >= (playlist()?.maxTracks ?? 16)
                  ? 'The engine allows no more lanes'
                  : 'Add a lane'
              }
              onClick={() => void app.sequencerCommand((backend) => backend.addPlaylistTrack())}
            >
              + Add lane
            </button>
            <div class="wv-pl__addfill" style={{ width: `${laneWidth()}px` }} />
          </div>

          <div class="wv-pl__playhead" ref={playheadEl} style={{ left: `${HEAD_W}px` }} />
        </div>

        <Show when={(playlist()?.clips.length ?? 0) === 0}>
          <div class="wv-pl__hint">
            <strong>The arrangement is empty.</strong>
            <span class="wv-empty__hint">
              Pick a pattern in the Picker on the right, then click a lane with the paint tool — or
              drag one straight onto a lane. Switch the transport to SONG to hear it.
            </span>
          </div>
        </Show>
      </div>
    </div>
  );
}

/** A lane's name, renamed in place. */
function LaneName(props: { name: string; onRename: (name: string) => void }): ReturnType<typeof Show> {
  const [editing, setEditing] = createSignal(false);
  return (
    <Show
      when={editing()}
      fallback={
        <button
          type="button"
          class="wv-pl__name"
          title="Double-click to rename"
          onDblClick={() => setEditing(true)}
        >
          {props.name}
        </button>
      }
    >
      <input
        class="wv-pl__nameinput"
        value={props.name}
        ref={(el) => queueMicrotask(() => el.select())}
        onKeyDown={(event) => {
          event.stopPropagation();
          if (event.key === 'Escape') setEditing(false);
          if (event.key !== 'Enter') return;
          const name = event.currentTarget.value.trim();
          setEditing(false);
          if (name && name !== props.name) props.onRename(name);
        }}
        onBlur={(event) => {
          const name = event.currentTarget.value.trim();
          setEditing(false);
          if (name && name !== props.name) props.onRename(name);
        }}
      />
    </Show>
  );
}

/** One clip: a coloured block with either a note sketch or a real waveform. */
function Clip(props: {
  clip: ClipDto;
  pxPerTick: number;
  name: string;
  colour: string;
  waveform: WaveformDto | undefined;
  selected: boolean;
  onDown: (event: PointerEvent, mode: 'move' | 'resize') => void;
  onDelete: () => void;
  onOpen: () => void;
}): ReturnType<typeof Show> {
  /** The waveform as one SVG path, drawn from the engine's own min/max pairs. */
  const path = () => {
    const wave = props.waveform;
    if (!wave || wave.peaks.length === 0) return '';
    const step = 100 / wave.peaks.length;
    const top = wave.peaks.map((pair, index) => `${index * step},${50 - pair[1] * 46}`);
    const bottom = wave.peaks
      .map((pair, index) => `${(wave.peaks.length - 1 - index) * step},${50 - pair[0] * 46}`)
      .reverse()
      .reverse();
    return `M${top.join(' L')} L${bottom.join(' L')} Z`;
  };

  return (
    <div
      class="wv-clip"
      classList={{ 'is-selected': props.selected }}
      style={{
        left: `${props.clip.start * props.pxPerTick}px`,
        width: `${Math.max(6, props.clip.length * props.pxPerTick - 2)}px`,
        background: alpha(props.colour === 'var(--acc)' ? '#4f8ef7' : props.colour, 0.18),
        'box-shadow': `inset 0 0 0 1px ${alpha(props.colour === 'var(--acc)' ? '#4f8ef7' : props.colour, props.selected ? 1 : 0.55)}`,
      }}
      onPointerDown={(event) => props.onDown(event, 'move')}
      onDblClick={() => props.onOpen()}
      onContextMenu={(event) => {
        event.preventDefault();
        event.stopPropagation();
        props.onDelete();
      }}
      title={
        props.clip.kind === 'pattern'
          ? `${props.name} — drag to move, drag the right edge to resize, double-click to edit, right-click to delete`
          : `${props.name} — audio clip. Audio clips play into the master bus; per-lane routing is not implemented yet.`
      }
    >
      <span class="wv-clip__label">{props.name}</span>
      <Show when={props.clip.kind === 'audio' && props.waveform}>
        <svg class="wv-clip__wave" viewBox="0 0 100 100" preserveAspectRatio="none">
          <path d={path()} fill="currentColor" />
        </svg>
      </Show>
      <span class="wv-clip__grip" onPointerDown={(event) => props.onDown(event, 'resize')} />
    </div>
  );
}
