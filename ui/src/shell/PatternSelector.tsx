/**
 * The active pattern, always on the top bar so the rest of the shell can say
 * which pattern is being edited without opening the rack.
 *
 * Click opens the list. A second click within the double-click window opens
 * that pattern's editor (the same button cannot listen for both `click` and
 * `dblclick` without flashing the menu). F2 turns the chip into a rename field.
 */

import { For, Show, createEffect, createSignal } from 'solid-js';

import { usePatternNav } from './patternNav';
import { useAppState } from '../state';
import { trackColor } from '../theme/tokens';
import { IconCaret } from '../ui/icons';
import { useView } from './view';

/** Windows' default double-click time. */
const DOUBLE_CLICK_MS = 500;

export function PatternSelector(): ReturnType<typeof Show> {
  const app = useAppState();
  const view = useView();
  const nav = usePatternNav();
  const [menu, setMenu] = createSignal(false);
  let lastChipClick = 0;

  const activeId = () => app.sequencer()?.patternSet.activeId ?? -1;
  const active = () => app.patterns().find((row) => row.id === activeId());
  const full = () => {
    const max = app.sequencer()?.patternSet.maxPatterns ?? 0;
    return max > 0 && app.patterns().length >= max;
  };
  const onlyOne = () => app.patterns().length <= 1;
  const colour = (id: number, color: string | null | undefined) => color ?? trackColor(id);
  const renaming = () => view.renamingPattern() === activeId();

  createEffect(() => {
    if (view.renamingPattern() !== null) setMenu(false);
  });
  createEffect(() => {
    view.view();
    view.rollOpen();
    setMenu(false);
  });

  function commitRename(raw: string, current: string, id: number): void {
    view.setRenamingPattern(null);
    const name = raw.trim();
    if (name && name !== current) {
      void app.sequencerCommand((backend) => backend.renamePattern(id, name));
    }
  }

  function onChipClick(event: MouseEvent): void {
    const now = Date.now();
    if (now - lastChipClick < DOUBLE_CLICK_MS) {
      lastChipClick = 0;
      setMenu(false);
      const id = activeId();
      if (id < 0) return;
      void nav.openPattern(id, { altKey: event.altKey, surface: 'menu' });
      return;
    }
    lastChipClick = now;
    setMenu((open) => !open);
  }

  return (
    <div class="wv-patselect">
      <Show
        when={renaming() && active()}
        fallback={
          <button
            type="button"
            class="wv-patselect__btn"
            title="Click to switch pattern · double-click to edit · F2 to rename"
            onClick={onChipClick}
          >
            <span
              class="wv-swatch"
              style={{ background: colour(activeId(), active()?.color) }}
            />
            <span class="wv-patselect__name">{active()?.name ?? 'Pattern'}</span>
            <IconCaret />
          </button>
        }
      >
        {(pattern) => (
          <input
            class="wv-patselect__input"
            value={pattern().name}
            ref={(el) => queueMicrotask(() => el.select())}
            onKeyDown={(event) => {
              event.stopPropagation();
              if (event.key === 'Escape') view.setRenamingPattern(null);
              if (event.key !== 'Enter') return;
              commitRename(event.currentTarget.value, pattern().name, pattern().id);
            }}
            onBlur={(event) =>
              commitRename(event.currentTarget.value, pattern().name, pattern().id)
            }
          />
        )}
      </Show>

      <Show when={menu()}>
        <>
          <div class="wv-scrim" onClick={() => setMenu(false)} />
          <div class="wv-menu wv-menu--pattern">
            <For each={app.patterns()}>
              {(summary) => (
                <button
                  type="button"
                  class="wv-menu__row"
                  classList={{ 'is-active': summary.id === activeId() }}
                  title="Click to make this the active pattern · double-click to open it"
                  onClick={() => {
                    void app.sequencerCommand((backend) => backend.setActivePattern(summary.id));
                  }}
                  onDblClick={(event) => {
                    event.preventDefault();
                    setMenu(false);
                    void nav.openPattern(summary.id, { altKey: event.altKey, surface: 'menu' });
                  }}
                >
                  <span class="wv-swatch" style={{ background: colour(summary.id, summary.color) }} />
                  <span class="wv-patselect__name">{summary.name}</span>
                </button>
              )}
            </For>
            <div class="wv-menu__rule" />
            <button
              type="button"
              class="wv-menu__row wv-can-disable"
              disabled={full()}
              title={full() ? 'The engine is at its pattern limit' : 'Add an empty pattern'}
              onClick={() => {
                setMenu(false);
                void app.sequencerCommand((backend) => backend.addPattern());
              }}
            >
              New pattern
            </button>
            <button
              type="button"
              class="wv-menu__row wv-can-disable"
              disabled={full() || activeId() < 0}
              title="Duplicate the active pattern"
              onClick={() => {
                const id = activeId();
                setMenu(false);
                if (id < 0) return;
                void app.sequencerCommand((backend) => backend.clonePattern(id));
              }}
            >
              Clone
            </button>
            <button
              type="button"
              class="wv-menu__row wv-menu__row--danger wv-can-disable"
              disabled={onlyOne() || activeId() < 0}
              title={onlyOne() ? 'The last pattern cannot be removed' : 'Remove the active pattern'}
              onClick={() => {
                const id = activeId();
                setMenu(false);
                if (id < 0) return;
                void app.sequencerCommand((backend) => backend.removePattern(id));
              }}
            >
              Delete
            </button>
          </div>
        </>
      </Show>
    </div>
  );
}
