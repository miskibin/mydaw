/**
 * The pattern selector's dropdown.
 *
 * Every pattern command the engine has is reachable from here -- switch,
 * create, duplicate, rename, recolour, delete -- because a picker that offers
 * only some of them sends the user hunting for the rest.
 *
 * Deleting takes the arrangement's clips of that pattern with it. The engine
 * does that in one command precisely so a set without the pattern and an
 * arrangement still naming it can never exist, so the row says how many clips
 * will go before it happens.
 */

import { For, Show, createSignal } from 'solid-js';

import { useAppState } from '../state';
import { trackColor, TRACK_COLORS } from '../theme/tokens';
import { IconX } from '../ui/icons';

export function PatternMenu(props: { onClose: () => void }): ReturnType<typeof Show> {
  const app = useAppState();
  const [renaming, setRenaming] = createSignal<number | null>(null);
  const [colouring, setColouring] = createSignal<number | null>(null);

  const clipCount = (patternId: number) =>
    app.playlist()?.clips.filter((clip) => clip.patternId === patternId).length ?? 0;

  function remove(patternId: number, name: string): void {
    const clips = clipCount(patternId);
    void app.sequencerCommand((backend) => backend.removePattern(patternId));
    app.clearHistory(`deleting ${name}`);
    app.say('Pattern · Delete', clips > 0 ? `${name} and ${clips} clip${clips > 1 ? 's' : ''}` : name);
  }

  return (
    <div class="wv-scrim" onClick={() => props.onClose()}>
      <div class="wv-menu wv-menu--pattern" onClick={(event) => event.stopPropagation()}>
        <For each={app.patterns()}>
          {(pattern) => (
            <div
              class="wv-menu__row"
              classList={{ 'is-active': pattern.id === app.sequencer()?.patternSet.activeId }}
            >
              <button
                type="button"
                class="wv-swatch wv-patrow__swatch"
                style={{ background: pattern.color ?? trackColor(pattern.id) }}
                title="Change this pattern's colour"
                onClick={(event) => {
                  event.stopPropagation();
                  setColouring((id) => (id === pattern.id ? null : pattern.id));
                }}
              />
              <Show
                when={renaming() === pattern.id}
                fallback={
                  <button
                    type="button"
                    class="wv-patrow__name"
                    title="Click to edit this pattern · double-click to rename"
                    onClick={() => {
                      void app.sequencerCommand((backend) => backend.setActivePattern(pattern.id));
                      props.onClose();
                    }}
                    onDblClick={(event) => {
                      event.stopPropagation();
                      setRenaming(pattern.id);
                    }}
                  >
                    {pattern.name}
                  </button>
                }
              >
                <input
                  class="wv-patrow__input"
                  value={pattern.name}
                  ref={(el) => queueMicrotask(() => el.select())}
                  onKeyDown={(event) => {
                    event.stopPropagation();
                    if (event.key === 'Escape') setRenaming(null);
                    if (event.key !== 'Enter') return;
                    const name = event.currentTarget.value.trim();
                    setRenaming(null);
                    if (name && name !== pattern.name) {
                      void app.sequencerCommand((backend) =>
                        backend.renamePattern(pattern.id, name),
                      );
                    }
                  }}
                  onBlur={(event) => {
                    const name = event.currentTarget.value.trim();
                    setRenaming(null);
                    if (name && name !== pattern.name) {
                      void app.sequencerCommand((backend) =>
                        backend.renamePattern(pattern.id, name),
                      );
                    }
                  }}
                />
              </Show>
              <span class="wv-spacer" />
              <span class="wv-menu__meta">
                {pattern.hitCount + pattern.noteCount} events
              </span>
              <button
                type="button"
                class="wv-menu__x"
                title={`Duplicate ${pattern.name}`}
                onClick={() =>
                  void app.sequencerCommand((backend) => backend.clonePattern(pattern.id))
                }
              >
                ⧉
              </button>
              <button
                type="button"
                class="wv-menu__x"
                title={
                  app.patterns().length === 1
                    ? 'A project must keep at least one pattern'
                    : `Delete ${pattern.name}${clipCount(pattern.id) > 0 ? ` and its ${clipCount(pattern.id)} clip(s)` : ''}`
                }
                disabled={app.patterns().length === 1}
                classList={{ 'wv-disabled': app.patterns().length === 1 }}
                onClick={() => remove(pattern.id, pattern.name)}
              >
                <IconX />
              </button>
              <Show when={colouring() === pattern.id}>
                <div class="wv-palette" onClick={(event) => event.stopPropagation()}>
                  <For each={TRACK_COLORS}>
                    {(colour) => (
                      <button
                        type="button"
                        class="wv-palette__swatch"
                        style={{ background: colour }}
                        onClick={() => {
                          setColouring(null);
                          void app.sequencerCommand((backend) =>
                            backend.setPatternColor(pattern.id, colour),
                          );
                        }}
                      />
                    )}
                  </For>
                  <button
                    type="button"
                    class="wv-palette__swatch wv-palette__swatch--clear"
                    title="Use the default colour"
                    onClick={() => {
                      setColouring(null);
                      void app.sequencerCommand((backend) =>
                        backend.setPatternColor(pattern.id, null),
                      );
                    }}
                  />
                </div>
              </Show>
            </div>
          )}
        </For>
        <button
          type="button"
          class="wv-dashed wv-menu__add"
          disabled={app.patterns().length >= (app.sequencer()?.patternSet.maxPatterns ?? 32)}
          title={
            app.patterns().length >= (app.sequencer()?.patternSet.maxPatterns ?? 32)
              ? 'This project already has as many patterns as the engine allows'
              : 'Add an empty pattern'
          }
          onClick={() => void app.sequencerCommand((backend) => backend.addPattern())}
        >
          + New pattern
        </button>
      </div>
    </div>
  );
}
