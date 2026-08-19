import { Show, type JSX } from 'solid-js';
import { useAppState } from '../state';
import { useTheme } from '../theme/ThemeProvider';
import { onFrame } from '../ui/frameLoop';
import { NumberField } from '../ui/NumberField';
import {
  IconLoop,
  IconMoon,
  IconPause,
  IconPlay,
  IconRecord,
  IconRewind,
  IconStop,
  IconSun,
  WaveMark,
} from '../ui/icons';

/** Tempo bounds, mirroring `crates/rt-core/src/transport.rs`. */
const MIN_BPM = 20;
const MAX_BPM = 999;

/** `mm:ss.mmm`, which is what the engine can actually tell us. */
function timecode(seconds: number): string {
  const clamped = Math.max(0, seconds);
  const minutes = Math.floor(clamped / 60);
  const secs = Math.floor(clamped % 60);
  const millis = Math.floor((clamped % 1) * 1000);
  return `${String(minutes).padStart(2, '0')}:${String(secs).padStart(2, '0')}.${String(
    millis,
  ).padStart(3, '0')}`;
}

/**
 * `bar.beat.hundredths`, counting from one.
 *
 * Computed from the same interpolated sample position the timecode uses, so the
 * two readouts can never disagree about where the transport is. Four beats to
 * the bar because the engine's signature is fixed at 4/4 -- which is also why
 * the SIG field beside it is a label rather than a control.
 */
function barsBeats(seconds: number, bpm: number): string {
  if (!Number.isFinite(bpm) || bpm <= 0) return '001.1.00';
  const beats = Math.max(0, seconds) * (bpm / 60);
  const bar = Math.floor(beats / 4) + 1;
  const beat = Math.floor(beats % 4) + 1;
  const fraction = Math.floor((beats % 1) * 100);
  return `${String(bar).padStart(3, '0')}.${beat}.${String(fraction).padStart(2, '0')}`;
}

/** A linear amplitude as dBFS, or `-inf` at silence. */
function peakDb(amplitude: number): string {
  if (!Number.isFinite(amplitude) || amplitude <= 0) return '−∞';
  const db = 20 * Math.log10(amplitude);
  if (db <= -60) return '−∞';
  // A real minus sign, matching the design's readouts.
  return `${db < 0 ? '−' : '+'}${Math.abs(db).toFixed(1)}`;
}

/**
 * The top bar: transport, position, tempo and the theme switch.
 *
 * The clock and the peak readout are painted from the shared animation frame
 * rather than from signals, for the reason `frameLoop.ts` explains: a 30-60 Hz
 * meter stream must not drive the reactive scheduler. Everything else here is
 * ordinary reactive markup.
 */
export function TransportBar(): JSX.Element {
  const app = useAppState();
  const theme = useTheme();

  let clockEl: HTMLSpanElement | undefined;
  let barsEl: HTMLSpanElement | undefined;
  let peakEl: HTMLElement | undefined;

  onFrame(() => {
    const seconds = app.playhead.positionSeconds();
    if (clockEl) clockEl.textContent = timecode(seconds);
    if (barsEl) barsEl.textContent = barsBeats(seconds, app.bpm());
    const frame = app.meters.latest;
    if (peakEl) peakEl.textContent = peakDb(Math.max(frame?.peakL ?? 0, frame?.peakR ?? 0));
  });

  return (
    <header class="wv-transport">
      <div class="wv-brand">
        <WaveMark />
        <div class="wv-brand__names">
          <span class="wv-brand__app">WAVE</span>
          <span class="wv-brand__project" title={app.project().path ?? 'never saved'}>
            {app.project().name}
          </span>
        </div>
      </div>

      <div class="wv-transport__group">
        <span
          class="wv-tbtn wv-disabled"
          title="Return to start — coming soon"
          aria-hidden="true"
        >
          <IconRewind />
        </span>

        <button
          type="button"
          class="wv-tbtn wv-tbtn--play"
          classList={{ 'is-playing': app.playing() }}
          title={app.playing() ? 'Pause (Space)' : 'Play (Space)'}
          aria-label={app.playing() ? 'Pause' : 'Play'}
          aria-pressed={app.playing()}
          onClick={() => (app.playing() ? app.stop() : app.play())}
        >
          <Show when={app.playing()} fallback={<IconPlay />}>
            <IconPause />
          </Show>
        </button>

        <button
          type="button"
          class="wv-tbtn"
          title="Stop. The playhead stays where it is — there is no rewind yet."
          aria-label="Stop"
          onClick={() => app.stop()}
        >
          <IconStop />
        </button>

        <span class="wv-tbtn wv-tbtn--rec wv-disabled" title="Record — coming soon">
          <IconRecord />
        </span>
        <span class="wv-tbtn wv-tbtn--loop wv-disabled" title="Loop — coming soon">
          <IconLoop />
        </span>
      </div>

      <div class="wv-clock">
        <span class="wv-clock__time" ref={clockEl}>
          00:00.000
        </span>
        <span class="wv-clock__bars" ref={barsEl}>
          001.1.00
        </span>
      </div>

      <div class="wv-meta">
        <div class="wv-meta__field">
          <NumberField
            class="wv-meta__value"
            value={app.bpm}
            onInput={(bpm) => app.setBpm(Math.round(bpm * 100) / 100)}
            min={MIN_BPM}
            max={MAX_BPM}
            step={0.25}
            aria-label="Tempo in beats per minute"
            title="Drag to change the tempo, double-click to type one"
          />
          <span class="wv-meta__label">BPM</span>
        </div>
        <div class="wv-meta__field">
          <span class="wv-meta__value">4/4</span>
          <span class="wv-meta__label">SIG</span>
        </div>
        <div class="wv-meta__field wv-disabled" title="Key detection — coming soon">
          <span class="wv-meta__value">—</span>
          <span class="wv-meta__label">KEY</span>
        </div>
      </div>

      <span class="wv-spacer" />

      <div class="wv-levels">
        <span>
          PEAK <b ref={peakEl}>−∞</b>
        </span>
        <span class="wv-disabled" title="Loudness metering — coming soon">
          LUFS <b>—</b>
        </span>
      </div>

      <button
        type="button"
        class="wv-chipbtn"
        title={theme.isDark() ? 'Switch to the light theme' : 'Switch to the dark theme'}
        aria-label="Toggle theme"
        onClick={() => theme.toggle()}
      >
        <Show when={theme.isDark()} fallback={<IconMoon />}>
          <IconSun />
        </Show>
      </button>

      <span class="wv-avatar wv-disabled" title="Accounts — coming soon">
        M
      </span>
    </header>
  );
}
