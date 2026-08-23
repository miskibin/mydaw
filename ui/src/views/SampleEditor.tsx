/**
 * The floating window for a rack channel: the waveform, the played region, and
 * the three knobs a sampled channel has.
 *
 * Clip and one-shot channels share it -- they are the same file behind
 * different triggers. Sounds-tab drums are one-shots too, so Kick/Snare get
 * the waveform, fades and region. Poly Synth keeps its own editor; the test
 * tone still opens here for a preview of one hit, minus the region controls
 * it has no frames to apply to.
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

import type { ParamDefDto, PitchAnalysis, SourcePreviewDto, TrackDto, WaveformDto } from '../api/types';
import { channelColor, channelKind } from '../lib/channel';
import { noteName } from '../lib/pattern';
import {
  decideWaveformAction,
  formatDetectedPitch,
  noPitchReasonCopy,
  pitchAnalysisKey,
  targetTransposeCents,
} from '../lib/samplePitch';
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
import { stereoWaveformPaths } from '../lib/sampleWaveform';
import { usesPolySynthEditor } from '../lib/polySynth';
import { useAppState } from '../state';
import { useView } from '../shell/view';
import { Knob, spec } from '../ui/Knob';
import { PluginWindow } from '../ui/PluginWindow';
import { SampleKeyboard } from '../ui/SampleKeyboard';
import { WHEEL_COMMIT_MS, debounce } from '../ui/drag';
import { IconX } from '../ui/icons';
import { PolySynthEditor } from './synth/PolySynthEditor';

import '../styles/sample.css';

/** How long a preview note is held before it is released. */
const AUDITION_MS = 400;
/** Middle C, which the single drums read as their own base pitch. */
const AUDITION_KEY = 60;
/** Drum kit only answers GM keys. */
const KIT_KEY = 36;
/** Travel under this is a click, not a drag: it must not redraw the region. */
const DRAG_SLOP_PX = 3;
const TARGET_NOTES = Array.from({ length: 61 }, (_, index) => 36 + index);

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
  /** Instrument knobs mid-drag, by parameter id. See `setSourceParam`. */
  const [localParams, setLocalParams] = createSignal<Record<string, number>>({});
  const [localRegion, setLocalRegion] = createSignal<Region | null>(null);
  const [analyzing, setAnalyzing] = createSignal(false);
  const [analysis, setAnalysis] = createSignal<{ key: string; result: PitchAnalysis } | null>(
    null,
  );
  const [kbCenter, setKbCenter] = createSignal(60);
  const [targetNote, setTargetNote] = createSignal(60);
  let pendingSampleEdit: Promise<void> = Promise.resolve();
  let noteGate: Promise<void> = Promise.resolve();
  let analysisGen = 0;
  const sampleEditFailed = Symbol('sample edit failed');

  const track = (): TrackDto | undefined => {
    const index = view.sampleEditor();
    if (index === null) return undefined;
    return app.tracks()[index];
  };

  const samplePlay = (): boolean => {
    const current = track();
    return !!current && (current.engine === 'clip' || current.engine === 'sample');
  };

  async function waitForSampleEdits(): Promise<void> {
    for (;;) {
      const pending = pendingSampleEdit;
      await pending.catch(() => undefined);
      if (pending === pendingSampleEdit) return;
    }
  }

  const settle = debounce(WHEEL_COMMIT_MS, () => {
    const trackId = track()?.id;
    void waitForSampleEdits()
      .then(() => {
        if (track()?.id !== trackId) return;
        return app.mixerCommand((backend) => backend.getTracks());
      })
      .finally(() => {
        if (track()?.id !== trackId) return;
        setLocalDb(null);
        setLocalPan(null);
        setLocalCents(null);
        setLocalFades(null);
        setLocalParams({});
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
      setLocalParams({});
      setLocalRegion(null);
      setAnalysis(null);
      setAnalyzing(false);
      analysisGen += 1;
      const current = track();
      const detected = current?.detectedPitch;
      if (detected && current && (current.engine === 'clip' || current.engine === 'sample')) {
        setAnalysis({
          key: pitchAnalysisKey(
            current.id,
            current.audioSourceId,
            current.sampleStartFrames,
            current.sampleEndFrames,
          ),
          result: { kind: 'detected', ...detected },
        });
        setKbCenter(detected.midiNote);
        setTargetNote(detected.midiNote);
      }
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
        if (sourceId === 'poly-synth') {
          setPreview(null);
          setLoading(false);
          return;
        }
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
  let auditionTimer: ReturnType<typeof setTimeout> | undefined;
  let skipWaveAudition = false;

  function queueSampleEdit(trackId: number, operation: (index: number) => Promise<unknown>): void {
    pendingSampleEdit = pendingSampleEdit.catch(() => undefined).then(async () => {
      const latest = app.mixer()?.tracks.find((entry) => entry.id === trackId);
      if (!latest) return;
      try {
        await operation(latest.index);
      } catch (error) {
        app.report(error);
        throw sampleEditFailed;
      }
    });
  }

  function release(): void {
    if (held === null) return;
    const key = held;
    held = null;
    app.backend.noteOff(key).catch(() => {});
  }

  function auditionWave(): void {
    const current = track();
    if (!current || !samplePlay()) return;
    const trackId = current.id;
    void pendingSampleEdit
      .then(async () => {
        if (track()?.id !== trackId) return;
        const latest = app.mixer()?.tracks.find((entry) => entry.id === trackId);
        if (!latest) return;
        await app.backend.previewTrackSample(latest.index);
      })
      .catch((error: unknown) => {
        if (error !== sampleEditFailed) app.report(error);
      });
  }

  function audition(): void {
    const current = track();
    if (!current) return;
    if (samplePlay()) {
      auditionWave();
      return;
    }
    const trackId = current.id;
    void pendingSampleEdit
      .then(async () => {
        if (track()?.id !== trackId) return;
        const latest = app.mixer()?.tracks.find((entry) => entry.id === trackId);
        if (!latest) return;
        await app.mixerCommand((backend) => backend.setSelectedTrack(latest.index));
        const selected = track();
        if (!selected || selected.id !== trackId) return;
        clearTimeout(timer);
        release();
        const key = selected.sourceId === 'drum-kit' ? KIT_KEY : AUDITION_KEY;
        held = key;
        await app.backend.noteOn(key, 0.9);
        timer = setTimeout(release, AUDITION_MS);
      })
      .catch((error: unknown) => {
        if (error !== sampleEditFailed) app.report(error);
      });
  }

  onCleanup(() => {
    clearTimeout(timer);
    clearTimeout(auditionTimer);
    release();
  });

  const kind = () => {
    const current = track();
    return current ? channelKind(current) : null;
  };
  const colour = () => channelColor(track());
  const owner = () => track()?.name ?? 'Sample';
  const synthEditor = () => usesPolySynthEditor(track()?.sourceId, track()?.sourceParamDefs.length ?? 0);
  const file = () => app.audioSources().find((source) => source.id === track()?.audioSourceId);
  const sampleRate = () => file()?.sampleRate ?? 0;

  const peaks = (): [number, number][] => fileWave()?.peaks ?? preview()?.peaks ?? [];
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
    queueSampleEdit(current.id, (index) =>
      app.backend.setTrackSampleRegion(index, next.start, next.storedEnd),
    );
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
    const onUp = (event: PointerEvent) => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      window.removeEventListener('pointercancel', onUp);
      document.body.style.removeProperty('cursor');
      document.body.style.removeProperty('user-select');
      const movedPx = Math.abs(event.clientX - startX);
      const action = decideWaveformAction({
        kind: grab === 'new' ? 'body' : 'handle',
        movedPx,
        slopPx: DRAG_SLOP_PX,
        doubleClick: false,
      });
      if (action === 'audition') {
        setLocalRegion(null);
        if (skipWaveAudition) return;
        clearTimeout(auditionTimer);
        auditionTimer = setTimeout(() => {
          if (!skipWaveAudition) auditionWave();
        }, 280);
        return;
      }
      if (!moved && grab === 'new') {
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
  const wavePaths = createMemo(() => {
    return stereoWaveformPaths(peaks(), {
      gainDb: gainDb(),
      pan: pan(),
      pitchCents: cents(),
      fadeInMs: fadeIn(),
      fadeOutMs: fadeOut(),
      sampleRate: sampleRate(),
      totalFrames: totalFrames(),
      // This large editor keeps the whole source under the region handles.
      // The compact inspector crops to the region; here the shades must still
      // line up with the original file behind them.
      startFrames: 0,
      endFrames: totalFrames(),
    });
  });

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
    queueSampleEdit(current.id, (index) => app.backend.setTrackPitch(index, next));
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
    queueSampleEdit(current.id, (index) => app.backend.setTrackFades(index, next[0], next[1]));
    settle();
  }

  // --- the instrument's own knobs ------------------------------------------
  //
  // The generic panel: the engine sends what the instrument declares (id, range,
  // curve, default) and this draws a knob per declaration, exactly as the
  // inspector does for an effect. Poly synth is the one exception -- it has
  // pictures of its oscillators, filter and envelope in PolySynthEditor -- so
  // the next instrument's window stays free.

  const paramDefs = (): ParamDefDto[] => track()?.sourceParamDefs ?? [];

  const paramValue = (def: ParamDefDto): number => {
    const held = localParams()[def.id];
    if (held !== undefined) return held;
    return track()?.sourceParams.find((entry) => entry.id === def.id)?.value ?? def.defaultValue;
  };

  function setParam(def: ParamDefDto, value: number): void {
    const current = track();
    if (!current) return;
    setLocalParams((prev) => ({ ...prev, [def.id]: value }));
    void app.backend.setSourceParam(current.index, def.id, value).catch(app.report);
    settle();
  }

  function transpose(delta: number): void {
    const next = stepSemitone(cents(), delta);
    if (next === cents()) return;
    setPitch(next);
    app.say(`${owner()} · Pitch`, pitchLabel(next));
  }

  const analysisKeyNow = (): string => {
    const current = track();
    if (!current) return '';
    const { start, end } = region();
    return pitchAnalysisKey(current.id, current.audioSourceId, start, end);
  };

  const detectedPitch = () => {
    const held = analysis();
    if (!held || held.key !== analysisKeyNow() || held.result.kind !== 'detected') return null;
    return held.result;
  };

  const pitchStatus = (): string | null => {
    const held = analysis();
    if (!held || held.key !== analysisKeyNow()) return analyzing() ? 'Listening…' : null;
    if (held.result.kind === 'detected') {
      const source =
        held.result.source === 'metadata'
          ? 'metadata'
          : held.result.source === 'filename'
            ? 'filename'
            : 'analysis';
      return `${formatDetectedPitch(held.result)} · ${source}`;
    }
    return noPitchReasonCopy(held.result.reason);
  };

  async function findPitch(): Promise<void> {
    const current = track();
    if (!current || !samplePlay()) return;
    await waitForSampleEdits();
    const key = analysisKeyNow();
    const gen = ++analysisGen;
    setAnalyzing(true);
    try {
      const result = await app.backend.analyzeTrackPitch(current.index);
      if (gen !== analysisGen || analysisKeyNow() !== key) return;
      setAnalysis({ key, result });
      if (result.kind === 'detected') {
        setKbCenter(result.midiNote);
        setTargetNote(result.midiNote);
        app.say(`${owner()} · Pitch`, formatDetectedPitch(result));
      } else {
        app.say(`${owner()} · Pitch`, noPitchReasonCopy(result.reason));
      }
    } catch (error) {
      app.report(error);
    } finally {
      if (gen === analysisGen) setAnalyzing(false);
    }
  }

  function useAsRoot(found: { midiNote: number; cents: number }): void {
    const current = track();
    if (!current) return;
    queueSampleEdit(current.id, (index) =>
      app.mixerCommand((backend) =>
        backend.setTrackSampleRoot(index, found.midiNote, found.cents, 'detected'),
      ),
    );
    settle();
    setKbCenter(found.midiNote);
    app.say(`${owner()} · Root`, `${noteName(found.midiNote)}`);
  }

  function enqueueNote(work: () => Promise<void>): void {
    noteGate = noteGate.catch(() => undefined).then(work);
  }

  function playSampleNote(key: number, velocity: number): void {
    const current = track();
    if (!current) return;
    const index = current.index;
    enqueueNote(async () => {
      if (app.mixer()?.selected !== index) {
        await app.mixerCommand((backend) => backend.setSelectedTrack(index));
      }
      await app.backend.noteOn(key, velocity);
    });
  }

  function releaseSampleNote(key: number): void {
    enqueueNote(async () => {
      await app.backend.noteOff(key);
    });
  }

  function retuneTo(found: { midiNote: number; cents: number }, target: number): void {
    setPitch(targetTransposeCents(found.midiNote, found.cents, target));
    app.say(`${owner()} · Pitch`, pitchLabel(targetTransposeCents(found.midiNote, found.cents, target)));
  }

  createEffect(
    on(
      () => track()?.id,
      (id) => {
        if (id == null || !samplePlay()) return;
        const current = track();
        if (!current) return;
        void app.mixerCommand((backend) => backend.setSelectedTrack(current.index));
        setKbCenter(current.rootKey);
      },
    ),
  );

  return (
    <Show when={track()}>
      {(current) => (
        <PluginWindow
          title={current().name}
          color={colour()}
          z={80}
          focused
          defaultWidth={synthEditor() ? 880 : 660}
          defaultHeight={synthEditor() ? 540 : 520}
          offset={0}
          onFocus={() => {
            if (!synthEditor()) return;
            const index = track()?.index;
            if (index === undefined) return;
            void app.backend.watchSpectrum({ kind: 'track', index }, null).catch(() => {});
          }}
          onClose={() => view.closeSampleEditor()}
        >
          <div class="wv-sample wv-sample--window">
          <Show when={!synthEditor()}>
            <div class="wv-sample__tools">
              <Show when={kind()}>
                {(info) => (
                  <span
                    class="wv-chrow__kind"
                    style={{ background: colour() }}
                    title={info().title}
                  />
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
              <Show when={samplePlay()}>
                <button
                  type="button"
                  class="wv-chip"
                  disabled={analyzing()}
                  title="Detect the sample's root pitch. Unpitched sounds are reported, never forced to C."
                  onClick={() => void findPitch()}
                >
                  {analyzing() ? 'Finding…' : 'Find pitch'}
                </button>
                <Show when={pitchStatus()}>
                  {(copy) => <span class="wv-sample__pitchfind">{copy()}</span>}
                </Show>
                <Show when={detectedPitch()}>
                  {(found) => (
                    <>
                      <button
                        type="button"
                        class="wv-chip"
                        title="Calibrate the keyboard and piano roll to this pitch without changing transpose"
                        onClick={() => useAsRoot(found())}
                      >
                        Use as root
                      </button>
                      <label class="wv-sample__target">
                        Target
                        <select
                          value={String(targetNote())}
                          onChange={(event) => {
                            const note = Number(event.currentTarget.value);
                            setTargetNote(note);
                            retuneTo(found(), note);
                          }}
                        >
                          <For each={TARGET_NOTES}>
                            {(note) => <option value={String(note)}>{noteName(note)}</option>}
                          </For>
                        </select>
                      </label>
                    </>
                  )}
                </Show>
                <span class="wv-sample__root">
                  Root {noteName(track()?.rootKey ?? 60)}
                  {Math.round(track()?.rootFineCents ?? 0) !== 0
                    ? ` ${Math.round(track()?.rootFineCents ?? 0) > 0 ? '+' : ''}${Math.round(track()?.rootFineCents ?? 0)} ct`
                    : ''}
                </span>
              </Show>
            </div>
          </Show>

            <Show
              when={synthEditor()}
              fallback={
                <>
            <div
              class="wv-sample__wave"
              classList={{ 'is-edit': samplePlay() }}
              style={{ color: colour() }}
              ref={waveEl}
              onPointerDown={(event) => {
                if (samplePlay()) onRegionPointer(event, 'new');
                else if (event.button === 0) audition();
              }}
              onDblClick={() => {
                if (!samplePlay()) return;
                skipWaveAudition = true;
                clearTimeout(auditionTimer);
                resetRegion();
                window.setTimeout(() => {
                  skipWaveAudition = false;
                }, 350);
              }}
              title={
                samplePlay()
                  ? 'Click to preview, drag to set the played region, drag a handle to move one end, double-click for the whole file'
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
                  <path d={wavePaths().left} fill="currentColor" />
                  <path d={wavePaths().right} fill="currentColor" opacity="0.72" />
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

            <Show when={paramDefs().length > 0}>
              <div class="wv-sample__inst">
                <For each={paramDefs()}>
                  {(def) => (
                    <Knob
                      def={def}
                      typeId={current().sourceId}
                      value={() => paramValue(def)}
                      onInput={(value) => setParam(def, value)}
                      owner={owner()}
                      size={34}
                      color={colour()}
                    />
                  )}
                </For>
              </div>
            </Show>
                </>
              }
            >
              <PolySynthEditor
                defs={paramDefs()}
                owner={owner()}
                color={colour()}
                paramValue={paramValue}
                setParam={setParam}
                sampleRate={app.sequencer()?.sampleRate ?? app.streamInfo()?.sampleRate ?? 0}
                trackIndex={current().index}
                onPreview={audition}
              />
            </Show>

            <Show when={!synthEditor()}>
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

            <Show when={samplePlay()}>
              <SampleKeyboard
                trackToken={sourceKey() ?? ''}
                center={kbCenter()}
                heldNotes={app.heldNotes()}
                rootKey={track()?.rootKey ?? 60}
                onCenter={setKbCenter}
                noteOn={playSampleNote}
                noteOff={releaseSampleNote}
              />
            </Show>
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
            </Show>
          </div>
        </PluginWindow>
      )}
    </Show>
  );
}
