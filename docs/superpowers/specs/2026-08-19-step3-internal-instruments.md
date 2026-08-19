# Step 3 — Internal instruments: a drum kit and a poly synth, with no assets

Date: 2026-08-19
Status: Done. Verified on hardware (Focusrite USB Audio, 44.1 kHz).

Step 2 carried a key press into a CLAP plugin and proved it with a fixture. That left the DAW in an
awkward place: it could play notes, but only into somebody else's plugin, named by an environment
variable. Step 3 gives it **instruments of its own** — two `RtNode`s that read
`ProcessCtx::events` and make a sound — and a way to choose between them **while the engine is
running**.

The audible claim, measured through the device rather than asserted: pressing `H` produces a
sawtooth whose fundamental is 439.993 Hz with harmonics at 220 / 880 / 1320 / 1760 Hz; pressing `C`
on the drum kit produces a kick whose loudest partial is 50.8 Hz and whose next peak is 115 dB
quieter.

## What got built

| Where | What |
| --- | --- |
| `plugins/instruments/src/dsp.rs` | `Noise` (xorshift32), `OnePole`, `poly_blep`/`poly_blep_saw`, `key_to_hz`, decay/ramp coefficients. 11 unit tests. |
| `plugins/instruments/src/drum_kit.rs` | `DrumKitNode`, `DrumKitFactory`, `Drum`, the four voices and the choke. 12 unit tests. |
| `plugins/instruments/src/poly_synth.rs` | `PolySynthNode`, `PolySynthFactory`, the ADSR and voice allocator. 13 unit tests. |
| `plugins/instruments/src/lib.rs` | `InstrumentsPlugin`, `InstrumentRegistry`, `InstrumentInfo`. 5 unit tests. |
| `plugins/instruments/src/test_support.rs` | The block-wise offline render loop the unit *and* integration tests share. |
| `plugins/instruments/tests/` | `determinism.rs` (6), `spectral.rs` (10), `rt_alloc.rs` (4). |
| `app/src/source.rs` | `Source`, `SourceSlot`/`SourceHandle` (node-granularity swap), `SourceDto`, `ClapSourceBridge`. 17 unit tests. |
| `app/src/graph.rs` | `TransportGate` is now flag-driven; `install_engine_graph` builds `SourceSlot -> [effect] -> gate`; `source_chain`. |
| `app/src/state.rs` | `start_audio_with_source`, `set_source`, `sources()`, the `InstrumentRegistry` service. |
| `app/src/commands.rs` | `get_sources`, `set_source`. |
| `app/tests/source_swap.rs` | 4 integration tests over the real chain. |
| `app/tests/instrument_audio.rs` | 3 `#[ignore]`d hardware captures. |
| `ui/src/lib/keyboard.ts` | The drum pads (`C V B N`) and `drumPadKeyFor`. |
| `ui/src/shell/TransportBar.tsx`, `ui/src/styles/app.css` | The segmented source selector. |
| `ui/src/state.tsx`, `ui/src/shell/StatusBar.tsx` | Source state, `selectSource`, the source-dependent keyboard hint. |

## Part 1 — the instruments

### Why synthesised, and why no assets

A sampled kit sounds better and costs far more: binary assets in the repository, a decoder, a
loader that can fail, a licence to track, and — worst — a test suite that can only assert "the WAV
decoded". Four oscillator-and-noise voices need none of that, and every claim about them is a claim
about DSP that a unit test can check. `the_kick_puts_its_energy_below_a_hundred_hertz` is a real
test; "the kick sample loaded" is not.

Sample playback arrives in Phase 2 as an instrument with a project-relative asset path, where the
loading, the licensing and the disk streaming are the point rather than an accident.

### The drum kit (`drum-kit`)

One voice slot per drum, four in total. That is what a drum machine does and what a drummer
physically does — you cannot hit the same drum twice without the first hit stopping — and it bounds
polyphony at four, which is why `DrumKitNode` is a fixed-size struct.

General MIDI keys, because that is what every pad controller and every MIDI file already speaks.

| Key | Drum | Signal | Envelope |
| --- | --- | --- | --- |
| 36 | Kick | sine, pitch swept 150 → 50 Hz + a 2 ms noise click at -10 dB | 250 ms exponential |
| 38 | Snare | white noise through a 1 kHz one-pole highpass (0.8) + a 180 Hz sine body (0.5) | noise 150 ms, body 110 ms |
| 42 | Closed hat | white noise through a 6 kHz one-pole highpass | 60 ms |
| 46 | Open hat | the same, different seed | 350 ms, or 4 ms when choked |

**The pitch sweep is what makes a sine a kick.** 150 Hz has a transient the ear reads as an attack,
50 Hz has the weight, and the ~13 ms time constant between them is the difference between a kick
drum and a bass note. The sweep is implemented as a multiplicative decay of a `sweep` value from 1
to 0, so it costs one multiply per sample and no `exp`.

**No attack ramp on the kick, deliberately.** A sine starting at phase 0 starts at amplitude 0, so a
hard start is not a step discontinuity. The click *is* wanted; a discontinuity is not.

**A decay constant is the -60 dB time**, the convention drum machines and reverbs both use, so
"250 ms" in this crate means what a reader assumes. A voice is *freed* at -100 dB
(`SILENCE_THRESHOLD`), which is why the 350 ms open hat actually finishes at 583 ms — and why
"released" is testable as exact digital silence rather than as "quiet enough".

**Gain staging.** There is no master gain between this node and the device yet, so the headroom
lives in four constants — kick 0.45, snare 0.35, hats 0.20 at velocity 1.0 — set together so that
the worst case a player can produce (all four drums on the same frame at full velocity) stays clear
of full scale. `the_whole_kit_hit_at_once_stays_below_full_scale` is what holds them to it.

#### The choke

Closing a hi-hat physically stops the open one ringing, so key 42 kills key 46's voice. Every drum
machine since the CR-78 does this, and a kit that did not would let an open hat ring *through* a
closed one — an unmistakable wrongness that "it is technically two voices" does not excuse.

It is not a mute. The open hat's decay coefficient is switched to a 4 ms one, which reaches
`SILENCE_THRESHOLD` inside 7 ms even from full amplitude: instant to a listener, and still a ramp,
so it does not replace a ringing hat with a click. Re-triggering the open hat restores its own
coefficient, so hitting it again reopens it.

Measured: 10 ms after the choke the open hat's tail RMS is **0.000** against an unchoked
**1.679 × 10⁻²**.

### The poly synth (`poly-synth`)

```text
  note on ──> voice ──> polyBLEP saw ──┐
                   └──> sine, -1 oct ──┴─> one-pole LP 3 kHz ──> ADSR ──> mix
```

Eight voices. Fixed parameters: attack 5 ms, decay 80 ms, sustain 0.6, release 120 ms, filter
3 kHz, sub -6.02 dB, voice gain 0.11 at velocity 1.0.

**Why a saw.** It contains every harmonic, so it is the waveform a filter has something to work
with. Generated naively — `2t - 1`, sampled — it also contains every harmonic *above* Nyquist,
which folds back down as inharmonic aliasing that gets worse the higher you play and that no later
filter can remove, because the aliases land *below* the cutoff. PolyBLEP replaces the two samples
either side of the wrap with a band-limited step; it costs one branch and two multiplies.

**Why a sine sub and not a second saw.** One partial, at a frequency the 3 kHz lowpass passes
untouched, so it adds weight without adding anything for the filter to smear. Every classic poly
synth has one for the same reason.

**The envelope segments each cross exactly the range they are responsible for, in exactly their
advertised time**: attack `0 → 1` in 5 ms, decay `1 → 0.6` in 80 ms, release `0.6 → 0` in 120 ms.
That is what makes those constants mean what a reader assumes, and it is asserted directly
(`the_envelope_segments_take_the_advertised_time`).

**Voice allocation**: the same key steals its own voice (so a repeated note neither consumes two
slots nor doubles in level, and keeps its oscillator phase so the retrigger is an envelope event
rather than a click), otherwise the first free slot, otherwise the oldest. Stealing rather than
dropping is the failure that points in the right direction: a missing note reads as a broken
engine, a stolen one as a busy synth. `stolen_voices()` counts them, because "the chord lost its
bottom note" deserves an honest explanation.

### Determinism

The same events at the same offsets produce the same samples, **bit for bit**. That is not an
aesthetic preference:

- it is what makes `tests/determinism.rs` a real test rather than a statistical one;
- it is what lets the spectral tests assert numbers instead of tolerances-of-tolerances;
- it is what makes `an_insert_survives_a_source_change_and_keeps_processing` able to compare the
  wet signal against `dry * 0.5` with `assert_eq` on raw bits;
- it is what will make an offline render of a project reproducible.

The only randomness in the crate is `dsp::Noise`, a xorshift32 whose state is never allowed to be
zero (xorshift's one fixed point, which would output silence forever). Every voice **re-seeds it
from a per-voice constant on every trigger**, so two identical hits are sample-identical. Samples
are built from the top 24 bits of the state, because every 24-bit integer is exactly representable
in `f32` and the mapping therefore introduces no rounding of its own.

Three separate determinism claims are tested: two fresh nodes agree; a node that has gone quiet
replays a hit identically (nothing survives a voice — not a noise state, not a filter, not a
phase); and **the output does not depend on the block size**.

That last one caught a real bug. Each voice originally tested its "am I finished?" threshold once
per rendered span, which made the frame a voice ended on depend on where the block boundaries fell.
A real device hands the engine whatever block size it likes and can change it between runs, so the
check moved inside the per-sample loop. The divergence was about 4 × 10⁻⁷ in amplitude — inaudible,
and exactly the kind of thing that makes a "reproducible" render not reproduce.

### RT safety

Both nodes are fixed-size structs. Everything that costs — an `exp`, a `powf`, a division, a filter
coefficient — is computed in `DrumKitNode::new` / `PolySynthNode::new`, which the factory calls on
the graph-building thread. `process` writes the mono render into the left channel and
`copy_from_slice`s it to the right, so there is not even a scratch buffer.

`tests/rt_alloc.rs` measures it with a counting global allocator rather than asserting it:
**0 allocator events** across 12 block sizes × 8 passes each, with dense event traffic (a choke
mid-block, an unmapped key, a ninth note stealing a voice, a retrigger, note-offs), plus a
steady-state run of 360 empty blocks per node covering the voice-freeing and empty-buffer paths.
The counter is armed per-thread and proved to work first, because a broken counter would pass
everything.

Both nodes split the block at every event offset and render each span separately, so a note at
frame *n* starts at frame *n*. Live input is always at offset 0 today; the sequencer will not be.

## Part 2 — switching instruments on a running engine

### The problem with rebuilding the graph

The obvious `set_source` is "build a new `GraphSpec` and install it", which is what startup does.
It does not work, for a reason worth writing down: **a hosted CLAP plugin can only be activated
once at a time.** Rebuilding the graph means building a new node for the *effect* as well, and the
old effect node is not retired until the old graph is dropped — which cannot happen until the new
graph is installed. The requirements are circular. The ways out (install a graph without the
effect, wait for the collector, install again) drop the user's insert for a few milliseconds every
time they change instrument.

### Swapping at node granularity

```text
  install once:   SourceSlot -> [CLAP effect] -> TransportGate
                       ^                              ^
                       |                              |
           new source node, by value           reads the same flag
           (rtrb + basedrop, like GraphHandle)  the slot publishes
```

The graph — and the effect node inside it — is built once per stream and never touched again; only
the head of the chain moves. This is also what a DAW actually does: changing the instrument on a
track does not rebuild the mixer.

The mechanism is `GraphHandle`'s, one level down. A new node crosses the RT wall **by value**
through an `rtrb` queue as a `basedrop::Owned`; the superseded one is dropped on the audio thread,
which under `basedrop` means *queued for the collector*, not freed.
`a_superseded_source_is_freed_by_the_collector_and_not_by_the_audio_thread` measures the allocation
count either side of the swap to prove it.

A second benefit falls out: because a built-in instrument's factory is `Send + Sync` and the swap is
a wait-free queue push, `set_source` for the built-ins runs **entirely on the calling command
thread**. No main-thread round trip, no timeout, no waker.

### The gate travels with the source

`TransportGate` exists because the test tone is a free-running oscillator; an instrument must sound
with the transport stopped, or a user could not audition one without pressing Play first. In step 2
that was expressed by *omitting* the gate from the instrument chain — which is impossible now that
the source changes at runtime.

So the gate is always in the chain, always last (Stop still silences an effect's reverb tail), and
**armed by a flag the source publishes**. `SourceSlot::poll_swap` stores the incoming node's
`gated` flag as it picks the node up, at the head of the same chain, in the same block, so the flag
can never describe the previous source. `the_gate_follows_the_source_without_a_second_graph_install`
switches tone → synth → tone and checks both directions.

### The one source that needs the main thread

A `LoadedClapPlugin` is `!Send`, lives on the main thread, and is the only thing that can build a
node for itself. So `Source::Clap` goes through `ClapSourceBridge`, a mailbox in the shape of the
step-3 `ClapGuiBridge`: the command queues a request and waits, the run loop serves it in
`MainEventsCleared`, the command gets the answer or a timeout.

Switching *away* from the CLAP instrument hands its node to `basedrop`, and `activate()` cannot run
again until the collector has actually dropped it — so the main-thread handler waits (up to
`CLAP_RETIRE_WAIT`, 500 ms) for `collect_retired()` before re-activating. That wait only happens on
that path, and only after the user asked for it.

Availability is published at startup (`ClapSourceBridge::set_available`), so a session with no
bundle rejects `clap` immediately with a sentence naming the environment variable rather than after
a five-second round trip — and the UI greys the button out instead of offering one that always
fails.

### Default source

`Source::DEFAULT` is `poly-synth`. A DAW that greets you with a playable instrument is telling the
truth about what it is; a continuous sine is a diagnostic. The tone is one click away, and is still
what the hardware tests use.

Two exceptions:

- **`MYDAW_CLAP_INSTRUMENT` still wins**, because setting it is an explicit request.
- **Headless capture mode starts on the test tone.** `MYDAW_DEBUG_CAPTURE` presses Play with nobody
  at the keyboard; an instrument that is silent until played would (correctly) record silence and
  exit non-zero. Verified: a headless run still reports `440.0 Hz at -13.97 dBFS`, exit code 0.

`AppState::start_audio()` and `start_audio_with_effect()` also still mean the test tone, so every
existing hardware test asserts exactly what it asserted before.

## The UI

A **segmented button group** in the transport bar rather than a dropdown: the list is short and
fixed, and which instrument is playing is the single most important piece of state on that bar — a
collapsed `<select>` would hide it behind a click. `CLAP` stays visible and disabled when no bundle
was loaded, because "you could have one" is useful information and a button that vanishes teaches
nothing.

### The drum pads

`C` kick, `V` snare, `B` closed hat, `N` open hat — live only while the drum kit is selected.

The brief suggested `Z X C V`; `Z` and `X` are the octave shift and taking them away would break the
piano the moment somebody switched instrument and back. `C V B N` continue the same bottom row
rightwards, are otherwise unused, and are in the order a drummer would name them (low to high). The
status bar shows `C kick  V snare  B closed hat  N open hat` whenever they are live, and hides it
otherwise — a static hint would be wrong three quarters of the time.

They are **additional**. The piano keys keep working, so `A` in octave 2 still lands on key 36 and
still hits the kick; the pads exist so a hit does not need an octave shift first. Pads are not
transposed by the octave — a kick is key 36, always — which is why `drumPadKeyFor` is a separate
function rather than an exception inside `midiKeyFor`.

Switching source **releases every held key first**: the note-offs have to reach the *old*
instrument, or a key still down when the source changes would leave a voice sounding on a node that
no longer receives its release.

The mock backend serves the same four sources with `clap` unavailable, so the selector's disabled
path, the hint and the pads can all be developed in a plain browser.

## Measurements

### Offline, from `tests/spectral.rs`

| Subject | Measured |
| --- | --- |
| kick | dominant **50.8 Hz**, **99.9%** of the energy below 100 Hz, peak 0.453 |
| kick envelope | 50 ms RMS windows `0.186, 0.045, 0.011, 0.0028, 0.00071, 0.00018` — monotonic, -60 dB over 300 ms |
| snare | flatness **0.418**, centroid 9 840 Hz, **76.9%** of the energy above 1 kHz, peak 0.404 |
| closed hat | centroid **14 146 Hz**, flatness 0.410, 0.07% below 1 kHz, peak 0.131 |
| open hat | centroid **14 562 Hz**, flatness 0.408, 0.06% below 1 kHz, peak 0.134 |
| choke | tail RMS **0.000** vs **1.679 × 10⁻²** unchoked, 10 ms after the event |
| A4 | dominant 440.04 Hz; peaks 440/-28, 220/-30, 880/-34, 1320/-38, 1760/-41, 2200/-43, 2640/-46, 3080/-47 dBFS |
| sub level | **-2.06 dB** against the saw's fundamental (arithmetic: -2.10 dB) |
| key 108 aliasing | off-grid energy **-35.6 dB** below on-grid |

**The aliasing method**, since "no aliasing" is not directly measurable: a voice's only legitimate
partials are integer multiples of the sub-oscillator's frequency (`f0/2`) — the sub contributes odd
and even multiples, the saw contributes multiples of `f0`, which are a subset. Everything else is
aliasing or window leakage. The test sums the energy in every bin more than 60 Hz from any multiple
of `f0/2` (60 Hz is ~20 bins here, far outside a Hann main lobe and well down its sidelobes) and
compares it with the energy on the grid. Key 108 is 4 186 Hz, whose 6th harmonic is already past
Nyquist at 48 kHz, so a naive saw would have nowhere to put it but back inside the audible band. A
separate oscillator-level test (`poly_blep_suppresses_the_aliasing_a_naive_saw_produces`) compares
sub-fundamental energy against a naive saw directly.

### On hardware

`app/tests/instrument_audio.rs`, `#[ignore]`d, run on this machine:

```sh
cargo test -p mydaw-app --test instrument_audio -- --ignored --nocapture --test-threads=1
```

**Device: Speakers (Focusrite USB Audio) @ 44100 Hz, 2 ch, max block 4096.**

| Capture | Frames | Peak (L) | dBFS | RMS | Dominant | Flatness |
| --- | --- | --- | --- | --- | --- | --- |
| poly idle (no notes, transport stopped) | 8 820 | 0.0000 | -inf | 0.0000 | none | 0.0000 |
| poly A4, velocity 1.0 | 44 100 | 0.0652 | -23.72 | 0.0424 | **439.993 Hz** | 0.0000 |
| poly release, +600 ms | 13 230 | **0.0000** | -inf | 0.0000 | none | 0.0000 |
| poly chord C4+E4+G4 | 44 100 | 0.1945 | -14.22 | 0.0741 | 329.619 Hz | 0.0000 |
| poly chord off, +600 ms | 13 230 | **0.0000** | -inf | 0.0000 | none | 0.0000 |
| drums idle | 8 820 | 0.0000 | -inf | 0.0000 | none | 0.0000 |
| drum kick | 22 050 | 0.4515 | -6.91 | 0.0606 | **50.763 Hz** | 0.0000 |
| drum snare | 13 230 | 0.3939 | -8.09 | 0.0340 | **180.071 Hz** | **0.4470** |
| drum open hat | 13 230 | 0.1267 | -17.95 | 0.0168 | **17 931 Hz** | 0.4030 |
| open hat choked, +200 ms | 8 820 | **0.0000** | -inf | 0.0000 | none | 0.0000 |

A4 spectrum, top five interpolated peaks (Hz, dBFS):

```
(440, -28)  (220, -29)  (880, -34)  (1320, -38)  (1760, -41)
```

440 is the fundamental, 220 the sub an octave down, and 880/1320/1760 the saw's 2nd, 3rd and 4th
harmonics falling off as `1/k` — which is what a sawtooth is.

Kick spectrum:

```
(51, -39)  (524, -154)  (532, -155)  (559, -156)  (570, -157)
```

The next peak is **115 dB** below the fundamental. Snare spectrum, for contrast:

```
(180, -49)  (18165, -65)  (9304, -65)  (6224, -65)  (19429, -66)
```

The 180 Hz body tone is the loudest single partial by design — the noise is louder in total but
spread across the whole spectrum, which is what a flatness of 0.447 means next to the kick's 0.000.

Every capture reported 0 dropped samples, 0 non-finite samples and 0 clipped samples.

Artefacts from the verification run:

```
app/.debug/captures/20260818-235620-1/  poly-idle
app/.debug/captures/20260818-235622/    poly-a4        (capture.png, stats.json, capture.wav)
app/.debug/captures/20260818-235623/    poly-release
app/.debug/captures/20260818-235616/    poly-chord
app/.debug/captures/20260818-235618/    poly-chord-off
app/.debug/captures/20260818-235644/    drums-idle
app/.debug/captures/20260818-235644-1/  drums-kick
app/.debug/captures/20260818-235646/    drums-snare
app/.debug/captures/20260818-235646-1/  drums-open-hat
app/.debug/captures/20260818-235647/    drums-choke
```

## Limitations

Named here so nobody discovers them by surprise:

- **Every parameter is a constant.** No cutoff knob, no tuning, no per-drum level, no envelope
  times, no presets. Parameters need a parameter queue, smoothing and automation, which is the
  mixer phase's work — a knob wired to a raw field with no smoothing clicks, and a knob wired to
  nothing is a lie.
- **The drum kit answers to four keys.** Anything else is counted in `unmapped_notes()` and ignored.
  Toms, ride, crash, claps: more voices, same shape.
- **Note-offs mean nothing to the drum kit.** A drum is a one-shot; its length is a property of the
  drum, not of how long a pad was held. There is no choke group beyond hat-on-hat.
- **No velocity layers.** Velocity scales amplitude linearly and nothing else, so a soft hit is a
  quiet hard hit rather than a different sound.
- **The poly synth's filter has no resonance and no envelope.** A one-pole at a fixed 3 kHz is the
  right size of filter for "take the glassy top off"; a ladder with a cutoff envelope is a feature
  with knobs, and knobs are the automation phase.
- **Eight full-velocity voices can approach full scale.** A four-note chord at velocity 1.0 is
  tested to stay clear; eight is louder. There is no master gain between the instrument and the
  device yet.
- **Mono, fanned out to stereo.** Neither instrument has anything stereo to say. No pan, no spread,
  no stereo width.
- **One instrument at a time.** The source is a property of the whole engine, not of a track,
  because there are no tracks. Per-track instruments arrive with the mixer.
- **No pattern, no sequencer, no recording.** Notes come from the computer keyboard and nowhere
  else, and nothing stores them.
- **The source selection is not persisted.** Every session starts on `poly-synth`.
- **Switching to the CLAP source can block the main thread for up to 500 ms** while it waits for the
  previous node to be retired by the collector. Only on that path, only after an explicit request.

## Gates

| Gate | Result |
| --- | --- |
| `cargo build --workspace` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo fmt --all --check` | clean |
| `cargo test --workspace` | **503 passed**, 0 failed, 9 ignored (413 before this step; 90 new) |
| `cargo test -p mydaw-app --test instrument_audio -- --ignored` | 3 passed on hardware |
| `MYDAW_DEBUG_CAPTURE=1 mydaw-app` | 440.0 Hz at -13.97 dBFS, exit 0 |
| `pnpm --dir ui build` | clean (`tsc --noEmit` + vite) |

New tests, by area: 43 unit + 20 integration + 1 doctest in `mydaw-plugin-instruments`; 21 unit
(17 source, 4 state) + 4 integration + 3 ignored hardware in `mydaw-app`.

## Deviations from the brief

- **Instrument switching swaps a node inside a persistent graph rather than reinstalling the graph.**
  The brief said "rebuilds chain via `GraphHandle` install (existing pattern), keeps CLAP effect if
  configured". Those two requirements are mutually exclusive — see "The problem with rebuilding the
  graph" above — so the second one won, and the swap moved one level down to the same `rtrb` +
  `basedrop` mechanism at node granularity. `install_engine_graph` still uses `GraphHandle::install`
  exactly once per stream.
- **The transport gate stays in the chain for every source, armed by a flag**, rather than being
  present for the tone and absent for instruments. With a runtime-swappable source it cannot be
  structural. The behaviour is identical, including "the gate is last, so Stop silences a reverb
  tail".
- **The drum pads are `C V B N`, not `Z X C V`.** `Z`/`X` are the octave shift; taking them would
  break the piano. Documented in the status-bar hint, which the brief asked for.
- **The `CLAP` source is labelled `CLAP`, not `CLAP Instrument`.** Segmented buttons in a transport
  bar that also has to fit a transport; the description carries the rest and is the button's
  tooltip.
- **Headless capture mode starts on the test tone** rather than the new default. It presses Play
  with nobody at the keyboard, so an instrument default would have turned a working agent-facing
  mode into one that always exits 1.
- **`InstrumentRegistry` is one service, not two factories.** The brief allowed either. The shell has
  to answer "what can I switch to?" as well as "make me one of those"; a bare factory knows its
  `type_id` and nothing a menu can show, and hard-coding the list in the shell would put this
  crate's node names in a file that does not depend on them being right.
- **`plugins/instruments` has a public `test_support` module.** An integration test in `tests/` can
  only reach the public API, and duplicating a block-wise render loop into four files is how two of
  them quietly stop agreeing. Same reasoning as `mydaw-plugin-host-clap::test_util`.
- **`engine_graph`/`tone_graph` were replaced by one `engine_graph(source, effect, gated)`.** The old
  pair took a `TestToneFactory` and decided the gate from whether an instrument was passed, which no
  longer describes anything the engine does; the offline renders in `tests/clap_graph.rs` were
  updated to the new shape.
- **The block-size-independence test was not asked for.** It found a real determinism bug (voices
  ended on a frame that depended on the block boundaries), which is the argument for keeping it.
