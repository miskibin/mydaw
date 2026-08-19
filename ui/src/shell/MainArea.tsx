import { createSignal, Match, Show, Switch, type JSX } from 'solid-js';
import { PianoRollPanel } from '../panels/piano-roll/PianoRollPanel';
import { useAppState } from '../state';

/** The views the workspace can show. `mixer` is Phase 2 and stays disabled. */
type WorkspaceView = 'arrangement' | 'piano-roll';

/** Manifest id of the piano roll. Mirrors `PIANO_ROLL_PANEL` in the plugin. */
const PIANO_ROLL_ID = 'piano-roll';

/**
 * The main workspace.
 *
 * Two tabs are real now. **Arrangement** is still the Phase 2 placeholder plus
 * the playhead ruler that gives `lib/playhead.ts` something visible to drive;
 * **Piano Roll** is the step 9 editor, and it lives here rather than in the left
 * dock because it is the one panel that needs the window (see `WORKSPACE_PANELS`
 * in `panels/registry.ts`).
 *
 * The tab only appears once the kernel has actually contributed the panel. A tab
 * that is present before the manifest arrives would be a tab that opens an empty
 * box, and a shell that draws views the engine has not offered is a shell that
 * will eventually draw one the engine no longer has.
 */
export function MainArea(): JSX.Element {
  const app = useAppState();
  const [view, setView] = createSignal<WorkspaceView>('arrangement');

  const hasPianoRoll = () => app.panels().some((panel) => panel.id === PIANO_ROLL_ID);

  /** Seconds of timeline shown across the ruler. */
  const windowSeconds = 8;
  const playheadPercent = () => ((app.positionSeconds() % windowSeconds) / windowSeconds) * 100;

  const tab = (id: WorkspaceView, label: string) => (
    <button
      type="button"
      class="workspace__tab"
      classList={{ 'is-active': view() === id }}
      aria-pressed={view() === id}
      onClick={() => setView(id)}
    >
      {label}
    </button>
  );

  return (
    <main class="workspace">
      <div class="workspace__toolbar">
        {tab('arrangement', 'Arrangement')}
        <Show when={hasPianoRoll()}>{tab('piano-roll', 'Piano Roll')}</Show>
        <span class="workspace__tab is-disabled">Mixer</span>
      </div>

      <Switch>
        <Match when={view() === 'piano-roll'}>
          <PianoRollPanel
            manifest={{ id: PIANO_ROLL_ID, title: 'Piano Roll', component: PIANO_ROLL_ID }}
          />
        </Match>
        <Match when={view() === 'arrangement'}>
          <div class="ruler" aria-hidden="true">
            <div class="ruler__ticks" />
            <div class="ruler__playhead" style={{ left: `${playheadPercent()}%` }} />
          </div>

          <div class="workspace__canvas">
            <div class="workspace__placeholder">
              <h1>Arrangement</h1>
              <p>
                The timeline arrives in Phase 2. Patterns are written in the Piano Roll tab
                and the Step Sequencer panel; both edit the same looping pattern, and both
                feed the same scheduler.
              </p>
              <ul class="workspace__checklist">
                <li>Panel registry mirrors Rust-side contributions</li>
                <li>Transport buttons call the kernel through one abstraction</li>
                <li>Meters draw on canvas at display rate, fed by a 30-60 Hz stream</li>
                <li>Playhead interpolates locally between sparse position updates</li>
              </ul>
            </div>
          </div>
        </Match>
      </Switch>
    </main>
  );
}
