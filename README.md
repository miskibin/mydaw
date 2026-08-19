# MYDAW

An FL Studio-style DAW built as a tiny Rust microkernel with **everything else as a plugin** —
audio I/O, metering, CLAP/VST3 hosting and UI panels all register against the same contract.
Tauri v2 + SolidJS on the front, a lock-free real-time audio graph behind the wall.

Status: **Phase 1 — the engine plays.** It boots, lists its plugins, streams meters to the UI, and
you can play it: a built-in eight-voice synth, a synthesised drum kit, a 440 Hz test tone or a
hosted CLAP instrument, chosen from the transport bar and driven from the computer keyboard
(`A W S E D F T G Y H U J`, `Z`/`X` for the octave; `C V B N` are drum pads when the kit is
selected). No sequencing, no mixer, no project files yet.
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
| `crates/rt-core` | RT primitives: `RtNode`, serial graph, lock-free graph swap, `rtrb` command queue, transport clock |
| `crates/ipc-bridge` | Kernel ⇄ webview data: serde DTOs, `UiSnapshot`, 30 Hz meter throttle. Tauri-free by design |
| `plugins/metering` | Owns the audio-thread → UI meter queue and the Meters panel |
| `plugins/debug-tap` | RT-safe capture of the output bus, plus WAV / annotated PNG / statistics — so the engine can be *inspected* without ears |
| `plugins/test-tone` | 440 Hz sine `RtNode`; the vehicle for the Phase 1 RT spike |
| `plugins/instruments` | The built-in instruments: a synthesised drum kit and a polyBLEP poly synth, driven by note events. No assets, bit-deterministic, zero-alloc |
| `plugins/audio-io` | cpal/WASAPI device, the real-time callback, transport and graph plumbing |
| `plugins/host-clap` | CLAP discovery **and hosting**: scans bundles, runs a plugin instance as a `ClapNode` in the RT graph |
| `plugins/host-vst3` | VST3 host stub (Phase 1 spike 5) |
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

Two tests are opt-in because they open the real output device and are audible (the second also
puts a plugin editor on screen):

```sh
cargo test -p mydaw-app --test audio_end_to_end -- --ignored --nocapture
cargo test -p mydaw-app --test gui_audio_smoke  -- --ignored --nocapture
```

## Plugin editors

An external plugin's editor opens in its **own native window** next to MYDAW's -- a plugin paints
with its own toolkit into an OS window handle, which cannot be composited into the webview. Load an
effect and use *Open GUI* in the CLAP Plugins panel:

```sh
MYDAW_CLAP_FX='C:\path\to\plugin.clap;com.vendor.plugin' cargo run -p mydaw-app --features custom-protocol
```

See [`docs/superpowers/specs/2026-08-18-spike3-plugin-gui.md`](docs/superpowers/specs/2026-08-18-spike3-plugin-gui.md).

It boots the kernel, installs the test-tone graph, plays for ~1 s and asserts that meter frames
move and the playhead advances — the whole audio path minus the webview. A second `#[ignore]`d test
in the same file inserts the fixture CLAP plugin and asserts the master peak halves (0.2 → 0.1).

CI runs the same gates on `windows-latest` ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)).

## How the shell fits together

```text
app/src/state.rs          boot_kernel() -> seven plugins -> UiSnapshot (+ the shell's kernel-status panel)
app/src/graph.rs          test-tone -> [CLAP effect] -> TransportGate  ->  AudioIoService::install_graph
                          or, with MYDAW_CLAP_INSTRUMENT: instrument -> [CLAP effect]  (no gate)
app/src/meters.rs         MeterThrottle -> pump thread (interval/4) -> Channel<MeterFrameDto>
app/src/debug_capture.rs  DebugTapService -> WAV + PNG + stats.json under .debug/captures/
app/src/commands.rs       get_ui_snapshot | get_clap_plugins | transport_play | transport_stop |
                          note_on | note_off | debug_capture | subscribe_meters
```

The kernel itself never enters Tauri's managed state (`Box<dyn Plugin>` is not `Send`); it stays on
the main thread and is shut down by the run loop when the window closes.
