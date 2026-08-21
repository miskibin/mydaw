/**
 * The shell.
 *
 * Three main views and a fullscreen piano roll over them. Playlist and Rack
 * also set what the transport plays (song vs pattern). Mixer leaves the mode
 * alone so you can mix whichever is already going.
 */

import { Match, Show, Switch, createEffect, onCleanup, untrack } from 'solid-js';

import { Browser } from './shell/Browser';
import { SidePanel } from './shell/SidePanel';
import { StatusBar } from './shell/StatusBar';
import { TopBar } from './shell/TopBar';
import { midiKeyFor, noteName } from './lib/keyboard';
import { f7PatternId, rollChannelFromStrip, tabCounterpart } from './lib/navigate';
import { useView } from './shell/view';
import { useAppState } from './state';
import { IconChevron } from './ui/icons';
import { EffectEditors } from './views/EffectEditor';
import { Landing } from './views/Landing';
import { Mixer } from './views/Mixer';
import { Playlist } from './views/Playlist';
import { Rack } from './views/Rack';
import { Roll } from './views/Roll';
import { SampleEditor } from './views/SampleEditor';
import { Settings } from './views/Settings';

export function App(): ReturnType<typeof Show> {
  if (window.location.pathname === '/' && !window.location.hash) return <Landing />;

  const app = useAppState();
  const view = useView();

  // Playlist plays the arrangement; Rack loops the pattern. Mixer does not
  // touch the mode. `untrack` so a manual PAT/SONG click on the current view
  // is not immediately overwritten.
  createEffect(() => {
    const next = view.view();
    untrack(() => {
      if (next === 'playlist' && app.playMode() !== 'song') app.setPlayMode('song');
      else if (next === 'rack' && app.playMode() !== 'pat') app.setPlayMode('pat');
    });
  });

  // UI zoom is the webview's own (`zoomHotkeysEnabled` in tauri.conf.json):
  // Ctrl+= / Ctrl+- / Ctrl+0 are handled natively by WebView2, which scales
  // viewport units and fixed positioning correctly — a CSS `zoom` on the body
  // did not (100vh stays viewport-sized while everything else grows).

  /** The view hotkeys. F7 opens the roll on whichever channel is selected. */
  function onKeyDown(event: KeyboardEvent): void {
    const target = event.target as HTMLElement | null;
    if (target && (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.isContentEditable)) return;
    if ((event.ctrlKey || event.metaKey) && !event.altKey && event.key.toLowerCase() === 't') {
      event.preventDefault();
      view.setTypingKeyboard((on) => !on);
      app.say(
        'Typing keyboard',
        view.typingKeyboard()
          ? `Q is ${noteName(midiKeyFor('KeyQ', app.octave()) ?? 60)}`
          : 'off',
      );
      return;
    }
    if (event.key === 'Tab' && !event.ctrlKey && !event.altKey && !event.metaKey) {
      event.preventDefault();
      const current = view.rollOpen() ? view.previousView() : view.view();
      view.go(tabCounterpart(current));
      return;
    }
    switch (event.key) {
      case 'Escape':
        if (view.view() === 'settings') {
          event.preventDefault();
          const back = view.previousView();
          view.go(back === 'settings' ? 'rack' : back);
        }
        break;
      case 'F2': {
        if (view.rollOpen() || view.view() === 'settings') break;
        const id = app.sequencer()?.patternSet.activeId;
        if (id === undefined) break;
        event.preventDefault();
        view.setRenamingPattern(id);
        break;
      }
      case 'F5':
        event.preventDefault();
        view.go('playlist');
        break;
      case 'F6':
        event.preventDefault();
        view.go('rack');
        break;
      case 'F8':
        event.preventDefault();
        view.setBrowserOpen((open) => !open);
        break;
      case 'F9':
        event.preventDefault();
        view.go('mixer');
        break;
      case 'F7': {
        event.preventDefault();
        const selected = view.selectedClips();
        const clips = (app.playlist()?.clips ?? []).filter((clip) => selected.includes(clip.id));
        const patternId = f7PatternId(view.view(), clips);
        const channel = rollChannelFromStrip(view.strip(), app.mixer()?.selected ?? 0);
        if (patternId !== null) {
          void app.sequencerCommand((backend) => backend.setActivePattern(patternId)).then(() => {
            app.promoteStepsToRoll(channel);
            view.openRoll(channel);
          });
          break;
        }
        app.promoteStepsToRoll(channel);
        view.openRoll(channel);
        break;
      }
      default:
        break;
    }
  }

  window.addEventListener('keydown', onKeyDown);
  onCleanup(() => window.removeEventListener('keydown', onKeyDown));

  return (
    <div class="wv-shell">
      <TopBar />
      <Show
        when={!view.rollOpen()}
        fallback={
          <div class="wv-body">
            <Roll />
          </div>
        }
      >
        <div class="wv-body">
          <Show
            when={view.browserOpen()}
            fallback={
              <div
                class="wv-rail wv-rail--left"
                onClick={() => view.setBrowserOpen(true)}
                title="Show the browser"
              >
                <IconChevron direction="right" />
                <span class="wv-rail__label">BROWSER</span>
              </div>
            }
          >
            <Browser />
          </Show>

          <Switch fallback={<Rack />}>
            <Match when={view.view() === 'settings'}>
              <Settings />
            </Match>
            <Match when={view.view() === 'playlist'}>
              <Playlist />
            </Match>
            <Match when={view.view() === 'rack'}>
              <Rack />
            </Match>
            <Match when={view.view() === 'mixer'}>
              <Mixer />
            </Match>
          </Switch>

          <Show
            when={view.sideOpen()}
            fallback={
              <div
                class="wv-rail wv-rail--right"
                onClick={() => view.setSideOpen(true)}
                title="Show the picker and inspector"
              >
                <IconChevron direction="left" />
                <span class="wv-rail__label">PICKER · INSPECTOR</span>
              </div>
            }
          >
            <SidePanel />
          </Show>

          <EffectEditors />
          <Show when={view.sampleEditor() !== null}>
            <SampleEditor />
          </Show>
        </div>
      </Show>
      <StatusBar />
    </div>
  );
}
