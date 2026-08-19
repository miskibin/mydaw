/**
 * The floating window for a rack channel.
 *
 * Clip and one-shot channels share this editor: waveform, start/end, volume,
 * pan and pitch. Everything else — a kick, a piano, the test tone — still
 * opens here for a preview of one hit.
 */

import { Show, createEffect, createMemo, createSignal, on, onCleanup, type JSX } from 'solid-js';

import type { SourcePreviewDto, TrackDto, WaveformDto } from '../api/types';
import { channelKind, isAudioClipChannel } from '../lib/channel';
import { useAppState } from '../state';
import { useView } from '../shell/view';
import { Knob, spec } from '../ui/Knob';
import { PluginWindow } from '../ui/PluginWindow';
import { IconPlay } from '../ui/icons';
import { trackColor } from '../theme/tokens';

import '../styles/sample.css';

/** How long a preview note is held before it is released. */
const AUDITION_MS = 400;
/** Middle C, which the single drums read as their own base pitch. */
const AUDITION_KEY = 60;
/** Drum kit only answers GM keys. */
const KIT_KEY = 36;

const PAN_DEF = spec('pan', 'Pan', '', -1, 1, 0);
const PITCH_DEF = spec('pitch', 'Pitch', 'ct', -1200, 1200, 0);

export function SampleEditor(): JSX.Element {
  const app = useAppState();
  const view = useView();
  const [fileWave, setFileWave] = createSignal<WaveformDto | null>(null);
  const [preview, setPreview] = createSignal<SourcePreviewDto | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [loading, setLoading] = createSignal(false);

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
   * the object would flash "rendering…" and refetch on every knob commit.
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
  const startFrames = (): number => track()?.sampleStartFrames ?? 0;
  const endFrames = (): number => {
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

  function setRegion(start: number, end: number): void {
    const current = track();
    if (!current) return;
    const total = totalFrames();
    const lo = Math.max(0, Math.min(start, total));
    let hi = Math.max(0, Math.min(end, total));
    if (hi > 0 && hi <= lo) hi = total;
    const storedEnd = hi >= total ? 0 : hi;
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
    };
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
    window.addEventListener('pointercancel', onUp);
  }

  const mixer = () => app.mixer();
  const minGain = () => mixer()?.minGainDb ?? -60;
  const maxGain = () => mixer()?.maxGainDb ?? 6;
  const volDef = () => spec('vol', 'Vol', 'dB', minGain(), maxGain(), 0);

  return (
    <Show when={track()}>
      {(current) => (
        <PluginWindow
          title={current().name}
          color={colour()}
          z={80}
          focused
          defaultWidth={640}
          defaultHeight={420}
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

            <div class="wv-sample__knobs">
              <Knob
                def={volDef()}
                value={() => current().gainDb}
                onInput={(db) =>
                  void app.backend.setTrackGain(current().index, db).catch(app.report)
                }
                owner={owner()}
                size={36}
                color={colour()}
              />
              <Knob
                def={PAN_DEF}
                value={() => current().pan}
                onInput={(pan) =>
                  void app.backend.setTrackPan(current().index, pan).catch(app.report)
                }
                owner={owner()}
                size={36}
                color={colour()}
              />
              <Show when={samplePlay()}>
                <Knob
                  def={PITCH_DEF}
                  value={() => current().pitchCents}
                  onInput={(cents) =>
                    void app.backend.setTrackPitch(current().index, cents).catch(app.report)
                  }
                  owner={owner()}
                  size={36}
                  color={colour()}
                  title="Pitch in cents — shift for fine"
                />
              </Show>
            </div>

            <div class="wv-sample__foot">
              <span class="wv-sample__desc">{current().sourceLabel}</span>
              <span class="wv-spacer" />
              <Show when={fileWave()}>
                {(wave) => (
                  <span class="wv-sample__meta">
                    {wave().frames.toLocaleString()} frames
                    {samplePlay()
                      ? ` · ${Math.round(startFrames())}–${
                          current().sampleEndFrames === 0 ? 'end' : Math.round(endFrames())
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
