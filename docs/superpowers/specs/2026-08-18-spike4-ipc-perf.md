# Phase 1 Spike 4 — IPC performance: what the Rust → webview boundary actually costs

Date: 2026-08-18
Status: Done. Gate passed — **go**, the webview UI stays.

Spike goal (from the scaffold spec): measure the real latency and throughput of the Rust → webview
IPC paths and decide whether a WebView2 front end can carry high-frequency DAW data (meters,
playhead, waveforms). The research doc's threshold was explicit: **sustained meter latency above
20–30 ms means the webview approach must be rethought.**

The measured meter latency on this machine is **0.30 ms median, 1.1 ms p99**. The nearest thing to
a limit anywhere in the matrix is 12 ms for a *one-megabyte* frame. Nothing in the tested envelope —
up to 60 MB/s sustained — dropped a frame or blocked the UI thread for 50 ms.

## What got built

| Where | What |
| --- | --- |
| `app/src/ipc_bench.rs` | The harness: scenario config + validation, the standard and stress plans, exact-size JSON/binary frame builders, the paced sender, nearest-rank percentiles, the clock-sync estimator, the report types, the results writer and the stdout report. 29 unit tests. |
| `app/src/commands.rs` | Seven dev commands: `bench_ping`, `bench_ping_async`, `bench_plan`, `bench_sync`, `bench_stream`, `bench_meter_stamp`, `bench_finish`. |
| `app/src/meters.rs` | `MeterPump::set_stamping` — the *only* change to the live meter path: one relaxed atomic load per emitted frame, and a timestamp when the bench asks for it. |
| `app/src/dto.rs` | `MeterFrameDto::pump_ms`, `skip_serializing_if = "Option::is_none"` — absent from the wire in every normal run. |
| `app/src/state.rs` | Contributes the `ipc-bench` panel manifest, and only under an environment variable. |
| `ui/src/dev/ipcBench.ts` | The webview half: clock handshake, per-scenario receive loop with drain detection, the live meter measurement, `PerformanceObserver('longtask')`, and the hand-off back to Rust. |
| `ui/src/dev/IpcBenchPanel.tsx` | Hidden panel: run the matrix or a single scenario, results table, JSON download. |
| `ui/src/state.tsx` | Auto-run wiring and `resubscribeMeters()`, so the bench can borrow the meter stream and give it back. |

### It is inert unless asked for

| Environment | Effect |
| --- | --- |
| *(nothing)* | No panel manifest, no thread, no timestamp on the wire, and the UI never fetches the bench chunk — Vite splits `src/dev/` out and the lazy import is keyed on a manifest that is not there. |
| `MYDAW_DEV_PANELS=1` | Contributes the `ipc-bench` panel; run scenarios by hand. |
| `MYDAW_IPC_BENCH=1` | Contributes the auto panel: the UI runs the full matrix unattended, Rust writes `.debug/ipc-bench/<timestamp>.json`, prints a report between `===== MYDAW IPC BENCH BEGIN/END =====`, and exits. Mirrors `MYDAW_DEBUG_CAPTURE`. |
| `MYDAW_IPC_BENCH=stress` | The same, plus eight past-the-cliff scenarios (30 and 60 MB/s). |

```sh
MYDAW_IPC_BENCH=1 cargo run -p mydaw-app --release --features custom-protocol
```

## Method

### The two clocks, and the error bar

Frames are stamped in Rust time (`ipc_bench::epoch_ms()`, an `Instant` epoch in milliseconds) and
received in JavaScript time (`performance.now()`). These are unrelated monotonic clocks, so the
offset is measured first, by a 40-ping handshake:

```text
js_before = performance.now()
rust_ms   = await invoke('bench_ping')     // Rust's epoch_ms()
js_after  = performance.now()
offset    = rust_ms - (js_before + js_after) / 2
```

The estimator keeps the offset of the **lowest-RTT ping**, not the median offset: on a quiet path
the fastest round trip is the one least distorted by scheduling at either end (the argument NTP
makes). It reports `rtt_min / 2` as the error bar, because that is the most the unmeasurable path
asymmetry can hide.

**On these runs the best round trip was 0.7–0.8 ms, so every one-way number below carries
±0.35–0.40 ms.** That matters for the sub-millisecond rows (a 0.24 ms figure is "well under a
millisecond", not "0.24") and is negligible for everything at 2 ms and above.

The offset is computed in Rust (`bench_sync`), not in the UI, so the number used *during* a run is
by construction the number printed in the report.

One-way latency is then `js_receive_ms - (rust_sent_ms - offset)` and includes everything real:
serialisation, the hop to the webview's UI thread, `eval` or the internal fetch, JSON parse or
`ArrayBuffer` hand-over, and the JS callback being scheduled.

The clock-free cross-check the brief asked for is reported alongside: a bare `invoke` round trip
(sync command, so Tauri runs it on the main thread) had **p50 0.9 ms, p95 1.5–1.7 ms, max 1.9 ms**;
the `async` variant on Tauri's worker pool was 0.1–0.2 ms slower. Halving the RTT gives ≈0.45 ms as
the naive one-way estimate, and the measured 64 B channel latency (0.24 ms) sits just under it —
which is what it should be, since a channel send skips the command-dispatch leg.

### The two transports

Both are `tauri::ipc::Channel`, which is what `subscribe_meters` already uses. The payload type is
`InvokeResponseBody`, the one type that carries either shape with no further conversion:

- **`channel-json`** — `InvokeResponseBody::Json(String)`: a pre-serialised JSON document, exactly
  what `Channel<MeterFrameDto>` produces today.
- **`channel-binary`** — `InvokeResponseBody::Raw(Vec<u8>)`: the rawest thing Tauri 2.11.5 offers on
  a channel (there is no zero-copy or shared-memory channel in this version), with a 16-byte
  little-endian header (`seq: u32`, `sent_ms: f64`, `len: u32`) and filler.

Reading `tauri-2.11.5/src/ipc/channel.rs` explains the shape of the results before the results
arrive. Tauri picks a delivery mechanism per message by size, with **different thresholds per
encoding**:

| Body | Below the threshold | Above it |
| --- | --- | --- |
| `Json` | < 8192 B: `webview.eval` of the JSON literal | the internal `plugin:__TAURI_CHANNEL__|fetch` round trip |
| `Raw` | < 1024 B: `eval` of a `new Uint8Array([...]).buffer` literal | the same fetch path (`application/octet-stream` → `ArrayBuffer`) |

So a 4 KB JSON frame takes the cheap eval path while a 4 KB *binary* frame takes the fetch path.
That asymmetry, not the encoding itself, is what the 4 KB row below shows.

### Payloads, pacing and counting

- Both encodings are built to **exactly** the requested byte count, so a JSON row and a binary row at
  the same size move the same number of bytes: the comparison is about encoding, not volume.
- Frames are sent from a dedicated std thread (never the audio thread, never the main thread), paced
  by sleep-until-1 ms-then-spin. `thread::sleep` on Windows overshoots by a scheduler quantum, which
  at 240 Hz would be a third of the frame period; the spin keeps the *sender's* jitter well below
  what is being measured. Achieved sender rates came out within 0.1 % of target throughout.
- **Achieved rate** is measured at the receiver: `(received - 1) / (last_recv - first_recv)`.
- **Drops** are scheduled frames that never arrived. After each scenario the harness drains the
  channel until 250 ms pass with nothing arriving, and counts anything that lands there as *late*
  rather than lost — "delivered, but seconds behind" is a different failure from "gone", and letting
  a backlog spill into the next scenario would charge the wrong row for it. No run produced a single
  late or lost frame.
- **Long tasks** are `PerformanceObserver('longtask')` entries (>50 ms of blocked main thread)
  observed during the scenario.
- Percentiles are nearest-rank: with a few hundred samples per scenario, interpolation would invent
  precision the measurement does not have, and a reported p99 should be a value that occurred.
- Every scenario runs 5 s. Three full runs; the tables report the **median across runs** of each
  statistic, and drops/long tasks as the worst of the three.

### Environment

| | |
| --- | --- |
| OS | Windows 11 Pro, build 10.0.26200 |
| CPU | Intel Core i5-12400F (6 cores / 12 threads); `navigator.hardwareConcurrency` = 12 |
| WebView2 | Evergreen runtime **151.0.4129.86** (registry). The UA reports `Edg/151.0.0.0` — Chromium's reduced user agent, which is why the JSON records the shorter string |
| Tauri / wry | tauri 2.11.5 |
| Build | `--release` (LTO thin, codegen-units 1) unless stated; one debug-profile run is reported for comparison |
| Audio | Focusrite USB, 44.1 kHz, 2 ch — open and playing during the live meter scenario |
| Display | devicePixelRatio 1.0 |
| Noise | A second MYDAW instance (left open from spike 3) was running on the desktop throughout. That is a *pessimistic* condition, not a favourable one, and it is disclosed rather than eliminated because the numbers are comfortable anyway |

Runs: `.debug/ipc-bench/20260818-223116.json`, `-223326`, `-223535` (standard),
`-223829` (stress), `-224109` (debug profile). That directory is git-ignored — the tables below are
the record; the harness regenerates the files on demand.

## Results — standard matrix, release build

Latency is one-way in milliseconds, median of three runs, ±0.4 ms of clock-sync uncertainty.

| scenario | p50 | p95 | p99 | max | achieved Hz | MB/s | drops | long tasks |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `channel-json/64B@30Hz` | 0.24 | 0.44 | 0.78 | 0.94 | 30.0 | 0.00 | 0 | 0 |
| `channel-json/64B@60Hz` | 0.24 | 0.46 | 0.61 | 0.84 | 60.0 | 0.00 | 0 | 0 |
| `channel-json/64B@240Hz` | 0.17 | 0.37 | 0.54 | 0.70 | 240.0 | 0.01 | 0 | 0 |
| `channel-json/4KB@30Hz` | 0.32 | 0.59 | 0.89 | 1.09 | 30.0 | 0.12 | 0 | 0 |
| `channel-json/4KB@60Hz` | 0.30 | 0.53 | 0.84 | 1.14 | 60.0 | 0.23 | 0 | 0 |
| `channel-json/4KB@240Hz` | 0.23 | 0.46 | 0.66 | 0.81 | 240.0 | 0.94 | 0 | 0 |
| `channel-json/64KB@30Hz` | 2.52 | 3.19 | 3.56 | 4.42 | 30.0 | 1.87 | 0 | 0 |
| `channel-json/64KB@60Hz` | 2.41 | 3.08 | 3.48 | 3.84 | 60.0 | 3.75 | 0 | 0 |
| `channel-json/64KB@240Hz` | 2.08 | 2.64 | 3.21 | 5.47 | 240.0 | 15.00 | 0 | 0 |
| `channel-json/1MB@5Hz` | 11.82 | 14.02 | 14.51 | 14.51 | 5.0 | 5.00 | 0 | 0 |
| `channel-json/1MB@10Hz` | 11.82 | 13.82 | 17.37 | 17.37 | 10.0 | 10.00 | 0 | 0 |
| `channel-binary/64B@30Hz` | 0.28 | 0.58 | 0.81 | 0.99 | 30.0 | 0.00 | 0 | 0 |
| `channel-binary/64B@60Hz` | 0.26 | 0.52 | 0.80 | 1.12 | 60.0 | 0.00 | 0 | 0 |
| `channel-binary/64B@240Hz` | 0.19 | 0.42 | 0.59 | 0.89 | 240.0 | 0.01 | 0 | 0 |
| `channel-binary/4KB@30Hz` | 1.82 | 2.45 | 2.96 | 4.12 | 30.0 | 0.12 | 0 | 0 |
| `channel-binary/4KB@60Hz` | 1.68 | 2.25 | 2.48 | 3.00 | 60.0 | 0.23 | 0 | 0 |
| `channel-binary/4KB@240Hz` | 1.40 | 1.97 | 2.39 | 7.27 | 240.0 | 0.94 | 0 | 0 |
| `channel-binary/64KB@30Hz` | 2.42 | 3.05 | 3.69 | 4.29 | 30.0 | 1.88 | 0 | 0 |
| `channel-binary/64KB@60Hz` | 2.33 | 3.09 | 3.36 | 3.76 | 60.0 | 3.75 | 0 | 0 |
| `channel-binary/64KB@240Hz` | 2.06 | 2.56 | 3.00 | 12.16 | 240.0 | 15.00 | 0 | 0 |
| `channel-binary/1MB@5Hz` | 11.97 | 14.55 | 15.15 | 15.15 | 5.0 | 5.00 | 0 | 0 |
| `channel-binary/1MB@10Hz` | 11.86 | 12.84 | 14.46 | 14.46 | 10.0 | 10.00 | 0 | 0 |
| **`meter-path (live)`** | **0.29** | **0.64** | **0.88** | **1.09** | **21.8** | — | 0 | 0 |

Every scenario delivered **every** scheduled frame: 0 dropped, 0 late, in all three runs.
Run-to-run spread on p50 was within ±0.1 ms below 64 KB and ±0.3 ms at 1 MB.

## Results — stress run: where does it fall over?

It does not, within an envelope four times what the standard matrix asks for.
(`MYDAW_IPC_BENCH=stress`, single run, release.)

| scenario | p50 | p95 | p99 | max | achieved Hz | MB/s | received | long tasks |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `channel-json/64KB@480Hz` | 1.99 | 3.43 | 8.58 | 13.19 | 480.2 | 30.0 | 2400/2400 | 0 |
| `channel-json/256KB@240Hz` | 3.71 | 5.11 | 12.91 | 16.94 | 240.0 | 60.0 | 1200/1200 | 0 |
| `channel-json/1MB@30Hz` | 11.78 | 12.88 | 13.84 | 15.11 | 30.0 | 30.0 | 150/150 | 0 |
| `channel-json/1MB@60Hz` | 11.37 | 12.77 | 15.98 | 17.07 | 60.0 | 60.0 | 300/300 | 0 |
| `channel-binary/64KB@480Hz` | 1.97 | 2.88 | 6.08 | 16.45 | 480.1 | 30.0 | 2400/2400 | 0 |
| `channel-binary/256KB@240Hz` | 3.56 | 4.52 | 12.00 | 16.89 | 240.2 | 60.1 | 1200/1200 | 0 |
| `channel-binary/1MB@30Hz` | 11.74 | 13.01 | 16.67 | 22.04 | 30.0 | 30.0 | 150/150 | 0 |
| `channel-binary/1MB@60Hz` | 11.28 | 12.48 | 15.91 | 19.81 | 60.0 | 60.0 | 300/300 | 0 |

At 60 MB/s sustained for five seconds — 300 MB pushed through the boundary — the p50 is unchanged
from the same payload at a sixth of the rate, nothing is dropped, and the main thread never blocks
for 50 ms. The queue is not building: latency stays flat instead of climbing, which is the signature
of a transport that has headroom rather than one that is about to fail.

**The cliff was not reached.** What is visible is the tail widening (p99 rising to ~2–4× p50 above
30 MB/s) while the median holds — the first symptom, still a long way from saturation.

## Results — debug profile, for reference

One run, `cargo build` without `--release`. Same machine, same webview.

| payload | JSON p50 (debug → release) | binary p50 (debug → release) |
| --- | --- | --- |
| 64 B @ 60 Hz | 0.36 → 0.24 | 0.38 → 0.26 |
| 4 KB @ 60 Hz | 0.53 → 0.30 | 2.07 → 1.68 |
| 64 KB @ 60 Hz | 3.61 → 2.41 | 3.35 → 2.33 |
| 1 MB @ 10 Hz | 25.87 → 11.82 | 20.99 → 11.86 |
| meter path | 0.47 → 0.29 | — |

A debug shell costs roughly 1.5× at meter sizes and 2× at a megabyte. Worth knowing when reading
numbers taken during `cargo tauri dev`: **they are not the shipping numbers**, and only the 1 MB row
gets anywhere near the 20–30 ms threshold — in the profile nobody ships.

## The real meter path, end to end

Measured through the app's actual `subscribe_meters` stream, with the pump stamping the frame it is
about to send (audio thread → `rtrb` queue → `MeterThrottle` → `MeterPump` → `Channel` → JS
callback). Four runs:

| run | frames | min | p50 | p95 | p99 | max | achieved Hz |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 223116 | 109 | 0.13 | 0.27 | 0.64 | 0.88 | 1.09 | 21.84 |
| 223326 | 110 | 0.18 | 0.29 | 0.76 | 1.15 | 1.30 | 21.83 |
| 223535 | 110 | 0.22 | 0.32 | 0.48 | 0.71 | 0.73 | 21.86 |
| 223829 | 109 | 0.20 | 0.31 | 0.72 | 1.11 | 1.30 | 21.86 |

**Current real meter-path IPC latency: 0.30 ms typical, 1.1 ms p99, 1.3 ms worst observed.**
Against a 20–30 ms threshold that is a margin of roughly 20–100×.

### The one real finding: 21.9 Hz, not 30 Hz

The pump was supposed to deliver 30 frames a second and delivers 21.85. This is not the IPC's doing —
it is quantisation between two independent timers in our own code:

- `MeterThrottle` enforces a minimum spacing of 33.3 ms between emissions (`DEFAULT_METER_HZ = 30`).
- `MeterPump` only *looks* every `POLL_INTERVAL = 15 ms`.

A poll at 30 ms is refused (33.3 ms has not elapsed), so the frame goes out at 45 ms: the emission
grid is 15 ms × 3 = 45 ms → 22.2 Hz, and the measurement says 21.85 Hz. The 15 ms comment in
`meters.rs` reasons about added jitter but not about this beat.

So the user-visible age of a meter reading decomposes as:

| Contribution | Cost |
| --- | --- |
| audio block period (WASAPI shared, 44.1 kHz) | ~10 ms |
| waiting for the next emission on the 45 ms grid | 0–45 ms |
| **IPC hop (this spike's subject)** | **0.3 ms** |
| waiting for the next `requestAnimationFrame` | 0–16.7 ms |

The transport is under **1 %** of it. The cheap win — Phase 2, not this spike, because it changes
runtime behaviour and this spike promised not to — is to make `POLL_INTERVAL` a divisor of the
throttle interval (5 ms → 35 ms grid → 28.6 Hz) or to have the throttle wake the pump instead of
being polled. It costs nothing on the IPC side: 60 Hz of 64 B frames measured 0.24 ms.

## Verdict

### Against the research threshold

| Question | Answer |
| --- | --- |
| Sustained meter latency > 20–30 ms? | **No.** 0.30 ms p50 / 1.1 ms p99 on the live path — 20–100× under. |
| Meters viable over the webview? | **Yes**, with room for 60 Hz or 240 Hz if a use case ever wants it (240 Hz of meter-size frames: 0.17 ms p50, zero drops). |
| Playhead viable? | **Yes**, and it is a smaller problem than meters: a position update is a 64 B frame, and the UI already interpolates locally between updates (`ui/src/lib/playhead.ts`). Even a 240 Hz position stream costs 0.01 MB/s and measured p99 0.54 ms. |
| Where does it fall over? | Not found. 60 MB/s sustained, 480 frames/s, 1 MB frames — all clean. The tail widens above ~30 MB/s (p99 rising to 2–4× p50) which is where to look first if a future feature needs more. |
| Channel JSON vs binary? | See below — **no meaningful difference at equal bytes**, and JSON is *faster* in the 1–8 KB band because of Tauri's threshold asymmetry. |

### JSON vs binary, honestly

At **equal wire bytes**, which is what this harness holds constant:

| payload | JSON p50 | binary p50 | why |
| --- | --- | --- | --- |
| 64 B | 0.24 | 0.28 | both take the `eval` path; JSON's literal is cheaper to produce than a `Uint8Array([...])` literal |
| 4 KB | 0.30 | 1.68 | JSON still evals (< 8192 B); binary already fetches (> 1024 B) — **a Tauri threshold, not an encoding cost** |
| 64 KB | 2.41 | 2.33 | both fetch; identical within noise |
| 1 MB | 11.82 | 11.86 | both fetch; identical |

The conclusion is *not* "binary is pointless". It is that **binary's advantage is fewer bytes, not
faster bytes.** 4096 min/max `i16` pairs are 16 KB as a binary blob and roughly 50 KB as a JSON array
of numbers; the binary version wins by moving a third of the data and by landing as an `ArrayBuffer`
the canvas code can view directly, with no parse and no per-sample JS number boxing. Nothing in
these tables contradicts that — they simply show that once the bytes are equal, Tauri's two channel
encodings cost the same.

Practical rule for MYDAW: **JSON for structured, small, human-shaped messages (meters, transport,
snapshots); binary for arrays of numbers (waveforms, spectra, automation curves)** — and if a binary
message would land between 1 KB and 8 KB, either accept the fetch path or batch it up to a size
where the fetch path is amortised anyway.

### Waveform transfer

Extrapolating from the 64 KB and 1 MB rows:

| Strategy | Size | Measured/implied cost |
| --- | --- | --- |
| One mipmap tile, 4096 min/max `i16` pairs, binary | 16 KB | ≈1–2 ms (between the 4 KB and 64 KB rows) |
| A screenful of tiles during a scroll (20 × 64 KB) | 1.3 MB | ~25 ms of transfer at the measured 60 MB/s, spread over 20 messages of ~2.4 ms each |
| Whole-arrangement overview, one blob | 1 MB | **11.8 ms p50, 17.4 ms max**, zero drops |
| Pre-rendered PNG strip, 1920×120 | ~30–80 KB | ~2.5 ms transfer; decode via `createImageBitmap` (off the main thread) |

All four are affordable. The recommendation is **binary min/max mipmap tiles, not pre-rendered
images**, for the zoomable arrangement: the transfer cost is the same order, but tiles let the
webview re-render at any zoom without another round trip, which is where the real latency of a
"pre-rendered image" approach would show up (a round trip *per zoom step*, not per tile). Keep
pre-rendered images for a fixed overview strip if one is wanted, where the image never has to change
scale.

Tile at 64 KB or below: that keeps every message under ~2.5 ms, well inside a 16.7 ms frame budget,
so streaming tiles during a scroll cannot drop a display frame.

### Go / no-go

**Go. The webview UI stays.** The boundary is not the constraint at any data rate this DAW plausibly
needs; the meter path is 1 % of the meter's own end-to-end latency, and the tested envelope has more
than an order of magnitude of headroom over the plan.

No mitigation is required today. For the record, the ones to reach for *if* a future feature ever
strains this, in order:

1. **Fewer bytes** — binary for numeric arrays (the 3× saving described above). Cheapest win, no
   architectural change.
2. **Batching** — one 64 KB message costs 2.4 ms; sixteen 4 KB messages cost 4.8 ms in JSON and
   27 ms in binary. Coalescing beats per-item sends by a wide margin, especially for binary payloads
   caught above the 1 KB eval threshold.
3. **Decimation** — send what the display can show. Nothing above 60 Hz has a consumer today.
4. Only then anything exotic (a localhost socket, shared memory, a custom protocol handler).

## Deviations from the brief

- **The localhost WebSocket comparison was skipped**, as the brief permitted. The core question —
  Channel JSON vs binary — is answered above, and the Channel path came in at 0.3 ms for meter frames
  with zero drops at 60 MB/s. A WebSocket could not plausibly change any decision here, and buying
  that non-answer would cost a tungstenite dependency plus a listening socket inside a DAW shell.
  If a future spike needs it, the harness's scenario/report types are transport-agnostic — adding a
  third `BenchTransport` variant is a small, local change.
- **1 MB was measured at 5 and 10 Hz, not 1 Hz.** A 5 s run at 1 Hz yields five samples, which cannot
  support a p95, let alone a p99. Both rates are inside the brief's 1–10 Hz band.
- **Stress scenarios were added** (64 KB @ 480 Hz, 256 KB @ 240 Hz, 1 MB @ 30/60 Hz) because the
  standard matrix answered "is it fast enough" without answering "how much headroom is there". They
  are opt-in via `MYDAW_IPC_BENCH=stress` and not part of the default plan.
- **A debug-profile run was added** so nobody reads a `cargo tauri dev` number as a shipping number.

## Gates

| Gate | Result |
| --- | --- |
| `cargo build --workspace` | clean |
| `cargo test --workspace` | **372 passed**, 0 failed, 4 ignored (341 before this spike; 31 new: percentiles, epoch-sync math, config parsing/validation, exact frame sizes, the paced sender, report aggregation and file layout, panel gating, meter stamping, DTO wire shape) |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo fmt --all --check` | clean |
| `pnpm --dir ui build` (`tsc --noEmit` + vite) | clean; bench code lands in its own lazily-loaded chunks (`ipcBench`, `IpcBenchPanel`) |
| Benchmark matrix | 3 full runs + 1 stress run + 1 debug-profile run on this machine (WebView2 151.0.4129.86) |
