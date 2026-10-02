/**
 * The shell.
 *
 * Editors share the Browser. PAT/SONG is an explicit transport choice, so
 * opening the rack or piano roll while a song plays never interrupts it.
 */

import { Match, Show, Switch, createEffect, createMemo, on, onCleanup } from 'solid-js';

import { Browser } from './shell/Browser';
import { SidePanel, PlaylistPicker } from './shell/SidePanel';
import { StatusBar } from './shell/StatusBar';
import { TopBar } from './shell/TopBar';
import { midiKeyFor, noteName } from './lib/keyboard';
import { f7PatternId, rollChannelFromStrip, tabCounterpart } from './lib/navigate';
import { useView } from './shell/view';
import { useAppState } from './state';
import { EffectEditors } from './views/EffectEditor';
import { Mixer } from './views/Mixer';
import { Playlist } from './views/Playlist';
import { Rack } from './views/Rack';
import { Roll } from './views/Roll';
import { SampleEditor } from './views/SampleEditor';
import { Settings } from './views/Settings';

export function App(): ReturnType<typeof Show> {
  const app = useAppState();
  const view = useView();

  // Restore the selected channel after Open/New without changing a mixer bus
  // selection on every ordinary engine refresh.
  // Memoize readiness: reading mixer() in a dependency function still reruns
  // on every fresh DTO, even when that function keeps returning `true`.
  const mixerReady = createMemo(() => app.mixer() !== undefined);
  createEffect(on([app.projectRevision, mixerReady], () => {
    const index = app.mixer()?.selected;
    if (index === undefined) return;
    view.setStrip({ kind: 'track', index });
    view.setSelectedClips([]);
    view.closeRoll();
    view.closeSampleEditor();
    view.closeInsertEditor();
  }));

  // UI zoom is the webview's own (`zoomHotkeysEnabled` in tauri.conf.json):
  // Ctrl+= / Ctrl+- / Ctrl+0 are handled natively by WebView2, which scales
  // viewport units and fixed positioning correctly — a CSS `zoom` on the body
  // did not (100vh stays viewport-sized while everything else grows).

  /** The view hotkeys. F7 opens the roll on whichever channel is selected. */
  function onKeyDown(event: KeyboardEvent): void {
    const target = event.target as HTMLElement | null;
    if (target && (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.tagName === 'SELECT' || target.isContentEditable)) return;
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
    if (event.defaultPrevented) return;
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'f') {
      event.preventDefault();
      view.setBrowserOpen(true);
      const search = document.querySelector<HTMLInputElement>('[aria-label="Search browser"]');
      search?.focus(); search?.select(); return;
    }
    if (event.key === 'Tab' && !event.ctrlKey && !event.altKey && !event.metaKey && !target?.closest('button, a, [tabindex], select')) {
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
        if (target?.closest('.wv-plug, .wv-menu, .wv-modal')) break;
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
        if (view.rollOpen()) { view.closeRoll(); break; }
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
      <div class="wv-body">
        <Browser />
        <Show when={!view.rollOpen() && view.view() === 'playlist'}><PlaylistPicker /></Show>
        <Show when={view.rollOpen()} fallback={
          <Switch fallback={<Rack />}>
            <Match when={view.view() === 'settings'}><Settings /></Match>
            <Match when={view.view() === 'playlist'}><Playlist /></Match>
            <Match when={view.view() === 'rack'}><Rack /></Match>
            <Match when={view.view() === 'mixer'}><Mixer /></Match>
          </Switch>
        }><Roll /></Show>
        <Show when={!view.rollOpen()}><SidePanel /></Show>
        <EffectEditors />
        <Show when={view.sampleEditor() !== null}><SampleEditor /></Show>
      </div>
      <StatusBar />
    </div>
  );
}
