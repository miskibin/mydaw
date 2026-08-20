/**
 * The browser: what you can put on a track, and where audio comes from.
 *
 * Three trees: `get_sources` for the instruments, the plugin scan for CLAP and
 * VST3, and the sample library (factory Packs, extra-search folders, and the
 * session's loaded files). Nothing here is decorative -- a row that cannot do
 * anything is drawn `.wv-disabled` with a `title` saying why.
 *
 * Two deliberate departures from `.design/wave-studio.html`:
 *
 * - The mock's third tab is **Presets**. The engine has no preset system at all,
 *   so drawing one would be a tab that answers nothing. It is **Audio** instead:
 *   a filesystem index of samples, the same idea as FL Studio's Browser.
 * - The mock's footer is a waveform preview of the highlighted row. Clicking a
 *   sample auditions it through the engine; the footer is the asset-download
 *   row, which is real and has nowhere else to live.
 */

import { createMemo, createSignal, For, Index, onCleanup, onMount, Show, type JSX } from 'solid-js';

import type { AudioSourceDto, LibraryEntryDto, LibraryRootDto, SourceDto } from '../api/types';
import { encodeDrop } from '../lib/dnd';
import { useAppState } from '../state';
import { trackColor } from '../theme/tokens';
import { IconChevron, IconClose } from '../ui/icons';
import { useView, type BrowserTab } from './view';

import '../styles/browser.css';

/** The drum sources, by the ids `app/src/source.rs` gives them. */
const DRUM_IDS = new Set(['kick', 'snare', 'clap', 'hat-closed', 'hat-open', 'drum-kit']);
/** The melodic built-ins. */
const SYNTH_IDS = new Set(['poly-synth', 'test-tone']);
/** The groups, in the order they are drawn. */
const SOUND_GROUPS = ['Drums', 'Synths', 'Sampler', 'Other'] as const;

/** Middle C, at a comfortable velocity: what an audition plays. */
const PREVIEW_KEY = 60;
/** How long an audition is held before it is released. */
const PREVIEW_MS = 400;

const BROWSER_TABS: readonly { id: BrowserTab; label: string }[] = [
  { id: 'sounds', label: 'Sounds' },
  { id: 'plugins', label: 'Plugins' },
  { id: 'audio', label: 'Audio' },
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
  return (
    <aside class="wv-browser">
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
        <Show when={view.browserTab() === 'sounds'}>
          <SoundsTree />
        </Show>
        <Show when={view.browserTab() === 'plugins'}>
          <PluginsTree />
        </Show>
        <Show when={view.browserTab() === 'audio'}>
          <AudioTree />
        </Show>
      </div>

      <AssetFooter />
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
  remove?: { title: string; run: () => void } | undefined;
}): JSX.Element {
  return (
    <button
      type="button"
      class="wv-row wv-tree__dir"
      classList={{ 'wv-tree__dir--extra': props.extra === true }}
      style={{ 'padding-left': `${8 + (props.depth ?? 0) * 10}px` }}
      aria-expanded={props.open}
      onClick={() => props.onToggle()}
    >
      <span class="wv-tree__chev" classList={{ 'is-open': props.open }}>
        <IconChevron direction="right" />
      </span>
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
  color: string;
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
  return (
    <div
      class="wv-row wv-tree__leaf"
      classList={{ 'is-active': props.active === true, 'wv-tree__leaf--dim': props.dim === true }}
      style={{ 'padding-left': `${22 + (props.depth ?? 0) * 10}px` }}
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
      <span class="wv-tree__guide" aria-hidden="true" />
      <span class="wv-swatch" style={{ background: props.color }} />
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

/** Open/closed state for a set of directory rows, defaulting to open. */
function useFolders(): [(key: string) => boolean, (key: string) => void] {
  const [closed, setClosed] = createSignal<Record<string, true>>({});
  const isOpen = (key: string) => closed()[key] === undefined;
  const toggle = (key: string) =>
    setClosed((current) => {
      const next = { ...current };
      if (next[key] === undefined) next[key] = true;
      else delete next[key];
      return next;
    });
  return [isOpen, toggle];
}

// ---------------------------------------------------------------------------
// Sounds
// ---------------------------------------------------------------------------

/** Which folder an engine source belongs in. Hosted plugins are not in this tree. */
function groupOf(source: SourceDto): (typeof SOUND_GROUPS)[number] | null {
  if (source.kind !== 'internal') return null;
  if (DRUM_IDS.has(source.id)) return 'Drums';
  if (SYNTH_IDS.has(source.id)) return 'Synths';
  if (source.id.startsWith('sampler:')) return 'Sampler';
  return 'Other';
}

/**
 * The instrument tree.
 *
 * Hosted CLAP and VST3 sources are deliberately left out even though
 * `get_sources` offers them: they are in the Plugins tab with their format
 * badge, and a plugin listed twice under two names is a browser you cannot
 * trust.
 */
function SoundsTree(): JSX.Element {
  const app = useAppState();
  const [isOpen, toggle] = useFolders();

  const groups = createMemo(() => {
    const buckets = new Map<string, SourceDto[]>();
    for (const source of app.sources()) {
      const group = groupOf(source);
      if (group === null) continue;
      const bucket = buckets.get(group);
      if (bucket) bucket.push(source);
      else buckets.set(group, [source]);
    }
    return SOUND_GROUPS.map((name) => ({ name, sources: buckets.get(name) ?? [] })).filter(
      (group) => group.sources.length > 0,
    );
  });

  const full = () => {
    const mixer = app.mixer();
    return mixer !== undefined && mixer.tracks.length >= mixer.maxTracks;
  };

  /** The instrument a preview will actually be heard through. */
  const liveSource = () => app.sources().find((source) => source.selected);

  // Live input is addressed to the *selected track's* channel -- `note_on` takes
  // a key and a velocity and nothing else -- so an audition is honest about
  // whose instrument it plays rather than pretending to solo the clicked row.
  let timer: ReturnType<typeof setTimeout> | undefined;
  let held: number | null = null;

  function release(): void {
    if (held === null) return;
    const key = held;
    held = null;
    app.backend.noteOff(key).catch(() => {});
  }

  function preview(source: SourceDto): void {
    if (!source.takesNotes) return;
    clearTimeout(timer);
    release();
    held = PREVIEW_KEY;
    app.backend.noteOn(PREVIEW_KEY, 0.8).catch(app.report);
    timer = setTimeout(release, PREVIEW_MS);
  }

  onCleanup(() => {
    clearTimeout(timer);
    release();
  });

  const rowTitle = (source: SourceDto) => {
    const live = liveSource();
    const heard = live ? live.label : 'the selected track';
    return source.takesNotes
      ? `${source.description}\nClick auditions through ${heard} — live input plays the selected track, not a browser row.\nDrag onto the rack, or + for a channel of its own.`
      : `${source.description}\nNo audition: this source takes no notes.\nDrag onto the rack, or + for a channel of its own.`;
  };

  return (
    <Show
      when={groups().length > 0}
      fallback={
        <div class="wv-empty">
          No instruments.
          <span class="wv-empty__hint">the engine reported an empty source list</span>
        </div>
      }
    >
      <Index each={groups()}>
        {(group) => (
          <>
            <DirRow
              label={group().name}
              count={group().sources.length}
              open={isOpen(group().name)}
              onToggle={() => toggle(group().name)}
            />
            <Show when={isOpen(group().name)}>
              <Index each={group().sources}>
                {(source) => (
                  <LeafRow
                    name={source().label}
                    meta={source().takesNotes ? 'keys' : 'tone'}
                    color={source().selected ? 'var(--acc)' : 'var(--dim)'}
                    title={rowTitle(source())}
                    dragData={`inst::${source().id}`}
                    active={source().selected}
                    onClick={() => preview(source())}
                    add={{
                      title: full()
                        ? `The rack is full (${app.mixer()?.maxTracks ?? 0} channels)`
                        : `Add a rack channel playing ${source().label}`,
                      disabled: full(),
                      run: () =>
                        void app.mixerCommand((backend) =>
                          backend.addTrack(source().id, source().label),
                        ),
                    }}
                  />
                )}
              </Index>
            </Show>
          </>
        )}
      </Index>
    </Show>
  );
}

// ---------------------------------------------------------------------------
// Plugins
// ---------------------------------------------------------------------------

/** One scanned plugin, flattened out of the two format-specific DTOs. */
interface PluginRow {
  key: string;
  format: 'CLAP' | 'VST3';
  name: string;
  vendor: string;
  path: string;
  pluginId: string;
  kind: string;
  instrument: boolean;
}

/**
 * The plugin list, in two folders.
 *
 * Opening a module runs vendor `init()`, which is how unlicensed plugins pop an
 * activation window, so this tab only reads the on-disk cache. Rescan is the
 * one control that opens plugins.
 */
function PluginsTree(): JSX.Element {
  const app = useAppState();
  const [isOpen, toggle] = useFolders();

  onMount(() => void app.loadDevices());

  const clap = createMemo<PluginRow[]>(() =>
    app.clapPlugins().map((plugin) => ({
      key: `clap:${plugin.path}:${plugin.id}`,
      format: 'CLAP',
      name: plugin.name.length > 0 ? plugin.name : plugin.id,
      vendor: plugin.vendor,
      path: plugin.path,
      pluginId: plugin.id,
      kind: plugin.kind,
      instrument: plugin.kind === 'instrument',
    })),
  );

  const vst3 = createMemo<PluginRow[]>(() =>
    app.vst3Plugins().map((plugin) => ({
      key: `vst3:${plugin.path}:${plugin.id}`,
      format: 'VST3',
      name: plugin.name.length > 0 ? plugin.name : plugin.id,
      vendor: plugin.vendor,
      path: plugin.path,
      pluginId: plugin.id,
      kind: plugin.kind,
      instrument: plugin.kind === 'instrument',
    })),
  );

  const folders = createMemo(() => [
    { name: 'CLAP', rows: clap() },
    { name: 'VST3', rows: vst3() },
  ]);

  const full = () => {
    const mixer = app.mixer();
    return mixer !== undefined && mixer.tracks.length >= mixer.maxTracks;
  };

  /**
   * Add a channel and put the plugin at its head, as one command.
   *
   * `add_track` only takes a built-in source id, so the plugin has to be placed
   * afterwards -- and both calls go inside one `mixerCommand` so that only the
   * *final* state is adopted. Adopting the intermediate one would flash a track
   * called "Diva" playing the poly synth.
   */
  function addPluginTrack(row: PluginRow): void {
    void app.mixerCommand(async (backend) => {
      const placed = await backend.addTrack('poly-synth', row.name);
      const index = placed.tracks.length - 1;
      return row.format === 'CLAP'
        ? backend.setTrackSourceClap(index, row.path, row.pluginId)
        : backend.setTrackSourceVst3(index, row.path, row.pluginId);
    });
  }

  const anyRows = () => clap().length > 0 || vst3().length > 0;

  return (
    <>
      <div class="wv-browser__bar">
        <button
          type="button"
          class="wv-chip wv-can-disable"
          disabled={app.scanning()}
          title="Open each new plugin to catalogue it. Unlicensed plugins may show an activation window — only click this when you want that."
          onClick={() => void app.loadDevices(true)}
        >
          {app.scanning() ? 'Scanning…' : 'Rescan'}
        </button>
        <span class="wv-spacer" />
        <span class="wv-row__meta">{clap().length + vst3().length} found</span>
      </div>

      <Show when={app.scanning()}>
        <div class="wv-row wv-tree__note">
          <span class="wv-row__name">scanning…</span>
        </div>
      </Show>

      <Show
        when={anyRows() || app.scanning()}
        fallback={
          <div class="wv-empty">
            No plugins listed.
            <span class="wv-empty__hint">Rescan opens each module — unlicensed plugins may ask to activate</span>
          </div>
        }
      >
        <Index each={folders()}>
          {(folder) => (
            <Show when={folder().rows.length > 0}>
              <DirRow
                label={folder().name}
                count={folder().rows.length}
                open={isOpen(folder().name)}
                onToggle={() => toggle(folder().name)}
              />
              <Show when={isOpen(folder().name)}>
                <Index each={folder().rows}>
                  {(row) => (
                    <Show
                      when={row().instrument}
                      fallback={
                        <div
                          class="wv-row wv-tree__leaf wv-disabled"
                          title="Effects are added from the Inspector's + buttons"
                        >
                          <span class="wv-tree__guide" aria-hidden="true" />
                          <span class="wv-swatch" style={{ background: 'var(--faint)' }} />
                          <span class="wv-row__name">{row().name}</span>
                          <span class="wv-spacer" />
                          <span class="wv-badge">{row().kind}</span>
                        </div>
                      }
                    >
                      <LeafRow
                        name={row().name}
                        meta={row().kind}
                        color="var(--acc)"
                        title={`${row().name}${row().vendor.length > 0 ? ` — ${row().vendor}` : ''}\n${row().path}\nDrag onto the rack, or + for a channel of its own.`}
                        dragData={`inst::${row().format === 'CLAP' ? 'clap' : 'vst3'}:${row().pluginId}`}
                        add={{
                          title: full()
                            ? `The rack is full (${app.mixer()?.maxTracks ?? 0} channels)`
                            : `Add a rack channel hosting ${row().name}`,
                          disabled: full(),
                          run: () => addPluginTrack(row()),
                        }}
                      />
                    </Show>
                  )}
                </Index>
              </Show>
            </Show>
          )}
        </Index>
      </Show>
    </>
  );
}

// ---------------------------------------------------------------------------
// Audio
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
function AudioTree(): JSX.Element {
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
            {(source) => (
              <LeafRow
                name={source().name}
                meta={
                  source().missing
                    ? 'missing'
                    : `${formatDuration(source().durationSeconds)} · ${formatRate(source().sampleRate)}`
                }
                color={source().missing ? 'var(--rec)' : trackColor(source().id)}
                title={`${source().path}${source().missing ? '\nThe file has moved: its clips stay and are silent.' : ''}\nClick to load the brush, or drag onto a playlist lane.`}
                dragData={encodeDrop('audio', source().id)}
                active={brushId() === source().id}
                dim={source().missing}
                onClick={() => view.setBrush({ kind: 'audio', id: source().id })}
                remove={{
                  title: 'Unload — refused while a clip still plays it',
                  run: () => unload(source()),
                }}
              />
            )}
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
  return (
    <LeafRow
      name={props.entry.name}
      meta=""
      color="var(--acc)"
      title={`${props.entry.path}\nClick to preview. Drag onto a playlist lane.`}
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
