import { Show, type JSX } from 'solid-js';
import { useAppState } from '../state';
import { useLayout } from './layout';
import { MixerView } from '../panels/mixer/MixerView';
import { StepGrid } from '../panels/editor/StepGrid';
import { RollView } from '../panels/editor/RollView';
import { IconMaximise } from '../ui/icons';
import { hitCount, PATTERN_LENGTHS, withPatternLength } from '../lib/pattern';

/** Height of the bottom panel when it is not maximised. The mock's 300 px. */
const BOTTOM_HEIGHT = 300;

/**
 * The bottom half: the mixer, or the editor.
 *
 * Two tabs rather than two panels, because they are two views of the same
 * selected track and only one of them is ever wanted at a time. F7 (and the
 * grip) hands the whole centre column to whichever is showing -- which is what a
 * piano roll needs and a mixer with sixteen strips appreciates.
 */
export function BottomPanel(): JSX.Element {
  const app = useAppState();
  const layout = useLayout();

  const pattern = () => app.pattern();
  const editorMeta = () => {
    const current = pattern();
    if (!current) return '';
    return layout.editorMode() === 'notes'
      ? `${current.notes.length} notes`
      : `${hitCount(current)} steps`;
  };

  return (
    <section
      class="wv-bottom"
      style={
        layout.maximised()
          ? { flex: '1', height: 'auto' }
          : { flex: 'none', height: `${BOTTOM_HEIGHT}px` }
      }
    >
      <button
        type="button"
        class="wv-bottom__grip"
        title={layout.maximised() ? 'Restore the arrangement (F7)' : 'Maximise (F7)'}
        aria-label="Toggle the maximised editor"
        onClick={() => layout.toggleMaximised()}
      >
        <span />
      </button>

      <div class="wv-bottom__tabs">
        <button
          type="button"
          class="wv-btab"
          classList={{ 'is-active': layout.bottomTab() === 'mixer' }}
          aria-pressed={layout.bottomTab() === 'mixer'}
          onClick={() => layout.setBottomTab('mixer')}
        >
          MIXER
          <span class="wv-btab__meta">
            {app.mixer()?.tracks.length ?? 0} ch
          </span>
        </button>
        <button
          type="button"
          class="wv-btab"
          classList={{ 'is-active': layout.bottomTab() === 'editor' }}
          aria-pressed={layout.bottomTab() === 'editor'}
          onClick={() => layout.setBottomTab('editor')}
        >
          EDITOR
          <span class="wv-btab__meta">{editorMeta()}</span>
        </button>

        <span class="wv-spacer" />

        <div class="wv-bottom__actions">
          <Show when={layout.bottomTab() === 'editor'}>
            <label class="wv-f7" title="Pattern length, in bars">
              <select
                aria-label="Pattern length in bars"
                value={pattern()?.steps ?? 16}
                onChange={(event) => {
                  const steps = Number(event.currentTarget.value);
                  app.editPattern((current) => withPatternLength(current, steps));
                }}
              >
                {PATTERN_LENGTHS.map((steps) => (
                  <option value={steps}>{steps / 16} bar{steps === 16 ? '' : 's'}</option>
                ))}
              </select>
            </label>
            <button
              type="button"
              class="wv-modetoggle"
              title="Switch between the note editor and the step grid"
              onClick={() =>
                layout.setEditorMode(layout.editorMode() === 'notes' ? 'steps' : 'notes')
              }
            >
              {layout.editorMode() === 'notes' ? 'Notes / Steps' : 'Steps / Notes'}
            </button>
          </Show>
          <button
            type="button"
            class="wv-f7"
            title={layout.maximised() ? 'Restore the arrangement' : 'Maximise this panel'}
            onClick={() => layout.toggleMaximised()}
          >
            <IconMaximise />
            F7
          </button>
        </div>
      </div>

      <Show when={layout.bottomTab() === 'mixer'}>
        <MixerView />
      </Show>
      <Show when={layout.bottomTab() === 'editor'}>
        <Show when={layout.editorMode() === 'notes'} fallback={<StepGrid />}>
          <RollView />
        </Show>
      </Show>
    </section>
  );
}
