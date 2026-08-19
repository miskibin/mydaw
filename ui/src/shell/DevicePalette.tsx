import { createMemo, createSignal, For, onMount, Show, type JSX } from 'solid-js';
import { useAppState } from '../state';
import { useLayout } from './layout';
import { IconSearch } from '../ui/icons';

/** One offerable effect, whichever format it came from. */
interface Entry {
  key: string;
  name: string;
  vendor: string;
  format: 'CLAP' | 'VST3';
  path: string;
  pluginId: string;
}

/**
 * The ⌘K / Ctrl+K device palette: a searchable list of every scanned audio
 * effect, placed on the **master insert**.
 *
 * The header says "master insert" in as many words rather than "add device",
 * because that is the only insert MYDAW has: a palette that implied the effect
 * was going on the selected track would be lying about where the sound goes.
 * When per-track chains exist this becomes a palette with a target, and the
 * honest label is what will make that change visible rather than silent.
 */
export function DevicePalette(): JSX.Element {
  const app = useAppState();
  const layout = useLayout();
  const [query, setQuery] = createSignal('');
  const [active, setActive] = createSignal(0);
  let input: HTMLInputElement | undefined;

  onMount(() => {
    app.ensureDevices();
    input?.focus();
  });

  const entries = createMemo<Entry[]>(() => {
    const needle = query().trim().toLowerCase();
    const all: Entry[] = [
      ...app.masterEffects().map((plugin) => ({
        key: `clap:${plugin.path}:${plugin.id}`,
        name: plugin.name || plugin.id,
        vendor: plugin.vendor,
        format: 'CLAP' as const,
        path: plugin.path,
        pluginId: plugin.id,
      })),
      ...app.vst3MasterEffects().map((plugin) => ({
        key: `vst3:${plugin.path}:${plugin.id}`,
        name: plugin.name || plugin.id,
        vendor: plugin.vendor,
        format: 'VST3' as const,
        path: plugin.path,
        pluginId: plugin.id,
      })),
    ];
    if (needle.length === 0) return all;
    return all.filter((entry) =>
      `${entry.name} ${entry.vendor} ${entry.format}`.toLowerCase().includes(needle),
    );
  });

  const place = (entry: Entry) => {
    app.mixerCommand((api) =>
      entry.format === 'CLAP'
        ? api.setMasterEffectClap(entry.path, entry.pluginId)
        : api.setMasterEffectVst3(entry.path, entry.pluginId),
    );
    layout.setPaletteOpen(false);
  };

  const onKeyDown = (event: KeyboardEvent) => {
    const list = entries();
    if (event.key === 'Escape') {
      event.preventDefault();
      layout.setPaletteOpen(false);
      return;
    }
    if (event.key === 'ArrowDown') {
      event.preventDefault();
      setActive((index) => Math.min(list.length - 1, index + 1));
      return;
    }
    if (event.key === 'ArrowUp') {
      event.preventDefault();
      setActive((index) => Math.max(0, index - 1));
      return;
    }
    if (event.key === 'Enter') {
      event.preventDefault();
      const chosen = list[active()];
      if (chosen) place(chosen);
      return;
    }
    // Everything else is typing, and must not reach the window-level piano.
    event.stopPropagation();
  };

  return (
    <div
      class="wv-palette"
      role="dialog"
      aria-label="Master insert devices"
      style={{ right: layout.inspectorOpen() ? '344px' : '46px' }}
      onKeyDown={onKeyDown}
    >
      <div class="wv-palette__head">
        <IconSearch />
        <input
          ref={input}
          type="text"
          value={query()}
          placeholder="Search effects"
          aria-label="Search effects"
          onInput={(event) => {
            setQuery(event.currentTarget.value);
            setActive(0);
          }}
        />
        <button
          type="button"
          class="wv-palette__esc"
          onClick={() => layout.setPaletteOpen(false)}
        >
          esc
        </button>
      </div>
      <span class="wv-palette__title">MASTER INSERT</span>
      <div class="wv-palette__list">
        <For
          each={entries()}
          fallback={
            <span class="wv-note">
              {app.scanning() ? 'Scanning…' : 'No audio effects installed.'}
            </span>
          }
        >
          {(entry, index) => (
            <button
              type="button"
              class="wv-palette__row"
              classList={{ 'is-active': index() === active() }}
              title={`${entry.name}${entry.vendor ? ` — ${entry.vendor}` : ''}\n${entry.path}`}
              onPointerEnter={() => setActive(index())}
              onClick={() => place(entry)}
            >
              <span>{entry.name}</span>
              <span class="wv-spacer" />
              <span class="wv-palette__kind">{entry.format}</span>
            </button>
          )}
        </For>
      </div>
      <Show when={app.mixer()?.master.clap ?? app.mixer()?.master.vst3}>
        <button
          type="button"
          class="wv-palette__row"
          onClick={() => {
            app.mixerCommand((api) => api.clearMasterEffect());
            layout.setPaletteOpen(false);
          }}
        >
          <span>Remove the master insert</span>
        </button>
      </Show>
    </div>
  );
}
