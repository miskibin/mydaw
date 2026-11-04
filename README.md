# MYDAW

An FL Studio-style DAW built as a tiny Rust microkernel with **everything else as a plugin** —
audio I/O, metering, CLAP/VST3 hosting and UI panels all register against the same contract.
Tauri v2 + SolidJS on the front, a lock-free real-time audio graph behind the wall.

The app includes a sequencer, piano roll, playlist, mixer, project files and CLAP/VST3
hosting. The **[live session CLI](docs/cli.md)** lets agents edit the same unsaved project
as the musician, with shared undo, revision checks, plugin parameters and independent
snapshot rendering/analysis.
See [`docs/superpowers/specs/2026-08-19-step3-internal-instruments.md`](docs/superpowers/specs/2026-08-19-step3-internal-instruments.md).

Seeing what it plays, without speakers:

```sh
MYDAW_DEBUG_CAPTURE=2 cargo run -p mydaw-app --features custom-protocol
```

captures two seconds of the output bus into `.debug/captures/<timestamp>/` — a float WAV, a
waveform/spectrum PNG with labelled axes, and a statistics JSON — prints a report and exits.
See [`docs/superpowers/specs/2026-08-18-debug-tap-harness.md`](docs/superpowers/specs/2026-08-18-debug-tap-harness.md).

Measuring what the webview costs:

```sh
MYDAW_IPC_BENCH=1 cargo run -p mydaw-app --release --features custom-protocol
```

runs the Rust → WebView2 transport matrix (JSON vs binary channels, 64 B → 1 MB, 30 → 240 Hz),
writes `.debug/ipc-bench/<timestamp>.json`, prints a report and exits. Verdict and numbers:
[`docs/superpowers/specs/2026-08-18-spike4-ipc-perf.md`](docs/superpowers/specs/2026-08-18-spike4-ipc-perf.md).
Architecture and roadmap: [`docs/superpowers/specs/2026-08-18-mydaw-scaffold-design.md`](docs/superpowers/specs/2026-08-18-mydaw-scaffold-design.md).

## Layout

| Path | What lives there |
| --- | --- |
| `crates/kernel` | Microkernel: `Plugin` trait, typed service registry, non-RT event bus, dependency-ordered lifecycle |
| `crates/rt-core` | RT primitives: `RtNode`, the multi-track mixer graph with buses and send taps, lock-free graph swap, `rtrb` command queue, transport clock. See [`the routing spec`](docs/superpowers/specs/2026-08-19-mixer-v2-routing.md) |
| `crates/ipc-bridge` | Kernel ⇄ webview data: serde DTOs, `UiSnapshot`, 30 Hz meter throttle. Tauri-free by design |
| `crates/plugin-scan` | Crash-proof plugin discovery, shared by both hosts: one child process per unknown module, a metadata cache so unchanged ones are never opened again, and a persistent block list for the ones that will not survive being read |
| `plugins/metering` | Owns the audio-thread → UI meter queue and the Meters panel |
| `plugins/debug-tap` | RT-safe capture of the output bus, plus WAV / annotated PNG / statistics — so the engine can be *inspected* without ears |
| `plugins/test-tone` | 440 Hz sine `RtNode`; the vehicle for the Phase 1 RT spike |
| `plugins/instruments` | The polyBLEP poly synth, driven by note events. No assets, bit-deterministic, zero-alloc — which makes it the honest fallback when the sampled instruments are not installed. The drums used to be here; they are `sampler:808-kit` now |
| `plugins/effects` | The mixer effects that ship inside the DAW: EQ, compressor, delay, saturator and utility, driven by the parameter system. Transparent at their defaults, click-free on bypass, zero-alloc. See [`the spec`](docs/superpowers/specs/2026-08-19-param-system-effects-v1.md) |
| `plugins/sequencer` | Patterns, the playlist and the scheduler: a set of named patterns, an arrangement of clips, and PAT/SONG playback that is drift-free by construction. See [`the spec`](docs/superpowers/specs/2026-08-19-engine-patterns-playlist.md) |
| `plugins/sampler` | The sampled instrument: a manifest-driven, 32-voice sample player, plus the checksummed fetcher that installs its content. See [Sample content](#sample-content) |
| `plugins/audio-io` | cpal/WASAPI devices in **both** directions: the output callback with its transport and graph plumbing, and the capture path behind the REC button — device list, RT-safe input ring, rate conversion to the engine's clock, and the take it builds. See [Recording](#recording) |
| `plugins/host-clap` | CLAP discovery **and hosting**: scans bundles, runs a plugin instance as a `ClapNode` in the RT graph |
| `plugins/host-vst3` | VST3 discovery **and hosting**: scans modules, runs a plugin instance as a `Vst3Node` in the RT graph (step 7) |
| `plugins/plugin-windows` | Native top-level windows for external plugin GUIs, pumped by the shell's own message loop (Phase 1 spike 3) |
| `app/` | The shell: boots the kernel, registers the plugin set, hosts the webview, owns the Tauri commands |
| `ui/` | SolidJS + TypeScript + Vite frontend ([`ui/README.md`](ui/README.md)) |
| `test-fixtures/clap-gain` | A real CLAP plugin (-6 dB stereo gain) built by the workspace, so the CLAP host is tested against something deterministic rather than whatever happens to be installed |

## Quickstart

Prerequisites: Rust stable, Node 22+, pnpm, and the WebView2 runtime (preinstalled on Windows 11).

```sh
pnpm --dir ui install
```

**With the Tauri CLI** (`cargo install tauri-cli --version "^2"`, or `pnpm add -D @tauri-apps/cli`):

```sh
cd app && cargo tauri dev    # runs `pnpm --dir ../ui dev` and the shell together
```

The CLI finds `app/tauri.conf.json` because that is where it is run from; `app/` is an ordinary
workspace member, so `cargo build -p mydaw-app` keeps working with or without the CLI.

**Without it** — the CLI only orchestrates the two processes, so run them yourself:

```sh
pnpm --dir ui dev            # terminal 1: Vite on http://localhost:1420
cargo run -p mydaw-app       # terminal 2: the shell, pointed at the dev server
```

**Against built assets** (no dev server; `ui/dist` is embedded in the binary):

```sh
pnpm --dir ui build
cargo run -p mydaw-app --features custom-protocol
```

The window shows the kernel status panel (seven plugins, all `STARTED`), a meters panel and a
transport bar. **Play** starts the transport and the test tone; **Stop** silences it and keeps the
playhead where it is. With no audio device the shell logs a warning and runs on without sound.

`RUST_LOG=debug` raises the log level; the debug build keeps its console for exactly that reason.

## Tests

```sh
cargo test --workspace                   # every crate; silent, no device needed
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
pnpm --dir ui build                      # tsc --noEmit + vite build
```

Several tests are opt-in because they open the real output device and are audible (the second also
puts a plugin editor on screen):

```sh
cargo test -p mydaw-app --test audio_end_to_end -- --ignored --nocapture
cargo test -p mydaw-app --test gui_audio_smoke  -- --ignored --nocapture
cargo test -p mydaw-app --test clap_hardware    -- --ignored --nocapture --test-threads=1
cargo test -p mydaw-app --test mixer_audio      -- --ignored --nocapture --test-threads=1
cargo test -p mydaw-app --test sampler_audio    -- --ignored --nocapture --test-threads=1
cargo test -p mydaw-app --test song_mode_audio -- --ignored --nocapture --test-threads=1
```

The sampler's own tests split the same way. The offline ones need nothing:

```sh
cargo test -p mydaw-plugin-sampler                        # voices, RT-alloc, manifest parsing
cargo test -p mydaw-plugin-sampler --test piano_assets -- --nocapture
cargo test -p mydaw-plugin-sampler --test live_download -- --ignored   # network
```

`piano_assets` and `sampler_audio` **skip themselves** when the sample content is not installed,
which is why `cargo test --workspace` is green on a fresh clone and in CI.

## Sample content

`plugins/sampler` plays recordings, and recordings are not in this repository: `assets/` is
gitignored and the piano alone is 131 MB. What *is* committed is
[`assets-manifest.json`](assets-manifest.json), which pins every file's URL and SHA-256 against a
specific upstream commit.

Seven instruments are offered — the Salamander piano and the six kits of **WAVE Factory Vol. 1**
(see [`docs/factory-library.md`](docs/factory-library.md)) — and each is a **separate download**:

| Instrument | Samples | Download | Licence |
| --- | ---: | ---: | --- |
| Salamander Grand Piano | 90 | 131 MiB | CC-BY-3.0 |
| 808 Kit | 30 | 2.1 MiB | CC0-1.0 |
| 808 Bass | 10 | 2.1 MiB | CC0-1.0 |
| Percussion & FX | 55 | 14.4 MiB | CC0-1.0 |
| Studio Drums | 36 | 21.9 MiB | CC0-1.0 |
| Marimba | 20 | 21.1 MiB | CC0-1.0 |
| Glockenspiel | 10 | 8.9 MiB | CC0-1.0 |

**A checkout with no assets builds, tests and runs.** The sampler starts, reports zero instruments,
and logs a hint. The Files tab lists `assets/instruments` as a read-only **Factory** root, so a
single kick can be dragged out of a kit as a one-shot; its **Packs** root is `assets/packs` (also
gitignored), where you drop kits of your own. A missing or empty folder is fine — the pane just has nothing to list. Install
whichever instruments you want, either way you like:

```sh
pwsh -File scripts/fetch-assets.ps1 -List                 # what is on offer
pwsh -File scripts/fetch-assets.ps1                       # all of it
pwsh -File scripts/fetch-assets.ps1 -Instrument 808-kit,808-bass
```

…or from inside the running app: the Browser's footer lists every instrument with a `Download`
button and a progress bar. Nothing downloads without that click. Both installers read the same
manifest, verify every file against its SHA-256, and write each one atomically (`<name>.part`, then
rename) so an interrupted run never leaves a truncated sample behind. When one finishes, that
instrument appears in the track source picker **without a restart**.

`scripts/gen-asset-manifest.ps1` regenerates the piano's entry from the upstream SFZ mapping;
`scripts/gen-factory-kits.mjs` regenerates the six factory kits from their curation tables. Both are
maintenance tools rather than part of a normal install — and both *merge* into the manifest, so
running one never drops what the other owns. The Node generator also writes each kit's runtime
`manifest.json`, which makes it a working cross-platform installer for the kits on a machine with no
PowerShell.

### If your network inspects TLS

The in-app downloader validates the server's certificate against **the operating system's trust
store**, not against a bundled copy of Mozilla's roots. That is what makes it work behind a
corporate proxy that terminates TLS and re-signs with a private CA: your IT department already
installed that CA on the machine, and MYDAW now believes it.

If the download still fails with `invalid peer certificate: UnknownIssuer`, the proxy's root is not
in the system store either. As a last resort:

```sh
MYDAW_TLS_NO_VERIFY=1 cargo run -p mydaw-app     # or set it for the packaged app
```

This disables certificate verification for asset downloads only, and logs a loud warning. It is safe
in the narrow sense that matters here: **every downloaded file is checked against the SHA-256 pinned
in `assets-manifest.json`, which ships inside the binary.** An attacker on the wire can make the
download fail, not make you install different samples. Unset it once the certificate is installed
properly.

### Attribution

The bundled piano is the **[Salamander Grand Piano V3](https://github.com/sfzinstruments/SalamanderGrandPiano)**
by **Alexander Holm**, licensed **[CC-BY-3.0](https://creativecommons.org/licenses/by/3.0/)**, pinned
at commit `3382bf9`. A subset is installed: every one of the 30 sampled notes at 3 of the 16 recorded
velocity layers.

CC-BY-3.0 is an attribution licence. MYDAW does not redistribute these files — it downloads them from
the source above — but **anything you export that contains their sound carries the credit
requirement**. `scripts/fetch-assets.ps1` writes `assets/ATTRIBUTION.md` beside the samples, and the
credit is also carried in the manifest and surfaced by the sampler at runtime.

Every kit of WAVE Factory Vol. 1 is **CC0-1.0** — public domain, no credit required and no
share-alike — from [tidalcycles/sounds-tr808-fischer](https://github.com/tidalcycles/sounds-tr808-fischer)
(Michael Fischer, 1994) and the [Versilian Community Sample Library](https://github.com/sgossner/VCSL)
(Versilian Studios LLC). The credits are written anyway, into the same `assets/ATTRIBUTION.md`;
[`docs/factory-library.md`](docs/factory-library.md) has the full table and the key maps.

Design notes, the manifest format, the memory budget and the measured verification numbers are in
[`docs/superpowers/specs/2026-08-19-sampler-plugin.md`](docs/superpowers/specs/2026-08-19-sampler-plugin.md).

## Hosted CLAP plugins

Pick an instrument for a track from that strip's source dropdown, and an effect for the master bus
from the master strip's selector; both list what the CLAP scan found on this machine. Point the scan
at a build directory with the CLAP specification's own override:

```sh
CLAP_PATH='C:\path\to\my\plugins' cargo run -p mydaw-app --features custom-protocol
```

An external plugin's editor opens in its **own native window** next to MYDAW's -- a plugin paints
with its own toolkit into an OS window handle, which cannot be composited into the webview. Every
placed plugin gets its own *Editor* button on its strip, and several can be open at once.

`MYDAW_CLAP_FX` and `MYDAW_CLAP_INSTRUMENT` were retired in step 6; the pickers do strictly more.

See [`docs/superpowers/specs/2026-08-19-step6-clap-instruments.md`](docs/superpowers/specs/2026-08-19-step6-clap-instruments.md)
and [`docs/superpowers/specs/2026-08-18-spike3-plugin-gui.md`](docs/superpowers/specs/2026-08-18-spike3-plugin-gui.md).

It boots the kernel, installs the test-tone graph, plays for ~1 s and asserts that meter frames
move and the playhead advances — the whole audio path minus the webview. A second `#[ignore]`d test
in the same file inserts the fixture CLAP plugin and asserts the master peak halves (0.2 → 0.1).

CI runs the same gates on `windows-latest` ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)).

## Recording

FL Studio's "audio into the playlist", on one button:

1. Drop the beat (WAV, FLAC, Ogg or **MP3**) from Explorer or the Browser onto a playlist lane.
2. **REC** (or **R**) arms the input: the device opens, the bar shows its level, and you hear
   yourself through the recording channel (the headphones toggle turns that off for interfaces
   with direct monitoring).
3. **Space** plays the song *and* records. **Space** again stops; the take lands as an audio clip
   at the position you performed it, on the first lane free for its length, and is selected.
   Record mode stays on, so Space again is the next take, stacked on the next lane.
4. While the song is playing, **REC/R** punches in and out. **×** discards a running take.
   **Ctrl+Space** pauses and keeps the take. A song that ends on its own ends the take with it.
5. Right-click a take: **Mute** sets it aside, **Split here** / the **C** slice tool cut it,
   **Volume** trims it, and the left edge trims its start. **Ctrl+Z** removes a whole take.

```text
  capture thread (RT)                worker (non-RT)                  output thread (RT)
  -------------------                ---------------                  ------------------
  build_input_stream                 RecordService                    build_output_stream
    |-- capture instant -> anchor      drain from the anchor frame      |-- playback instant
    |-- fold_frame -> L R ==rtrb==>    + RateConverter -> take          |-- transport run ->
    |-- monitor ring ==rtrb==========================================>  MonitorNode (Vocal head)
    '-- peak / RMS -> atomics                                          PlaybackTimeline
                                             |                               |
                     stop: take start = run start + (capture - audible) <----'
                                             |
   <cache>/WAVE Studio/recordings/Recording N.wav  -> audio clip on the playlist, routed to "Vocal"
```

- **Latency compensated from the devices' own clocks.** The output callback publishes when each
  block becomes audible and at which transport sample; the input callback stamps when the take's
  first frame was captured. The difference places the take where the performer heard the song,
  covering both output and input latency. Settings › Audio input has a manual offset for whatever a
  driver does not report.
- **One recording channel.** The first take creates a "Vocal" audio channel; every later take
  plays through it (its fader, EQ, compressor, reverb), so takes never use up mixer strips. The
  mixer's **R** picks another audio channel as the target.
- **Live monitoring** reads a second ring through an adaptive resampler (the two devices' clocks
  drift), at the head of the Vocal channel — before its inserts — or on the master bus before the
  first take exists. It aims for the smallest latency the two callback schedules allow and backs off
  on its own after an underrun.
- **The callbacks allocate nothing.** Both rings are pre-allocated `rtrb` queues; the monitor slot
  is a `try_lock`, so a re-arm costs one silent block, never a stalled callback.
- Takes are converted to the project's rate as they are captured, are capped at ten minutes, and
  are written as 32-bit float, so an overload survives to be looked at rather than being clipped.

Commands: `get_input_devices`, `set_input_device`, `get_input_status`, `set_input_armed`,
`set_input_monitoring`, `set_record_offset`, `start_recording`, `stop_recording`,
`cancel_recording`, `set_record_channel`, `set_clip_muted`.

## How the shell fits together

```text
app/src/state.rs          boot_kernel() -> nine plugins -> UiSnapshot (+ the shell's kernel-status and mixer panels)
app/src/project.rs        TrackDef / Project: the editable mixer, and the DTOs the panel reads
app/src/tracks.rs         TrackHost: project -> MixerParams (atomics) + one SourceSlot per track
app/src/graph.rs          per track: SourceSlot -> [inserts] -> TransportGate
                          master:    sum -> [CLAP effect] -> master fader   ->  AudioIoService::install_graph
app/src/meters.rs         MeterThrottle -> pump thread (interval/4) -> Channel<MeterFrameDto>
app/src/debug_capture.rs  DebugTapService -> WAV + PNG + stats.json under .debug/captures/
app/src/recording.rs      RecordedTake -> "Recording N.wav" under <cache>/WAVE Studio/recordings/
app/src/commands.rs       get_ui_snapshot | get_clap_plugins | transport_play | transport_stop |
                          note_on | note_off | get_sources | set_source |
                          get_tracks | set_track_gain | set_track_pan | set_track_mute |
                          set_track_solo | set_master_gain | set_selected_track |
                          set_track_source | add_track | remove_track |
                          get_pattern | set_pattern | set_bpm |
                          debug_capture | subscribe_meters
```

The kernel itself never enters Tauri's managed state (`Box<dyn Plugin>` is not `Send`); it stays on
the main thread and is shut down by the run loop when the window closes.
