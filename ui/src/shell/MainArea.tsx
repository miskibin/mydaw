import { type JSX } from 'solid-js';
import { useAppState } from '../state';

/**
 * The main workspace. Phase 2 fills this with the arrangement/pattern views;
 * for now it carries the playhead ruler stub so the interpolation in
 * `lib/playhead.ts` has something visible to drive.
 */
export function MainArea(): JSX.Element {
  const app = useAppState();
  /** Seconds of timeline shown across the ruler. */
  const windowSeconds = 8;
  const playheadPercent = () => ((app.positionSeconds() % windowSeconds) / windowSeconds) * 100;

  return (
    <main class="workspace">
      <div class="workspace__toolbar">
        <span class="workspace__tab is-active">Arrangement</span>
        <span class="workspace__tab is-disabled">Piano Roll</span>
        <span class="workspace__tab is-disabled">Mixer</span>
      </div>

      <div class="ruler" aria-hidden="true">
        <div class="ruler__ticks" />
        <div class="ruler__playhead" style={{ left: `${playheadPercent()}%` }} />
      </div>

      <div class="workspace__canvas">
        <div class="workspace__placeholder">
          <h1>Arrangement</h1>
          <p>
            Timeline, patterns and the mixer arrive in Phase 2. This shell exists to prove the
            kernel boots, contributes panels, and can stream meters through the IPC bridge.
          </p>
          <ul class="workspace__checklist">
            <li>Panel registry mirrors Rust-side contributions</li>
            <li>Transport buttons call the kernel through one abstraction</li>
            <li>Meters draw on canvas at display rate, fed by a 30-60 Hz stream</li>
            <li>Playhead interpolates locally between sparse position updates</li>
          </ul>
        </div>
      </div>
    </main>
  );
}
