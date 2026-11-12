<p align="center"><img src="app/icons/128x128@2x.png" width="96" alt=""></p>
<h1 align="center">WAVE</h1>
<p align="center">
  A free, open-source DAW that works the FL Studio way: patterns, Channel Rack, piano roll, playlist.<br>
  Audio recording and vocal tuning are included, and an AI agent can edit the project you have open.
</p>
<p align="center">
  <a href="https://github.com/miskibin/wave-DAW/releases/latest"><b>Download</b> (Windows, Linux)</a> ·
  <a href="https://www.youtube.com/watch?v=wtuHDyIQpYk">Watch the video</a> ·
  <a href="https://miskibin.github.io/wave-DAW/">Hear a beat made in it</a> ·
  <a href="docs/cli.md">Command line</a>
</p>
<p align="center">
  <a href="https://github.com/miskibin/wave-DAW/releases/latest"><img src="https://img.shields.io/github/v/release/miskibin/wave-DAW?label=release" alt="Latest release"></a>
  <a href="https://github.com/miskibin/wave-DAW/actions/workflows/ci.yml"><img src="https://github.com/miskibin/wave-DAW/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <img src="https://img.shields.io/badge/licence-MIT%20or%20Apache--2.0-blue" alt="MIT or Apache-2.0">
</p>

[![WAVE: playlist, automation and the mixer. Click for the video.](site/img/hero.png)](https://www.youtube.com/watch?v=wtuHDyIQpYk)

## Note from developer (This part is not AI SLOP)

![From the first FL Studio in 2010 to WAVE in 2026](site/img/journey.svg)

- **2010:** I downloaded FL Studio for the first time and started making music.
- **2015:** I started music school: cello, guitar and piano (graduated in 2019).
- **2016–2022:** I sold beats to underground rappers in Poland (pop, true school, trap, love songs).
- Along the way I tried almost every genre (EDM, lo-fi, classical, my own pop songs), often with real instruments in my beats. I also run a YouTube channel and master the voice for it myself.
- **2022:** I joined Nokia, where I work on noise cancellation for telecom radios. That is a lot of DSP math, so I understand how sound works under the hood.

My whole life led me to this product, and with AI I can finally build it. Yes, the app is vibe-coded with claude Fable, but at least I know what the code is doing.

I now use WAVE for everything I used FL Studio for.

## Why WAVE

**If you know FL Studio, you already know WAVE.** Patterns on a free playlist, a Channel Rack
with a step sequencer, a piano roll per channel. The gestures are the same too: right-click
deletes, middle-button drag pans, Ctrl+Q quantizes, Alt+wheel changes velocity. A beat plays
on the first press of Space.

**Recording and vocals are not an edition.** Loop recording with takes and comping, measured
latency compensation, a note-level pitch editor, live pitch correction, Tune to Reference
(pull a take onto a guide vocal), a vocal rider and a one-click vocal chain. All of it is in
the one free build.

**Every effect draws what it does to your audio.** The picture above the knobs comes from the
signal passing through, so you see the compressor's gain reduction or the saturator's curve
instead of guessing. These are the real windows:

<p align="center">
  <img src="site/img/eq.png" width="32%" alt="24-band dynamic EQ">
  <img src="site/img/saturator.png" width="32%" alt="Saturator">
  <img src="site/img/pitch.png" width="32%" alt="Vocal pitch editor">
</p>

**An AI agent can work in the project you have open.** WAVE installs a `wave` command that
talks to the running app. The agent reads the song as JSON, writes notes and sets knobs on
your screen, then bounces offline and reads back peak, level and clipping, because it cannot
hear. Every command is one step in your undo history, so Ctrl+Z removes whatever you do not
like. [This trap beat](https://miskibin.github.io/wave-DAW/) was made that way.

```sh
wave context --pretty                     # tracks, inserts, patterns, channels
wave pattern --id 2 --ch hat --steps "x.x. x.x. x.x5 x.x9"
wave add-fx --insert 6 --type reverb --preset 3
wave render --out check.wav --to 16       # offline bounce + level statistics
```

**The engine is written for the audio thread first.** The callback never allocates, locks or
touches a file (the benchmark counts allocations on the audio thread: zero). Moving a knob never rebuilds the graph.
The synth's oscillators stay more than 60 dB above their aliasing up to C7
([measurements](docs/synth.md)). How it is put together: [`docs/architecture.md`](docs/architecture.md).

## What is in the box

| | |
| --- | --- |
| **Arranging** | Pattern, audio and automation clips on free lanes; slice, fade, reverse, normalise; automation curves edited on the clip |
| **Writing** | Step sequencer with swing and velocity, piano roll, computer keyboard or MIDI keyboard as the instrument |
| **Instruments** | Two-oscillator wavetable [synth](docs/synth.md) (unison, mod matrix, 46 presets); [WAVE Keys](docs/keys.md), a sampled concert grand; a sampler with a built-in drum and tonal library plus downloadable CC0 kits |
| **Mixing** | 24-band dynamic EQ, compressor, FET compressor, [saturator](docs/saturator.md), delay, [reverb, limiter](docs/reverb-limiter-v2.md), de-esser, denoiser; presets on every effect |
| **Recording** | Takes and comping, latency compensation, live monitoring |
| **Vocals** | Pitch editor, live correction, Tune to Reference, vocal rider, vocal chain |
| **Plugins and audio** | VST3 effects with their own windows after a crash-proof scan; WASAPI and ASIO on Windows, ALSA on Linux |
| **Export** | Offline bounce of the song or a loop to 32-bit float WAV or MP3 |
| **Language** | English and Polish |

## WAVE and FL Studio, honestly

WAVE borrows FL Studio's way of working, so that is the fair comparison. FL Studio has more
than twenty-five years behind it and wins on breadth.

| | WAVE 0.9 | FL Studio |
| --- | --- | --- |
| Price | Free | About $99 to $450 depending on the edition |
| Source code | Open, MIT or Apache-2.0 | Closed |
| Audio recording | Included, with takes and comping | Producer edition or higher |
| Vocal pitch editing | Included | NewTone and Pitcher come with the higher editions |
| Working with an AI agent | The agent edits the open project and measures its own renders | No command line into the open project |
| Built-in instruments and effects | 14 | More than a hundred across the editions |
| Third-party plugins | VST3 effects only | VST instruments and effects, and more formats |
| Platforms | Windows and Linux | Windows and macOS |
| Maturity | Version 0.9, one developer, expect rough edges | Since 1998, large community, tutorials, sample packs |

FL Studio details as of November 2026; see Image-Line's edition comparison for current
prices. FL Studio is a trademark of Image-Line, and WAVE is not affiliated with it.

**Coming from Ableton Live?** The arrangement side will feel familiar. WAVE has no Session
view or clip launcher, so if you write by launching clips, it does not replace Live yet.

## Where it stands

Version 0.9. It covers a complete beat and a vocal session, and I make my own music in it.
What it does not do yet:

- no VST instruments, only VST3 effects;
- no macOS build;
- no Session view or clip launching;
- releases come often, so things still move. The [changelog](CHANGELOG.md) is written for
  musicians, not for developers.

Found a bug or miss something from FL? [Open an issue](https://github.com/miskibin/wave-DAW/issues).

## Install

Download the installer from the [latest release](https://github.com/miskibin/wave-DAW/releases/latest).
The Windows build includes ASIO support (Settings → Audio → ASIO) and the WAVE Keys piano.
Linux packages are attached to the same release.

## Build from source

Prerequisites: Rust stable, Node 22 + pnpm, and on Linux the WebKitGTK/ALSA dev packages
(`libwebkit2gtk-4.1-dev libasound2-dev`).

```sh
pnpm --dir ui install
cargo install tauri-cli --version "^2"
cd app && cargo tauri dev
```

**ASIO (Windows):** build with the `asio` feature. It needs LLVM/Clang
(`winget install LLVM.LLVM`, then `LIBCLANG_PATH=C:\Program Files\LLVM\bin`); cpal downloads
the Steinberg SDK by itself.

```powershell
cd app; cargo tauri build --features asio
```

**WAVE Keys in your own build:** `node scripts/fetch-keys.mjs` downloads the piano (138 MB,
checksummed) into `app/resources/keys` so `tauri build` ships it. Without it the app offers to
download the piano the first time you add the instrument.

## The `wave` command line

It works as soon as the app has run once (open a new terminal). To let an agent such as
Claude Code work on your song, start it in any folder and tell it to run `wave context`.
Full reference: [`docs/cli.md`](docs/cli.md).

```sh
wave channels --keys                      # add the WAVE Keys piano
wave undo                                 # or press Ctrl+Z in the app
```

## Layout

| path | what |
| --- | --- |
| `crates/engine` | project model, ops + undo, sequencer, synth and piano voices, built-in DSP, mixer graph, recording, offline render, analysis |
| `crates/audio-io` | cpal devices: WASAPI / ASIO / CoreAudio / ALSA, duplex in/out |
| `crates/vst3-host`, `crates/plugin-windows`, `crates/plugin-scan` | VST3 effects as engine processors, crash-proof out-of-process scan, native editor windows |
| `crates/audio-file` | WAV/FLAC/OGG/MP3 decoding |
| `crates/session`, `crates/cli` | the local socket the CLI uses, and the CLI itself |
| `app` | Tauri shell: commands, library downloads, settings, CLI launcher |
| `ui` | SolidJS + TypeScript frontend (runs in a plain browser with a mock backend too) |
| `site` | the landing page published to GitHub Pages |

## Tests

```sh
cargo test --workspace
pnpm --dir ui test
cargo run -p wave-engine --example bench --release   # callback load, heavy mixer
```

## Licence

MIT or Apache-2.0. Third-party components and sample credits:
[`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md).
