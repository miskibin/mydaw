/**
 * The floating window for a rack channel: the waveform, the played region, and
 * the three knobs a sampled channel has.
 *
 * Clip and one-shot channels share it -- they are the same file behind
 * different triggers -- and everything else (a kick, a piano, the test tone)
 * still opens here for a preview of one hit, minus the region controls it has
 * no frames to apply to.
 *
 * ## Why the knobs keep a local copy
 *
 * Every control here writes to the engine and draws from `local()` until a
 * debounced `get_tracks` reconciles it, which is what the mixer's fader already
 * does. The window used to call `backend.setTrackPitch` and *drop the
 * `MixerState` it returned on the floor*: the engine moved, the drawn knob did
 * not, and since the next drag starts from the value the knob is showing, a
 * second gesture pulled the parameter back to where the first one began. A
 * control that does not move under the hand reads as a dead control, and this
 * one was as good as dead.
 *
 * ## Why the region commits on release
 *
 * Gain, pan and pitch are continuous and audible: they go to the engine on
 * every step of the gesture. The region is not -- setting it recompiles the
 * arrangement (a clip channel) or rebuilds the RT node (a one-shot) -- so the
 * drag paints locally and the engine hears about it once, on release.
 */

import { For, Show, createEffect, createMemo, createSignal, on, onCleanup, type JSX } from 'solid-js';

import type { SourcePreviewDto, TrackDto, WaveformDto } from '../api/types';
import { channelKind, isAudioClipChannel } from '../lib/channel';
import {
  dragRegion,
  formatFrames,
  frameAt,
  framePercent,
  pitchLabel,
  resolveRegion,
  stepSemitone,
  type Region,
} from '../lib/sampleRegion';
import { useAppState } from '../state';
import { useView } from '../shell/view';
import { Knob, spec } from '../ui/Knob';
import { PluginWindow } from '../ui/PluginWindow';
import { WHEEL_COMMIT_MS, debounce } from '../ui/drag';
import { IconPlay, IconX } from '../ui/icons';
import { trackColor } from '../theme/tokens';

import '../styles/sample.css';

/** How long a preview note is held before it is released. */
const AUDITION_MS = 400;
/** Middle C, which the single drums read as their own base pitch. */
const AUDITION_KEY = 60;
/** Drum kit only answers GM keys. */
const KIT_KEY = 36;
/** Travel under this is a click, not a drag: it must not redraw the region. */
const DRAG_SLOP_PX = 3;

const PAN_DEF = spec('pan', 'Pan', '', -1, 1, 0);
const PITCH_DEF = spec('pitch', 'Pitch', 'ct', -1200, 1200, 0);
/**
 * The fades, in milliseconds.
 *
 * Two seconds of travel rather than the engine's ten-second ceiling: a fade
 * knob is used for tens of milliseconds far more often than for seconds, and a
 * range ten times longer than the useful one is a knob you cannot set to 20 ms.
 * The engine still accepts up to ten seconds from a project file.
 */
const FADE_IN_DEF = spec('fadeIn', 'Fade in', 'ms', 0, 2_000, 0);
const FADE_OUT_DEF = spec('fadeOut', 'Fade out', 'ms', 0, 2_000, 0);

/** Which edge of the region a pointer gesture is moving. */
type Grab = 'start' | 'end' | 'new';

export function SampleEditor(): JSX.Element {
  const app = useAppState();
  const view = useView();
  const [fileWave, setFileWave] = createSignal<WaveformDto | null>(null);
  const [preview, setPreview] = createSignal<SourcePreviewDto | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [loading, setLoading] = createSignal(false);

  // What the window draws while the engine's copy is still catching up.
  const [localDb, setLocalDb] = createSignal<number | null>(null);
  const [localPan, setLocalPan] = createSignal<number | null>(null);
  const [localCents, setLocalCents] = createSignal<number | null>(null);
  const [localFades, setLocalFades] = createSignal<[number, number] | null>(null);
  const [localRegion, setLocalRegion] = createSignal<Region | null>(null);

  const track = (): TrackDto | undefined => {
    const index = view.sampleEditor();
    if (index === null) return undefined;
    return app.tracks()[index];
  };

  const samplePlay = (): boolean => {
    const current = track();
    return !!current && (current.engine === 'clip' || current.engine === 'sample');
  };

  const settle = debounce(WHEEL_COMMIT_MS, () => {
    void app.mixerCommand((backend) => backend.getTracks()).finally(() => {
      setLocalDb(null);
      setLocalPan(null);
      setLocalCents(null);
      setLocalFades(null);
      setLocalRegion(null);
    });
  });
  onCleanup(() => settle.cancel());

  /**
   * What the waveform panel shows, as a stable key — not the `TrackDto`
   * itself, which is a fresh object on every mixer echo. Keying the fetch on
   * the object would flash "rendering…" and refetch on every knob commit.
   */
  const sourceKey = createMemo((): string | null => {
    const current = track();
    if (!current) return null;
    return current.audioSourceId != null
      ? `file:${current.audioSourceId}`
      : `inst:${current.sourceId}`;
  });

  // A different channel is a different sound: nothing local survives the swap.
  createEffect(
    on(sourceKey, () => {
      setLocalDb(null);
      setLocalPan(null);
      setLocalCents(null);
      setLocalFades(null);
      setLocalRegion(null);
    }),
  );

  createEffect(
    on(sourceKey, (key) => {
      if (key === null) {
        setFileWave(null);
        setPreview(null);
        setError(null);
        setLoading(false);
        return;
      }
      setLoading(true);
      setError(null);
      let cancelled = false;
      if (key.startsWith('file:')) {
        const sourceId = Number(key.slice('file:'.length));
        setPreview(null);
        app.backend
          .getAudioWaveform(sourceId, 1_024)
          .then((wave) => {
            if (cancelled) return;
            setFileWave(wave);
            setLoading(false);
          })
          .catch((err: unknown) => {
            if (cancelled) return;
            setFileWave(null);
            setError(err instanceof Error ? err.message : String(err));
            setLoading(false);
          });
      } else {
        const sourceId = key.slice('inst:'.length);
        setFileWave(null);
        app.backend
          .previewSource(sourceId, 512)
          .then((wave) => {
            if (cancelled) return;
            setPreview(wave);
            setLoading(false);
          })
          .catch((err: unknown) => {
            if (cancelled) return;
            setPreview(null);
            setError(err instanceof Error ? err.message : String(err));
            setLoading(false);
          });
      }
      onCleanup(() => {
        cancelled = true;
      });
    }),
  );

  createEffect(() => {
    if (app.mixer() && view.sampleEditor() !== null && !track()) {
      view.closeSampleEditor();
    }
  });

  let timer: ReturnType<typeof setTimeout> | undefined;
  let held: number | null = null;

  function release(): void {
    if (held === null) return;
    const key = held;
    held = null;
    app.backend.noteOff(key).catch(() => {});
  }

  function audition(): void {
    const current = track();
    if (!current) return;
    void app.mixerCommand((backend) => backend.setSelectedTrack(current.index));
    if (isAudioClipChannel(current)) {
      const path = app.audioSources().find((source) => source.id === current.audioSourceId)?.path;
      if (path) void app.backend.previewAudioFile(path).catch(app.report);
      return;
    }
    clearTimeout(timer);
    release();
    const key = current.sourceId === 'drum-kit' ? KIT_KEY : AUDITION_KEY;
    held = key;
    app.backend.noteOn(key, 0.9).catch(app.report);
    timer = setTimeout(release, AUDITION_MS);
  }

  onCleanup(() => {
    clearTimeout(timer);
    release();
  });

  const kind = () => {
    const current = track();
    return current ? channelKind(current) : null;
  };
  const colour = () => trackColor(track()?.id ?? 0);
  const owner = () => track()?.name ?? 'Sample';
  const file = () => app.audioSources().find((source) => source.id === track()?.audioSourceId);
  const sampleRate = () => file()?.sampleRate ?? 0;

  const peaks = (): [number, number][] => fileWave()?.peaks ?? preview()?.peaks ?? [];
  const peak = (): number => {
    const pairs = peaks();
    let max = 0.0001;
    for (const pair of pairs) max = Math.max(max, Math.abs(pair[0]), Math.abs(pair[1]));
    return max;
  };
  const path = (): string => {
    const pairs = peaks();
    if (pairs.length === 0) return '';
    const scale = peak();
    const step = 100 / pairs.length;
    const top = pairs.map((pair, index) => `${index * step},${50 - (pair[1] / scale) * 46}`);
    const bottom = pairs
      .map((pair, index) => `${index * step},${50 - (pair[0] / scale) * 46}`)
      .reverse();
    return `M${top.join(' L')} L${bottom.join(' L')} Z`;
  };

  // --- the region ----------------------------------------------------------

  const totalFrames = (): number => fileWave()?.frames ?? 0;

  const region = (): Region => {
    const local = localRegion();
    if (local) return local;
    const current = track();
    return resolveRegion(
      current?.sampleStartFrames ?? 0,
      current?.sampleEndFrames ?? 0,
      totalFrames(),
    );
  };

  const startPct = () => framePercent(region().start, totalFrames());
  const endPct = () => framePercent(region().end, totalFrames());
  /** Whether the region is anything other than the whole file. */
  const trimmed = () => {
    const { start, end } = region();
    return start > 0 || (end > 0 && end < totalFrames());
  };

  function commitRegion(next: Region & { storedEnd: number }): void {
    const current = track();
    if (!current) return;
    setLocalRegion({ start: next.start, end: next.end });
    void app.backend
      .setTrackSampleRegion(current.index, next.start, next.storedEnd)
      .catch(app.report);
    settle();
  }

  function resetRegion(): void {
    const total = totalFrames();
    if (total <= 0) return;
    commitRegion({ start: 0, end: total, storedEnd: 0 });
    app.say(`${owner()} · Region`, 'whole file');
  }

  /**
   * Drag one edge of the region, or draw a new one on the body.
   *
   * The edges are their own elements with a real grab width, rather than the
   * old "whichever end of the region your click was nearer to" — which meant a
   * click in the middle of a long sample moved an edge you were not pointing at
   * and could not see coming.
   */
  function onRegionPointer(event: PointerEvent, grab: Grab): void {
    if (event.button !== 0 || !samplePlay()) return;
    const total = totalFrames();
    if (total <= 0) return;
    const box = waveEl?.getBoundingClientRect();
    if (!box) return;
    event.preventDefault();
    event.stopPropagation();

    const startX = event.clientX;
    const at = (clientX: number) => frameAt((clientX - box.left) / box.width, total);
    const held = region();
    // A new region is anchored where the press landed; an edge drag is anchored
    // on the *other* edge, so dragging one end past the other flips the region
    // rather than collapsing it.
    const anchor = grab === 'start' ? held.end : grab === 'end' ? held.start : at(event.clientX);
    let moved = grab !== 'new';
    let latest = dragRegion(anchor, grab === 'new' ? anchor : at(event.clientX), total);

    const apply = (clientX: number) => {
      latest = dragRegion(anchor, at(clientX), total);
      setLocalRegion({ start: latest.start, end: latest.end });
      app.say(
        `${owner()} · Region`,
        `${formatFrames(latest.start, sampleRate())} – ${formatFrames(latest.end, sampleRate())}`,
      );
    };
    if (grab !== 'new') apply(event.clientX);

    const onMove = (next: PointerEvent) => {
      if (!moved && Math.abs(next.clientX - startX) < DRAG_SLOP_PX) return;
      moved = true;
      apply(next.clientX);
    };
    const onUp = () => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      window.removeEventListener('pointercancel', onUp);
      document.body.style.removeProperty('cursor');
      document.body.style.removeProperty('user-select');
      if (!moved) {
        setLocalRegion(null);
        return;
      }
      commitRegion(latest);
    };
    document.body.style.setProperty('cursor', 'ew-resize');
    document.body.style.setProperty('user-select', 'none');
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
    window.addEventListener('pointercancel', onUp);
  }

  let waveEl: HTMLDivElement | undefined;

  // --- the knobs -----------------------------------------------------------

  const mixer = () => app.mixer();
  const minGain = () => mixer()?.minGainDb ?? -60;
  const maxGain = () => mixer()?.maxGainDb ?? 6;
  const volDef = () => spec('vol', 'Vol', 'dB', minGain(), maxGain(), 0);

  const gainDb = () => localDb() ?? track()?.gainDb ?? 0;
  const pan = () => localPan() ?? track()?.pan ?? 0;
  const cents = () => localCents() ?? track()?.pitchCents ?? 0;
  const fadeIn = () => localFades()?.[0] ?? track()?.fadeInMs ?? 0;
  const fadeOut = () => localFades()?.[1] ?? track()?.fadeOutMs ?? 0;

  function setGain(db: number): void {
    const current = track();
    if (!current) return;
    setLocalDb(db);
    void app.backend.setTrackGain(current.index, db).catch(app.report);
    settle();
  }

  function setPan(value: number): void {
    const current = track();
    if (!current) return;
    const next = Math.round(value * 100) / 100;
    setLocalPan(next);
    void app.backend.setTrackPan(current.index, next).catch(app.report);
    settle();
  }

  function setPitch(value: number): void {
    const current = track();
    if (!current) return;
    const next = Math.round(value);
    setLocalCents(next);
    void app.backend.setTrackPitch(current.index, next).catch(app.report);
    settle();
  }

  /**
   * Both fades in one call, because the engine takes them together.
   *
   * Setting one at a time would republish the arrangement twice per gesture --
   * a fade edit recompiles every clip of the file, which is the one thing on
   * this window that is not a single atomic store.
   */
  function setFades(fadeInMs: number, fadeOutMs: number): void {
    const current = track();
    if (!current) return;
    const next: [number, number] = [Math.max(0, Math.round(fadeInMs)), Math.max(0, Math.round(fadeOutMs))];
    setLocalFades(next);
    void app.backend.setTrackFades(current.index, next[0], next[1]).catch(app.report);
    settle();
  }

  function transpose(delta: number): void {
    const next = stepSemitone(cents(), delta);
    if (next === cents()) return;
    setPitch(next);
    app.say(`${owner()} · Pitch`, pitchLabel(next));
  }

  return (
    <Show when={track()}>
      {(current) => (
        <PluginWindow
          title={current().name}
          color={colour()}
          z={80}
          focused
          defaultWidth={660}
          defaultHeight={440}
          offset={0}
          onFocus={() => {}}
          onClose={() => view.closeSampleEditor()}
        >
          <div class="wv-sample wv-sample--window">
            <div class="wv-sample__tools">
              <Show when={kind()}>
                {(info) => (
                  <span
                    class={`wv-chrow__kind wv-chrow__kind--${info().kind}`}
                    title={info().title}
                  >
                    {info().badge}
                  </span>
                )}
              </Show>
              <span class="wv-sample__file" title={file()?.path ?? current().sourceLabel}>
                {file()?.name ?? current().sourceLabel}
              </span>
              <span class="wv-spacer" />
              <Show when={samplePlay() && trimmed()}>
                <button
                  type="button"
                  class="wv-chip"
                  title="Play the whole file again — the same as double-clicking the wave"
                  onClick={resetRegion}
                >
                  <IconX />
                  Full file
                </button>
              </Show>
              <button
                type="button"
                class="wv-sample__play"
                title="Preview this sound through the selected channel"
                onClick={audition}
              >
                <IconPlay />
                Preview
              </button>
            </div>

            <div
              class="wv-sample__wave"
              classList={{ 'is-edit': samplePlay() }}
              style={{ color: colour() }}
              ref={waveEl}
              onPointerDown={(event) => onRegionPointer(event, 'new')}
              onDblClick={() => samplePlay() && resetRegion()}
              title={
                samplePlay()
                  ? 'Drag across the wave to set the played region, drag a handle to move one end, double-click for the whole file'
                  : undefined
              }
            >
              <Show when={loading()}>
                <span class="wv-sample__msg">rendering…</span>
              </Show>
              <Show when={!loading() && error()}>
                <span class="wv-sample__msg">{error()}</span>
              </Show>
              <Show when={!loading() && preview() && (preview()?.peak ?? 1) < 0.001}>
                <span class="wv-sample__msg">
                  silent — this instrument has no samples loaded yet
                </span>
              </Show>
              <Show when={!loading() && peaks().length > 0}>
                <span class="wv-sample__axis" />
                <svg class="wv-sample__svg" viewBox="0 0 100 100" preserveAspectRatio="none">
                  <path d={path()} fill="currentColor" />
                </svg>
              </Show>
              <Show when={samplePlay() && peaks().length > 0 && totalFrames() > 0}>
                <div class="wv-sample__shade" style={{ width: `${startPct()}%` }} />
                <div
                  class="wv-sample__shade wv-sample__shade--end"
                  style={{ width: `${100 - endPct()}%` }}
                />
                <div
                  class="wv-sample__handle"
                  style={{ left: `${startPct()}%` }}
                  title="Where the sample starts — drag"
                  onPointerDown={(event) => onRegionPointer(event, 'start')}
                >
                  <span class="wv-sample__flag">{formatFrames(region().start, sampleRate())}</span>
                </div>
                <div
                  class="wv-sample__handle wv-sample__handle--end"
                  style={{ left: `${endPct()}%` }}
                  title="Where the sample ends — drag"
                  onPointerDown={(event) => onRegionPointer(event, 'end')}
                >
                  <span class="wv-sample__flag">{formatFrames(region().end, sampleRate())}</span>
                </div>
              </Show>
            </div>

            <div class="wv-sample__knobs">
              <Knob
                def={volDef()}
                value={gainDb}
                onInput={setGain}
                owner={owner()}
                size={38}
                color={colour()}
              />
              <Knob
                def={PAN_DEF}
                value={pan}
                onInput={setPan}
                owner={owner()}
                size={38}
                color={colour()}
              />
              <Show when={samplePlay()}>
                <Knob
                  def={FADE_IN_DEF}
                  value={fadeIn}
                  onInput={(ms) => setFades(ms, fadeOut())}
                  owner={owner()}
                  size={38}
                  color={colour()}
                  title="Fade in — every clip (or note) of this channel starts from silence over this long"
                />
                <Knob
                  def={FADE_OUT_DEF}
                  value={fadeOut}
                  onInput={(ms) => setFades(fadeIn(), ms)}
                  owner={owner()}
                  size={38}
                  color={colour()}
                  title="Fade out — silence is reached at the clip's end, not after it"
                />
                <div class="wv-sample__pitch">
                  <Knob
                    def={PITCH_DEF}
                    value={cents}
                    onInput={setPitch}
                    owner={owner()}
                    size={38}
                    color={colour()}
                    title="Pitch in cents — drag or scroll, shift for fine, double-click for the root"
                  />
                  <div class="wv-sample__steps">
                    <For each={[-12, -1, 1, 12]}>
                      {(delta) => (
                        <button
                          type="button"
                          class="wv-sample__step"
                          title={`${delta > 0 ? 'Up' : 'Down'} ${Math.abs(delta)} semitone${
                            Math.abs(delta) > 1 ? 's' : ''
                          }`}
                          onClick={() => transpose(delta)}
                        >
                          {delta > 0 ? `+${delta}` : delta}
                        </button>
                      )}
                    </For>
                  </div>
                  <span class="wv-sample__pitchlabel">{pitchLabel(cents())}</span>
                </div>
              </Show>
            </div>

            <div class="wv-sample__foot">
              <span class="wv-sample__desc">{current().sourceLabel}</span>
              <span class="wv-spacer" />
              <Show when={fileWave()}>
                {(wave) => (
                  <span class="wv-sample__meta">
                    {formatFrames(wave().frames, sampleRate())}
                    {sampleRate() > 0 ? ` · ${(sampleRate() / 1000).toFixed(1)} kHz` : ''}
                    {samplePlay() && trimmed()
                      ? ` · plays ${formatFrames(region().end - region().start, sampleRate())}`
                      : ''}
                  </span>
                )}
              </Show>
            </div>
          </div>
        </PluginWindow>
      )}
    </Show>
  );
}
