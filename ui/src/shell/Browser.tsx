/** FL-style Browser: find, audition, replace, then keep writing. */
import {
  createContext,
  createEffect,
  createMemo,
  createSignal,
  For,
  onCleanup,
  onMount,
  Show,
  useContext,
  type Accessor,
  type JSX,
} from 'solid-js';
import type {
  BrowserPreviewDto,
  LibraryEntryDto,
  LibraryRootDto,
  LibrarySearchDto,
} from '../api/types';
import {
  clampBrowserWidth,
  DEFAULT_BROWSER_WIDTH,
  MIN_BROWSER_WIDTH,
  MAX_BROWSER_WIDTH,
} from '../lib/browserSize';
import { LatestRequest, sampleKeyAction, waveformPath } from '../lib/browserWorkflow';
import { encodeDrop } from '../lib/dnd';
import { filterInstruments, instrumentRows, type InstrumentRow } from '../lib/instruments';
import { useAppState } from '../state';
import { startDrag } from '../ui/drag';
import {
  IconCaret,
  IconChevron,
  IconClose,
  IconMore,
  IconPlay,
  IconRefresh,
  IconSearch,
  IconStop,
  IconVolume,
  IconWaveform,
} from '../ui/icons';
import { useView, type BrowserTab } from './view';
import { usePatternNav } from './patternNav';
import '../styles/browser.css';

export function formatDuration(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return '--';
  return seconds >= 60
    ? `${Math.floor(seconds / 60)}:${Math.floor(seconds % 60)
        .toString()
        .padStart(2, '0')}`
    : `${seconds.toFixed(2)}s`;
}
export function formatRate(rate: number): string {
  if (!Number.isFinite(rate) || rate <= 0) return '--';
  return `${(rate / 1000).toFixed(rate % 1000 === 0 ? 0 : 1)}k`;
}

interface BrowserActions {
  selected: Accessor<{ path: string; name: string } | null>;
  select(path: string, name: string): void;
  place(path: string, replace: boolean): void;
  audition(): void;
  menu(event: MouseEvent, path: string, name: string): void;
}
const BrowserContext = createContext<BrowserActions>();
function useBrowser(): BrowserActions {
  return useContext(BrowserContext)!;
}

export function Browser(): JSX.Element {
  const view = useView();
  // Keep the tree and query mounted when hidden or when the roll is opened.
  return (
    <>
      <Show when={!view.browserOpen()}>
        <button
          type="button"
          class="wv-rail wv-rail--left"
          aria-label="Show browser"
          title="Browser (Alt+F8)"
          onClick={() => view.setBrowserOpen(true)}
        >
          <IconChevron direction="right" />
          <span class="wv-rail__label">BROWSER</span>
        </button>
      </Show>
      <BrowserBody />
    </>
  );
}

function BrowserBody(): JSX.Element {
  const app = useAppState();
  const view = useView();
  const [query, setQuery] = createSignal('');
  const [roots, setRoots] = createSignal<LibraryRootDto[]>([]);
  const [revision, setRevision] = createSignal(0);
  const [rootError, setRootError] = createSignal<string | null>(null);
  const [selected, setSelected] = createSignal<{ path: string; name: string } | null>(null);
  const [preview, setPreview] = createSignal<BrowserPreviewDto | null>(null);
  const [previewError, setPreviewError] = createSignal<string | null>(null);
  const [previewBusy, setPreviewBusy] = createSignal(false);
  const [autoPreview, setAutoPreview] = createSignal(true);
  const [previewInfo, setPreviewInfo] = createSignal(false);
  const [optionsMenu, setOptionsMenu] = createSignal<{ x: number; y: number } | null>(null);
  const [volume, setVolume] = createSignal(80);
  const [placing, setPlacing] = createSignal(false);
  const [menu, setMenu] = createSignal<{ path: string; name: string; x: number; y: number } | null>(
    null,
  );
  const requests = new LatestRequest();
  let searchInput: HTMLInputElement | undefined;
  let tree: HTMLDivElement | undefined;
  let volumeTimer: ReturnType<typeof setTimeout> | undefined;
  let lastAudibleVolume = 80;

  function changePreviewVolume(percent: number): void {
    setVolume(percent);
    if (percent > 0) lastAudibleVolume = percent;
    clearTimeout(volumeTimer);
    volumeTimer = setTimeout(
      () => void app.backend.setPreviewVolume(percent).catch(app.report),
      100,
    );
  }

  function toggleAutoPreview(): void {
    setAutoPreview((on) => !on);
    if (!autoPreview()) {
      requests.cancel();
      setPreviewBusy(false);
      void app.backend.stopBrowserPreview().catch(app.report);
    }
  }

  function refresh(): void {
    setRootError(null);
    void app.backend
      .getLibraryRoots()
      .then(setRoots)
      .catch((error) => setRootError(String(error)));
    setRevision((n) => n + 1);
  }
  onMount(() => {
    refresh();
    void app.backend
      .getPreviewSettings()
      .then((settings) => {
        setVolume(settings.volumePercent);
        if (settings.volumePercent > 0) lastAudibleVolume = settings.volumePercent;
      })
      .catch(app.report);
    void app.loadDevices();
  });
  onCleanup(() => {
    requests.cancel();
    clearTimeout(volumeTimer);
  });

  async function select(path: string, name: string, forceAudition = false): Promise<void> {
    setSelected({ path, name });
    setPreview(null);
    setPreviewError(null);
    setPreviewBusy(true);
    const ticket = requests.begin();
    try {
      const result = await app.backend.previewBrowserFile(path, forceAudition || autoPreview());
      if (requests.current(ticket)) setPreview(result);
    } catch (error) {
      if (requests.current(ticket)) setPreviewError(String(error));
    } finally {
      if (requests.current(ticket)) setPreviewBusy(false);
    }
  }

  async function place(path: string, replace: boolean): Promise<void> {
    if (placing()) return;
    const chosen = app.tracks().find((track) => track.index === app.mixer()?.selected);
    if (replace && !chosen) return;
    setPlacing(true);
    let applied = false;
    try {
      if (replace) {
        const source = await app.backend.loadAudioSource(path);
        await app.mixerCommand(async (backend) => {
          const next = await backend.setTrackSource(chosen!.index, `oneshot:${source.id}`);
          applied = true;
          return next;
        });
        if (!applied) return;
        app.say('Replace sample', `${chosen!.name} → ${source.name}`);
      } else {
        await app.mixerCommand(async (backend) => {
          const next = await backend.addSampleChannel(path);
          applied = true;
          return backend.setSelectedTrack(next.tracks.length - 1);
        });
        if (!applied) return;
        view.setStrip({ kind: 'track', index: app.mixer()?.selected ?? 0 });
        app.say('New sampler channel', path.split(/[\\/]/).at(-1) ?? path);
      }
      await app.refresh();
      // A source replacement changes the meaning of old notes; expose this
      // established structural-history boundary instead of stale inverse edits.
      app.clearHistory(replace ? 'replacing a channel sample' : 'adding a sampler channel');
    } catch (error) {
      app.report(error);
    } finally {
      setPlacing(false);
    }
  }

  const actions: BrowserActions = {
    selected,
    select: (path, name) => void select(path, name),
    place: (path, replace) => void place(path, replace),
    audition: () => {
      const item = selected();
      if (item) void select(item.path, item.name, true);
    },
    menu: (event, path, name) => {
      event.preventDefault();
      setSelected({ path, name });
      // Loading visual data does not change the song or selected channel.
      void select(path, name);
      setMenu({
        path,
        name,
        x: Math.max(8, Math.min(event.clientX, window.innerWidth - 230)),
        y: Math.max(8, Math.min(event.clientY, window.innerHeight - 150)),
      });
    },
  };

  function keyDown(event: KeyboardEvent): void {
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'f') {
      event.preventDefault();
      event.stopPropagation();
      searchInput?.focus();
      searchInput?.select();
      return;
    }
    if (event.key === 'Escape') {
      if (optionsMenu()) {
        event.preventDefault();
        event.stopPropagation();
        setOptionsMenu(null);
      } else if (menu()) {
        event.preventDefault();
        event.stopPropagation();
        setMenu(null);
      } else if (query()) {
        event.preventDefault();
        event.stopPropagation();
        setQuery('');
      }
      return;
    }
    if (event.target instanceof HTMLInputElement && event.target !== searchInput) return;
    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      const rows = Array.from(
        tree?.querySelectorAll<HTMLElement>('[data-browser-file]') ?? [],
      ).filter((row) => row.getClientRects().length > 0);
      if (rows.length === 0) return;
      const current = rows.findIndex((row) => row === document.activeElement);
      const next = Math.max(
        0,
        Math.min(
          rows.length - 1,
          current < 0
            ? event.key === 'ArrowUp'
              ? rows.length - 1
              : 0
            : current + (event.key === 'ArrowDown' ? 1 : -1),
        ),
      );
      const row = rows[next]!;
      event.preventDefault();
      event.stopPropagation();
      row.focus();
      row.scrollIntoView({ block: 'nearest' });
      const path = row.dataset.browserFile;
      if (path) void select(path, row.dataset.browserName ?? path);
      if (event.shiftKey && path) void place(path, true);
      return;
    }
    // Search typing and action buttons retain their native keyboard behavior.
    if (
      event.target === searchInput ||
      !(event.target instanceof HTMLElement) ||
      !event.target.hasAttribute('data-browser-file')
    )
      return;
    const action = sampleKeyAction(event.key, event.shiftKey);
    if (action) {
      event.preventDefault();
      event.stopPropagation();
      const path = event.target.dataset.browserFile!;
      if (action === 'preview') void select(path, event.target.dataset.browserName ?? path, true);
      else void place(path, action === 'replace');
    }
  }

  function width(value: number): void {
    view.setBrowserWidth(
      clampBrowserWidth(
        value,
        window.innerWidth,
        view.sideOpen(),
        view.view() === 'playlist' && !view.rollOpen(),
      ),
    );
  }
  const tabs: { id: BrowserTab; label: string }[] = [
    { id: 'all', label: 'All' },
    { id: 'files', label: 'Samples' },
    { id: 'instruments', label: 'Instruments' },
  ];

  return (
    <BrowserContext.Provider value={actions}>
      <aside
        class="wv-browser"
        style={{ width: `${view.browserWidth()}px`, display: view.browserOpen() ? 'flex' : 'none' }}
        aria-label="Browser"
        onKeyDown={keyDown}
      >
        <div class="wv-browser__head">
          <div class="wv-browser__tabs wv-tabs" role="tablist" aria-label="Browser sections">
            <For each={tabs}>
              {(tab) => (
                <button
                  type="button"
                  role="tab"
                  class="wv-tab"
                  classList={{ 'is-active': view.browserTab() === tab.id }}
                  aria-label={tab.label}
                  title={tab.label}
                  aria-selected={view.browserTab() === tab.id}
                  onClick={() => view.setBrowserTab(tab.id)}
                >
                  <span classList={{ 'wv-browser__tab-full': tab.id === 'instruments' }}>
                    {tab.label}
                  </span>
                  <Show when={tab.id === 'instruments'}>
                    <span class="wv-browser__tab-short" aria-hidden="true">
                      Inst.
                    </span>
                  </Show>
                </button>
              )}
            </For>
          </div>
          <button
            type="button"
            class="wv-browser__collapse"
            aria-label="Refresh browser"
            title="Refresh folders and search"
            onClick={refresh}
          >
            <IconRefresh />
          </button>
          <button
            type="button"
            class="wv-browser__collapse"
            aria-label="Browser options"
            aria-expanded={optionsMenu() !== null}
            title="Browser options"
            onClick={(event) => {
              const rect = event.currentTarget.getBoundingClientRect();
              setOptionsMenu((open) =>
                open ? null : { x: Math.max(8, rect.right - 216), y: rect.bottom + 6 },
              );
            }}
          >
            <IconMore />
          </button>
        </div>
        <div class="wv-browser__searchbar">
          <span class="wv-browser__search-icon" aria-hidden="true">
            <IconSearch />
          </span>
          <input
            ref={searchInput}
            type="search"
            class="wv-browser__search"
            placeholder="Search sounds…"
            aria-label="Search browser"
            value={query()}
            onInput={(e) => setQuery(e.currentTarget.value)}
          />
          <Show when={query()}>
            <button
              type="button"
              class="wv-tree__btn wv-browser__clear-search"
              aria-label="Clear search"
              onClick={() => {
                setQuery('');
                searchInput?.focus();
              }}
            >
              <IconClose />
            </button>
          </Show>
        </div>
        <div class="wv-browser__tree" ref={tree}>
          <Show when={rootError()}>
            {(error) => (
              <div class="wv-empty" role="alert">
                {error()}
                <button type="button" class="wv-chip" onClick={refresh}>
                  Retry
                </button>
              </div>
            )}
          </Show>
          <div style={{ display: view.browserTab() === 'instruments' ? 'none' : 'contents' }}>
            <div class="wv-browser__actions">
              <button
                type="button"
                class="wv-chip"
                title="Add your sample pack folder"
                onClick={() =>
                  void app.backend
                    .addLibraryFolder()
                    .then((next) => {
                      if (next) {
                        setRoots(next);
                        setRevision((n) => n + 1);
                      }
                    })
                    .catch(app.report)
                }
              >
                + Folder
              </button>
              <button
                type="button"
                class="wv-chip"
                title="Import a sample into a new channel"
                onClick={() =>
                  void app.backend
                    .openAudioFile()
                    .then((source) => {
                      if (source) void place(source.path, false);
                    })
                    .catch(app.report)
                }
              >
                Import sample…
              </button>
            </div>
            <Show when={query().trim()}>
              <SearchFiles query={query().trim()} revision={revision()} />
            </Show>
            <div style={{ display: query().trim() ? 'none' : 'contents' }}>
              <div style={{ display: view.browserTab() === 'all' ? 'contents' : 'none' }}>
                <ProjectTree />
              </div>
              <For each={roots()}>
                {(root) => (
                  <LibraryDir
                    root={root}
                    relPath=""
                    label={root.name}
                    depth={0}
                    revision={revision()}
                    initiallyOpen={root.kind !== 'extra'}
                    remove={
                      root.kind === 'extra'
                        ? () =>
                            void app.backend
                              .removeLibraryFolder(root.id)
                              .then(setRoots)
                              .catch(app.report)
                        : undefined
                    }
                  />
                )}
              </For>
            </div>
          </div>
          <div style={{ display: view.browserTab() === 'files' ? 'none' : 'contents' }}>
            <InstrumentsTree
              query={query()}
              collapsed={view.browserTab() === 'all' && !query().trim()}
            />
          </div>
        </div>
        <AssetFooter />

        <div class="wv-browser__preview">
          <div class="wv-browser__preview-tabs" role="group" aria-label="Sample details">
            <button
              type="button"
              class="wv-chip"
              classList={{ 'is-active': !previewInfo() }}
              aria-pressed={!previewInfo()}
              onClick={() => setPreviewInfo(false)}
            >
              Audition
            </button>
            <button
              type="button"
              class="wv-chip"
              classList={{ 'is-active': previewInfo() }}
              aria-pressed={previewInfo()}
              onClick={() => setPreviewInfo(true)}
            >
              Info
            </button>
          </div>
          <Show
            when={selected()}
            fallback={
              <div class="wv-browser__preview-hint">
                Select a sample to listen.
                <br />
                Drag to the rack or playlist.
              </div>
            }
          >
            {(item) => (
              <>
                <div class="wv-browser__preview-name" title={item().path}>
                  {item().name}
                </div>
                <Show when={previewBusy()}>
                  <div class="wv-tree__note" role="status">
                    Loading preview…
                  </div>
                </Show>
                <Show when={previewError()}>
                  {(error) => (
                    <div class="wv-browser__preview-error" role="alert">
                      {error()}
                    </div>
                  )}
                </Show>
                <Show
                  when={previewInfo()}
                  fallback={
                    <Show when={preview()}>
                      {(wave) => (
                        <>
                          <svg
                            class="wv-browser__waveform"
                            viewBox="0 0 256 48"
                            preserveAspectRatio="none"
                            role="img"
                            aria-label={`Waveform of ${item().name}`}
                          >
                            <path
                              d={waveformPath(wave().peaks)}
                              fill="none"
                              stroke="currentColor"
                              stroke-width="1"
                            />
                          </svg>
                          <span class="wv-row__meta wv-browser__preview-meta">
                            {wave().truncated ? 'First ' : ''}
                            {formatDuration(wave().durationSeconds)} ·{' '}
                            {formatRate(wave().sampleRate)}Hz
                          </span>
                        </>
                      )}
                    </Show>
                  }
                >
                  <dl class="wv-browser__info">
                    <dt>File</dt>
                    <dd class="wv-browser__info-path" title={item().path}>
                      {item().path}
                    </dd>
                    <Show when={preview()}>
                      {(wave) => (
                        <>
                          <dt>Preview</dt>
                          <dd>
                            {wave().truncated ? 'First ' : ''}
                            {formatDuration(wave().durationSeconds)}
                          </dd>
                          <dt>Sample rate</dt>
                          <dd>{wave().sampleRate.toLocaleString()} Hz</dd>
                        </>
                      )}
                    </Show>
                  </dl>
                </Show>
              </>
            )}
          </Show>
          <div class="wv-browser__preview-controls">
            <button
              type="button"
              class="wv-browser__preview-button"
              aria-label="Play sample preview"
              title="Play selected sample"
              disabled={!selected()}
              onClick={actions.audition}
            >
              <IconPlay />
            </button>
            <button
              type="button"
              class="wv-browser__preview-button"
              aria-label="Stop sample preview"
              title="Stop preview; keep the song playing"
              onClick={() => {
                requests.cancel();
                setPreviewBusy(false);
                void app.backend.stopBrowserPreview().catch(app.report);
              }}
            >
              <IconStop />
            </button>
            <button
              type="button"
              class="wv-browser__volume-button"
              aria-label="Mute sample preview"
              aria-pressed={volume() === 0}
              title={volume() === 0 ? 'Restore preview volume' : 'Mute preview'}
              onClick={() => changePreviewVolume(volume() === 0 ? lastAudibleVolume : 0)}
            >
              <IconVolume muted={volume() === 0} />
            </button>
            <input
              type="range"
              min="0"
              max="100"
              value={volume()}
              style={{ '--preview-level': `${volume()}%` }}
              aria-label="Preview volume"
              title={`Preview volume ${volume()}%`}
              onInput={(event) => changePreviewVolume(Number(event.currentTarget.value))}
            />
          </div>
          <div class="wv-browser__sample-actions">
            <button
              type="button"
              class="wv-chip"
              disabled={
                !selected() || placing() || app.tracks().length >= (app.mixer()?.maxTracks ?? 16)
              }
              title="New sampler channel (Shift+Enter or double-click)"
              onClick={() => {
                const item = selected();
                if (item) void place(item.path, false);
              }}
            >
              + Channel
            </button>
            <button
              type="button"
              class="wv-chip"
              disabled={!selected() || placing() || !app.tracks().length}
              title={`Send to ${app.tracks()[app.mixer()?.selected ?? 0]?.name ?? 'selected channel'} (Enter)`}
              onClick={() => {
                const item = selected();
                if (item) void place(item.path, true);
              }}
            >
              Replace
            </button>
          </div>
        </div>
        <Show when={optionsMenu()}>
          {(position) => (
            <div class="wv-scrim" onClick={() => setOptionsMenu(null)}>
              <div
                class="wv-menu"
                role="menu"
                style={{
                  position: 'fixed',
                  left: `${position().x}px`,
                  top: `${position().y}px`,
                  width: '216px',
                }}
                onClick={(event) => event.stopPropagation()}
              >
                <button
                  type="button"
                  class="wv-menu__row"
                  role="menuitemcheckbox"
                  aria-checked={autoPreview()}
                  onClick={() => {
                    toggleAutoPreview();
                    setOptionsMenu(null);
                  }}
                >
                  Audition on selection
                  <span class="wv-spacer" />
                  <span class="wv-menu__meta">{autoPreview() ? 'On' : 'Off'}</span>
                </button>
                <button
                  type="button"
                  class="wv-menu__row"
                  role="menuitem"
                  onClick={() => {
                    changePreviewVolume(80);
                    setOptionsMenu(null);
                  }}
                >
                  Reset preview volume
                  <span class="wv-spacer" />
                  <span class="wv-menu__meta">80%</span>
                </button>
                <div class="wv-menu__rule" />
                <button
                  type="button"
                  class="wv-menu__row"
                  role="menuitem"
                  onClick={() => {
                    setOptionsMenu(null);
                    view.setBrowserOpen(false);
                  }}
                >
                  Hide Browser
                  <span class="wv-spacer" />
                  <span class="wv-menu__meta">F8</span>
                </button>
              </div>
            </div>
          )}
        </Show>
        <div
          class="wv-browser__resize"
          role="separator"
          tabindex="0"
          aria-label="Resize browser"
          aria-orientation="vertical"
          aria-valuemin={MIN_BROWSER_WIDTH}
          aria-valuemax={MAX_BROWSER_WIDTH}
          aria-valuenow={view.browserWidth()}
          title="Drag to resize · double-click to reset"
          onPointerDown={(event) =>
            startDrag(event, {
              from: view.browserWidth(),
              perPixel: 1,
              min: MIN_BROWSER_WIDTH,
              max: MAX_BROWSER_WIDTH,
              axis: 'horizontal',
              fineFactor: 1,
              onChange: width,
            })
          }
          onDblClick={() => width(DEFAULT_BROWSER_WIDTH)}
          onKeyDown={(event) => {
            if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
              event.preventDefault();
              event.stopPropagation();
              width(
                view.browserWidth() +
                  (event.key === 'ArrowRight' ? 1 : -1) * (event.shiftKey ? 32 : 8),
              );
            }
          }}
        />
      </aside>
      <Show when={menu()}>
        {(item) => (
          <div
            class="wv-scrim"
            onClick={() => setMenu(null)}
            onContextMenu={(event) => {
              event.preventDefault();
              setMenu(null);
            }}
          >
            <div
              class="wv-menu"
              style={{
                position: 'fixed',
                left: `${item().x}px`,
                top: `${item().y}px`,
                width: '222px',
              }}
              onClick={(e) => e.stopPropagation()}
              role="menu"
            >
              <button
                type="button"
                class="wv-menu__row"
                role="menuitem"
                onClick={() => {
                  actions.audition();
                  setMenu(null);
                }}
              >
                Preview
                <span class="wv-spacer" />
                <span class="wv-menu__meta">Space</span>
              </button>
              <button
                type="button"
                class="wv-menu__row"
                role="menuitem"
                disabled={placing() || !app.tracks().length}
                onClick={() => {
                  void place(item().path, true);
                  setMenu(null);
                }}
              >
                Send to selected channel
                <span class="wv-spacer" />
                <span class="wv-menu__meta">Enter</span>
              </button>
              <button
                type="button"
                class="wv-menu__row"
                role="menuitem"
                disabled={placing() || app.tracks().length >= (app.mixer()?.maxTracks ?? 16)}
                onClick={() => {
                  void place(item().path, false);
                  setMenu(null);
                }}
              >
                Open in new channel
                <span class="wv-spacer" />
                <span class="wv-menu__meta">⇧ Enter</span>
              </button>
            </div>
          </div>
        )}
      </Show>
    </BrowserContext.Provider>
  );
}

function Folder(props: {
  label: string;
  open: boolean;
  toggle(): void;
  depth?: number;
  count?: number;
  remove?: (() => void) | undefined;
}): JSX.Element {
  return (
    <div class="wv-browser__folder-row">
      <button
        type="button"
        class="wv-row wv-tree__dir"
        style={{ 'padding-left': `${8 + (props.depth ?? 0) * 12}px` }}
        aria-expanded={props.open}
        onClick={props.toggle}
        onKeyDown={(e) => {
          if ((e.key === 'ArrowRight' && !props.open) || (e.key === 'ArrowLeft' && props.open)) {
            e.preventDefault();
            e.stopPropagation();
            props.toggle();
          }
        }}
      >
        <span class="wv-tree__chev" classList={{ 'is-open': props.open }}>
          <IconChevron direction="right" />
        </span>
        <span class="wv-row__name">{props.label}</span>
        <span class="wv-spacer" />
        <Show when={props.count !== undefined}>
          <span class="wv-row__meta">{props.count}</span>
        </Show>
      </button>
      <Show when={props.remove}>
        <button
          type="button"
          class="wv-tree__btn"
          aria-label={`Remove folder ${props.label}`}
          title="Remove from Browser; keep files on disk"
          onClick={props.remove}
        >
          <IconClose />
        </button>
      </Show>
    </div>
  );
}

function SampleRow(props: {
  entry: LibraryEntryDto;
  depth?: number;
  location?: string;
}): JSX.Element {
  const browser = useBrowser();
  return (
    <div
      class="wv-row wv-tree__leaf wv-browser__sample"
      classList={{ 'is-active': browser.selected()?.path === props.entry.path }}
      style={{ 'padding-left': `${22 + (props.depth ?? 0) * 12}px` }}
      role="button"
      tabindex="0"
      aria-label={props.entry.name}
      data-browser-file={props.entry.path}
      data-browser-name={props.entry.name}
      draggable="true"
      title={`${props.entry.path}\nClick: preview · Double-click: new channel · Enter: replace selected · Drag: rack or playlist`}
      onClick={() => browser.select(props.entry.path, props.entry.name)}
      onDblClick={() => browser.place(props.entry.path, false)}
      onContextMenu={(e) => browser.menu(e, props.entry.path, props.entry.name)}
      onDragStart={(e) => {
        e.dataTransfer?.setData('text/plain', encodeDrop('file', props.entry.path));
        if (e.dataTransfer) e.dataTransfer.effectAllowed = 'copy';
      }}
    >
      <span class="wv-browser__file-icon" aria-hidden="true">
        <IconWaveform />
      </span>
      <span class="wv-browser__file-text">
        <span class="wv-row__name">{props.entry.name}</span>
        <Show when={props.location}>
          <span class="wv-browser__location">{props.location}</span>
        </Show>
      </span>
      <button
        type="button"
        class="wv-tree__btn wv-browser__add"
        aria-label={`Actions for ${props.entry.name}`}
        title="Preview, replace or add channel"
        onClick={(e) => {
          e.stopPropagation();
          browser.menu(e, props.entry.path, props.entry.name);
        }}
        onDblClick={(event) => event.stopPropagation()}
      >
        <IconMore />
      </button>
    </div>
  );
}

function LibraryDir(props: {
  root: LibraryRootDto;
  relPath: string;
  label: string;
  depth: number;
  revision: number;
  initiallyOpen?: boolean;
  remove?: (() => void) | undefined;
}): JSX.Element {
  const app = useAppState();
  const [open, setOpen] = createSignal(props.initiallyOpen ?? false);
  const [entries, setEntries] = createSignal<LibraryEntryDto[] | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const requests = new LatestRequest();
  let loadedRevision = -1;
  async function load(): Promise<void> {
    const ticket = requests.begin();
    setError(null);
    try {
      const listed = await app.backend.listLibraryDir(props.root.id, props.relPath);
      if (requests.current(ticket)) {
        setEntries(listed);
        loadedRevision = props.revision;
      }
    } catch (err) {
      if (requests.current(ticket)) setError(String(err));
    }
  }
  createEffect(() => {
    const revision = props.revision;
    if (open() && revision !== loadedRevision) void load();
  });
  onCleanup(() => requests.cancel());
  return (
    <>
      <Folder
        label={props.label}
        depth={props.depth}
        open={open()}
        toggle={() => setOpen((value) => !value)}
        remove={props.remove}
      />
      {/* Hiding keeps already visited folders and samples intact. */}
      <div style={{ display: open() ? 'contents' : 'none' }}>
        <Show when={error()}>
          {(message) => (
            <div class="wv-empty" role="alert">
              Folder unavailable.<span class="wv-empty__hint">{message()}</span>
              <button type="button" class="wv-chip" onClick={() => void load()}>
                Retry
              </button>
            </div>
          )}
        </Show>
        <Show
          when={entries()}
          fallback={
            <div class="wv-tree__note" role="status">
              Loading folder…
            </div>
          }
        >
          {(listed) => (
            <>
              <Show when={listed().length === 0 && !error()}>
                <div class="wv-tree__note">
                  {props.root.kind === 'packs' && !props.relPath
                    ? 'Add a sample folder or import a file.'
                    : 'No audio files here.'}
                </div>
              </Show>
              <Show when={listed().length >= 500}>
                <div class="wv-tree__note">Showing up to 500 items. Use search for more.</div>
              </Show>
              <For each={listed()}>
                {(entry) =>
                  entry.kind === 'dir' ? (
                    <LibraryDir
                      root={props.root}
                      relPath={entry.relPath}
                      label={entry.name}
                      depth={props.depth + 1}
                      revision={props.revision}
                    />
                  ) : (
                    <SampleRow entry={entry} depth={props.depth + 1} />
                  )
                }
              </For>
            </>
          )}
        </Show>
      </div>
    </>
  );
}

function SearchFiles(props: { query: string; revision: number }): JSX.Element {
  const app = useAppState();
  const [result, setResult] = createSignal<LibrarySearchDto | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const requests = new LatestRequest();
  let timer: ReturnType<typeof setTimeout> | undefined;
  createEffect(() => {
    const query = props.query;
    props.revision;
    const ticket = requests.begin();
    clearTimeout(timer);
    setResult(null);
    setError(null);
    timer = setTimeout(
      () =>
        void app.backend
          .searchLibrary(query)
          .then((next) => {
            if (requests.current(ticket)) setResult(next);
          })
          .catch((err) => {
            if (requests.current(ticket)) setError(String(err));
          }),
      180,
    );
  });
  onCleanup(() => {
    clearTimeout(timer);
    requests.cancel();
  });
  return (
    <>
      <Show when={error()}>
        {(message) => (
          <div class="wv-empty" role="alert">
            Search failed.<span class="wv-empty__hint">{message()}</span>
          </div>
        )}
      </Show>
      <Show
        when={result()}
        fallback={
          <Show when={!error()}>
            <div class="wv-tree__note" role="status">
              Searching samples…
            </div>
          </Show>
        }
      >
        {(found) => (
          <>
            <div class="wv-browser__section-label">{found().entries.length} samples</div>
            <Show when={found().truncated}>
              <div class="wv-tree__note">Search limit reached. Narrow your search.</div>
            </Show>
            <Show when={found().unavailableRoots.length}>
              <div class="wv-tree__note">Unavailable: {found().unavailableRoots.join(', ')}</div>
            </Show>
            <Show when={!found().entries.length}>
              <div class="wv-empty">
                No matching samples.
                <span class="wv-empty__hint">Try a name like kick, clap or hat.</span>
              </div>
            </Show>
            <For each={found().entries}>
              {(row) => (
                <SampleRow
                  entry={row.entry}
                  location={`${row.rootName} / ${row.entry.relPath.split('/').slice(0, -1).join('/')}`}
                />
              )}
            </For>
          </>
        )}
      </Show>
    </>
  );
}

function InstrumentsTree(props: { query: string; collapsed: boolean }): JSX.Element {
  const app = useAppState();
  const view = useView();
  const [open, setOpen] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const rows = createMemo(() =>
    filterInstruments(
      instrumentRows(app.sources(), app.clapPlugins(), app.vst3Plugins()),
      props.query,
    ),
  );
  async function add(row: InstrumentRow): Promise<void> {
    if (busy() || app.tracks().length >= (app.mixer()?.maxTracks ?? 16)) return;
    setBusy(true);
    let applied = false;
    try {
      await app.mixerCommand(async (backend) => {
        const added = await backend.addTrack(
          row.place.via === 'source' ? row.place.id : 'poly-synth',
          row.label,
        );
        const index = added.tracks.length - 1;
        try {
          if (row.place.via === 'clap')
            await backend.setTrackSourceClap(index, row.place.path, row.place.pluginId);
          if (row.place.via === 'vst3')
            await backend.setTrackSourceVst3(index, row.place.path, row.place.pluginId);
        } catch (error) {
          await backend.removeTrack(index);
          throw error;
        }
        applied = true;
        return backend.setSelectedTrack(index);
      });
      if (!applied) return;
      view.setStrip({ kind: 'track', index: app.mixer()?.selected ?? 0 });
      app.clearHistory('adding an instrument channel');
    } finally {
      setBusy(false);
    }
  }
  return (
    <>
      <Show when={props.collapsed}>
        <Folder
          label="Instruments"
          count={rows().length}
          open={open()}
          toggle={() => setOpen((value) => !value)}
        />
      </Show>
      <div style={{ display: !props.collapsed || open() ? 'contents' : 'none' }}>
        <div class="wv-browser__actions">
          <span class="wv-browser__section-label">Instruments</span>
          <span class="wv-spacer" />
          <button
            type="button"
            class="wv-chip"
            disabled={app.scanning()}
            onClick={() => void app.loadDevices(true)}
            title="Scan installed instrument plugins"
          >
            {app.scanning() ? 'Scanning…' : 'Rescan'}
          </button>
        </div>
        <Show when={!rows().length}>
          <div class="wv-empty">No matching instruments.</div>
        </Show>
        <For each={rows()}>
          {(row) => (
            <div
              class="wv-row wv-tree__leaf"
              role="button"
              tabindex="0"
              aria-label={row.label}
              draggable="true"
              title={`${row.detail}\nDouble-click or +: new channel. Drag to rack.`}
              onClick={() => {
                if (row.place.via === 'source')
                  void app.backend.previewInstrument(row.place.id).catch(app.report);
              }}
              onDblClick={() => void add(row)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') {
                  e.preventDefault();
                  e.stopPropagation();
                  void add(row);
                }
              }}
              onDragStart={(e) => {
                e.dataTransfer?.setData('text/plain', row.dragData);
                if (e.dataTransfer) e.dataTransfer.effectAllowed = 'copy';
              }}
            >
              <span class="wv-browser__file-icon" aria-hidden="true">
                <IconWaveform />
              </span>
              <span class="wv-row__name">{row.label}</span>
              <span class="wv-row__meta">{row.badge}</span>
              <button
                type="button"
                class="wv-tree__btn"
                aria-label={`Add ${row.label} to rack`}
                disabled={busy() || app.tracks().length >= (app.mixer()?.maxTracks ?? 16)}
                onClick={(e) => {
                  e.stopPropagation();
                  void add(row);
                }}
              >
                +
              </button>
            </div>
          )}
        </For>
      </div>
    </>
  );
}

function ProjectTree(): JSX.Element {
  const app = useAppState();
  const view = useView();
  const nav = usePatternNav();
  const [open, setOpen] = createSignal(false);
  return (
    <>
      <Folder label="Current project" open={open()} toggle={() => setOpen((value) => !value)} />
      <div style={{ display: open() ? 'contents' : 'none' }}>
        <div class="wv-browser__section-label">Patterns</div>
        <For each={app.patterns()}>
          {(pattern) => (
            <button
              type="button"
              class="wv-row wv-tree__leaf"
              draggable="true"
              title="Click to paint this pattern · double-click to edit"
              onClick={() => {
                view.setBrush({ kind: 'pattern', id: pattern.id });
                view.setPlaylistTool('paint');
              }}
              onDblClick={(e) =>
                void nav.openPattern(pattern.id, { surface: 'picker', altKey: e.altKey })
              }
              onDragStart={(e) => {
                e.dataTransfer?.setData('text/plain', encodeDrop('pattern', pattern.id));
                if (e.dataTransfer) e.dataTransfer.effectAllowed = 'copy';
              }}
            >
              <span class="wv-swatch" style={{ background: pattern.color ?? 'var(--acc)' }} />
              <span class="wv-row__name">{pattern.name}</span>
            </button>
          )}
        </For>
        <div class="wv-browser__section-label">Channels</div>
        <For each={app.tracks()}>
          {(track) => (
            <button
              type="button"
              class="wv-row wv-tree__leaf"
              title="Select channel · double-click for piano roll"
              onClick={() => {
                app.selectTrack(track.index);
                view.setStrip({ kind: 'track', index: track.index });
              }}
              onDblClick={() => {
                app.promoteStepsToRoll(track.index);
                view.openRoll(track.index);
              }}
            >
              <span class="wv-row__name">{track.name}</span>
            </button>
          )}
        </For>
        <Show when={app.audioSources().length}>
          <div class="wv-browser__section-label">Project audio</div>
          <For each={app.audioSources()}>
            {(source) => (
              <SampleRow
                entry={{ name: source.name, path: source.path, relPath: source.name, kind: 'file' }}
              />
            )}
          </For>
        </Show>
      </div>
    </>
  );
}

function AssetFooter(): JSX.Element {
  const app = useAppState();
  const available = () => app.assets().filter((asset) => asset.state !== 'ready');
  return (
    <Show when={available().length}>
      <details class="wv-browser__downloads">
        <summary title={`${available().length} sound packs available`}>
          <span class="wv-browser__get-sounds">
            <IconWaveform />
            Get more sounds
          </span>
          <IconCaret />
        </summary>
        <div class="wv-browser__download-list">
          <For each={available()}>
            {(asset) => (
              <div class="wv-asset">
                <div class="wv-asset__row">
                  <span class="wv-asset__name" title={asset.name}>
                    {asset.name}
                  </span>
                  <span class="wv-spacer" />
                  <Show
                    when={asset.state === 'downloading'}
                    fallback={
                      <button
                        type="button"
                        class="wv-chip"
                        title={`Download ${asset.name} (${Math.round(asset.bytesTotal / 1_000_000)} MB)`}
                        onClick={() => app.startAssetDownload(asset.id)}
                      >
                        {asset.state === 'failed' ? 'Retry' : 'Get'}
                      </button>
                    }
                  >
                    <span class="wv-row__meta">{Math.round(asset.progress * 100)}%</span>
                  </Show>
                </div>
                <Show when={asset.error}>
                  <span class="wv-asset__error" role="alert">
                    {asset.error}
                  </span>
                </Show>
              </div>
            )}
          </For>
        </div>
      </details>
    </Show>
  );
}
