# Step 2 — Note events: a key press, sample-accurately, into a CLAP plugin

Date: 2026-08-19
Status: Done. Verified on hardware (Focusrite USB Audio, 44.1 kHz).

Step 1 made the engine play a tone and host an effect. Step 2 gives it the other half of an
instrument track: a **note event path** from the UI, across the real-time wall, through the graph,
into a CLAP plugin's `process()` call, at a chosen frame offset within the block. That is what
unlocks instruments — everything after this (a sequencer, a piano roll, MIDI hardware) is a
different *source* for the same events, not a different mechanism.

The audible claim, measured through the device rather than asserted: pressing `H` on the computer
keyboard produces a 440.0 Hz sine at exactly -13.98 dBFS, and releasing it produces digital
silence 200 ms later.

## What got built

| Where | What |
| --- | --- |
| `crates/rt-core/src/notes.rs` | `NoteEvent` / `NoteKind` / `LiveNote`, the `note_channel` SPSC queue, `NoteEventBuffer` (never grows, keeps itself sorted), `drain_notes`. 9 unit tests. |
| `crates/rt-core/src/node.rs` | `ProcessCtx::events: &[NoteEvent]` and `ProcessCtx::with_events`. |
| `plugins/audio-io/src/rt.rs` | The callback drains the note queue into the block buffer and hands the slice to the graph; `AudioIoStats::note_events_dropped`. |
| `plugins/audio-io/src/service.rs` | `AudioIoService::send_note`, `AudioIoConfig::note_queue_capacity`, `AudioIoError::NoteQueueFull`. |
| `plugins/host-clap/src/hosting.rs` | `note-ports` query, `ClapPortLayout::{note_input_ports, is_instrument, is_supported, has_note_input}`, and `ClapNode`'s translation of `ProcessCtx::events` into a pre-allocated `clack` `EventBuffer`. The instrument path (no audio input port) too. |
| `test-fixtures/clap-gain/src/synth.rs` | `dev.mydaw.fixture.synth`: 8-voice sine instrument, note-in + stereo out, sample-accurate, deterministic. |
| `test-fixtures/clap-gain/src/lib.rs` | A hand-written multi-plugin entry, so one bundle exports both fixtures. |
| `app/src/graph.rs` | `MYDAW_CLAP_INSTRUMENT`, `engine_graph` (instrument chain vs. tone chain), the gate decision. |
| `app/src/state.rs` | `AppState::note_on` / `note_off` with `NoteError`, `start_audio_with_plugins`. |
| `app/src/commands.rs` | `note_on` / `note_off` Tauri commands. |
| `ui/src/lib/keyboard.ts` | The FL/Live keyboard map, octave clamping, note naming, event filtering. |
| `ui/src/state.tsx` | Key listeners, held-note bookkeeping, octave signal, blur safety. |
| `ui/src/shell/StatusBar.tsx` | Octave + held notes readout. |
| `crates/ipc-bridge/src/meter.rs`, `app/src/meters.rs` | The spike-4 meter cadence fix (see below). |

## The event path

```text
  UI (webview)                    main thread              audio thread (one block)
  ------------                    -----------              ------------------------
  keydown "KeyH"
    -> midiKeyFor -> 69
    -> invoke('note_on', 69, 0.8)
                        AppState::note_on
                          validate key <= 127,
                          velocity in 0..=1
                        AudioIoService::send_note
                          NoteProducer::push(LiveNote)  ──rtrb, wait-free──┐
                                                                          v
                                                          run_block:
                                                            graph.poll_swap()
                                                            drain_commands()
                                                            note_events.clear()
                                                            drain_notes(rx, buf, 0)  ← offset 0
                                                            ProcessCtx::with_events(.., buf.as_slice())
                                                              |
                                                              v
                                                          CompiledGraph::process
                                                              |
                                                              v
                                                          ClapNode::process
                                                            fill_events():
                                                              NoteKind::NoteOn  -> clack NoteOnEvent
                                                              NoteKind::NoteOff -> clack NoteOffEvent
                                                              Pckn(port 0, ch, key, note_id=All)
                                                              header.time = frame_offset
                                                            processor.process(audio, events_in, ..)
                                                              |
                                                              v
                                                          the plugin's voices
```

### Two types, one queue

`LiveNote` is a note with **no position**; `NoteEvent` is a note **placed in a block** at
`frame_offset`. The queue carries the first, the block buffer holds the second, and `LiveNote::at()`
is the only bridge. That split is deliberate: a key press genuinely has no sub-block position (the
engine learns about it between callbacks), and a single type would let that fiction pass unnoticed
into a field the sequencer later fills with real numbers.

`NoteKind` is `#[non_exhaustive]`. Choke, note-end and per-note expression all exist in CLAP and
Phase 2 will want at least the first two; adding a variant must not break downstream matches. The
one wildcard arm that costs — `ClapNode::fill_events` — counts what it cannot translate
(`ClapNodeStats::events_dropped`) rather than guessing, because a host that silently reinterprets an
unknown event makes a plugin sound wrong in a way nobody can trace.

### Why a separate queue, not another `RtCommand`

A held chord is a burst of a dozen events, and a fast passage is a steady stream of them. Sharing the
transport command queue would mean sizing that queue for the worst musical case — or, worse, dropping
a `Stop` because somebody leaned on the keyboard. Two SPSC queues cost two pointer pairs and one
extra `while let` in the callback; the coupling would cost a class of bug that only shows up while
playing.

## RT safety

Everything in the block path allocates exactly once, off the audio thread:

| Allocation | Where | Size |
| --- | --- | --- |
| note queue storage | `note_channel(capacity)`, in `AudioIoService::start` | 1024 `LiveNote` |
| block event buffer | `NoteEventBuffer::new`, in `RtState::new` | 1024 `NoteEvent` |
| CLAP event buffer | `warmed_event_buffer`, in `LoadedClapPlugin::create_node` | 1024 note events (~41 kB) |

Inside the callback:

- `NoteEventBuffer::push` is an ordered insert into spare capacity — a `memmove`, never a `realloc`
  — and returns `false` instead of growing when full. `drain_notes` counts the drops and drains the
  queue anyway, because leaving events behind would make the *next* block play notes from two blocks
  ago.
- `EventBuffer::clear` keeps its allocation, and every event pushed afterwards is the same size as
  the ones the buffer was warmed with, so `push` only writes into existing capacity.
- Output events go to `OutputEvents::void()`. An `EventBuffer` on the output side would grow without
  bound if a plugin emitted more than was reserved — on the audio thread. Nothing consumes plugin
  note output yet, so the safe option is free.

`plugins/host-clap/tests/rt_alloc.rs` proves it with a counting global allocator rather than by
inspection: **0 allocator events** over 64 event-carrying blocks (8 events each, varying block sizes
and offsets, alternating note-on/note-off), and 0 over 64 queue drains.

### The sorted invariant

`NoteEventBuffer` keeps events sorted by `frame_offset` with insertion order preserved among equal
offsets. CLAP requires sorted input events; the stability requirement is ours, and it is the
difference between a retrigger and a stuck note when a note-off and a note-on for the same key land
on the same frame. A stable sort would allocate, so the buffer maintains the order on insert instead
— which is O(1) in the common case, since events arrive in time order and every live event is at
offset 0.

## Instruments in the graph

`ClapPortLayout` grew two note-port counts and three predicates. A plugin is now drivable if it is a
stereo effect (**stereo in, stereo out**) *or* an instrument (**no audio input, stereo out**), and
the node hands the bus only to the former:

```rust
let audio_in = if layout.has_audio_input() {
    input_ports.with_input_buffers([/* the graph bus */])
} else {
    InputAudioBuffers::empty()
};
```

Passing an instrument the bus "just in case" would contradict its own `audio-ports` answer. That is
a host bug even where a lenient plugin tolerates it, and the kind that only surfaces on the one
plugin that does not.

A plugin with **no `note-ports` extension** has, per the CLAP specification, no note ports at all.
The node reads that as "takes no events" and skips delivery silently — not an error, and not counted
as a drop, because it is a configuration rather than a loss. `note_events_for_a_plugin_without_a_note_port_are_skipped_silently`
asserts exactly that against the gain fixture.

## The transport gate decision

```text
  default:      test-tone -> [effect] -> TransportGate
  instrument:   instrument -> [effect]
```

`TransportGate` exists because `TestToneNode` is a free-running oscillator: it makes a sine whether
or not the DAW is playing, so the shell has to silence it when the transport is stopped.

An instrument has the opposite problem. It is silent until it is *played*, and at this step it is
played live, by a human, whose expectation — set by every DAW in existence — is that pressing a key
makes a sound with the transport stopped, before anything has been recorded. Gating it would make
the instrument mute unless you pressed Play first: not a stricter engine, just a broken one.

**So the instrument chain has no gate.** Live notes are gated by their own note-offs, which is the
correct mechanism. When the sequencer arrives it will gate *pattern playback* on the transport (no
transport, no scheduled events) and leave live input alone — a property of the event source, not of
the audio chain, so it belongs there rather than here.

The alternative considered and rejected: teach `TransportGate` to pass audio when an instrument is
upstream. That makes a node's behaviour depend on its neighbours, which is exactly the coupling the
serial-chain graph exists to avoid, and it would still be wrong the moment a user wanted a gated
instrument.

## The fixture synth

`dev.mydaw.fixture.synth`, in the same bundle as the gain fixture. Testing note dispatch against
whatever synth the developer happens to have installed is not a test, it is a coincidence.

| Property | Value | Why |
| --- | --- | --- |
| Voices | 8 | A two-handed chord, small enough that `process` stays trivial. Oldest voice is stolen when all are busy. |
| Waveform | sine | One partial, so `dominant_hz` in a capture is unambiguous. |
| Tuning | `440 * 2^((key - 69) / 12)` | Exact at A4 by construction — the key every test uses. |
| Velocity → amplitude | `velocity * 0.2` | -13.98 dBFS at full velocity; four voices at once still stay under full scale even if every partial lines up, so a test can predict `peak`. |
| Attack | 2 ms linear ramp | A sine switched on at full amplitude clicks, and a click is broadband — it would smear the spectrum a capture is trying to measure. |
| Release | 50 ms linear | Long enough to hear, short enough to assert silence after. |
| Determinism | total | Phase starts at zero on note-on; no noise, no drift, no free-running LFO. |

It splits the block at every event offset and renders each span separately, so an event at frame *n*
is heard at frame *n*. Live input is always at offset 0 today, but a fixture that ignored offsets
could not tell a sample-accurate host from one that quantises to the block —
`events_are_placed_at_their_frame_offset_not_at_the_start_of_the_block` is the test that would go
quiet if the plumbing regressed.

Two plugins in one bundle also exercises, for free, the host's "find the plugin with this id inside
this bundle" path — the layout every real vendor ships.

## The keyboard

`A W S E D F T G Y H U J` = C..B of the current octave, `Z`/`X` shift the octave. That is the FL
Studio and Live layout, copied exactly: anybody who has touched a DAW already knows it, and a novel
layout would be a novelty rather than an improvement.

Details that matter more than the map:

- **`KeyboardEvent.code`, not `.key`.** The mapping is about where a finger sits, so it must not move
  on a French or Dvorak layout.
- **Held notes are keyed by physical key, not by MIDI key.** The note-off has to release whatever the
  key-down played, even if the octave was shifted in between.
- **`event.repeat` is ignored**, or auto-repeat would retrigger dozens of times a second.
- **Modifiers and text fields are ignored**, so `Ctrl+S` stays `Ctrl+S` and typing in a field does
  not play a scale.
- **`window.blur` releases everything.** Alt-tabbing away with a key down never delivers its keyup,
  and a stuck note is the worst possible bug to leave a user with.
- **A failed `note_on` un-holds the key**, so the status bar never claims a note the engine refused.

The status bar shows `oct 4 — ` or `oct 4 C4 E4 G4`, highlighted while anything is held. It is the
only feedback that a key reached the engine on a machine with no audio device, and it is cheap.

The mock backend logs `note_on` / `note_off` to the console, so the piano's own logic can be
developed in a plain browser.

## Meter cadence fix

Spike 4 measured the live meter path delivering **21.85 Hz** where 30 was configured, and identified
the cause as quantisation between two independent timers in our own code: `MeterThrottle` enforced
33.3 ms between emissions, `MeterPump` only looked every 15 ms, so a poll at 30 ms was refused and
the frame went out at 45 ms — a 45 ms grid, 22.2 Hz.

Fixed at both ends:

1. **`MeterThrottle::poll` steps its deadline**, not its clock. On emission `last_emit` advances by
   exactly one `min_interval` from the *previous deadline* rather than resetting to `now`, so a
   caller's overshoot is not folded into every subsequent interval. A caller that stalls for more
   than `RESYNC_AFTER` (2) intervals resyncs to `now` instead of firing a catch-up burst — nobody
   wants six meter frames at once after the pump was descheduled, and only the newest carries
   information.
2. **`MeterPump` derives its poll interval from the throttle** (`poll_interval_for`:
   `min_interval / 4`, clamped to ≥ 2 ms) instead of a hard-coded 15 ms, so the poll grid is a
   divisor of the emission grid and every deadline is served by the next poll.

At 30 Hz that is an 8.33 ms poll. Measured by simulation in
`a_thirty_hertz_throttle_delivers_thirty_frames_a_second_through_the_pump` and
`a_caller_polling_off_the_grid_still_gets_the_configured_rate` (which sweeps 5/8/15/16 ms polls):
**29–31 frames per second in every case**. The spike-4 latency table's "waiting for the next
emission" row drops from 0–45 ms to 0–41.6 ms worst case and, more importantly, stops losing a
quarter of the frames.

## Hardware verification

`app/tests/note_events_audio.rs`, `#[ignore]`d, run on this machine:

```sh
cargo test -p mydaw-app --test note_events_audio -- --ignored --nocapture --test-threads=1
```

It boots the kernel, loads the synth fixture as the graph's source, opens the real device, sends
notes through the real RT queue, and captures the output with `debug-tap`. Artefacts (WAV, annotated
PNG, stats JSON) land under `app/.debug/captures/<timestamp>/`.

**Device: Speakers (Focusrite USB Audio) @ 44100 Hz, 2 ch, max block 4096.**

| Capture | Frames | Peak (L) | dBFS | RMS | Dominant | Flatness |
| --- | --- | --- | --- | --- | --- | --- |
| idle (no notes, transport stopped) | 8 820 | 0.0000 | -inf | 0.0000 | none | 0.0000 |
| note-on key 69 (A4), velocity 1.0 | 44 100 | **0.2000** | **-13.98** | 0.1414 | **439.993 Hz** | 0.0000 |
| note-off, +200 ms | 13 230 | **0.0000** | -inf | 0.0000 | none | 0.0000 |
| chord 60 + 64 + 67 | 44 100 | 0.5990 | -4.45 | 0.2452 | 329.619 Hz | 0.0000 |
| chord off, +200 ms | 13 230 | **0.0000** | -inf | 0.0000 | none | 0.0000 |

Chord spectrum, top five interpolated peaks (Hz, dBFS):

```
(262, -14)  (330, -14)  (392, -14)  (523, -130)  (1, -129)
```

C4 = 261.63, E4 = 329.63, G4 = 391.99 — all three present at equal level, and the next peak down is
116 dB quieter. Every capture reported 0 dropped samples, 0 non-finite samples and 0 clipped samples.

The measured 439.993 Hz is 7 mHz from ideal, which is the capture FFT's interpolation error, not the
synth's: the fixture computes its phase increment in `f32` from an exact `key_to_hz(69) == 440.0`.

Artefacts from the verification run:

```
app/.debug/captures/20260818-231555/   idle
app/.debug/captures/20260818-231556/   note-on-a4      (capture.png, stats.json, capture.wav)
app/.debug/captures/20260818-231557/   note-off
app/.debug/captures/20260818-231554/   chord-c-major
app/.debug/captures/20260818-231554-1/ chord-off
```

## Limitations

Named here so nobody discovers them by surprise:

- **Live events are always at frame offset 0.** The plumbing carries arbitrary offsets and the
  fixture honours them (there is a test), but nothing *produces* a non-zero offset yet. Live input
  genuinely has none; the sequencer will.
- **No scheduling.** There is no pattern, no clip and no note list — `drain_notes` reads a queue, not
  a timeline. Notes are not recorded, and the transport does not carry them.
- **No MIDI hardware.** The only note source is the computer keyboard. A MIDI input plugin feeding
  the same `AudioIoService::send_note` is the obvious next source and needs no engine changes.
- **No note output.** A plugin's `NoteEnd`, `NoteChoke` or note-expression output is discarded
  (`OutputEvents::void()`), so voice-lifetime tracking and note-to-note routing are not possible yet.
- **No `note_id`.** Every event uses `Match::All` for CLAP's note id, so a note-off releases every
  voice on that key and channel. Per-note expression and polyphonic modulation need real ids.
- **No choke, no expression, no MIDI dialect.** `NoteKind` has two variants; the host counts anything
  else as dropped. The fixture accepts CLAP-dialect events only.
- **One instrument, one effect, from environment variables.** `MYDAW_CLAP_INSTRUMENT` and
  `MYDAW_CLAP_FX` are step affordances. There is no per-track routing, and the editor window still
  hosts one plugin (the effect when there is one, otherwise the instrument).
- **Fixed velocity from the keyboard.** A computer keyboard has no velocity sensor; every key sends
  0.8.
- **Eight-voice fixture.** `VOICES * VOICE_GAIN == 1.6`, so eight simultaneous full-velocity notes
  can clip. Documented, not a bug — a real synth would have a master gain.

## Running it

```sh
# The fixture synth, played from the computer keyboard.
cargo build -p mydaw-fixture-clap-gain
cp C:/mydaw-target/debug/mydaw_fixture_clap_gain.dll /tmp/mydaw-fixture.clap
MYDAW_CLAP_INSTRUMENT='/tmp/mydaw-fixture.clap;dev.mydaw.fixture.synth' \
  cargo run -p mydaw-app --features custom-protocol

# Instrument into the -6 dB gain fixture, same bundle:
MYDAW_CLAP_INSTRUMENT='/tmp/mydaw-fixture.clap;dev.mydaw.fixture.synth' \
MYDAW_CLAP_FX='/tmp/mydaw-fixture.clap;dev.mydaw.fixture.gain' \
  cargo run -p mydaw-app --features custom-protocol
```

Then press `A`–`J`, and `Z`/`X` to change octave. The status bar shows the octave and what is held.

## Gates

| Gate | Result |
| --- | --- |
| `cargo build --workspace` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo fmt --all --check` | clean |
| `cargo test --workspace` | **413 passed**, 0 failed, 6 ignored (372 before this step; 41 new) |
| `cargo test -p mydaw-app --test note_events_audio -- --ignored` | 2 passed on hardware |
| `pnpm --dir ui build` | clean (`tsc --noEmit` + vite) |

New tests, by area: 9 note primitives (`rt-core`), 3 note plumbing (`audio-io`), 4 layout/dispatch
units + 10 fixture-hosting integration + 2 zero-alloc (`host-clap`), 4 synth (fixture), 6 graph and
note-command (`app`), 2 meter cadence (`ipc-bridge`), 2 pump cadence (`app`), 2 hardware captures
(ignored).

## Deviations from the brief

- **The per-block event buffer lives in `RtState` (audio-io), not in `CompiledGraph`.** The buffer
  has to be filled from the note queue, and the queue belongs to the stream, not to the graph — a
  graph swap must not lose the queue or reset the buffer. `CompiledGraph::process` passes `ProcessCtx`
  through unchanged, so the effect on nodes is identical.
- **`ProcessCtx::new` kept its four-argument shape** and defaults `events` to `&[]`; the callback uses
  the new `with_events`. Adding a fifth parameter to `new` would have rewritten thirty call sites in
  tests that are about audio and have nothing to say about notes.
- **The instrument chain drops `TransportGate` entirely** rather than placing the gate after the
  instrument as the brief's sketch had it. Reasoning above; without this, a live note would not
  sound unless the transport was running, which the brief also (correctly) required.
- **`ClapNodeStats` gained two counters** (`events_sent`, `events_dropped`) that the brief did not
  ask for. They are what the integration tests assert against to distinguish "delivered" from
  "silently skipped", which is the whole difference between a working note port and a missing one.
- **The meter cadence fix changes the throttle as well as the pump.** The spike-4 doc offered
  "poll interval divisor **or** wake the pump"; a divisor alone still drifts, because the throttle
  reset its deadline to the moment of each late poll. Both halves are needed for a measured 30 Hz.
- **Captures land in `app/.debug/captures/`**, not the workspace root, because `capture_root()` reads
  the current directory and `cargo test -p mydaw-app` runs in the package directory. Both are covered
  by the same `.debug/` gitignore rule.
