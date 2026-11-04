/**
 * The top bar: the project, the transport, the tempo, and which view is up.
 *
 * The clock and the bar counter are painted on the shared frame loop rather
 * than from signals -- they change 60 times a second and must not touch the
 * reactive scheduler.
 *
 * Playback follows the active musical editor; there is no separate mode switch.
 */

import { Show, createSignal, onCleanup } from 'solid-js';

import { TempoField } from '../ui/TempoField';
import { bindWheel } from '../ui/drag';
import { onFrame } from '../ui/frameLoop';
import {
  IconLoop,
  IconMoon,
  IconSettings,
  IconPlay,
  IconStop,
  IconSun,
  IconTypingKeyboard,
  IconRoll,
  IconUndo,
  IconView,
  IconCaret,
} from '../ui/icons';
import { barsBeats, samplesToTicks, ticksToSamples } from '../lib/pattern';
import { foldToLoop } from '../lib/timeline';
import { midiKeyFor, noteName, clampOctave } from '../lib/keyboard';
import { formatTimecode } from '../lib/playhead';
import { useAppState } from '../state';
import { useTheme } from '../theme/ThemeProvider';
import { useView } from './view';
import { MasterVolume } from './MasterVolume';
import { PatternSelector } from './PatternSelector';
import { RecordControl } from './RecordControl';
import { WindowControls } from './WindowControls';

const VIEWS: readonly ['playlist' | 'rack' | 'mixer', string, string][] = [
  ['playlist', 'PLAYLIST', 'F5'],
  ['rack', 'RACK', 'F6'],
  ['mixer', 'MIXER', 'F9'],
];

export function TopBar(): ReturnType<typeof Show> {
  const app = useAppState();
  const view = useView();
  const theme = useTheme();

  const [projectMenu, setProjectMenu] = createSignal(false);
  const undoLabel = () => {
    app.undoRevision();
    return app.undo.undoLabel();
  };
  const redoLabel = () => {
    app.undoRevision();
    return app.undo.redoLabel();
  };

  function toggleTypingKeyboard(): void {
    view.setTypingKeyboard((on) => !on);
    app.say(
      'Typing keyboard',
      view.typingKeyboard() ? `Q is ${noteName(midiKeyFor('KeyQ', app.octave()) ?? 60)}` : 'off',
    );
  }

  let clockEl: HTMLSpanElement | undefined;
  let barEl: HTMLSpanElement | undefined;

  /**
   * The span the readouts fold into, in samples, or `0` for none.
   *
   * A looping arrangement is *repeated* by the engine rather than seeked, so the
   * raw position climbs for ever -- a clock reading 3:20 on the fourth time
   * round an eight-bar loop is telling the truth about the transport and lying
   * about the music.
   */
  const loopSpanSamples = (): number => {
    if (app.playMode() !== 'song' || !app.looping()) return 0;
    const end = app.playlist()?.endTicks ?? 0;
    return end > 0 ? ticksToSamples(end, app.playhead.getSampleRate(), app.bpm()) : 0;
  };

  // One read of the playhead per frame: `positionSamples` advances its own
  // error correction, so calling it twice would double-correct.
  onFrame(() => {
    const samples = foldToLoop(app.playhead.positionSamples(), loopSpanSamples());
    const rate = app.playhead.getSampleRate();
    if (clockEl) clockEl.textContent = formatTimecode(samples / rate);
    if (barEl) barEl.textContent = barsBeats(samplesToTicks(samples, rate, app.bpm()));
  });

  return (
    <header class="wv-top" data-tauri-drag-region>
      <div class="wv-projectmenu">
        <button
          type="button"
          class="wv-projectmenu__button"
          aria-label={`Project and playback options: ${app.project().name}`}
          aria-expanded={projectMenu()}
          title={`${app.project().name} — project and playback options`}
          onClick={() => setProjectMenu((open) => !open)}
        >
          File <IconCaret />
        </button>
        <Show when={projectMenu()}>
          <div class="wv-scrim" onClick={() => setProjectMenu(false)}>
            <div class="wv-menu wv-menu--project" onClick={(event) => event.stopPropagation()}>
              <div class="wv-menu__heading" title={app.project().name}>
                {app.project().name}
              </div>
              <button
                type="button"
                class="wv-menu__row"
                onClick={() => {
                  setProjectMenu(false);
                  app.newProject();
                }}
              >
                New
                <span class="wv-spacer" />
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
                Open…
                <span class="wv-spacer" />
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
                Save
                <span class="wv-spacer" />
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
                Save As…
                <span class="wv-spacer" />
                <span class="wv-menu__meta">Ctrl+Shift+S</span>
              </button>
              <button
                type="button"
                class="wv-menu__row"
                title="Render the song or pattern to a WAV file"
                onClick={() => {
                  setProjectMenu(false);
                  app.exporter.show();
                }}
              >
                Export WAV…
                <span class="wv-spacer" />
                <span class="wv-menu__meta">Ctrl+R</span>
              </button>
              <div class="wv-menu__rule" />
              <button
                type="button"
                class="wv-menu__row"
                aria-pressed={app.looping()}
                title="Repeat the song arrangement when it reaches the end"
                onClick={() => app.setLoop(!app.looping())}
              >
                <IconLoop /> Loop song <span class="wv-spacer" />
                <span class="wv-menu__meta">{app.looping() ? 'On' : 'Off'}</span>
              </button>
              <button
                type="button"
                class="wv-menu__row"
                aria-pressed={view.typingKeyboard()}
                title="Typing keyboard to piano (Ctrl+T). Scroll to change octave."
                onClick={toggleTypingKeyboard}
                ref={(el) =>
                  onCleanup(
                    bindWheel(el, (notches) => app.setOctave(clampOctave(app.octave() + notches))),
                  )
                }
              >
                <IconTypingKeyboard /> Typing keyboard <span class="wv-spacer" />
                <span class="wv-menu__meta">{view.typingKeyboard() ? 'On' : 'Off'}</span>
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
          title={app.playing() ? 'Stop and return to start (Space) · Pause: Ctrl+Space' : `Play ${view.playbackMode() === 'pat' ? 'pattern' : 'playlist'} (Space) · Resume: Ctrl+Space`}
          onClick={() => app.togglePlay()}
        >
          <IconPlay />
        </button>
        <button
          type="button"
          class="wv-tbtn"
          title="Stop and return to start (Space) · Home: beginning"
          onClick={() => app.stop()}
        >
          <IconStop />
        </button>
        <RecordControl />
      </div>

      <div class="wv-history" aria-label="Edit history">
        <button
          type="button"
          class="wv-history__button"
          disabled={!undoLabel()}
          aria-label="Undo"
          title={undoLabel() ? `Undo ${undoLabel()} (Ctrl+Z)` : 'Nothing to undo'}
          onClick={() => app.undoLast()}
        >
          <IconUndo />
        </button>
        <button
          type="button"
          class="wv-history__button"
          disabled={!redoLabel()}
          aria-label="Redo"
          title={redoLabel() ? `Redo ${redoLabel()} (Ctrl+Shift+Z)` : 'Nothing to redo'}
          onClick={() => app.redoLast()}
        >
          <IconUndo redo />
        </button>
      </div>

      <div class="wv-clock" data-tauri-drag-region>
        <span class="wv-clock__time" ref={clockEl} data-tauri-drag-region>
          00:00.000
        </span>
        <Show when={view.wideBar()}>
          <span class="wv-clock__bar" ref={barEl} data-tauri-drag-region>
            001.1.1
          </span>
        </Show>
      </div>

      <div class="wv-meta">
        <div class="wv-meta__item">
          <TempoField compact value={app.bpm} onInput={(value) => app.setBpm(value)} />
          <span class="wv-meta__tag">BPM</span>
        </div>
        <div
          class="wv-meta__item wv-meta__signature"
          title="The engine's grid is 4/4; other signatures are not implemented yet"
        >
          <span class="wv-meta__value wv-disabled">4/4</span>
          <span class="wv-meta__tag">SIG</span>
        </div>
      </div>

      <PatternSelector />

      <div class="wv-spacer" data-tauri-drag-region />

      <MasterVolume />

      <div class="wv-seg wv-viewtabs">
        {VIEWS.map(([id, label, hotkey]) => (
          <button
            type="button"
            class="wv-seg__btn"
            classList={{ 'is-active': view.view() === id && !view.rollOpen() }}
            title={`${label} (${hotkey})`}
            aria-label={`${label} (${hotkey})`}
            onClick={() => view.go(id)}
          >
            <span class="wv-viewtabs__icon">
              <IconView kind={id} />
            </span>
            <span class="wv-viewtabs__label">{label}</span>
            <span class="wv-seg__hot">{hotkey}</span>
          </button>
        ))}
        <button
          type="button"
          class="wv-seg__btn"
          classList={{ 'is-active': view.rollOpen() }}
          title="Piano roll (F7)"
          aria-label="Piano roll (F7)"
          onClick={() => {
            if (view.rollOpen()) view.closeRoll();
            else {
              const channel = app.mixer()?.selected ?? 0;
              app.promoteStepsToRoll(channel);
              view.openRoll(channel);
            }
          }}
        >
          <IconRoll />
          <span class="wv-seg__hot">F7</span>
        </button>
      </div>

      <button
        type="button"
        class="wv-settingsbtn"
        title="Open settings"
        aria-label="Open settings"
        onClick={() => view.go('settings')}
      >
        <IconSettings />
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
      <WindowControls />
    </header>
  );
}
