/**
 * The top bar: the project, the transport, the tempo, and which view is up.
 *
 * The clock and the bar counter are painted on the shared frame loop rather
 * than from signals -- they change 60 times a second and must not touch the
 * reactive scheduler.
 *
 * ## PAT / SONG lives here, next to the transport
 *
 * Opening Playlist switches to SONG; opening Rack switches to PAT. Mixer
 * leaves the mode alone so you can mix the arrangement or the pattern without
 * the view change stopping playback. The toggle is still here for an override
 * on the current view.
 */

import { Show, createSignal, onCleanup } from 'solid-js';

import { NumberField } from '../ui/NumberField';
import { bindWheel } from '../ui/drag';
import { onFrame } from '../ui/frameLoop';
import {
  IconLoop,
  IconMoon,
  IconPause,
  IconPlay,
  IconRecord,
  IconStop,
  IconSun,
  IconTypingKeyboard,
  WaveMark,
} from '../ui/icons';
import { barsBeats, samplesToTicks } from '../lib/pattern';
import { midiKeyFor, noteName, clampOctave } from '../lib/keyboard';
import { formatTimecode } from '../lib/playhead';
import { useAppState } from '../state';
import { useTheme } from '../theme/ThemeProvider';
import { useView, type MainView } from './view';
import { MasterVolume } from './MasterVolume';

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
        <button
          type="button"
          class="wv-tbtn"
          classList={{ 'is-active': view.typingKeyboard() }}
          aria-pressed={view.typingKeyboard()}
          title={
            view.typingKeyboard()
              ? `Typing keyboard on (Ctrl+T) — Q is ${noteName(midiKeyFor('KeyQ', app.octave()) ?? 60)}, 2 is the sharp, W is D. Scroll to change octave.`
              : 'Typing keyboard to piano (Ctrl+T) — Q is C4, 2 is C#4, W is D4, Z is C3'
          }
          onClick={() => {
            view.setTypingKeyboard((on) => !on);
            app.say(
              'Typing keyboard',
              view.typingKeyboard()
                ? `Q is ${noteName(midiKeyFor('KeyQ', app.octave()) ?? 60)}`
                : 'off',
            );
          }}
          ref={(el) =>
            // bindWheel, per drag.ts's rule: a JSX onWheel may end up passive
            // and its preventDefault a silent no-op.
            onCleanup(
              bindWheel(el, (notches) => {
                app.setOctave(clampOctave(app.octave() + notches));
              }),
            )
          }
        >
          <IconTypingKeyboard />
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

      <div class="wv-seg" title="What the transport plays. Playlist switches to SONG, Rack to PAT; Mixer leaves it.">
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

      <div class="wv-spacer" />

      <MasterVolume />

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

      <button type="button" class="wv-settingsbtn" title="Open settings" onClick={() => view.go('settings')}>
        <span aria-hidden="true">⌘</span>
        <span class="wv-settingsbtn__label">SETTINGS</span>
      </button>

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
