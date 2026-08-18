# Debug tap — an audio inspection harness for agents

Date: 2026-08-18
Status: Done. Verified on this machine.

## The problem

Everything downstream of `rt-core` is judged by ear. That is fine for a person with speakers and
fatal for everyone else: an AI agent developing this DAW cannot hear it, a CI runner has no output
device, and a developer on a remote session has no audio path at all. "The meter moves" only proves
that *something* is non-zero — it cannot tell a 440 Hz sine from white noise, cannot see a DC offset,
cannot see a NaN, and cannot see the difference between a CLAP plugin working and a CLAP plugin
bypassing.

So: let the engine be **seen**. Capture the real output bus, and turn it into a picture and a block
of numbers.

## What got built

| Where | What |
| --- | --- |
| `plugins/debug-tap` | `mydaw-plugin-debug-tap`, plugin id `debug-tap`. RT tap + capture service + analysis + WAV + PNG. |
| `plugins/debug-tap/src/tap.rs` | `TapShared` (arm flag, drop counter, sample rate), `TapRtSide::push_block` (the only RT code in the crate), `TapConsumer`, `debug_tap_channel`. |
| `plugins/debug-tap/src/capture.rs` | `Capture`, `CaptureError`, `DebugTapService::capture`, `summary_text`, ASCII envelope sparkline. |
| `plugins/debug-tap/src/analysis.rs` | `CaptureStats` / `ChannelStats` / `SpectralPeak`, `dbfs`, `hann_window`, `spectrum` (realfft), parabolic peak interpolation, spectral flatness, zero-crossing pitch. |
| `plugins/debug-tap/src/wav.rs` | 32-bit float stereo WAV writer and reader (hound). |
| `plugins/debug-tap/src/render.rs` | Own rasteriser (`Canvas`: Bresenham, rects, text) → `png`. Waveform + zoom + log spectrum, plus an axis sidecar JSON. |
| `plugins/debug-tap/src/font.rs` | A 5x7 bitmap font, written as rows of `#` and `.` so the glyphs can be proof-read. |
| `plugins/audio-io` | `RtStateParams::debug_tap`, `RtState::take_debug_tap`, one `push_block` call in `run_block`, `AudioIoService::attach_debug_tap`, return-on-teardown through `RtStateCarrier`, manifest dep on `debug-tap`. |
| `app/src/debug_capture.rs` | `capture_to_disk`, timestamped output directories, `CaptureReportDto`, the stdout report, `run_headless_capture`. |
| `app/src/commands.rs` | `debug_capture(seconds) -> CaptureReportDto`. |
| `app/src/lib.rs` | `MYDAW_DEBUG_CAPTURE` headless branch — no window is created at all. |

Nothing was added to the UI. The capture path is an agent/developer tool driven by an environment
variable and an IPC command; a panel duplicating it is a Phase 2 decision.

## Design

```text
  debug-tap (register)          audio-io (start)              app / agent
  --------------------          ----------------              -----------
  debug_tap_channel()  --tap-->  DebugTapHandoff::take()  ==>  DebugTapService
                                 (moves into the RT              ::capture()
                                  closure; run_block               |-> stats()
                                  pushes every block)              |-> write_wav()
                                                                   |-> render_png()
                                                                   '-> summary_text()
```

The hand-off deliberately mirrors `mydaw-plugin-metering`. Both ends of the ring are given away
exactly once through a `take()`-style service, so the single-producer/single-consumer invariant is
enforced by construction rather than by convention, and `RtStateCarrier` returns the RT side on
stream teardown so a stop/start cycle does not leave the harness dead.

### Why the tap is armed rather than always on

A capture is used for seconds at a time, in a session that runs for hours. An always-on tap would
copy every sample of every block forever to serve a feature nobody is using. One atomic flag buys
that back; see the cost analysis below.

### Why the tap sits on the bus, not on the device buffer

`run_block` writes the engine's stereo bus into whatever interleaved layout the device negotiated.
The tap reads the **bus**, so a capture is always 2 channels regardless of whether the card is
stereo, mono or 5.1, and it contains exactly the frames the graph produced (never the silenced tail
of an over-large block). A capture is a picture of the engine, not of the sound card.

### Where the sample rate comes from

The capture service never sees the device. `RtState::new` — non-RT, once per stream — writes the
negotiated rate into `TapShared`. Before any stream has opened, that value is 0 and `capture()`
returns `EngineNotRunning` rather than inventing 48 kHz and mislabelling every frequency in the
report.

### Timeout semantics

`capture(duration, timeout)` fills a pre-sized buffer and gives up at the deadline. A timeout is not
a bug — **a stopped transport plus a silent graph is the normal reason** — so the error carries the
partial capture:

```rust
CaptureError::TimedOutWithSamples { got, want, capture: Box<Capture> }
```

`capture_to_disk` treats a partial capture with `got > 0` as a success with `timed_out: true` (the
audio is still evidence) and `got == 0` as `DebugCaptureError::NoAudio` (there is nothing to look
at). Note that with the shell's `TransportGate` in the graph, a stopped transport still produces
blocks of *digital silence*, so it yields a full, successful capture whose peak is `-inf dBFS` —
a very different diagnosis from "no audio arrived", and the report distinguishes them.

## RT-cost analysis

`TapRtSide::push_block` is the only code this feature adds to the audio callback.

**Disarmed** (every block of normal use):

```rust
if !self.shared.is_armed() { return; }   // one Relaxed load of an AtomicBool
```

One uncontended relaxed load from a cache line the callback already touches nothing else in, plus a
perfectly-predicted branch. Single-digit nanoseconds; against a 128-frame block at 48 kHz (2.67 ms of
budget) that is a rounding error on a rounding error.

**Armed**:

1. `producer.slots()` — one acquire load.
2. `write_chunk_uninit(fit * 2)` — pointer arithmetic, no allocation.
3. `fill_from_iter` over a zip/flat_map — a straight-line copy of `frames * 2` `f32`s.
4. On overflow, one `fetch_add` on the drop counter.

That is one extra pass over the block, which is the same order as the metering pass that already
runs. There is **no allocation, no lock, no syscall and no blocking on any path** — including the
overflow path, which counts and discards rather than waiting. This is measured, not asserted:
`plugins/debug-tap/tests/rt_alloc.rs` installs a counting `GlobalAlloc` and requires **0** allocator
events across armed pushes at twelve different block sizes, across disarmed pushes, and across a
deliberately overflowing ring — after first proving the counter itself works.

**Frame alignment under overflow.** A block that only half fits contributes an integral number of
frames; the remainder is counted in `dropped` (in samples). The reader can therefore never lose
channel alignment, which would otherwise turn a dropped sample into a permanently swapped L/R for the
rest of the capture.

**Ring size.** 65 536 samples = 32 768 stereo frames ≈ 0.68 s at 48 kHz. The capture loop drains
every 1 ms, so steady-state occupancy is ~100 samples; the rest is headroom for a scheduling hiccup
on the draining thread. `dropped > 0` in a report means the capture has holes and its timings are not
trustworthy — the summary says so in as many words.

## Analysis

Per channel: peak (linear + dBFS), RMS (linear + dBFS), DC offset, non-finite count, clipped count
(`|s| >= 1.0`), zero-crossing frequency, dominant frequency and its magnitude, spectral flatness, and
the five loudest spectral peaks.

- **FFT**: `realfft`, largest power of two that fits, capped at 32 768. **Hann window**, periodic
  form, so the window's own spectrum lands on bin centres. Frequency resolution is
  `sample_rate / fft_size`, reported in `frequencyResolutionHz` — 1.35 Hz for a 2 s capture at
  44.1 kHz, 2.93 Hz for a 0.5 s capture at 48 kHz.
- **Magnitude scaling** is `2 / sum(window)` (`= 4/n` for Hann), so a bin reads the **amplitude** of
  the sinusoid in it: a full-scale sine is 0 dBFS at any FFT size. Tested at three sizes.
- **Peak frequency** is refined by parabolic interpolation over the three log-magnitude bins around
  the maximum, the standard companion to a Hann window. That is why a 440 Hz tone reports 439.99 Hz
  and not "somewhere in bin 327".
- **Top peaks** are local maxima, not the N largest bins — the N largest bins of a sine are the N
  bins of its one main lobe, which tells you nothing.
- **Non-finite samples** are excluded from every statistic and zeroed before the FFT, so one NaN
  cannot turn the whole measurement into NaN. The count is reported separately, and any non-zero
  value is a DSP bug.

### -inf and JSON

Digital silence is `-inf dBFS`, and that is what the types carry (`f32::NEG_INFINITY`). `serde_json`
renders a non-finite float as **`null`**, so a silent capture serialises as `"peakDbfs": null`. That
is the chosen behaviour — not a made-up floor like -144 — and it is pinned by tests both in
`analysis.rs` and on a capture that came out of the real RT path. Consumers must handle `null`.

## The picture

`capture.png`, 1200x600, dark background, bright traces, gridlines, tick labels baked into the pixels
with a 5x7 font at 2x:

- **Top left — envelope.** Min/max per pixel column for both channels (L blue `#58a6ff`, R orange
  `#f78166`), dB gridlines at 0/-6/-12/-24, a time axis in seconds. NaN/Inf columns are struck
  through in magenta.
- **Top right — zoom.** About four cycles of the dominant tone (or 5 ms when there is none), drawn as
  a connected polyline. This panel exists because the envelope *cannot* show waveform shape: 1 100
  columns for 88 200 frames means every column spans a whole cycle, so a sine and a square wave look
  identical. The zoom is where "is this actually a sine" is answered.
- **Bottom — spectrum.** Log-frequency 20 Hz–20 kHz, dBFS from -120 to +6, both channels, labelled
  decade and 1-2-5 gridlines, a green dashed marker with the dominant frequency, and a Nyquist marker
  when Nyquist falls inside the axis.

Everything clamps. The renderer is tested against silence, a single frame, a single orphan sample, an
empty capture, a NaN-riddled capture, a NaN sample rate, and a 10x10 requested size.

`capture.axes.json` carries the same axes as data: plot rectangles in pixels, every tick's value and
its pixel coordinate, the FFT size and bin width, and the zoom window. A reader that wants exact
numbers does not have to squint at the image.

## How an agent uses this

### Headless, one command

```sh
MYDAW_DEBUG_CAPTURE=2 cargo run -p mydaw-app --features custom-protocol
```

Boots the kernel, opens the device, installs the graph, starts the transport, captures 2 s, writes
the artefacts, prints a report to stdout and exits. **No window is created at all.** Exit code 0 if
audio was captured, 1 if it was not (including a malformed variable).

The report is delimited so it can be found in a log full of `env_logger` output:

```text
===== MYDAW DEBUG CAPTURE BEGIN =====
MYDAW capture: 88200 frames, 2.000 s, 2 ch @ 44100 Hz, dropped 0 samples
analysis: hann window, fft 32768 bins, resolution 1.35 Hz
  L: peak -13.98 dBFS (+0.2000)  rms -16.99 dBFS  dc -0.000000  clipped 0  non-finite 0
     dominant 440.0 Hz at -13.97 dBFS  zero-cross 439.8 Hz  flatness 0.0000
     peaks 440Hz/-13.97 dBFS  579Hz/-158.81 dBFS  ...
  R: ...
  env [%%%%%%%%%%%%%%%%%%%%...]  (space = -60 dBFS, @ = 0 dBFS)
----- STATS JSON -----
{ ... camelCase CaptureStats ... }
----- FILES -----
dir  D:\MYDAW\.debug\captures\20260818-203831
wav  ...\capture.wav
png  ...\capture.png
axes ...\capture.axes.json
json ...\stats.json
text ...\summary.txt
===== MYDAW DEBUG CAPTURE END =====
```

Then **look at the PNG**. That is the point of the whole exercise.

With a CLAP effect inserted:

```sh
MYDAW_DEBUG_CAPTURE=2 \
MYDAW_CLAP_FX='D:\MYDAW\target\clap-stage\mydaw-gain.clap;dev.mydaw.fixture.gain' \
  cargo run -p mydaw-app --features custom-protocol
```

Output directory: `<cwd>/.debug/captures/<YYYYMMDD-HHMMSS>/`, overridable with `MYDAW_DEBUG_DIR`.
Timestamps are UTC and sort chronologically, so a directory listing is a timeline; a second capture
within the same second gets a `-1` suffix rather than overwriting the first. `.debug/` is
git-ignored.

Env vars, in full:

| Variable | Meaning |
| --- | --- |
| `MYDAW_DEBUG_CAPTURE=<seconds>` | Run headlessly and capture. 0.01–120. Anything else exits 1. |
| `MYDAW_DEBUG_DIR=<path>` | Root for `.debug/captures`. Defaults to the working directory. |
| `MYDAW_CLAP_FX=<bundle>;<id>` | Pre-existing: insert a CLAP effect into the graph. |

### From the UI process

`invoke('debug_capture', { seconds: 2 })` returns a `CaptureReportDto` (camelCase): `stats`,
`summary`, `dir`, `wavPath`, `pngPath`, `axesJsonPath`, `statsJsonPath`, `summaryPath`,
`droppedSamples`, `timedOut`, `requestedFrames`, `capturedFrames`. It is `async`, so a two-second
capture does not freeze the window.

### From Rust

```rust
let capture = debug_tap.capture(Duration::from_secs(1), Duration::from_secs(3))?;
println!("{}", capture.summary_text());
capture.write_wav("capture.wav")?;
capture.render_png("capture.png")?;
```

## Verified on this machine

Windows 11, Focusrite USB Audio @ 44 100 Hz, 2 ch, `--features custom-protocol` over the built UI.

| Check | Result |
| --- | --- |
| `MYDAW_DEBUG_CAPTURE=2`, test tone only | 88 200 frames, 2.000 s, **0 dropped**. Peak **-13.979 dBFS** (0.19999996 — the test tone is 0.2 = -13.979 dB). Dominant **439.993 Hz**. RMS -16.99 dBFS. DC -3.7e-17. Clipped 0, non-finite 0. Flatness 4.5e-15. Exit 0. |
| The PNG | Envelope block at ±0.2, **a clean sine in the zoom panel**, a single spectral spike at 440 Hz labelled `PEAK 440.0HZ AT -14.0DB`, all axes legible. |
| Same, with the CLAP fixture inserted | Peak **-20.000 dBFS** (0.09999998) — exactly -6.02 dB below the tone, which is the fixture's `x0.5`. Dominant 439.993 Hz. Exit 0. |
| `MYDAW_DEBUG_CAPTURE=abc` | `MYDAW: MYDAW_DEBUG_CAPTURE must be a number of seconds, got 'abc'`, exit 1. |
| Kernel boot order | `metering -> debug-tap -> test-tone -> host-clap -> host-vst3 -> plugin-windows -> audio-io` |

Gates: `cargo build --workspace`, `cargo test --workspace` (**279 passed, 0 failed, 3 ignored** — was
160/2), `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check`. The UI was
not touched, so `pnpm --dir ui build` is unchanged.

## Limitations

- **One tap point.** The capture is the master bus after the whole graph. There is no per-node or
  per-track probe; that needs the Phase 2 mixer to have taps to attach to.
- **Post-graph, pre-device.** Sample-rate conversion, channel mapping and anything WASAPI does
  afterwards are invisible. This measures what MYDAW produced, not what the speaker moved.
- **One capture at a time.** The ring has one consumer; a concurrent `capture()` gets
  `CaptureError::Busy`.
- **Captures are RAM-resident.** `duration * sample_rate * 2` floats are pre-allocated, capped at
  `MAX_CAPTURE_SECONDS` (120 s ≈ 46 MB at 48 kHz). There is no streaming-to-disk mode.
- **The spectrum is one window, not an average.** A single Hann-windowed FFT over the first
  `fft_size` frames of the capture. Non-stationary material (a note attack, a sweep) is smeared;
  there is no Welch averaging and no spectrogram.
- **No loudness, no phase, no correlation.** No LUFS/K-weighting, no phase spectrum, no stereo
  correlation meter. All are straightforward additions to `analysis.rs` when something needs them.
- **The zoom panel picks its window from the dominant frequency**, taken from the middle of the
  capture. For polyphonic or percussive material that window is arbitrary — informative, not
  authoritative.
- **The headless mode always plays the default graph.** There is no way to script "load this project,
  seek there, then capture"; the transport is simply started. Projects do not exist yet.
- **No golden-image regression tests.** The PNG tests assert that rendering does not panic, that
  dimensions and file size are right, and that the expected colours appear — not pixel equality.
  Pinning pixels would make every cosmetic change a test failure.
- **`dropped > 0` is reported, not repaired.** A capture with holes stays in the report, flagged. The
  fix is a bigger ring or a shorter capture, never a blocking push on the audio thread.

## Pointers for later

- The per-node probe belongs on the same `TapShared` machinery: a second `TapRtSide` inside a
  `ProbeNode` that the graph builder can insert anywhere. `push_block` is already generic over the
  two slices it is handed.
- A spectrogram is `spectrum_with_planner` in a loop with hop-size overlap, and a third panel in
  `render.rs`; the `Canvas` already has everything it needs.
- If captures ever need to be long, the seam is `DebugTapService::capture`: swap the pre-sized `Vec`
  for a `hound::WavWriter` fed from the same drain loop.
