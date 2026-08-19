# MYDAW UI

SolidJS + TypeScript + Vite frontend for the MYDAW microkernel DAW.
See `docs/superpowers/specs/2026-08-18-mydaw-scaffold-design.md` for the architecture this mirrors.

## Commands

```sh
pnpm install
pnpm dev        # browser-only dev at http://localhost:1420 (MockBackend)
pnpm build      # tsc --noEmit && vite build  ->  dist/
pnpm typecheck  # tsc --noEmit
pnpm preview    # serve the built dist/
```

### `pnpm dev` runs standalone in a browser

The dev server is fixed to port **1420** with `strictPort: true` (Tauri convention) and
`clearScreen: false` so cargo output stays readable when Tauri launches Vite.

Opening `http://localhost:1420` in an ordinary browser works with no Rust side running:
`getBackend()` checks for `window.__TAURI_INTERNALS__` and falls back to `MockBackend`, which
serves the same two-track project Rust starts with, four fixture CLAP plugins, three fixture VST3
classes, a synthetic 30 Hz meter stream and one downloadable asset. Hit **Play** to start it. The
status bar shows which backend is live.

Inside Tauri, the same page uses `TauriBackend`. `@tauri-apps/api` is imported dynamically, so
the browser session never evaluates it — production builds are plain static files in `dist/`.

## The shell

The layout is **WAVE Shell v2**; the design and the reasoning behind every disabled control are in
[`docs/superpowers/specs/2026-08-19-wave-shell-v2-ui.md`](../docs/superpowers/specs/2026-08-19-wave-shell-v2-ui.md).

```
┌──────────────────────────────────────────────────────────────┐
│ TransportBar   play · clock · BPM · PEAK · theme             │
├──────────┬────────────────────────────────────┬──────────────┤
│ Browser  │ Arrangement  track rows + playhead │ Inspector    │
│ Sounds   ├────────────────────────────────────┤ instrument   │
│ Devices  │ BottomPanel   MIXER │ EDITOR       │ master insert│
│ Projects │   strips      │  steps / notes     │ + Device ⌘K  │
├──────────┴────────────────────────────────────┴──────────────┤
│ StatusBar   device · buffer · latency · errors               │
└──────────────────────────────────────────────────────────────┘
```

Everything drawn is either wired to the engine or **visibly disabled** (0.5 opacity,
`cursor: default`, a tooltip saying what is coming). There is no third category.

## Layout

```
src/
├── api/
│   ├── types.ts        # wire types mirrored from the Rust side
│   └── backend.ts      # Backend interface + TauriBackend / MockBackend + auto-pick
├── theme/
│   ├── tokens.ts       # DARK/LIGHT palettes, mix/alpha, track colours (Solid-free)
│   └── ThemeProvider   # writes --bg … --acc onto :root; persists to localStorage
├── ui/
│   ├── icons.tsx       # every SVG, paths copied from the design
│   ├── drag.ts         # the one pointer-drag helper (faders, knobs, the tempo)
│   ├── NumberField.tsx # drag-or-type numeric readout
│   ├── Meter.tsx       # one peak meter, painted on the frame loop
│   └── frameLoop.ts    # ONE requestAnimationFrame for the whole shell
├── shell/
│   ├── layout.tsx      # view state: panes open, which tab, palette, F7
│   ├── TransportBar    │ BrowserPane  │ Arrangement
│   ├── BottomPanel     │ Inspector    │ DevicePalette
│   └── StatusBar
├── panels/
│   ├── mixer/          # MixerView.tsx + scale.ts (the dB curve, Solid-free)
│   └── editor/         # StepGrid.tsx, RollView.tsx, draw.ts (canvas, Solid-free)
├── lib/                # playhead.ts, pattern.ts, keyboard.ts — unchanged
├── state.tsx           # the boundary with Rust: commands, meter stream, selection
└── styles/wave.css     # every dimension from the design; colours are all var(--…)
```

## Backend contract

The UI expects these Tauri commands (`src/api/backend.ts` is the only caller):

| Command | Shape |
| --- | --- |
| `get_ui_snapshot` | returns `UiSnapshot { plugins: PluginStatusDto[], panels: PanelManifestDto[] }` |
| `transport_play` | no args, no return |
| `transport_stop` | no args, no return |
| `note_on` | takes `{ key, velocity }` (MIDI key `0..=127`, velocity `0..=1`); sounds regardless of transport state |
| `note_off` | takes `{ key }` |
| `get_sources` | returns `SourceDto[]` — the three built-ins then every scanned CLAP instrument, one marked `selected` (the **selected track**'s) |
| `set_source` | takes `{ source }` (an id from `get_sources`), sets the selected track's source, returns the refreshed `SourceDto[]` |
| `get_tracks` | returns `MixerState { tracks, master, selected, maxTracks, minGainDb, maxGainDb }` |
| `set_track_gain` | takes `{ track, db }`, returns the refreshed `MixerState`. Works with no audio device |
| `set_track_pan` | takes `{ track, pan }` (`-1..=1`) |
| `set_track_mute` | takes `{ track, muted }` |
| `set_track_solo` | takes `{ track, soloed }` |
| `set_master_gain` | takes `{ db }` |
| `set_selected_track` | takes `{ track }` — which track the keyboard piano plays |
| `set_track_source` | takes `{ track, source }` — a **built-in** source id |
| `set_track_source_clap` | takes `{ track, bundlePath, pluginId }` — places a hosted CLAP instrument |
| `get_master_effects` | returns `ClapPluginInfoDto[]` — the scanned CLAP **audio effects** |
| `set_master_effect_clap` | takes `{ bundlePath, pluginId }`; `clear_master_effect` takes nothing |
| `get_clap_plugins` | takes `{ rescan }`; returns every scanned plugin with its `kind` and `features` |
| `open_clap_gui` / `close_clap_gui` | take `{ target }` (`{ slot: 'track', index }` or `{ slot: 'master' }`), return `ClapGuiStatus`. One native window per placed plugin, several at a time |
| `save_clap_state` / `load_clap_state` | take `{ target }` (plus `{ stateBase64 }`); an empty string means the plugin has no state extension |
| `add_track` | takes `{ source, name }`; rejects past `maxTracks` |
| `remove_track` | takes `{ track }`; rejects for the last track |
| `get_pattern` | returns `SequencerState { bpm, sampleRate, pattern }` — the step grid and the tempo |
| `set_pattern` | takes `{ pattern }` (a whole `PatternDto`), returns the refreshed `SequencerState` |
| `set_bpm` | takes `{ bpm }` (`20..=999`), returns the stored tempo; rejects rather than clamping |
| `get_stream_info` | no args; returns `StreamInfo { deviceName, sampleRate, channels, bufferFrames, latencyMs, running }` — the status bar's BUFFER and LATENCY. Answers `running: false` rather than failing when no device is open |
| `get_project_info` | returns `ProjectInfo { path, name, missingPlugins, warnings }` — which project is open, for the window title and the status bar |
| `new_project` | no args; throws the session away and returns the fresh `ProjectInfo` |
| `open_project` | takes `{ path }` (`null` shows the native dialog); returns `ProjectInfo`, or **`null` if the user cancelled** — not an error |
| `save_project` | takes `{ saveAs, path }`; with `saveAs: false` and a project already on disk it overwrites with no dialog; `null` means cancelled |
| `get_vst3_plugins` | takes `{ rescan }`; returns every scanned VST3 class. **Slow on the first call** — it opens every installed module — so the shell only asks when the Devices tab or the palette is first opened |
| `get_vst3_master_effects` | returns the VST3 classes eligible for the master insert |
| `set_track_source_vst3` | takes `{ track, modulePath, cid }` |
| `set_master_effect_vst3` | takes `{ modulePath, cid }` |
| `open_vst3_gui` / `close_vst3_gui` | take `{ target }`, return `Vst3GuiStatus` |
| `subscribe_meters` | takes `{ channel }` (Tauri v2 `Channel<MeterFrame>`), streams at 30 Hz |

The file dialogs are Rust's, not the webview's: `@tauri-apps/api`'s dialog plugin only exists
inside the app, so a panel that raised one directly would be a blank screen in a browser and the
mock would have nothing to stand in for. Every project command takes an optional explicit `path`
for the caller that already knows one, which is what keeps the dialog a convenience.

Plus the dev-only bench surface (`src/dev/ipcBench.ts` is the only caller): `bench_ping`,
`bench_ping_async`, `bench_plan`, `bench_sync`, `bench_stream`, `bench_meter_stamp`,
`bench_finish`.

## Not yet on the wire

`AssetStatus`, `Backend.getAssetStatus` and `Backend.startAssetDownload` describe the downloadable
instrument assets. The Rust commands are being written on another branch, so the two `Backend`
methods are **optional**: `MockBackend` implements them (so the download row is real in `pnpm dev`)
and `TauriBackend` does not, which is why the row hides itself inside the app rather than invoking a
command that is not registered.

## Performance notes

Meter frames do **not** flow through Solid signals. They land in a plain mutable `MeterBus`, and
`src/ui/frameLoop.ts` runs **one** `requestAnimationFrame` loop for the whole shell — the clock, the
bars/beats readout, the peak figure, every meter fill and both playheads paint by writing to the DOM
directly. A 30-60 Hz stream must not drive the reactive scheduler. The loop does not run when
nothing is subscribed, so an idle window is idle.

Track rows, mixer strips and step cells use `<Index>`, not `<For>`. Every mixer command answers with
a whole fresh `MixerState`, so a keyed `For` would rebuild every strip on every pointer move of a
fader drag. Drags listen on `window` rather than capturing the pointer on the control, because the
control can be re-rendered mid-gesture.

Those numbers now exist:
[`docs/superpowers/specs/2026-08-18-spike4-ipc-perf.md`](../docs/superpowers/specs/2026-08-18-spike4-ipc-perf.md).

## The IPC bench (dev only)

`src/dev/ipcBench.ts` holds the webview half of the spike-4 benchmark. It is loaded lazily and only
when the Rust side contributes the `ipc-bench-auto` manifest. The *manual* bench panel was removed
with the panel dock; the automatic matrix still runs:

```sh
MYDAW_IPC_BENCH=1  cargo run -p mydaw-app --features custom-protocol   # run the matrix and exit
MYDAW_IPC_BENCH=stress ...                                             # plus the past-the-cliff rows
```

Results land in `.debug/ipc-bench/<timestamp>.json` with a summary on stdout between
`===== MYDAW IPC BENCH BEGIN/END =====`. In a normal session none of this is downloaded: the
manifest is absent, so the lazy chunk is never fetched.
