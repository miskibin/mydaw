/**
 * The shell.
 *
 * Editors share the Browser. Playback follows the last musical editor used;
 * the channel rack floats above the playlist or mixer.
 */

import { Match, Show, Switch, createEffect, createMemo, on, onCleanup } from 'solid-js';

import { Browser } from './shell/Browser';
import { ExportDialog } from './shell/ExportDialog';
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
import { PluginWindow } from './ui/PluginWindow';
import { bindPointerActivation } from './ui/pointerActivation';

export function App(): ReturnType<typeof Show> {
  const app = useAppState();
  const view = useView();

  // Restore the selected channel after Open/New without changing a mixer bus
  // selection on every ordinary engine refresh.
  // Memoize readiness: reading mixer() in a dependency function still reruns
  // on every fresh DTO, even when that function keeps returning `true`.
  const mixerReady = createMemo(() => app.mixer() !== undefined);
  const initialRackHeight = () => Math.min(480, Math.max(180, app.tracks().length * 42 + 104));
  createEffect(on([app.projectRevision, mixerReady], () => {
    const index = app.mixer()?.selected;
    if (index === undefined) return;
    view.setStrip({ kind: 'track', index });
    view.setSelectedClips([]);
    view.closeRoll();
    view.closeSampleEditor();
    view.closeInsertEditor();
    app.setPlayMode(view.playbackMode());
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
    // FL's record toggle. Not while the typing keyboard is on: there R is a note.
    if (event.code === 'KeyR' && !event.ctrlKey && !event.metaKey && !event.altKey && !event.shiftKey &&
        !view.typingKeyboard() && !target?.closest('.wv-menu, .wv-modal, [role="menu"]')) {
      event.preventDefault();
      if (!event.repeat) app.toggleRecord();
      return;
    }
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
        if (target?.closest('.wv-plug:not(.wv-rack-window), .wv-menu, .wv-modal')) break;
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
        <Show when={!view.rollOpen() && view.workspaceView() === 'playlist'}><PlaylistPicker /></Show>
        <div class="wv-workspace" ref={(element) => bindPointerActivation(element, view.focusWorkspace)} onFocusIn={() => view.focusWorkspace()}>
          <Show when={view.rollOpen()} fallback={
            <Switch fallback={<Playlist />}>
              <Match when={view.workspaceView() === 'settings'}><Settings /></Match>
              <Match when={view.workspaceView() === 'playlist'}><Playlist /></Match>
              <Match when={view.workspaceView() === 'mixer'}><Mixer /></Match>
            </Switch>
          }><Roll /></Show>
        </div>
        <Show when={!view.rollOpen()}><SidePanel /></Show>
        <Show when={mixerReady() && view.rackOpen() && !view.rollOpen() && view.workspaceView() !== 'settings'}>
          <PluginWindow
            title="Channel Rack"
            class="wv-rack-window"
            embeddedHeader
            focused={view.view() === 'rack'}
            z={30}
            defaultWidth={1000}
            defaultHeight={initialRackHeight()}
            minHeight={180}
            offset={0}
            geometry={view.rackGeometry() ?? { x: view.browserWidth() + 192, y: 56, w: 1000, h: initialRackHeight() }}
            onGeometryChange={view.setRackGeometry}
            onFocus={() => view.go('rack')}
            onClose={() => view.closeRack()}
          ><Rack onClose={() => view.closeRack()} /></PluginWindow>
        </Show>
        <EffectEditors />
        <Show when={view.sampleEditor() !== null}><SampleEditor /></Show>
      </div>
      <Show when={app.exporter.open()}><ExportDialog /></Show>
      <StatusBar />
    </div>
  );
}
