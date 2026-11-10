# WAVE

A fast, FL-Studio-style DAW: Rust audio engine, Tauri 2 shell, SolidJS + canvas UI.
Playlist with patterns and audio clips, a built-in synth and a sampled grand piano
([WAVE Keys](docs/keys.md)), mixer with insert effects (EQ, compressor,
saturator, delay, reverb, limiter — all with presets and live visualisations),
recording, a sample library, VST3 effects, **ASIO** on Windows, and a `wave`
command line that edits the open project live.

v2 is a rewrite of the first prototype: same ideas, a much smaller codebase
(see [`docs/architecture.md`](docs/architecture.md)), and performance as the
first requirement — the audio callback never allocates or locks, parameter
moves never rebuild the graph, and every moving picture in the UI is drawn on
canvas from one binary telemetry frame.

## Run it

Prerequisites: Rust stable, Node 22 + pnpm, and on Linux the WebKitGTK/ALSA dev
packages (`libwebkit2gtk-4.1-dev libasound2-dev`).

```sh
pnpm --dir ui install
cargo install tauri-cli --version "^2"
cd app && cargo tauri dev
```

**ASIO (Windows):** build with the `asio` feature. It needs LLVM/Clang
(`winget install LLVM.LLVM`, then `LIBCLANG_PATH=C:\Program Files\LLVM\bin`);
cpal downloads the Steinberg SDK by itself.

```powershell
cd app; cargo tauri build --features asio
```

CI builds the Windows installer with ASIO on every push (Actions → artifact
`WAVE-windows`). Pick the driver in Settings (gear icon) → Audio → ASIO; input
and output then run on that driver, and the buffer size you pick there is used
when the driver allows it.

## The `wave` CLI

Starts working the moment the app has run once — see [`docs/cli.md`](docs/cli.md).

```sh
wave context --pretty
wave add-fx --insert 6 --type reverb --preset 3
wave render --out check.wav --to 16
```

## Layout

| path | what |
| --- | --- |
| `crates/engine` | project model, ops + undo, sequencer, synth voices, built-in DSP, mixer graph, recording, offline render, analysis |
| `crates/audio-io` | cpal devices: WASAPI / ASIO / CoreAudio / ALSA, duplex in/out |
| `crates/vst3-host`, `crates/plugin-windows`, `crates/plugin-scan` | VST3 effects as engine processors, crash-proof out-of-process scan, native editor windows |
| `crates/audio-file` | WAV/FLAC/OGG/MP3 decoding |
| `crates/session`, `crates/cli` | the local socket the CLI uses, and the CLI itself |
| `app` | Tauri shell: commands, library downloads, settings, CLI launcher |
| `ui` | SolidJS + TypeScript frontend (runs in a plain browser with a mock backend too) |
| `docs/design/playlist-v5.dc.html` | the interactive prototype the UI follows |

## Tests

```sh
cargo test --workspace
pnpm --dir ui test
cargo run -p wave-engine --example bench --release   # callback load, heavy mixer
```
