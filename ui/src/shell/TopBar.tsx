/**
 * The top bar: the project, the transport, the tempo, which pattern is being
 * edited, and which view is up.
 *
 * The clock and the bar counter are painted on the shared frame loop rather
 * than from signals -- they change 60 times a second and must not touch the
 * reactive scheduler.
 *
 * ## PAT / SONG lives here, next to the transport
 *
 * The mock implies the mode from the view: the rack plays the pattern, the
 * playlist plays the song. That is a trap. Looking at the mixer while a song
 * plays is the single most common thing a person does in a DAW, and a view
 * switch that silently stopped the arrangement and started looping one pattern
 * would be the worst kind of surprise -- a *hidden* mode change. So the mode is
 * an explicit, always-visible toggle next to the transport, exactly where FL
 * Studio puts it, and switching views never changes what plays.
 */

import { Show, createSignal } from 'solid-js';

import { NumberField } from '../ui/NumberField';
import { onFrame } from '../ui/frameLoop';
import {
  IconCaret,
  IconLoop,
  IconMoon,
  IconPause,
  IconPlay,
  IconRecord,
  IconStop,
  IconSun,
  WaveMark,
} from '../ui/icons';
import { barsBeats, samplesToTicks } from '../lib/pattern';
import { formatTimecode } from '../lib/playhead';
import { useAppState } from '../state';
import { useTheme } from '../theme/ThemeProvider';
import { trackColor } from '../theme/tokens';
import { useView, type MainView } from './view';
import { PatternMenu } from './PatternMenu';

const MIN_BPM = 20;
const MAX_BPM = 999;

const VIEWS: readonly [MainView, string, string][] = [
  ['playlist', 'PLAYLIST', 'F5'],
  ['rack', 'RACK', 'F6'],
  ['mixer', 'MIXER', 'F9'],
];

export function TopBar(): ReturnType<typeof Show> {
  const app = useAppState();
  const view = useView();
  const theme = useTheme();

  const [patternMenu, setPatternMenu] = createSignal(false);
  const [projectMenu, setProjectMenu] = createSignal(false);

  let clockEl: HTMLSpanElement | undefined;
  let barEl: HTMLSpanElement | undefined;

  // One read of the playhead per frame: `positionSamples` advances its own
  // error correction, so calling it twice would double-correct.
  onFrame(() => {
    const samples = app.playhead.positionSamples();
    const rate = app.playhead.getSampleRate();
    if (clockEl) clockEl.textContent = formatTimecode(samples / rate);
    if (barEl) barEl.textContent = barsBeats(samplesToTicks(samples, rate, app.bpm()));
  });

  const activePattern = () => {
    const id = app.sequencer()?.patternSet.activeId;
    return app.patterns().find((pattern) => pattern.id === id);
  };

  return (
    <header class="wv-top">
      <div class="wv-brand">
        <WaveMark />
        <div class="wv-brand__text">
          <span class="wv-brand__name">WAVE</span>
          <Show when={view.wideBar()}>
            <button
              type="button"
              class="wv-brand__project"
              title="Project — New, Open, Save"
              onClick={() => setProjectMenu((open) => !open)}
            >
              {app.project().name}
            </button>
          </Show>
        </div>
        <Show when={projectMenu()}>
          <div class="wv-scrim" onClick={() => setProjectMenu(false)}>
            <div class="wv-menu wv-menu--project" onClick={(event) => event.stopPropagation()}>
              <button
                type="button"
                class="wv-menu__row"
                onClick={() => {
                  setProjectMenu(false);
                  app.newProject();
                }}
              >
                New<span class="wv-spacer" />
                <span class="wv-menu__meta">Ctrl+N</span>
              </button>
              <button
                type="button"
                class="wv-menu__row"
                onClick={() => {
                  setProjectMenu(false);
                  app.openProject();
                }}
              >
                Open…<span class="wv-spacer" />
                <span class="wv-menu__meta">Ctrl+O</span>
              </button>
              <button
                type="button"
                class="wv-menu__row"
                onClick={() => {
                  setProjectMenu(false);
                  app.saveProject();
                }}
              >
                Save<span class="wv-spacer" />
                <span class="wv-menu__meta">Ctrl+S</span>
              </button>
              <button
                type="button"
                class="wv-menu__row"
                onClick={() => {
                  setProjectMenu(false);
                  app.saveProjectAs();
                }}
              >
                Save As…<span class="wv-spacer" />
                <span class="wv-menu__meta">Ctrl+Shift+S</span>
              </button>
            </div>
          </div>
        </Show>
      </div>

      <div class="wv-seg wv-transport">
        <button
          type="button"
          class="wv-tbtn wv-tbtn--play"
          classList={{ 'is-playing': app.playing() }}
          title={app.playing() ? 'Pause (Space)' : 'Play (Space)'}
          onClick={() => app.togglePlay()}
        >
          <Show when={app.playing()} fallback={<IconPlay />}>
            <IconPause />
          </Show>
        </button>
        <button
          type="button"
          class="wv-tbtn"
          title="Stop and rewind to the start"
          onClick={() => {
            app.stop();
            app.seek(0);
          }}
        >
          <IconStop />
        </button>
        <button
          type="button"
          class="wv-tbtn wv-tbtn--rec wv-can-disable"
          disabled
          title="Recording is not implemented yet — the engine has no capture path"
        >
          <IconRecord />
        </button>
        <button
          type="button"
          class="wv-tbtn wv-can-disable"
          disabled
          title="Loop points are not implemented yet — PAT mode already loops the pattern, and SONG mode plays to the end of the arrangement"
        >
          <IconLoop />
        </button>
      </div>

      <div class="wv-clock">
        <span class="wv-clock__time" ref={clockEl}>
          00:00.000
        </span>
        <Show when={view.wideBar()}>
          <span class="wv-clock__bar" ref={barEl}>
            001.1.1
          </span>
        </Show>
      </div>

      <Show when={view.wideBar()}>
        <div class="wv-meta">
          <div class="wv-meta__item">
            <NumberField
              class="wv-meta__value wv-meta__value--drag"
              value={app.bpm}
              onInput={(value) => app.setBpm(value)}
              min={MIN_BPM}
              max={MAX_BPM}
              step={0.5}
              format={(value) => value.toFixed(2)}
              title="Tempo — drag, scroll, or double-click to type"
              aria-label="Tempo in beats per minute"
            />
            <span class="wv-meta__tag">BPM</span>
          </div>
          <div class="wv-meta__item" title="The engine's grid is 4/4; other signatures are not implemented yet">
            <span class="wv-meta__value wv-disabled">4/4</span>
            <span class="wv-meta__tag">SIG</span>
          </div>
        </div>
      </Show>

      <div class="wv-seg" title="What the transport plays. Independent of which view is open.">
        <button
          type="button"
          class="wv-seg__btn wv-seg__btn--mode"
          classList={{ 'is-active': app.playMode() === 'pat' }}
          onClick={() => app.setPlayMode('pat')}
        >
          PAT
        </button>
        <button
          type="button"
          class="wv-seg__btn wv-seg__btn--mode"
          classList={{ 'is-active': app.playMode() === 'song' }}
          onClick={() => app.setPlayMode('song')}
        >
          SONG
        </button>
      </div>

      <div class="wv-patsel">
        <button
          type="button"
          class="wv-patsel__btn"
          classList={{ 'is-open': patternMenu() }}
          title="The pattern the rack and the piano roll are editing"
          onClick={() => setPatternMenu((open) => !open)}
        >
          <span
            class="wv-swatch"
            style={{ background: activePattern()?.color ?? trackColor(activePattern()?.id ?? 0) }}
          />
          <span class="wv-patsel__text">
            <span class="wv-patsel__name">{activePattern()?.name ?? '—'}</span>
            <span class="wv-patsel__tag">PATTERN</span>
          </span>
          <IconCaret />
        </button>
        <Show when={patternMenu()}>
          <PatternMenu onClose={() => setPatternMenu(false)} />
        </Show>
      </div>

      <div class="wv-spacer" />

      <div class="wv-seg">
        {VIEWS.map(([id, label, hotkey]) => (
          <button
            type="button"
            class="wv-seg__btn"
            classList={{ 'is-active': view.view() === id && !view.rollOpen() }}
            onClick={() => view.go(id)}
          >
            {label}
            <span class="wv-seg__hot">{hotkey}</span>
          </button>
        ))}
      </div>

      <button
        type="button"
        class="wv-themebtn"
        title={theme.isDark() ? 'Switch to the light theme' : 'Switch to the dark theme'}
        onClick={() => theme.toggle()}
      >
        <Show when={theme.isDark()} fallback={<IconMoon />}>
          <IconSun />
        </Show>
      </button>
    </header>
  );
}
