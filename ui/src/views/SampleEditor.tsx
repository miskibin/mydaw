/**
 * The floating window for a rack channel.
 *
 * Clip and one-shot channels share this editor: waveform, start/end, volume,
 * pan and pitch. Everything else — a kick, a piano, the test tone — still
 * opens here for a preview of one hit.
 *
 * The sliders write the engine per frame and draw from a local overlay until a
 * debounced `get_tracks` comes back. Routing every pixel through `mixerCommand`
 * is how a control only moves as fast as the round trip — the same defect the
 * mixer's faders and the inspector knobs already had to be cured of.
 */

import { Show, createEffect, createMemo, createSignal, on, onCleanup, type JSX } from 'solid-js';

import type { ParamDefDto, SourcePreviewDto, TrackDto, WaveformDto } from '../api/types';
import { channelKind, isAudioClipChannel } from '../lib/channel';
import { formatPan, formatValue } from '../lib/param';
import { useAppState } from '../state';
import { useView } from '../shell/view';
import { debounce, WHEEL_COMMIT_MS } from '../ui/drag';
import { PluginWindow } from '../ui/PluginWindow';
import { Slider } from '../ui/Slider';
import { IconPlay } from '../ui/icons';
import { spec } from '../ui/Knob';
import { trackColor } from '../theme/tokens';

import '../styles/sample.css';

/** How long a preview note is held before it is released. */
const AUDITION_MS = 400;
/** Middle C, which the single drums read as their own base pitch. */
const AUDITION_KEY = 60;
/** Drum kit only answers GM keys. */
const KIT_KEY = 36;

const PAN_DEF = spec('pan', 'Pan', '', -1, 1, 0);
const PITCH_DEF: ParamDefDto = { ...spec('pitch', 'Pitch', 'ct', -1200, 1200, 0), step: 1 };

interface SampleDraft {
  gainDb?: number;
  pan?: number;
  pitchCents?: number;
  startFrames?: number;
  endFrames?: number;
}

export function SampleEditor(): JSX.Element {
  const app = useAppState();
  const view = useView();
  const [fileWave, setFileWave] = createSignal<WaveformDto | null>(null);
  const [preview, setPreview] = createSignal<SourcePreviewDto | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [draft, setDraft] = createSignal<SampleDraft>({});

  const track = (): TrackDto | undefined => {
    const index = view.sampleEditor();
    if (index === null) return undefined;
    return app.tracks()[index];
  };

  const samplePlay = (): boolean => {
    const current = track();
    return !!current && (current.engine === 'clip' || current.engine === 'sample');
  };

  /**
   * What the waveform panel shows, as a stable key — not the `TrackDto`
   * itself, which is a fresh object on every mixer echo. Keying the fetch on
   * the object would flash "rendering…" and refetch on every slider commit.
   */
  const sourceKey = createMemo((): string | null => {
    const current = track();
    if (!current) return null;
    return current.audioSourceId != null
      ? `file:${current.audioSourceId}`
      : `inst:${current.sourceId}`;
  });

  createEffect(
    on(sourceKey, (key) => {
      setDraft({});
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
          .getAudioWaveform(sourceId, 512)
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
      if (path) {
        void app.backend
          .previewAudioFile(path, {
            pitchCents: pitchCents(),
            startFrames: startFrames(),
            endFrames: current.sampleEndFrames === 0 ? 0 : endFrames(),
          })
          .catch(app.report);
      }
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

  const totalFrames = (): number => fileWave()?.frames ?? 0;
  const startFrames = (): number => draft().startFrames ?? track()?.sampleStartFrames ?? 0;
  const endFrames = (): number => {
    const overlay = draft().endFrames;
    if (overlay !== undefined) return overlay;
    const current = track();
    const total = totalFrames();
    if (!current || current.sampleEndFrames === 0) return total;
    return Math.min(current.sampleEndFrames, total);
  };
  const startPct = (): number => {
    const total = totalFrames();
    return total > 0 ? (startFrames() / total) * 100 : 0;
  };
  const endPct = (): number => {
    const total = totalFrames();
    return total > 0 ? (endFrames() / total) * 100 : 100;
  };

  const gainDb = (): number => draft().gainDb ?? track()?.gainDb ?? 0;
  const pan = (): number => draft().pan ?? track()?.pan ?? 0;
  const pitchCents = (): number => draft().pitchCents ?? track()?.pitchCents ?? 0;

  const mixer = () => app.mixer();
  const minGain = () => mixer()?.minGainDb ?? -60;
  const maxGain = () => mixer()?.maxGainDb ?? 6;
  const volDef = () => spec('vol', 'Vol', 'dB', minGain(), maxGain(), 0);
  const startDef = (): ParamDefDto =>
    spec('start', 'Start', '', 0, Math.max(1, totalFrames()), 0);
  const endDef = (): ParamDefDto =>
    spec('end', 'End', '', 0, Math.max(1, totalFrames()), Math.max(1, totalFrames()));

  const settle = debounce(WHEEL_COMMIT_MS, () => {
    void app.mixerCommand((backend) => backend.getTracks()).finally(() => setDraft({}));
  });
  onCleanup(() => settle.cancel());

  function patch(next: SampleDraft): void {
    setDraft((prev) => ({ ...prev, ...next }));
  }

  function setGain(db: number): void {
    const current = track();
    if (!current) return;
    patch({ gainDb: db });
    void app.backend.setTrackGain(current.index, db).catch(app.report);
  }

  function setPan(value: number): void {
    const current = track();
    if (!current) return;
    patch({ pan: value });
    void app.backend.setTrackPan(current.index, value).catch(app.report);
  }

  function setPitch(cents: number): void {
    const current = track();
    if (!current) return;
    patch({ pitchCents: cents });
    void app.backend.setTrackPitch(current.index, cents).catch(app.report);
  }

  function setRegion(start: number, end: number): void {
    const current = track();
    if (!current) return;
    const total = totalFrames();
    const lo = Math.max(0, Math.min(start, total));
    let hi = Math.max(0, Math.min(end, total));
    if (hi > 0 && hi <= lo) hi = total;
    const storedEnd = hi >= total ? 0 : hi;
    patch({ startFrames: Math.floor(lo), endFrames: hi >= total ? total : Math.floor(hi) });
    void app.backend
      .setTrackSampleRegion(current.index, Math.floor(lo), Math.floor(storedEnd))
      .catch(app.report);
  }

  function onWavePointer(event: PointerEvent): void {
    if (event.button !== 0 || !samplePlay()) return;
    const target = event.currentTarget as HTMLElement;
    const total = totalFrames();
    if (total <= 0) return;
    event.preventDefault();
    settle.cancel();
    const framesAt = (clientX: number) => {
      const rect = target.getBoundingClientRect();
      const x = Math.min(1, Math.max(0, (clientX - rect.left) / rect.width));
      return Math.round(x * total);
    };
    const start = startFrames();
    const end = endFrames();
    const at = framesAt(event.clientX);
    const moveStart = Math.abs(at - start) <= Math.abs(at - end);
    const apply = (clientX: number) => {
      const next = framesAt(clientX);
      if (moveStart) setRegion(next, end);
      else setRegion(start, next);
    };
    apply(event.clientX);
    const onMove = (next: PointerEvent) => apply(next.clientX);
    const onUp = () => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      window.removeEventListener('pointercancel', onUp);
      settle();
    };
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
    window.addEventListener('pointercancel', onUp);
  }

  return (
    <Show when={track()}>
      {(current) => (
        <PluginWindow
          title={current().name}
          color={colour()}
          z={80}
          focused
          defaultWidth={560}
          defaultHeight={samplePlay() ? 460 : 400}
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
              <span class="wv-sample__hint">
                {samplePlay()
                  ? 'Drag the wave to crop · sliders write the channel'
                  : 'Preview one hit through this channel'}
              </span>
              <span class="wv-spacer" />
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
              onPointerDown={onWavePointer}
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
                <svg class="wv-sample__svg" viewBox="0 0 100 100" preserveAspectRatio="none">
                  <path d={path()} fill="currentColor" />
                </svg>
              </Show>
              <Show when={samplePlay() && peaks().length > 0}>
                <div class="wv-sample__shade" style={{ width: `${startPct()}%` }} />
                <div
                  class="wv-sample__shade wv-sample__shade--end"
                  style={{ width: `${100 - endPct()}%` }}
                />
                <div class="wv-sample__handle" style={{ left: `${startPct()}%` }} />
                <div class="wv-sample__handle" style={{ left: `${endPct()}%` }} />
              </Show>
            </div>

            <div class="wv-sample__sliders">
              <Show when={samplePlay() && totalFrames() > 0}>
                <Slider
                  def={startDef()}
                  value={() => startFrames()}
                  onInput={(frames) => setRegion(frames, endFrames())}
                  onCommit={() => settle()}
                  owner={owner()}
                  color={colour()}
                  format={(frames) => Math.round(frames).toLocaleString()}
                  title="Region start — drag the wave or this slider"
                />
                <Slider
                  def={endDef()}
                  value={() => endFrames()}
                  onInput={(frames) => setRegion(startFrames(), frames)}
                  onCommit={() => settle()}
                  owner={owner()}
                  color={colour()}
                  format={(frames) => Math.round(frames).toLocaleString()}
                  title="Region end — drag the wave or this slider"
                />
              </Show>
              <Slider
                def={volDef()}
                value={() => gainDb()}
                onInput={setGain}
                onCommit={() => settle()}
                owner={owner()}
                color={colour()}
              />
              <Slider
                def={PAN_DEF}
                value={() => pan()}
                onInput={setPan}
                onCommit={() => settle()}
                owner={owner()}
                color={colour()}
                format={formatPan}
                title="Pan — click or drag, double-click to centre"
              />
              <Show when={samplePlay()}>
                <Slider
                  def={PITCH_DEF}
                  value={() => pitchCents()}
                  onInput={setPitch}
                  onCommit={() => settle()}
                  owner={owner()}
                  color={colour()}
                  title="Pitch in cents — resample, not stretch. Shift for fine."
                />
              </Show>
            </div>

            <div class="wv-sample__foot">
              <span class="wv-sample__desc">{current().sourceLabel}</span>
              <span class="wv-spacer" />
              <Show when={samplePlay()}>
                <span class="wv-sample__meta">{formatValue('ct', pitchCents())}</span>
              </Show>
              <Show when={fileWave()}>
                {(wave) => (
                  <span class="wv-sample__meta">
                    {wave().frames.toLocaleString()} frames
                    {samplePlay()
                      ? ` · ${Math.round(startFrames())}–${
                          (draft().endFrames === undefined
                            ? current().sampleEndFrames === 0
                            : endFrames() >= totalFrames())
                            ? 'end'
                            : Math.round(endFrames())
                        }`
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
