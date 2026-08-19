# Step 4 — The step sequencer: the first thing you program rather than play

Date: 2026-08-19
Status: Done. Verified on hardware (Focusrite USB Audio, 44.1 kHz).

Step 3 gave MYDAW instruments and a computer-keyboard piano. That makes sounds, but a DAW is not
a synthesiser: it is a machine for laying sounds out **in time**. Step 4 is that machine's smallest
honest version — an FL-style 16-step grid, a tempo, and a scheduler that turns the two into note
events at exact sample positions, looping forever while the transport runs.

The audible claim, measured through the device rather than asserted: a four-on-the-floor kick with
a snare on 2 and 4 and eighth-note hats at 130 BPM produces onsets whose **worst deviation from the
tempo grid is 0.052 ms (2.3 samples) for the kick and 0.010 ms (0.5 samples) for the off-beat
hats**, over two bars, at 44.1 kHz, through a real WASAPI stream.

## What got built

| Where | What |
| --- | --- |
| `crates/rt-core/src/transport.rs` | Atomic BPM on `TransportClock` (f64 bits in an `AtomicU64`), `DEFAULT_BPM`/`MIN_BPM`/`MAX_BPM`, `clamp_bpm`, and `bpm` on `TransportSnapshot`. |
| `crates/rt-core/src/command.rs` | `RtCommand::SetBpm(f64)`, applied by `drain_commands`. |
| `plugins/sequencer/src/pattern.rs` | `Pattern` / `PatternRow` / `Step`, the bounds, `PatternError`, `row_label`, `compile`. |
| `plugins/sequencer/src/compiled.rs` | `CompiledPattern` (fixed arrays) and the `basedrop` + `rtrb` pattern channel. |
| `plugins/sequencer/src/schedule.rs` | `Scheduler`, `TransportWindow`, `step_len_samples`, `step_boundary`. The timing. |
| `plugins/sequencer/src/lib.rs` | `SequencerPlugin`, `SequencerService` (the non-RT pattern + tempo), the `step-sequencer` panel contribution. |
| `plugins/debug-tap/src/onset.rs` | `detect_onsets`, `Band`, `grid_report`, `align_grid` — how "on the grid" gets measured. |
| `plugins/audio-io/src/rt.rs` | `PatternSlot` + `Scheduler` in `RtState`; the scheduling step inside `run_block`. |
| `plugins/audio-io/src/service.rs` | The pattern channel per stream, `install_pattern`, `set_bpm`, `bpm`. |
| `app/src/dto.rs` | `PatternDto` / `PatternRowDto` / `StepDto` / `SequencerStateDto`, and the validating `TryFrom`. |
| `app/src/state.rs` | `SequencerService` in `AppState`; `set_pattern`, `set_bpm`, `sequencer_state`, `publish_sequencer`. |
| `app/src/commands.rs` | `get_pattern`, `set_pattern`, `set_bpm`. |
| `ui/src/lib/pattern.ts` | `toggleStep`, `withToggledStep`, `currentStep` — the interaction model and the highlight maths. |
| `ui/src/panels/sequencer/SequencerPanel.tsx` | The grid. |
| `ui/src/shell/TransportBar.tsx` | The tempo control (type or drag), replacing the placeholder readout. |
| `plugins/audio-io/tests/sequencer_engine.rs` | Offline end-to-end: pattern → callback → drum kit → onset analysis. Runs in CI. |
| `app/tests/sequencer_audio.rs` | The hardware capture. `#[ignore]`d. |

597 tests pass, up from 503, plus 10 `#[ignore]`d hardware tests.

## Part 1 — the timing

### The formula

```text
step_len = sample_rate * 60 / bpm / 4          (f64, fractional)
boundary(n) = round(n * step_len)              (u64, absolute step index n)
pattern step = n mod 16
```

16 steps is one bar of 4/4 in sixteenth notes. At 130 BPM and 48 kHz a step is 5538.4615 samples;
at 44.1 kHz it is 5088.4615.

### Why there is no drift

Nothing anywhere adds `step_len` to a running total. Every boundary is computed from the
**absolute** step index, so there is no accumulated error to grow. The rounding is to the nearest
sample and it is bounded by half a sample **forever**, not per step.

The alternative — `next += step_len` — is the classic sequencer bug: at 174.3 BPM the f64 rounding
per step is tiny, but a *sample-quantised* accumulator (which is what you get the moment you store
the position as an integer) is off by 4 samples after ten minutes, and by a musically obvious amount
after an hour. `ten_minutes_of_playback_does_not_drift_by_a_single_sample` asserts exact equality
with `round(n * step_len)` for every bar across ten simulated minutes, and that the final onset is
within half a sample of the ideal position.

The same property makes **looping free**. The pattern position is `n mod 16`, so a bar wrap is a
modulo, not an event: nothing is reset at the loop point, no phase is carried, and a note that
started before the wrap needs no special case. The scheduler has no idea a bar boundary exists.

A **seek** needs no cooperation either — the window simply starts somewhere else, and
`boundary(n)` still answers correctly. The only thing a seek disturbs is the note-offs already owed,
which is the next section.

### Rounding, stated precisely

`boundary(n) = round(n * step_len)` means a step boundary can land at most half a sample from where
an infinitely precise engine would put it. Two boundaries an odd distance apart therefore differ by
`ceil` or `floor` of `step_len` — the grid is *not* uniformly spaced in integers, and that is
correct: uniform integer spacing is exactly what drifts.

### The pending note-off state machine

A step's note-on fires at its boundary; its note-off is due half a step later (`GATE_FRACTION =
0.5`). Usually that is inside the same block. At a small block size, a slow tempo, or when the gate
closes after the block ends, it is not:

```text
   block k        |----------|            on(step n) at +12
                       off due at +900 -> parked
   block k+1      |----------|            off emitted at +388
```

So `Scheduler` owns a fixed `[PendingOff; 32]` array (`MAX_ROWS * 2`) and:

- emits every owed note-off that comes due inside the current window, at its real frame offset;
- parks the rest;
- **flushes all of them at frame 0** when the transport stops, or when the block does not start
  where the previous one ended (a seek).

Drums ignore note-offs — a one-shot is a one-shot — but the poly synth does not, and a sequencer
that leaks note-ons into a synth ends the session with sixteen stuck voices. That is why this is a
state machine and not a hope. `stopping_releases_every_note_the_scheduler_still_owes` and
`seeking_releases_owed_notes_and_resumes_on_the_grid_at_the_new_position` are the tests.

Edge detection is on the *window*, not on a callback: `playing` and the expected next sample are
both scheduler state, so stop, seek and resume all fall out of comparing the incoming window with
what was expected. While stopped the clock does not advance, so `next_sample` is pinned to the
window's start and a paused transport does not read as a seek every block.

### Merge ordering at the same frame offset

`NoteEventBuffer` is a stable ordered insert, so **push order is the tie-break** at any one offset.
The engine pushes:

1. **live input** (the keyboard piano) — always at offset 0;
2. **the sequencer** — note-offs before note-ons.

Live input first, because a key the user is pressing right now should not be pre-empted by the grid.
Offs before ons, because that turns a step retriggering a still-sounding key into a retrigger rather
than a stuck note. Both are asserted:
`live_notes_and_pattern_events_share_one_buffer_with_live_input_first`.

### Where the scheduler runs

Inside `run_block`, **before** the graph and after the live-note drain:

```text
  poll graph swap -> poll pattern swap -> drain commands -> snapshot transport
     -> drain live notes (offset 0) -> schedule(pattern, window) -> graph.process
```

It is not an `RtNode`. A node would run *inside* the graph, after `ProcessCtx` has been built with
the block's event slice — too late to add to it. The sequencer is a *source of events*, and the
event buffer is filled before the chain runs, which is also why gating pattern playback on the
transport lives here rather than in `TransportGate` (an instrument must still sound with the
transport stopped; see the step-3 spec).

One subtlety worth writing down: the scheduler's window uses `min(requested, max_block)` frames, not
what the device asked for. A device asking for more than `max_block` gets a truncated block and the
transport advances by what was produced; if the scheduler used the untruncated count it would
schedule steps into samples nobody ever hears, and the grid would develop holes.
`a_truncated_block_does_not_leave_a_gap_in_the_grid` covers it.

## Part 2 — the pattern

### Two shapes

`Pattern` is what the user edits: a `Vec<PatternRow>`, each with `[Step; 16]`. It never crosses the
RT wall. `CompiledPattern` is the same grid with every `Vec` replaced by a fixed array
(`[CompiledRow; 16]`, ~1.4 kB), which the audio thread may read, index and drop without touching
the allocator.

Publication is `GraphHandle`'s mechanism one level down: `rtrb` queue of `basedrop::Owned`, the
superseded pattern dropped on the audio thread (which under `basedrop` means *queued for the
collector*). An `ArcSwap` would also work here — unlike a graph, a pattern is read-only on the audio
thread — but one publication story with one set of tests is worth more than one saved queue.

### Bounds, and where they are enforced

| Bound | Value | Enforced by |
| --- | --- | --- |
| Steps per pattern | exactly 16 | `Pattern::validate` |
| Rows | ≤ 16 | `Pattern::validate` (the compiled array's size) |
| Key | 0..=127 | `Pattern::validate` |
| Channel | 0..=15 | `Pattern::validate` |
| Velocity | finite, 0.0..=1.0 | `Pattern::validate` |
| Row length | exactly 16 | `TryFrom<&PatternDto>` |
| Tempo | 20..=999 | `AppState::set_bpm` rejects; `TransportClock::set_bpm` clamps |

Everything reachable from a webview is checked at the boundary and reported as a sentence naming the
row that did it — never truncated, never padded. The tempo is checked twice on purpose: the command
*rejects* an out-of-range value (a user who typed 5000 should be told, not silently given 999), and
the clock *clamps* as a last line of defence, because a tempo of 0 would divide the step length by
zero on the audio thread.

### Where the pattern lives when there is no audio device

In `SequencerService`, provided by the sequencer plugin. A `TransportClock` is built around a device
sample rate and so starts fresh with every stream; the pattern and tempo are **project data** and
outlive streams. Consequences:

- Editing the grid or the tempo with no sound card **succeeds** — `AudioIoError::NotRunning` is
  mapped to `Ok`. A headless machine can still program a beat, and one day load one from a file.
- `start_audio_with_source` re-publishes both into every new stream (`publish_sequencer`), so a
  device change or a headless capture does not silently reset the beat.

## Part 3 — the IPC surface

| Command | Shape |
| --- | --- |
| `get_pattern` | → `{ bpm, sampleRate, pattern }` |
| `set_pattern(pattern)` | → the same, after validation |
| `set_bpm(bpm)` | → the stored tempo |

**Deviation from the brief, deliberate:** the tempo and pattern ride in the sequencer's own state
message rather than in `get_ui_snapshot`. `UiSnapshot` lives in `ipc-bridge`, which is deliberately
Tauri-free and webview-agnostic and must not learn about patterns; it is also taken once at boot,
from a kernel that has no audio device yet. Putting the two values a sequencer needs into one
message that arrives together is the same information with less coupling.

`sampleRate` is in that message for a small but visible reason: the panel receives the transport
position in *frames* and has to turn it into a step number. Assuming 48 kHz would put the moving
highlight in the wrong step on a 44.1 kHz interface.

The pattern is replaced whole rather than diffed. A 4×16 grid is about a kilobyte of JSON, spike 4
measured that as free, and one message per edit has no ordering problems, no missed updates and no
diff to get wrong.

## Part 4 — the UI

The panel is contributed by the sequencer plugin under the component key `step-sequencer`, so it
appears in the dock between the meters and the CLAP browser without the shell knowing it exists.

- **The grid**: 4 rows × 16 DOM buttons, columns grouped in fours by a wider gutter (FL-style beat
  groups). `--dock-width` went from 300 px to 380 px, which is what the sixteen columns plus a row
  label need to stay comfortably clickable; the columns themselves are `1fr`, so the grid fills
  whatever it is given.
- **Editing**: click toggles at velocity 0.78; shift-click places or *promotes to* an accent at 1.0,
  and only turns an existing accent off. Promotion rather than plain toggling because the
  alternative — two clicks to accent a step, one of which briefly deletes it — is the thing people
  complain about in step sequencers that get this wrong.
- **`<Index>`, not `<For>`**: an edit produces a new row and step object (the pattern is immutable so
  the push can be a plain send), and `<For>` keys by reference — it tore down and rebuilt sixteen
  buttons per click, which lost the first click of a fast drag across a row. Found by driving the
  real DOM in a browser, not by reading the code.
- **The moving highlight** is the one thing that moves on its own, so it is driven by
  `requestAnimationFrame` off `Playhead`'s locally interpolated position, with the same
  `floor(position / step_len) mod 16` the scheduler uses. Only the derived step index becomes a
  signal, so the grid re-renders eight times a second at 130 BPM rather than sixty.
- **Edits are debounced 50 ms** before being pushed. The local grid updates immediately, so this is
  never felt; a drag across four cells becomes one message. The backend's reply is deliberately *not*
  adopted on success — a reply landing mid-drag would overwrite newer edits, and it never carries
  anything new. On failure the whole state is re-fetched, which is the one case where the two can
  really disagree.
- **The tempo control** is a number input that also drags vertically (1 BPM per 4 px), because both
  are how people set a tempo: typing 174 is exact, dragging is how you find out that 128 was better.

The mock backend keeps an in-memory pattern and validates it the way Rust does, so the panel's error
path can be exercised in a plain browser.

## Testing

**The scheduler, as a pure function** (`plugins/sequencer`, 49 unit tests + 5 alloc tests):

- Step boundaries exact at 60 / 130 / 174.3 / 999 BPM × 32 / 256 / 441 / 4096 frames × 44.1 / 48 kHz
  — 32 combinations, each asserted against `round(n * step_len)` for a whole bar.
- No drift over ten simulated minutes at 174.3 BPM with 441-frame blocks.
- Note-off crossing a block boundary; note-off parked across a bar wrap; stop flushes; seek flushes
  and re-grids; velocity and channel pass through; 16-row maximum; empty pattern emits nothing;
  nonsense tempo/rate emits nothing rather than looping.
- Zero-allocation across every tempo/rate/block combination, including the overflow branch and the
  `basedrop` pattern swap (counting global allocator, per-thread armed).

**The engine** (`plugins/audio-io`): pattern events reach the graph at the exact sample the grid
says, across two bars; a truncated block leaves no gap; a pattern edited mid-play is heard from the
next block; live and pattern events merge in the documented order; overflow is counted, not grown.

**Offline end-to-end** (`plugins/audio-io/tests/sequencer_engine.rs`, runs in CI): the demo beat
rendered through the real callback into the real drum kit, then measured with the onset detector.
Kick worst error **0.047 ms**, off-beat hats **0.009 ms**. An empty pattern renders digital silence.
The second bar starts at exactly `boundary(16)`.

## On hardware

`app/tests/sequencer_audio.rs`, `#[ignore]`d, run on this machine:

```sh
cargo test -p mydaw-app --test sequencer_audio -- --ignored --nocapture --test-threads=1
```

**Device: Speakers (Focusrite USB Audio) @ 44100 Hz, 2 ch, max block 4096.**
**Tempo 130 BPM — step 5088.462 samples (115.385 ms), bar 1.8462 s.**
Capture: 162 831 frames (3.692 s), peak -3.53 dBFS, RMS 0.0527, **0 dropped samples**, no clipping,
no non-finite samples.

| Measurement | Onsets | Worst error | Mean error | Contiguous |
| --- | --- | --- | --- | --- |
| Kick (lowpass 120 Hz, beat grid) | 8 / 8 | **0.052 ms** (2.3 samples) | 0.044 ms | yes |
| Hats (highpass 4 kHz, eighth grid) | 16 / 16 | 0.123 ms (5.4 samples) | 0.057 ms | yes |
| Off-beat hats (single source, beat grid) | 8 / 8 | **0.010 ms** (0.5 samples) | 0.006 ms | yes |

Per-kick deviations, in milliseconds:

```
-0.043  +0.051  -0.036  +0.035  -0.052  +0.042  -0.045  +0.049
```

The alternating sign is the rounding, visible: a beat is 20 353.85 samples, so consecutive beats
round alternately down and up. Half a sample is 0.011 ms at 44.1 kHz, and 0.052 ms is 2.3 samples —
the *detector's* resolution, not the engine's.

Kick spectral dominance: the capture's loudest partial is **88.8 Hz**, and the five loudest peaks
are 89 / 50 / 78 / 82 / 85 Hz — all below 100 Hz, with a spectral flatness of 0.0149. The kick is
what the low band contains, which is what makes counting onsets there the same as counting kicks.

Artefacts (WAV, annotated PNG, `stats.json`, and a `grid.json` carrying every per-onset deviation)
under `.debug/captures/20260819-005058/`. The waveform shows eight evenly spaced kick transients
0.46 s apart with hats between them and taller peaks on 2 and 4.

Silence is asserted at both ends: a programmed pattern is digitally silent before Play, and silent
again 600 ms after Stop.

### The onset detector

`plugins/debug-tap/src/onset.rs` is the new measurement, and it is worth being explicit about what
it is and is not. It is a band filter, a boxcar envelope, a rise test and a refinement to the
sample. It is **not** a general-purpose polyphonic onset detector — no spectral flux, no adaptive
threshold, no source separation.

Two design points carry the accuracy:

- **Band-splitting.** A drum mix is unmeasurable as a whole; lowpassed at 120 Hz it contains the
  kick and nothing else, so counting onsets *is* counting kicks. Highpassed at 4 kHz it contains the
  hats.
- **Consistency over accuracy.** The refinement reports where a transient reaches a fixed fraction
  of its own level, which is slightly late — but by the *same* amount for the same attack. Since
  `align_grid` fits the grid's origin, a constant latency cancels and lands in the origin instead of
  in the errors. It is also why the sharp assertions are made on the off-beat hats: steps 4 and 12
  carry a snare *and* a hat, which is a different attack, so the two have different latencies. That
  0.123 ms of "hat" error is the measurement, not the engine.

## Limitations

Written down rather than hidden, in rough order of how soon they will hurt:

1. **One pattern.** No pattern list, no chaining, no arrangement. The bar loops forever.
2. **16 steps, fixed.** `Pattern::steps` exists and is validated so that 32 and 64 are a small
   change, but nothing offers them yet.
3. **No per-row instrument routing.** Every row's note goes to the one instrument at the head of the
   chain. A row addresses a *key*; whether anything is listening on it is the instrument's business.
   This is why the grid is useful with the drum kit and merely interesting with the poly synth.
4. **Note length is fixed at half a step.** Per-step length is a piano-roll feature, not a step-grid
   one.
5. **No swing, no shuffle, no per-row offsets.** The grid is exactly the grid.
6. **No pattern length in bars, no time signature.** 4/4 is assumed; the readout says so.
7. **Tempo changes re-grid from absolute zero.** Changing tempo mid-bar moves the boundaries,
   because they are always `round(n * step_len)` for the current step length. That is deliberate —
   carrying a phase across the change is what a tempo *ramp* needs, and there is no automation to
   ramp with yet — but it means changing tempo while playing can shift the next hit.
8. **Nothing is saved.** No project file, so the beat dies with the process.
9. **Velocity is the only per-step parameter.** No per-step probability, ratchet or micro-timing.
10. **The pattern is not visible in the arrangement view**, which is still a placeholder.
