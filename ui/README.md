# MYDAW UI

SolidJS + TypeScript + Vite frontend for the MYDAW microkernel DAW.
The shell is **WAVE Studio**; its design, every UX decision and every disabled
control are in
[`docs/superpowers/specs/2026-08-19-wave-studio-ui.md`](../docs/superpowers/specs/2026-08-19-wave-studio-ui.md).

## Commands

```sh
pnpm install
pnpm dev        # browser-only dev at http://localhost:1420 (MockBackend)
pnpm build      # tsc --noEmit && vite build  ->  dist/
pnpm typecheck  # tsc --noEmit
pnpm preview    # serve the built dist/
```

### `pnpm dev` runs standalone in a browser

`getBackend()` checks for `window.__TAURI_INTERNALS__` and falls back to
`MockBackend`, which stands in for the whole engine: the same five tracks Rust
starts with, a pattern set, an arrangement, buses, sends, insert chains built
from the **real** effect catalogue, an invented audio file, and a synthetic
30 Hz meter stream. Every view is fully usable with no Rust side running, and
the status bar says `MOCK` so nobody mistakes it for the engine.

The mock mirrors the engine's **refusals** as well as its answers — a pattern
row of the wrong length, a send that would loop, unloading a file clips still
play. A mock that accepted what Rust rejects would be a worse rehearsal than no
mock at all.

The mock is a lazy import registered from `main.tsx`, so a production build
inside Tauri never pulls the fixture file into the bundle.

## The shell

```
┌──────────────────────────────────────────────────────────────────────────┐
│ TopBar   WAVE · transport · clock · BPM · PAT/SONG · pattern · views · ☀ │
├──────────┬────────────────────────────────────────────┬──────────────────┤
│ Browser  │  PLAYLIST (F5) | RACK (F6) | MIXER (F9)    │ Picker           │
│ Sounds   │                                            │ Inspector        │
│ Plugins  │        or the fullscreen PIANO ROLL (F7)   │  instrument      │
│ Audio    │                                            │  insert cards    │
├──────────┴────────────────────────────────────────────┴──────────────────┤
│ StatusBar  device · buffer · latency · ⟨parameter readout⟩ · hint/error   │
└──────────────────────────────────────────────────────────────────────────┘
```

Everything drawn is either wired to the engine or **visibly disabled with a
tooltip saying why**. There is no third category, and the complete disabled list
is in the spec.

## Layout

```
src/
├── api/
│   ├── types.ts        # wire types mirrored from Rust
│   ├── backend.ts      # the Backend interface + TauriBackend + getBackend
│   └── mock.ts         # MockBackend: the whole engine, in a browser
├── theme/              # tokens.ts (palettes, mix/alpha, track colours) + provider
├── lib/
│   ├── pattern.ts      # pattern arithmetic + the note operations (Solid-free)
│   ├── param.ts        # ParamDef -> knob: curves, steps, formatting, switches
│   ├── undo.ts         # the UI's undo stack and the playlist reconciler
│   ├── playhead.ts     # anchor-and-extrapolate transport position
│   └── keyboard.ts     # the computer-keyboard piano map
├── ui/
│   ├── Knob.tsx        # ONE knob, laid out from a ParamDef
│   ├── drag.ts         # the one pointer-drag helper (shift = fine, live)
│   ├── NumberField · Meter · frameLoop · icons
├── shell/
│   ├── view.tsx        # what is *shown*: views, tools, snap, zoom, brush
│   ├── TopBar · PatternMenu · Browser · SidePanel · StatusBar
├── views/
│   ├── Rack · Playlist · Mixer · InstrumentMenu
│   └── Roll.tsx + roll/draw.ts   # the canvas piano roll
├── state.tsx           # the boundary with Rust: commands, gestures, undo
└── styles/             # base.css + one sheet per region
```

`state.tsx` is the **only** file that talks to the engine. `shell/view.tsx`
holds what is shown and never crosses the IPC wall. Nothing else imports
`@tauri-apps/api`.

## Backend contract

`src/api/backend.ts` is the only caller of any of these. Every mixer command
answers with the whole `MixerState` and every sequencer command with the whole
`SequencerState`, so a view never merges a partial update into what it had.

### Session, transport and instruments

| Command | Shape |
| --- | --- |
| `get_ui_snapshot` | `UiSnapshot { plugins, panels }` |
| `transport_play` / `transport_stop` | no args |
| `transport_seek` | `{ samplePos }` — **samples**, what the transport counts |
| `set_play_mode` | `{ mode: 'pat' \| 'song' }` → `SequencerState` |
| `note_on` / `note_off` | `{ key, velocity }` / `{ key }` — live, on the selected track |
| `get_sources` | `SourceDto[]` — built-ins, then samplers, then CLAP instruments (**not** VST3) |
| `set_source` | `{ source }` — the selected track's instrument |
| `set_bpm` | `{ bpm }` (20..999) → the stored tempo; **rejects** rather than clamping |
| `get_stream_info` | `StreamInfo` — answers `running: false` rather than failing |
| `get_asset_status` / `start_asset_download` | the sampler's downloadable content |
| `subscribe_meters` | `{ channel }` — a Tauri v2 `Channel<MeterFrame>` at 30 Hz |

### The mixer, buses, sends and insert chains

| Command | Shape |
| --- | --- |
| `get_tracks` | `MixerState { tracks, buses, sends, master, selected, max*, min/maxGainDb, maxSendLevel, maxSendDb }` |
| `set_track_gain` / `set_track_pan` / `set_track_mute` / `set_track_solo` | `{ track, … }` |
| `set_master_gain` | `{ db }` |
| `set_selected_track` | `{ track }` — which track the keyboard piano plays |
| `add_track` / `remove_track` | `{ source, name }` / `{ track }`; removing renumbers every pattern |
| `set_track_source` / `set_track_source_clap` / `set_track_source_vst3` | built-in id / `{ bundlePath, pluginId }` / `{ modulePath, cid }` |
| `get_effects` | `EffectInfoDto[]` — the internal catalogue. Static: fetched once |
| `add_bus` / `remove_bus` | removing takes **every send at either end of it** |
| `rename_bus` / `set_bus_color` | tracks have neither — see the spec's gaps |
| `set_bus_gain` / `set_bus_pan` / `set_bus_mute` / `set_bus_solo` | `{ bus, … }` |
| `add_send` | `{ sourceKind: 'track' \| 'bus', sourceIndex, dest }`; refuses duplicates and loops |
| `remove_send` | `{ sendId }` — sends are addressed by **id**, never by an index pair |
| `set_send_level` / `set_send_pre` | `{ sendId, … }` → **void**: these are the per-frame ones |
| `add_insert` / `remove_insert` / `move_insert` | `{ target: 'track'\|'bus'\|'master', targetIndex, … }` |
| `set_insert_param` | `{ …, paramId, value }` → **the value actually stored**, clamped and snapped |
| `set_insert_bypass` | `{ …, bypassed }` |
| `get_chain_readouts` | one chain's live meters — polled only while that chain is on screen |
| `get_master_effects` / `get_vst3_master_effects` | the scanned **effects**, for the master insert |
| `set_master_effect_clap` / `set_master_effect_vst3` / `clear_master_effect` | the one hosted effect slot |

### Patterns, the playlist and audio files

| Command | Shape |
| --- | --- |
| `get_pattern` | `SequencerState { bpm, sampleRate, playMode, ppq, ticksPerStep, ticksPerBar, patternSet, playlist }` |
| `set_pattern` | `{ pattern }` — whole-pattern replacement; `id: 0` means the active one |
| `add_pattern` | **does not switch to it** — that is `set_active_pattern` |
| `clone_pattern` / `split_pattern_by_channel` / `remove_pattern` / `rename_pattern` / `set_pattern_color` | `{ patternId, … }`; split makes one pattern per occupied channel and spreads playlist clips onto consecutive lanes; removing takes **every clip that played it** |
| `set_pattern_length` | `{ patternId, steps }` (16/32/64); shortening is not destructive |
| `set_active_pattern` | what the rack, the roll and PAT mode address |
| `add_playlist_track` / `remove_playlist_track` / `rename_playlist_track` / `set_playlist_track_mute` | removing a lane takes its clips |
| `add_pattern_clip` | `{ patternId, track, start, length }` — **ticks**; absent length means four bars |
| `add_audio_clip` | `{ sourceId, track, start, length, offsetFrames, gainDb }`; absent length means the whole file |
| `move_clip` / `resize_clip` / `remove_clip` | `{ clipId, … }` — one id space for both kinds |
| `set_clip_gain` / `set_clip_offset` | audio clips only |
| `load_audio_source` | `{ path }` → `AudioSourceDto`; loading a loaded path returns the one already there |
| **`open_audio_file`** | no args → `AudioSourceDto`, or **`null` if the user cancelled** — the native dialog |
| `get_audio_sources` / `unload_audio_source` | unloading is **refused** while a clip still plays the file |
| `get_audio_waveform` | `{ sourceId, targetBins }` → `[min, max]` pairs; `targetBins` is a request, not a promise |
| `add_sample_channel` | `{ path }` — decode a file as a one-shot and give it a rack channel |

### Destructive sample edits, and resampling

The sample editor's `EDIT FILE` row rewrites the **file**, not the channel's view
of it. Every edit is copy-on-write: the engine writes a new 32-bit float WAV
under `WAVE Studio/sample-edits`, loads it as a new source, and repoints only
the edited channel and that channel's own playlist clips — so a second channel
on the same take is untouched, and the original stays loaded, which is what
makes undo a pointer swap rather than a re-decode.

| Command | Shape |
| --- | --- |
| `edit_track_sample` | `{ track, op }` where `op` is `{kind:'reverse'\|'normalize'\|'trim'}` or `{kind:'gain', db}` → `SampleEditDto` carrying **both** pointers |
| `set_track_audio_source` | `{ track, sourceId, startFrames, endFrames, clipOffsetShiftFrames }` — the inverse half, and how Ctrl+Z works |
| `render_pattern_to_sample` | no args → `RenderToSampleDto`; renders one loop of the active pattern offline through the whole mixer and lands it as a new sample channel |

Normalize targets **−0.3 dBFS** over the whole file (not the region: a
region-relative normalize would leave the audio outside it above full scale).
Trim crops to the played region, resets the region, and shifts this lane's clip
offsets by the crop — which the undo negates. A render is exactly one pattern
loop, so it drops back on the grid seamlessly, and **hosted CLAP/VST3 channels
are silent in it** — only the main thread can instantiate one. `hostedChannels`
says how many, and the status bar repeats it rather than leaving the user to
wonder where the lead went.

### Hosted plugins and the project file

| Command | Shape |
| --- | --- |
| `get_clap_plugins` / `get_vst3_plugins` | `{ rescan }`. **Slow on the first call**, so the browser only asks when the Plugins tab is first opened |
| `open_clap_gui` / `close_clap_gui` / `open_vst3_gui` / `close_vst3_gui` | `{ target }` (`{slot:'track',index}` or `{slot:'master'}`). Native windows, several at once |
| `save_clap_state` / `load_clap_state` | an empty string means the plugin has no state extension |
| `get_project_info` | `ProjectInfo { path, name, missingPlugins, warnings }` |
| `new_project` / `open_project` / `save_project` | `open`/`save` take an optional explicit `path`; **`null` means the user cancelled** — not an error |

The file dialogs are **Rust's, not the webview's**: the dialog plugin only
exists inside the app, so a view raising one directly would be a blank screen in
a browser and the mock would have nothing to stand in for. Every dialog command
takes an optional explicit `path` for the caller that already knows one, which
keeps the dialog a convenience rather than the only door.

Plus the dev-only bench surface (`src/dev/ipcBench.ts` is the only caller):
`bench_ping`, `bench_ping_async`, `bench_plan`, `bench_sync`, `bench_stream`,
`bench_meter_stamp`, `bench_finish`. Its chunk is lazy and is only fetched when
Rust contributes the `ipc-bench-auto` panel.

## Performance notes

Meter frames do **not** flow through Solid signals. They land in a plain mutable
`MeterBus`, and `src/ui/frameLoop.ts` runs **one** `requestAnimationFrame` loop
for the whole shell — the clock, the bar counter, every meter fill and both
playheads paint by writing to the DOM directly. The loop does not run when
nothing is subscribed.

The piano roll is **one canvas**, and it redraws on *change* rather than on the
frame tick; the frame loop only drives its playhead. A thousand
absolutely-positioned notes would be a layout pass per pointer move.

Strips, rows and step cells use `<Index>`, not `<For>`: every mixer command
answers with a whole fresh `MixerState`, so a keyed `For` would rebuild every
strip on every pointer move of a fader drag. Drags listen on `window` rather
than capturing the pointer, because the control can be re-rendered mid-gesture.

Pattern edits are pushed on a 50 ms debounce and the echoed pattern is
deliberately **not** adopted on success — an edit made while the push was in
flight would be clobbered by it.
