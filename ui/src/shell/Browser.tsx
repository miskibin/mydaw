/**
 * The browser: what a channel can play, and where audio comes from.
 *
 * Two tabs. **Instruments** is one searched list of everything that can sit at
 * the head of a channel -- the built-ins, the sampled instruments, and every
 * scanned CLAP and VST3 -- with the format as a badge on the row rather than as
 * a tab of its own. **Files** is the sample library: the read-only Factory root,
 * the Packs drop folder, extra-search folders, and the session's loaded files.
 *
 * Nothing here is decorative -- a row that cannot do anything is drawn
 * `.wv-disabled` with a `title` saying why.
 *
 * # Why not three tabs
 * There were: Sounds, Plugins, Audio. That sorted instruments by *who wrote
 * them*, which is not a question anyone asks while writing music -- and it was
 * not even true, since Poly Synth and the sampler are plugins too. `InstrumentMenu`
 * had already merged the same list for the same reason.
 *
 * # Why effects are not here
 * They are added from the mixer's insert picker, where you are already looking
 * at the strip you want one on. A browser tab for them would be a second place
 * to look for something you reach for from one place.
 *
 * Two deliberate departures from `.design/wave-studio.html`:
 *
 * - The mock's third tab is **Presets**. The engine has no preset system at all,
 *   so drawing one would be a tab that answers nothing.
 * - The mock's footer is a waveform preview of the highlighted row. Clicking a
 *   sample auditions it through the engine; the footer is the asset-download
 *   row, which is real and has nowhere else to live.
 */

import { createMemo, createSignal, For, Index, onMount, Show, type JSX } from 'solid-js';

import type { AudioSourceDto, LibraryEntryDto, LibraryRootDto } from '../api/types';
import {
  clampBrowserWidth,
  DEFAULT_BROWSER_WIDTH,
  MAX_BROWSER_WIDTH,
  MIN_BROWSER_WIDTH,
} from '../lib/browserSize';
import { encodeDrop } from '../lib/dnd';
import {
  filterInstruments,
  instrumentRows,
  type InstrumentRow,
} from '../lib/instruments';
import { classifyLibraryEntry, type LibrarySemantic } from '../lib/librarySemantics';
import { useAppState } from '../state';
import { startDrag } from '../ui/drag';
import { IconChevron, IconClose } from '../ui/icons';
import { LibraryIcon } from '../ui/LibraryIcon';
import { useView, type BrowserTab } from './view';

import '../styles/browser.css';

const GENERIC_FOLDER_SEMANTIC: LibrarySemantic = {
  primary: 'audio',
  shape: null,
  content: null,
  format: null,
  evidence: 'name',
  label: 'Folder',
};

const GENERIC_INSTRUMENT_SEMANTIC: LibrarySemantic = {
  primary: 'synth',
  shape: null,
  content: 'synth',
  format: null,
  evidence: 'name',
  label: 'Instrument plugin',
};

function instrumentPluginSemantic(name: string): LibrarySemantic {
  const inferred = classifyLibraryEntry({ name, relPath: name, kind: 'file' });
  return inferred.primary === 'audio' ? GENERIC_INSTRUMENT_SEMANTIC : inferred;
}

const BROWSER_TABS: readonly { id: BrowserTab; label: string }[] = [
  { id: 'instruments', label: 'Instruments' },
  { id: 'files', label: 'Files' },
];

export function Browser(): JSX.Element {
  const view = useView();
  return (
    <Show when={view.browserOpen()} fallback={<BrowserRail />}>
      <BrowserBody />
    </Show>
  );
}

/** What the collapsed pane leaves behind: the thing that brings it back. */
function BrowserRail(): JSX.Element {
  const view = useView();
  return (
    <button
      type="button"
      class="wv-rail wv-rail--left"
      title="Show the browser"
      aria-label="Show the browser"
      onClick={() => view.setBrowserOpen(true)}
    >
      <IconChevron direction="right" />
      <span class="wv-rail__label">BROWSER</span>
    </button>
  );
}

function BrowserBody(): JSX.Element {
  const view = useView();

  function setWidth(width: number): void {
    view.setBrowserWidth(clampBrowserWidth(width, window.innerWidth, view.sideOpen()));
  }

  function resizeByKeyboard(event: KeyboardEvent): void {
    const step = event.shiftKey ? 32 : 8;
    let next: number | null = null;
    if (event.key === 'ArrowLeft') next = view.browserWidth() - step;
    if (event.key === 'ArrowRight') next = view.browserWidth() + step;
    if (event.key === 'Home') next = MIN_BROWSER_WIDTH;
    if (event.key === 'End') next = MAX_BROWSER_WIDTH;
    if (next === null) return;
    event.preventDefault();
    setWidth(next);
  }

  return (
    <aside class="wv-browser" style={{ width: `${view.browserWidth()}px` }}>
      <div class="wv-browser__head">
        <span class="wv-browser__title">BROWSER</span>
        <span class="wv-spacer" />
        <button
          type="button"
          class="wv-browser__collapse"
          title="Collapse the browser"
          aria-label="Collapse the browser"
          onClick={() => view.setBrowserOpen(false)}
        >
          <IconChevron direction="left" />
        </button>
      </div>

      <div class="wv-browser__tabs wv-tabs" role="tablist" aria-label="Browser sections">
        <For each={BROWSER_TABS}>
          {(tab) => (
            <button
              type="button"
              role="tab"
              class="wv-tab"
              classList={{ 'is-active': view.browserTab() === tab.id }}
              aria-selected={view.browserTab() === tab.id}
              onClick={() => view.setBrowserTab(tab.id)}
            >
              {tab.label}
            </button>
          )}
        </For>
      </div>

      <div class="wv-browser__tree">
        <Show when={view.browserTab() === 'instruments'}>
          <InstrumentsTree />
        </Show>
        <Show when={view.browserTab() === 'files'}>
          <FilesTree />
        </Show>
      </div>

      <AssetFooter />
      <div
        class="wv-browser__resize"
        role="separator"
        tabindex="0"
        aria-label="Resize browser"
        aria-orientation="vertical"
        aria-valuemin={MIN_BROWSER_WIDTH}
        aria-valuemax={clampBrowserWidth(
          MAX_BROWSER_WIDTH,
          window.innerWidth,
          view.sideOpen(),
        )}
        aria-valuenow={view.browserWidth()}
        title="Drag to resize · double-click to reset"
        onPointerDown={(event) => {
          startDrag(event, {
            axis: 'horizontal',
            from: view.browserWidth(),
            perPixel: 1,
            min: MIN_BROWSER_WIDTH,
            max: clampBrowserWidth(MAX_BROWSER_WIDTH, window.innerWidth, view.sideOpen()),
            fineFactor: 1,
            onChange: (width) => view.setBrowserWidth(Math.round(width)),
          });
        }}
        onKeyDown={resizeByKeyboard}
        onDblClick={() => setWidth(DEFAULT_BROWSER_WIDTH)}
      />
    </aside>
  );
}

// ---------------------------------------------------------------------------
// Shared row furniture
// ---------------------------------------------------------------------------

/** A directory row: a chevron that turns, a name and how many leaves are under it. */
function DirRow(props: {
  label: string;
  count?: number | undefined;
  extra?: boolean | undefined;
  depth?: number | undefined;
  open: boolean;
  onToggle: () => void;
  semantic: LibrarySemantic;
  remove?: { title: string; run: () => void } | undefined;
}): JSX.Element {
  return (
    <button
      type="button"
      class="wv-row wv-tree__dir"
      classList={{ 'wv-tree__dir--extra': props.extra === true }}
      style={{ 'padding-left': `${6 + (props.depth ?? 0) * 8}px` }}
      aria-expanded={props.open}
      onClick={() => props.onToggle()}
    >
      <LibraryIcon semantic={props.semantic} kind="dir" open={props.open} />
      <span class="wv-row__name" classList={{ 'is-open': props.open }}>
        {props.label}
      </span>
      <span class="wv-spacer" />
      <Show when={props.count !== undefined}>
        <span class="wv-row__meta">{props.count}</span>
      </Show>
      <Show when={props.remove}>
        {(remove) => (
          <span
            class="wv-tree__btn wv-tree__btn--danger"
            title={remove().title}
            role="button"
            onClick={(event) => {
              event.stopPropagation();
              remove().run();
            }}
          >
            <IconClose />
          </span>
        )}
      </Show>
    </button>
  );
}

/**
 * A leaf row: draggable, clickable, with an indent guide and a `+`.
 *
 * A `div` rather than a `button` because it carries a button of its own and is a
 * drag source; the role and the keyboard handler put back what that costs.
 */
function LeafRow(props: {
  name: string;
  meta: string;
  semantic?: LibrarySemantic | undefined;
  title: string;
  dragData: string;
  depth?: number;
  active?: boolean;
  dim?: boolean;
  /** Absent when the row has nothing to do on a click -- an unplaced plugin has
      no audition, and a row that says "press me" and then does nothing is worse
      than a row that only drags. */
  onClick?: () => void;
  add?: { title: string; disabled: boolean; run: () => void };
  remove?: { title: string; run: () => void };
}): JSX.Element {
  const semantic = createMemo(
    () =>
      props.semantic ??
      classifyLibraryEntry({ name: props.name, relPath: props.name, kind: 'file' }),
  );
  return (
    <div
      class="wv-row wv-tree__leaf"
      classList={{
        'is-active': props.active === true,
        'wv-tree__leaf--dim': props.dim === true,
        'wv-tree__leaf--semantic': true,
      }}
      style={{ 'padding-left': `${8 + (props.depth ?? 0) * 8}px` }}
      role={props.onClick ? 'button' : undefined}
      tabindex={props.onClick ? 0 : undefined}
      draggable={true}
      title={props.title}
      onClick={() => props.onClick?.()}
      onKeyDown={(event) => {
        if (!props.onClick) return;
        if (event.key !== 'Enter' && event.key !== ' ') return;
        event.preventDefault();
        props.onClick();
      }}
      onDragStart={(event) => {
        event.dataTransfer?.setData('text/plain', props.dragData);
        if (event.dataTransfer) event.dataTransfer.effectAllowed = 'copy';
      }}
    >
      <LibraryIcon semantic={semantic()} kind="file" />
      <span class="wv-row__name">{props.name}</span>
      <span class="wv-spacer" />
      <span class="wv-row__meta">{props.meta}</span>
      <Show when={props.add}>
        {(add) => (
          <button
            type="button"
            class="wv-tree__btn wv-can-disable"
            disabled={add().disabled}
            title={add().title}
            aria-label={add().title}
            onClick={(event) => {
              event.stopPropagation();
              add().run();
            }}
          >
            +
          </button>
        )}
      </Show>
      <Show when={props.remove}>
        {(remove) => (
          <button
            type="button"
            class="wv-tree__btn wv-tree__btn--danger"
            title={remove().title}
            aria-label={remove().title}
            onClick={(event) => {
              event.stopPropagation();
              remove().run();
            }}
          >
            <IconClose />
          </button>
        )}
      </Show>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Instruments
// ---------------------------------------------------------------------------

/**
 * Everything a channel can play, in one searched list.
 *
 * The rows are built by `lib/instruments`, which is where the ordering and the
 * search live; this draws them and places them. Rescan sits at the top because
 * it is the one control that opens plugin bundles, and the count next to it is
 * the answer to "did the scan find my plugins".
 */
function InstrumentsTree(): JSX.Element {
  const app = useAppState();
  const [query, setQuery] = createSignal('');

  onMount(() => void app.loadDevices());

  const rows = createMemo(() =>
    instrumentRows(app.sources(), app.clapPlugins(), app.vst3Plugins()),
  );
  const shown = createMemo(() => filterInstruments(rows(), query()));

  const full = () => {
    const mixer = app.mixer();
    return mixer !== undefined && mixer.tracks.length >= mixer.maxTracks;
  };

  /**
   * Add a channel and put the instrument at its head.
   *
   * A built-in is one call. A plugin is two, because `add_track` only takes a
   * built-in source id -- and both go inside one `mixerCommand` so that only the
   * *final* state is adopted. Adopting the intermediate one would flash a
   * channel called "Diva" playing the poly synth.
   */
  function add(row: InstrumentRow): void {
    void app.mixerCommand(async (backend) => {
      if (row.place.via === 'source') {
        return backend.addTrack(row.place.id, row.label);
      }
      const placed = await backend.addTrack('poly-synth', row.label);
      const index = placed.tracks.length - 1;
      return row.place.via === 'clap'
        ? backend.setTrackSourceClap(index, row.place.path, row.place.pluginId)
        : backend.setTrackSourceVst3(index, row.place.path, row.place.pluginId);
    });
  }

  /** Built-ins render offline, so only they can be auditioned from a row. */
  function preview(row: InstrumentRow): void {
    if (row.place.via !== 'source') return;
    void app.backend.previewInstrument(row.place.id).catch(app.report);
  }

  function title(row: InstrumentRow): string {
    const action =
      row.place.via === 'source'
        ? `Click to preview ${row.label}. Drag onto the rack, or + for a channel of its own.`
        : 'Drag onto the rack, or + for a channel of its own.';
    return `${row.detail}\n${action}`;
  }

  return (
    <>
      <div class="wv-browser__bar">
        <input
          type="search"
          class="wv-browser__search"
          placeholder="Search instruments"
          aria-label="Search instruments"
          value={query()}
          onInput={(event) => setQuery(event.currentTarget.value)}
        />
        <button
          type="button"
          class="wv-chip wv-can-disable"
          disabled={app.scanning()}
          title="Open each new plugin to catalogue it. Unlicensed plugins may show an activation window — only click this when you want that."
          onClick={() => void app.loadDevices(true)}
        >
          {app.scanning() ? 'Scanning…' : 'Rescan'}
        </button>
      </div>

      <Show when={app.scanning()}>
        <div class="wv-row wv-tree__note">
          <span class="wv-row__name">scanning…</span>
        </div>
      </Show>

      <Show
        when={shown().length > 0}
        fallback={
          <div class="wv-empty">
            {query().trim().length > 0 ? 'Nothing matches.' : 'No instruments.'}
            <span class="wv-empty__hint">
              {query().trim().length > 0
                ? 'clear the search to see everything'
                : 'Rescan opens each module — unlicensed plugins may ask to activate'}
            </span>
          </div>
        }
      >
        <Index each={shown()}>
          {(row) => (
            <LeafRow
              name={row().label}
              meta={row().badge}
              semantic={
                row().builtIn ? undefined : instrumentPluginSemantic(row().label)
              }
              title={title(row())}
              dragData={row().dragData}
              active={row().selected}
              {...(row().place.via === 'source' ? { onClick: () => preview(row()) } : {})}
              add={{
                title: full()
                  ? `The rack is full (${app.mixer()?.maxTracks ?? 0} channels)`
                  : `Add a rack channel playing ${row().label}`,
                disabled: full(),
                run: () => add(row()),
              }}
            />
          )}
        </Index>
      </Show>
    </>
  );
}


// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

/** `2:07` or `4.20s`, from the file's own duration. */
export function formatDuration(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return '--';
  const minutes = Math.floor(seconds / 60);
  const rest = seconds - minutes * 60;
  return minutes > 0
    ? `${minutes}:${Math.floor(rest).toString().padStart(2, '0')}`
    : `${rest.toFixed(2)}s`;
}

/** `48k`, `44.1k`. */
export function formatRate(rate: number): string {
  if (!Number.isFinite(rate) || rate <= 0) return '--';
  return `${(rate / 1000).toFixed(rate % 1000 === 0 ? 0 : 1)}k`;
}

/**
 * Factory packs, extra-search folders, and the session's loaded files.
 *
 * The tree is a filesystem index: expanding a folder lists it, clicking a
 * sample auditions it, dragging it onto a playlist lane loads it. Decode
 * happens on use, not when the row is drawn.
 */
function FilesTree(): JSX.Element {
  const app = useAppState();
  const view = useView();
  const [roots, setRoots] = createSignal<LibraryRootDto[]>([]);
  const [loadedOpen, setLoadedOpen] = createSignal(true);

  const brushId = () => {
    const brush = view.brush();
    return brush?.kind === 'audio' ? brush.id : null;
  };

  function refreshRoots(): void {
    app.backend.getLibraryRoots().then(setRoots).catch(app.report);
  }

  onMount(refreshRoots);

  function loadFile(): void {
    app.backend
      .openAudioFile()
      .then(async (source) => {
        if (source === null) return;
        await app.refreshAudioSources();
        view.setBrush({ kind: 'audio', id: source.id });
      })
      .catch(app.report);
  }

  function addFolder(): void {
    app.backend
      .addLibraryFolder()
      .then((next) => {
        if (next === null) return;
        setRoots(next);
      })
      .catch(app.report);
  }

  function unload(source: AudioSourceDto): void {
    app.backend
      .unloadAudioSource(source.id)
      .then(() => {
        if (brushId() === source.id) view.setBrush(null);
        return app.refreshAudioSources();
      })
      .catch(app.report);
  }

  return (
    <>
      <button type="button" class="wv-row wv-tree__load" onClick={addFolder}>
        <span class="wv-tree__plus" aria-hidden="true">
          +
        </span>
        <span class="wv-row__name">Add folder…</span>
        <span class="wv-spacer" />
        <span class="wv-row__meta">stays on launch</span>
      </button>
      <button type="button" class="wv-row wv-tree__load" onClick={loadFile}>
        <span class="wv-tree__plus" aria-hidden="true">
          +
        </span>
        <span class="wv-row__name">Load file…</span>
        <span class="wv-spacer" />
        <span class="wv-row__meta">wav · flac · ogg · mp3</span>
      </button>

      <For each={roots()}>
        {(root) => (
          <LibraryDir
            rootId={root.id}
            relPath=""
            label={root.name}
            extra={root.kind === 'extra'}
            depth={0}
            autoOpen={root.kind === 'packs'}
            remove={
              root.kind === 'extra'
                ? {
                    title: 'Remove this folder from the browser — files on disk are not deleted',
                    run: () => {
                      app.backend
                        .removeLibraryFolder(root.id)
                        .then(setRoots)
                        .catch(app.report);
                    },
                  }
                : undefined
            }
          />
        )}
      </For>

      <DirRow
        label="Loaded"
        count={app.audioSources().length}
        open={loadedOpen()}
        onToggle={() => setLoadedOpen((open) => !open)}
        semantic={GENERIC_FOLDER_SEMANTIC}
      />
      <Show when={loadedOpen()}>
        <Show
          when={app.audioSources().length > 0}
          fallback={
            <div class="wv-empty">
              Nothing in this project yet.
              <span class="wv-empty__hint">drag a sample onto a playlist lane</span>
            </div>
          }
        >
          <Index each={app.audioSources()}>
            {(source) => {
              const semantic = createMemo(() =>
                classifyLibraryEntry({
                  name: source().name,
                  relPath: source().path,
                  kind: 'file',
                }),
              );
              return (
                <LeafRow
                  name={source().name}
                  meta={
                    source().missing
                      ? 'missing'
                      : `${formatDuration(source().durationSeconds)} · ${formatRate(source().sampleRate)}`
                  }
                  semantic={semantic()}
                  title={`${source().path}\n${semantic().label} — inferred from ${semantic().evidence}.${source().missing ? '\nThe file has moved: its clips stay and are silent.' : ''}\nClick to load the brush, or drag onto a playlist lane.`}
                  dragData={encodeDrop('audio', source().id)}
                  active={brushId() === source().id}
                  dim={source().missing}
                  onClick={() => view.setBrush({ kind: 'audio', id: source().id })}
                  remove={{
                    title: 'Unload — refused while a clip still plays it',
                    run: () => unload(source()),
                  }}
                />
              );
            }}
          </Index>
        </Show>
      </Show>
    </>
  );
}

function LibraryDir(props: {
  rootId: string;
  relPath: string;
  label: string;
  extra?: boolean | undefined;
  depth: number;
  autoOpen?: boolean | undefined;
  remove?: { title: string; run: () => void } | undefined;
}): JSX.Element {
  const app = useAppState();
  const [open, setOpen] = createSignal(false);
  const [entries, setEntries] = createSignal<LibraryEntryDto[] | null>(null);
  const semantic = classifyLibraryEntry({
    name: props.label,
    relPath: props.relPath || props.label,
    kind: 'dir',
  });

  function load(): void {
    app.backend
      .listLibraryDir(props.rootId, props.relPath)
      .then(setEntries)
      .catch(app.report);
  }

  function toggle(): void {
    const next = !open();
    setOpen(next);
    if (next && entries() === null) load();
  }

  onMount(() => {
    if (props.autoOpen) {
      setOpen(true);
      load();
    }
  });

  return (
    <>
      <DirRow
        label={props.label}
        extra={props.extra}
        depth={props.depth}
        open={open()}
        onToggle={toggle}
        semantic={semantic}
        remove={props.remove}
      />
      <Show when={open()}>
        <Show when={entries()} fallback={<div class="wv-row wv-tree__note">listing…</div>}>
          {(listed) => (
            <Show
              when={listed().length > 0}
              fallback={<div class="wv-row wv-tree__note">empty</div>}
            >
              <For each={listed()}>
                {(entry) =>
                  entry.kind === 'dir' ? (
                    <LibraryDir
                      rootId={props.rootId}
                      relPath={entry.relPath}
                      label={entry.name}
                      extra={props.extra}
                      depth={props.depth + 1}
                    />
                  ) : (
                    <LibraryFile entry={entry} depth={props.depth + 1} />
                  )
                }
              </For>
            </Show>
          )}
        </Show>
      </Show>
    </>
  );
}

function LibraryFile(props: { entry: LibraryEntryDto; depth: number }): JSX.Element {
  const app = useAppState();
  const semantic = classifyLibraryEntry({
    name: props.entry.name,
    relPath: props.entry.relPath,
    kind: 'file',
  });
  return (
    <LeafRow
      name={props.entry.name}
      meta=""
      semantic={semantic}
      title={`${props.entry.path}\n${semantic.label} — inferred from ${semantic.evidence}.\nClick to preview. Drag onto a playlist lane.`}
      dragData={encodeDrop('file', props.entry.path)}
      depth={props.depth}
      onClick={() => {
        app.backend.previewAudioFile(props.entry.path).catch(app.report);
      }}
    />
  );
}

// ---------------------------------------------------------------------------
// The asset footer
// ---------------------------------------------------------------------------

/**
 * The downloadable instruments.
 *
 * Absent from most builds -- `assets()` is empty until the sampler has something
 * to fetch -- and the whole footer goes with it rather than leaving a heading
 * over nothing.
 */
function AssetFooter(): JSX.Element {
  const app = useAppState();
  return (
    <Show when={app.assets().length > 0}>
      <div class="wv-browser__foot">
        <Index each={app.assets()}>
          {(asset) => (
            <div class="wv-asset">
              <div class="wv-asset__row">
                <span class="wv-asset__name" title={asset().name}>
                  {asset().name}
                </span>
                <span class="wv-spacer" />
                <Show when={asset().state === 'ready'}>
                  <span class="wv-asset__tick" title="Installed and playable">
                    ✓
                  </span>
                </Show>
                <Show when={asset().state === 'downloading'}>
                  <span class="wv-row__meta">{Math.round(asset().progress * 100)}%</span>
                </Show>
                <Show when={asset().state === 'absent' || asset().state === 'failed'}>
                  <button
                    type="button"
                    class="wv-chip"
                    title={
                      asset().error ??
                      `Download ${asset().name}${
                        asset().bytesTotal > 0
                          ? ` (${Math.round(asset().bytesTotal / 1_000_000)} MB)`
                          : ''
                      }`
                    }
                    onClick={() => app.startAssetDownload(asset().id)}
                  >
                    {asset().state === 'failed' ? 'Retry' : 'Download'}
                  </button>
                </Show>
              </div>
              <Show when={asset().state === 'downloading'}>
                <div
                  class="wv-progress"
                  role="progressbar"
                  aria-valuemin={0}
                  aria-valuemax={100}
                  aria-valuenow={Math.round(asset().progress * 100)}
                >
                  <span
                    class="wv-progress__fill"
                    style={{ width: `${Math.round(asset().progress * 100)}%` }}
                  />
                </div>
              </Show>
              <Show when={asset().state === 'failed' && asset().error}>
                {(error) => <span class="wv-asset__error">{error()}</span>}
              </Show>
            </div>
          )}
        </Index>
      </div>
    </Show>
  );
}
