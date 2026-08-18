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
serves a fake snapshot (kernel-status, meters, and one deliberately unregistered `mixer-view`
panel to exercise the unknown-panel path) plus a synthetic 30 Hz meter stream. Hit **Play** to
start it. The status bar shows which backend is live.

Inside Tauri, the same page uses `TauriBackend`. `@tauri-apps/api` is imported dynamically, so
the browser session never evaluates it — production builds are plain static files in `dist/`.

## Layout

```
src/
├── api/
│   ├── types.ts        # wire types mirrored from the Rust ipc-bridge crate
│   └── backend.ts      # Backend interface + TauriBackend / MockBackend + auto-pick
├── lib/playhead.ts     # local playhead interpolation between sparse RT updates
├── lib/keyboard.ts     # the computer-keyboard piano: key map, octave, note names
├── panels/
│   ├── registry.ts     # component-key -> Solid component (mirror of Rust contributions)
│   ├── PanelFrame.tsx  # panel chrome + registry resolution, unknown-key fallback
│   ├── UnknownPanel.tsx
│   ├── kernel-status/  # plugin registry rows with lifecycle dots
│   └── meters/         # canvas stereo peak/RMS meter (draw.ts is Solid-free)
├── shell/              # TransportBar, PanelDock, MainArea, StatusBar
├── state.tsx           # Solid context: snapshot, transport, meter bus, playhead, held notes
└── styles/             # theme.css (tokens) + app.css (layout)
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
| `subscribe_meters` | takes `{ channel }` (Tauri v2 `Channel<MeterFrame>`), streams at 30 Hz |

Plus the dev-only bench surface (`src/dev/ipcBench.ts` is the only caller): `bench_ping`,
`bench_ping_async`, `bench_plan`, `bench_sync`, `bench_stream`, `bench_meter_stamp`,
`bench_finish`.

## Adding a panel

1. Rust side contributes a `PanelManifestDto` with a `component` key.
2. Add `'<key>': MyPanel` to `PANEL_REGISTRY` in `src/panels/registry.ts`.

An unregistered key renders as an "unknown panel" card rather than breaking the dock, so a UI
build older than the kernel degrades visibly instead of silently dropping panels.

## Performance notes

Meter frames do **not** flow through Solid signals. They land in a plain mutable `MeterBus`, and
`MetersPanel` polls the latest frame from its own `requestAnimationFrame` loop — a 30-60 Hz
stream must not drive the reactive scheduler. Text readouts (timecode, sample position) mirror the
playhead at 10 Hz instead. All meter drawing lives in `src/panels/meters/draw.ts`, kept free of
Solid and allocation-free in its hot path, because that file is the vehicle for the Phase 1 IPC
performance spike.

Those numbers now exist:
[`docs/superpowers/specs/2026-08-18-spike4-ipc-perf.md`](../docs/superpowers/specs/2026-08-18-spike4-ipc-perf.md).

## The IPC bench (dev only)

`src/dev/` holds the webview half of the spike-4 benchmark. It is loaded lazily and only when the
Rust side contributes the `ipc-bench` panel:

```sh
MYDAW_DEV_PANELS=1 cargo run -p mydaw-app --features custom-protocol   # panel, run it by hand
MYDAW_IPC_BENCH=1  cargo run -p mydaw-app --features custom-protocol   # run the matrix and exit
MYDAW_IPC_BENCH=stress ...                                             # plus the past-the-cliff rows
```

Results land in `.debug/ipc-bench/<timestamp>.json` with a summary on stdout between
`===== MYDAW IPC BENCH BEGIN/END =====`. In a normal session none of this is downloaded: the
manifest is absent, so the lazy chunk is never fetched.
