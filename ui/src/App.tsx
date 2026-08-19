/**
 * The shell.
 *
 * Three main views and a fullscreen piano roll over them. Which one is up is
 * pure view state -- switching never changes what the transport plays, which is
 * what the PAT/SONG toggle in the top bar is for.
 */

import { Match, Show, Switch, onCleanup } from 'solid-js';

import { Browser } from './shell/Browser';
import { SidePanel } from './shell/SidePanel';
import { StatusBar } from './shell/StatusBar';
import { TopBar } from './shell/TopBar';
import { useView } from './shell/view';
import { useAppState } from './state';
import { IconChevron } from './ui/icons';
import { EffectEditor } from './views/EffectEditor';
import { Mixer } from './views/Mixer';
import { Playlist } from './views/Playlist';
import { Rack } from './views/Rack';
import { Roll } from './views/Roll';

export function App(): ReturnType<typeof Show> {
  const app = useAppState();
  const view = useView();

  // UI zoom is the webview's own (`zoomHotkeysEnabled` in tauri.conf.json):
  // Ctrl+= / Ctrl+- / Ctrl+0 are handled natively by WebView2, which scales
  // viewport units and fixed positioning correctly — a CSS `zoom` on the body
  // did not (100vh stays viewport-sized while everything else grows).

  /** The view hotkeys. F7 opens the roll on whichever channel is selected. */
  function onKeyDown(event: KeyboardEvent): void {
    const target = event.target as HTMLElement | null;
    if (target && (target.tagName === 'INPUT' || target.isContentEditable)) return;
    switch (event.key) {
      case 'F5':
        event.preventDefault();
        view.go('playlist');
        break;
      case 'F6':
        event.preventDefault();
        view.go('rack');
        break;
      case 'F9':
        event.preventDefault();
        view.go('mixer');
        break;
      case 'F7': {
        event.preventDefault();
        const strip = view.strip();
        view.openRoll(strip.kind === 'track' ? strip.index : (app.mixer()?.selected ?? 0));
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

          {/*
            The effect editor covers the three views the way the roll does, but
            *over* this row rather than instead of it: the inspector behind it is
            what polls the chain's meters, and unmounting it would stop the
            gain-reduction bar the editor is drawing. Never up at the same time
            as the roll -- `openInsertEditor` and `openRoll` each close the other.
          */}
          <Show when={view.insertEditor()}>
            <EffectEditor />
          </Show>
        </div>
      </Show>
      <StatusBar />
    </div>
  );
}
