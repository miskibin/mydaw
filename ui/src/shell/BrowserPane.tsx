import { createMemo, createSignal, For, onMount, Show, type JSX } from 'solid-js';
import { useAppState } from '../state';
import { useLayout, type BrowserTab } from './layout';
import { IconChevron, IconSearch } from '../ui/icons';
import { alpha } from '../theme/tokens';
import { useTheme } from '../theme/ThemeProvider';
import type { ClapPluginInfoDto, SourceDto, Vst3PluginInfoDto } from '../api/types';

/** One row of a device list, whichever format it came from. */
interface DeviceRow {
  key: string;
  name: string;
  vendor: string;
  format: 'CLAP' | 'VST3';
  path: string;
  pluginId: string;
  instrument: boolean;
}

function clapRow(plugin: ClapPluginInfoDto): DeviceRow {
  return {
    key: `clap:${plugin.path}:${plugin.id}`,
    name: plugin.name || plugin.id,
    vendor: plugin.vendor,
    format: 'CLAP',
    path: plugin.path,
    pluginId: plugin.id,
    instrument: plugin.kind === 'instrument',
  };
}

function vst3Row(plugin: Vst3PluginInfoDto): DeviceRow {
  return {
    key: `vst3:${plugin.path}:${plugin.id}`,
    name: plugin.name || plugin.id,
    vendor: plugin.vendor,
    format: 'VST3',
    path: plugin.path,
    pluginId: plugin.id,
    instrument: plugin.kind === 'instrument',
  };
}

/** Case-insensitive substring match over everything a row shows. */
function matches(query: string, ...fields: string[]): boolean {
  if (query.length === 0) return true;
  const needle = query.toLowerCase();
  return fields.some((field) => field.toLowerCase().includes(needle));
}

/**
 * The left pane: what you can put on a track, and what to do with the project.
 *
 * Collapses to a 32 px rail rather than disappearing, so the thing that brings
 * it back is where it was -- and so a narrow window loses 218 px rather than the
 * ability to load an instrument.
 */
export function BrowserPane(): JSX.Element {
  const layout = useLayout();

  return (
    <Show when={layout.browserOpen()} fallback={<BrowserRail />}>
      <BrowserBody />
    </Show>
  );
}

function BrowserRail(): JSX.Element {
  const layout = useLayout();
  return (
    <button
      type="button"
      class="wv-rail wv-rail--left"
      title="Show the browser"
      aria-label="Show the browser"
      onClick={() => layout.toggleBrowser()}
    >
      <IconChevron direction="right" />
      <span class="wv-rail__label">BROWSER</span>
    </button>
  );
}

function BrowserBody(): JSX.Element {
  const app = useAppState();
  const layout = useLayout();
  const theme = useTheme();
  const [query, setQuery] = createSignal('');

  const tabs: readonly { id: BrowserTab; label: string }[] = [
    { id: 'sounds', label: 'Sounds' },
    { id: 'devices', label: 'Devices' },
    { id: 'projects', label: 'Projects' },
  ];

  /** The track a click in this pane loads onto. */
  const targetTrack = () => {
    const selection = app.selection();
    return selection.kind === 'track' ? selection.index : (app.mixer()?.selected ?? 0);
  };

  const currentSourceLabel = () => {
    const mixer = app.mixer();
    const track = mixer?.tracks[targetTrack()];
    return track ? `${track.name} — ${track.sourceLabel}` : 'no track';
  };

  const rate = () => {
    const info = app.streamInfo();
    return info?.running ? `${(info.sampleRate / 1000).toFixed(1)}k · ${info.channels}ch` : 'no device';
  };

  return (
    <aside class="wv-browser">
      <div class="wv-pane-head">
        <span class="wv-pane-head__title">BROWSER</span>
        <span class="wv-spacer" />
        <button
          type="button"
          class="wv-pane-head__btn"
          title="Collapse the browser"
          aria-label="Collapse the browser"
          onClick={() => layout.toggleBrowser()}
        >
          <IconChevron direction="left" />
        </button>
      </div>

      <div class="wv-browser__controls">
        <label class="wv-search">
          <IconSearch />
          <input
            type="search"
            placeholder="Search"
            value={query()}
            aria-label="Filter the browser"
            onInput={(event) => setQuery(event.currentTarget.value)}
          />
        </label>
        <div class="wv-tabs" role="tablist" aria-label="Browser sections">
          <For each={tabs}>
            {(tab) => (
              <button
                type="button"
                role="tab"
                class="wv-tab"
                classList={{ 'is-active': layout.browserTab() === tab.id }}
                aria-selected={layout.browserTab() === tab.id}
                onClick={() => layout.setBrowserTab(tab.id)}
              >
                {tab.label}
              </button>
            )}
          </For>
        </div>
      </div>

      <Show when={layout.browserTab() === 'sounds'}>
        <SoundsTab query={query()} track={targetTrack()} />
      </Show>
      <Show when={layout.browserTab() === 'devices'}>
        <DevicesTab query={query()} track={targetTrack()} />
      </Show>
      <Show when={layout.browserTab() === 'projects'}>
        <ProjectsTab />
      </Show>

      <div class="wv-browser__foot">
        <div class="wv-preview">
          <div
            class="wv-preview__wave"
            style={{
              'background-image':
                `repeating-linear-gradient(90deg,${alpha(theme.accentDrawn(), 0.8)} 0 1.5px,transparent 1.5px 4px),` +
                `repeating-linear-gradient(90deg,${alpha(theme.accentDrawn(), 0.25)} 0 1.5px,transparent 1.5px 4px)`,
            }}
          />
        </div>
        <div class="wv-preview__caption">
          <span title={currentSourceLabel()}>{currentSourceLabel()}</span>
          <span>{rate()}</span>
        </div>
      </div>
    </aside>
  );
}

/**
 * Instruments the engine can place without loading anything from disk, plus
 * whatever downloadable content this build knows about.
 *
 * The hosted CLAP and VST3 instruments are deliberately *not* here even though
 * `get_sources` offers them: they live in Devices with their format badge, and a
 * plugin listed twice under two names is a browser you cannot trust.
 */
function SoundsTab(props: { query: string; track: number }): JSX.Element {
  const app = useAppState();

  const rows = createMemo(() =>
    app
      .sources()
      .filter((source) => source.kind !== 'clap' && source.kind !== 'vst3')
      .filter((source) => matches(props.query, source.label, source.description)),
  );

  const place = (source: SourceDto) => app.placeSource(props.track, source);

  return (
    <div class="wv-list" role="listbox" aria-label="Instruments">
      <span class="wv-row__group">INSTRUMENTS</span>
      <For
        each={rows()}
        fallback={<span class="wv-note">Nothing matches “{props.query}”.</span>}
      >
        {(source) => (
          <button
            type="button"
            role="option"
            class="wv-row"
            classList={{ 'is-active': source.selected }}
            aria-selected={source.selected}
            title={source.description}
            onClick={() => place(source)}
          >
            <span class="wv-row__name">{source.label}</span>
            <span class="wv-spacer" />
            <span class="wv-row__meta">{source.takesNotes ? 'keys' : 'tone'}</span>
          </button>
        )}
      </For>

      <AssetRows />
    </div>
  );
}

/**
 * The downloadable-instrument rows.
 *
 * Renders nothing at all when the backend has no asset surface, which is every
 * Tauri build until the sampler branch lands its `get_asset_status` /
 * `start_asset_download` commands. The component is written against the typed
 * `AssetStatus` in `api/types.ts`, so connecting it is implementing two methods
 * on `TauriBackend` and nothing here.
 *
 * TODO(merge): drop this comment once the sampler branch is in.
 */
function AssetRows(): JSX.Element {
  const app = useAppState();
  return (
    <Show when={app.assetsSupported && app.assets().length > 0}>
      <span class="wv-row__group">DOWNLOADS</span>
      <For each={app.assets()}>
        {(asset) => (
          <div class="wv-stack">
            <button
              type="button"
              class="wv-row"
              classList={{ 'is-active': asset.state === 'ready' }}
              disabled={asset.state === 'downloading' || asset.state === 'ready'}
              title={asset.error ?? `${asset.name}`}
              onClick={() => app.startAssetDownload(asset.id)}
            >
              <span class="wv-row__name">{asset.name}</span>
              <span class="wv-spacer" />
              <span class="wv-row__meta">
                {asset.state === 'ready'
                  ? 'installed'
                  : asset.state === 'downloading'
                    ? `${Math.round(asset.progress * 100)}%`
                    : asset.state === 'failed'
                      ? 'failed'
                      : `${Math.round(asset.bytesTotal / 1_000_000)} MB`}
              </span>
            </button>
            <Show when={asset.state === 'downloading'}>
              <div class="wv-progress">
                <div
                  class="wv-progress__fill"
                  style={{ width: `${Math.round(asset.progress * 100)}%` }}
                />
              </div>
            </Show>
            <Show when={asset.error}>
              {(message) => <span class="wv-note">{message()}</span>}
            </Show>
          </div>
        )}
      </For>
    </Show>
  );
}

/**
 * Every scanned plugin, in both formats, in two groups.
 *
 * The groups are what the engine can do with them, not a taxonomy: an
 * instrument goes at the head of the selected track, and an effect goes on the
 * master insert -- which is the only insert MYDAW has today, and is labelled as
 * such rather than implied.
 */
function DevicesTab(props: { query: string; track: number }): JSX.Element {
  const app = useAppState();

  // The first time this tab is looked at, pay for the scan. Doing it here rather
  // than at launch is what keeps a machine with 400 VST3 modules from spending a
  // minute of every session on a list it may never open.
  onMount(() => app.ensureDevices());

  const all = createMemo<DeviceRow[]>(() => [
    ...app.clapPlugins().map(clapRow),
    ...app.vst3Plugins().map(vst3Row),
  ]);

  const instruments = createMemo(() =>
    all()
      .filter((row) => row.instrument)
      .filter((row) => matches(props.query, row.name, row.vendor, row.format)),
  );
  const effects = createMemo(() =>
    all()
      .filter((row) => !row.instrument)
      .filter((row) => matches(props.query, row.name, row.vendor, row.format)),
  );

  const loadInstrument = (row: DeviceRow) => {
    app.mixerCommand((api) =>
      row.format === 'CLAP'
        ? api.setTrackSourceClap(props.track, row.path, row.pluginId)
        : api.setTrackSourceVst3(props.track, row.path, row.pluginId),
    );
  };

  const loadEffect = (row: DeviceRow) => {
    app.mixerCommand((api) =>
      row.format === 'CLAP'
        ? api.setMasterEffectClap(row.path, row.pluginId)
        : api.setMasterEffectVst3(row.path, row.pluginId),
    );
  };

  const row = (entry: DeviceRow, onClick: () => void) => (
    <button
      type="button"
      class="wv-row"
      title={`${entry.name}${entry.vendor ? ` — ${entry.vendor}` : ''}\n${entry.path}`}
      onClick={onClick}
    >
      <span class="wv-row__name">{entry.name}</span>
      <span class="wv-spacer" />
      <span class="wv-badge">{entry.format}</span>
    </button>
  );

  return (
    <div class="wv-list">
      <div class="wv-stack">
        <button
          type="button"
          class="wv-btn wv-btn--ghost"
          disabled={app.scanning()}
          onClick={() => app.rescanPlugins()}
        >
          {app.scanning() ? 'Scanning…' : 'Rescan plugins'}
        </button>
      </div>

      <span class="wv-row__group">INSTRUMENTS → SELECTED TRACK</span>
      <For
        each={instruments()}
        fallback={
          <span class="wv-note">
            {app.scanning() ? 'Scanning…' : 'No instruments found.'}
          </span>
        }
      >
        {(entry) => row(entry, () => loadInstrument(entry))}
      </For>

      <span class="wv-row__group">EFFECTS → MASTER INSERT</span>
      <For
        each={effects()}
        fallback={
          <span class="wv-note">{app.scanning() ? 'Scanning…' : 'No effects found.'}</span>
        }
      >
        {(entry) => row(entry, () => loadEffect(entry))}
      </For>
    </div>
  );
}

/** New / Open / Save / Save As, straight through to the Rust-side dialogs. */
function ProjectsTab(): JSX.Element {
  const app = useAppState();
  return (
    <div class="wv-list">
      <div class="wv-stack">
        <button type="button" class="wv-btn" onClick={() => app.newProject()}>
          New project <span class="wv-row__meta">Ctrl+N</span>
        </button>
        <button type="button" class="wv-btn" onClick={() => app.openProject()}>
          Open… <span class="wv-row__meta">Ctrl+O</span>
        </button>
        <button type="button" class="wv-btn wv-btn--accent" onClick={() => app.saveProject()}>
          Save <span class="wv-row__meta">Ctrl+S</span>
        </button>
        <button type="button" class="wv-btn" onClick={() => app.saveProjectAs()}>
          Save as… <span class="wv-row__meta">Ctrl+Shift+S</span>
        </button>
      </div>
      <span class="wv-row__group">OPEN PROJECT</span>
      <span class="wv-note">
        {app.project().name}
        <br />
        {app.project().path ?? 'never saved'}
      </span>
      <Show when={app.project().warnings.length > 0}>
        <span class="wv-row__group">WARNINGS</span>
        <For each={app.project().warnings}>
          {(warning) => <span class="wv-note">{warning}</span>}
        </For>
      </Show>
    </div>
  );
}
