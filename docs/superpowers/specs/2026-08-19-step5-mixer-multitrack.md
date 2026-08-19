# Step 5 — Multi-track graph and mixer

**Date:** 2026-08-19
**Status:** implemented
**Predecessors:** [step 3 — internal instruments](2026-08-19-step3-internal-instruments.md),
[step 4 — step sequencer](2026-08-19-step4-step-sequencer.md)

---

## What changed, in one sentence

The audio graph stopped being a serial chain and became a **mixer**: parallel track chains,
each with its own source, insert chain and strip (gain, pan, mute), summed into a master bus
with its own fader — plus the parameter path, note routing, metering, project model, commands
and panel that a mixer needs to be usable.

## Why this step, and why now

Steps 1–4 built one voice: a device, a source, a note path, a grid. Everything they produced
went through a single chain, which meant the DAW could play *a* thing and not *things*. A
mixer is the first structure that makes the engine a DAW rather than an instrument, and it is
also the piece that everything after it hangs off: a project file records tracks, automation
automates a strip's parameters, per-track plugin inserts go in a chain that has to exist first.

---

## 1. Graph topology

```text
 track 0:  SourceSlot ─▶ [insert…] ─▶ TransportGate ─▶ ┐ strip: gain × pan × mute
 track 1:  SourceSlot ─▶ [insert…] ─▶ TransportGate ─▶ ┤ (10 ms ramped)
   …                                                   ├──▶  Σ  master bus
 track n:  SourceSlot ─▶ [insert…] ─▶ TransportGate ─▶ ┘        │
                                                                ▼
                                                        [master insert…]
                                                                │
                                                                ▼
                                                       master fader (ramped)
                                                                │
                                                                ▼
                                              device ▸ meters ▸ debug tap
```

Owned by `crates/rt-core/src/graph.rs` (`GraphSpec` → `CompiledGraph`) and
`crates/rt-core/src/mixer.rs` (`MixerParams`, `TrackStrip`, `MasterStrip`).

| Property | Value | Why |
| --- | --- | --- |
| `MAX_TRACKS` | 16 | The RT-side state is fixed arrays: the parameter block, the peak block and the meter frame are all `[_; MAX_TRACKS]`. It is also the number of channels a `NoteEvent` can address, which is not a coincidence — see §3. |
| Per-track scratch | one `AudioBuffers` (stereo, `max_block` frames) | A track renders into its own bus so the strip has something to scale. Allocated in `GraphSpec::compile`, never after. |
| Per-track event buffer | `Vec<NoteEvent>` with `max_events` capacity | The block's events filtered to this track. `clear()` + `push()` inside the existing allocation; a push past capacity drops rather than growing. |
| Master bus | `ctx.buffers`, the callback's own | Cleared at the top of every block — it is a *sum*, so what the last block left in it is not part of this one. |
| Execution | tracks processed in order, one thread | See §7. |

A track's input is **silence**: the scratch is zeroed before the source runs. That is the one
observable behaviour change for a node that used to sit at the head of a chain and read what was
already in the shared bus — an effect used as a source now gets nothing to work on, which is
correct, and which `plugins/host-clap/tests/rt_alloc.rs` was updated to reflect.

### Back-compatibility of `GraphSpec`

`GraphSpec::push`/`with` still build a **serial chain**, and that chain is now one track: the
first node pushed is its source, everything after it an insert. `spec.len()` still counts nodes.
Every pre-step-5 caller — the offline renders, the CLAP tests, `engine_graph`, `source_chain` —
compiles unchanged and produces the same samples, because a default mixer is transparent (§2).

### Graph limits

`GraphLimits { sample_rate, max_block, max_events }` are what a compiled graph allocates for.
A builder can pin them with `GraphSpec::with_limits`; left unset,
`AudioIoService::install_graph` fills them in from the **open stream**. A graph builder should
not have to know the device's block size to be correct, and one built against a stale block size
would truncate every block.

---

## 2. Parameters: atomics, smoothing, and the pan law

### Why atomics and not a queue

A fader is a *level*, not an event. A dropped queued gain change leaves the mixer at the wrong
volume forever, and a queue deep enough never to drop one is a queue sized for a mouse. So
`MixerParams` is a block of `AtomicU32` (f32 bits) and `AtomicBool` that the audio thread reads
**once per block, per track**: three loads plus the pan law, never per sample. Transport commands
stay on a queue because *they* are events and their order matters.

`MixerParams` is shared by `Arc` and handed to every graph compiled from a spec that names it, so
a rebuild never resets a fader.

### The ramp

Reading a new gain once per block and multiplying by it is a step change at the block boundary —
a click, and at 30 fader updates a second, a zipper. Each strip therefore owns its current
coefficient and walks it towards the target:

```text
ramp_frames = max(64, round(sample_rate × 0.010))      // 480 at 48 kHz, 441 at 44.1 kHz
step        = (goal − current) / frames_remaining      // recomputed at each block
per sample:   remaining −= 1;  current = (remaining == 0) ? goal : current + step
```

Deriving the step from the frames *remaining* rather than from a fixed denominator is what makes
the ramp land exactly on the target (f32 accumulation over 480 additions otherwise misses it by
~3 × 10⁻⁵) and take exactly `ramp_frames` from the moment the control moved, whatever the block
size. A ramp longer than the block continues on the next one; both channels of a strip step in
lockstep, so a pan change cannot smear the image while it moves.

**Bound:** from a constant input, no sample-to-sample step exceeds `|Δcoefficient| / ramp_frames`
— at most 1/480 of full scale for a full-scale mute. Asserted directly in
`mixer::tests::a_gain_change_ramps_instead_of_stepping`,
`graph::tests::a_mute_applied_mid_play_is_a_fade_and_never_a_step`, and through the shell's own
graph in `app/tests/mixer_graph.rs::a_mute_never_steps_the_waveform`.

**Mute is the same mechanism with a target of zero.** A mute is a 10 ms fade, not a cut — which is
the difference between a mute button and a click.

### The pan law

Equal power. For `pan ∈ [−1, 1]`:

```text
θ = (pan + 1) · π/4                    // 0 at hard left, π/4 centre, π/2 hard right
L = √2 · cos θ        R = √2 · sin θ
L² + R² = 2                            // constant across the whole sweep
```

| Position | L | R | L in dB, relative to the edge |
| --- | --- | --- | --- |
| hard left (−1) | √2 (+3.01 dB) | 0 (exactly) | 0 dB |
| centre (0) | 1.0 | 1.0 | −3.01 dB |
| hard right (+1) | ~8.7 × 10⁻¹⁷ (−321 dBFS) | √2 | — |

**Deviation from the brief, stated plainly.** The brief asked for "equal-power pan law (−3 dB
centre)". The law implemented *is* the −3 dB law — constant power, centre 3.01 dB below a
hard-panned edge, which is the relationship the name refers to and which
`mixer::tests::the_pan_law_is_equal_power_with_the_centre_at_unity` asserts to 0.01 dB. What
differs is the **normalisation**: the gains are scaled by √2 so that *centre* is unity and an edge
is +3.01 dB, rather than centre being −3.01 dB and an edge unity.

Two reasons:

1. **A default project must be transparent.** Every track at unity gain and centre pan has to sum
   to exactly what its sources produced. Without the normalisation, adding a mixer would also
   have meant "everything got 3 dB quieter", and every existing amplitude assertion in the
   workspace (~600 tests, several of them spectral hardware measurements) would have had to be
   rewritten around a constant that means nothing musically.
2. **Centre is where faders live.** A control whose neutral position is a 3 dB cut is a control
   you have to compensate for.

It is one constant — `PAN_CENTRE_NORMALISATION` in `crates/rt-core/src/mixer.rs`. Setting it to
1.0 gives the un-normalised law and nothing else changes.

### Solo

Resolved **off** the audio thread. "Is this track audible" is a question about the whole project —
track 2 is silent because track 0 is soloed — and answering it per block per track would be the RT
thread doing the UI's arithmetic. `Project::effective_mute` folds solo into a single bool per
track:

```text
any track soloed ?  audible ⇔ this track is soloed
otherwise        ?  audible ⇔ this track is not muted
```

A soloed *and* muted track stays audible: solo is an override, not a vote, which is what every DAW
does and what keeps a solo button from sometimes doing nothing. The audio thread only ever reads
`muted`, so soloing is exactly as click-free as muting — because it *is* muting.

### Gain scale

Stored in dB (`TrackDef::gain_db`), published as a linear multiplier. A fader is a dB control —
that is the scale it is drawn on, the scale a user thinks in, and the scale a project file should
record, because −6.0 dB is exact and 0.5011872 is a rounding of it. The conversion happens once,
on the way in; the audio thread never calls `powf`. `SILENCE_DB = −60` is exactly zero, so a fader
pulled to the bottom is silence rather than −60 dB of hiss.

---

## 3. Note routing: a channel *is* a track

`NoteEvent::channel` is the track index. `CompiledGraph::process` filters the block's event slice
per track into that track's pre-allocated buffer, so a track's source sees only the events
addressed to it. The filter preserves order, so the subset is still sorted by `frame_offset`.

| Source of events | Channel |
| --- | --- |
| Sequencer row | `PatternRow::channel` — already existed, now means "track". The four default drum rows are on channel 0, which is why track 0 of the default project is the kit. |
| Keyboard piano / `note_on` | the **selected track**'s index (`AppState::note_channel`). |
| A track's own channel | its index, unless `TrackSpec::on_channel` overrides it. |

`MAX_TRACKS == 16 == ` the number of MIDI channels a `NoteEvent` can carry, so the mapping is
total and the clamp in `note_channel` is unreachable rather than lossy.

**Sharp edge:** removing a track renumbers the ones after it, and a track index *is* a note
channel — so a sequencer row addressed at a later track follows the renumbering. Track *ids*
(`TrackDef::id`) are stable and are what the UI keys its list on; indices are not. Making rows
reference ids is project-file work, not step-5 work.

---

## 4. Metering

The mixer measures each track's post-strip, **pre-master** peak during its sum pass — no second
pass over the audio, and no queue of its own. `CompiledGraph` keeps the block's peaks in a fixed
`TrackPeaks` array; `run_block` copies it into the `MeterFrame` it was already pushing.

```text
TrackStrip::mix_into  ──▶ TrackPeaks (fixed, Copy)  ──▶ MeterFrame.tracks[16] + track_count
                                                            │  30 Hz throttle, unchanged
                                                            ▼
                                              MeterFrameDto.tracks: Vec<TrackMeterDto>
```

- **RT side stays fixed:** `[TrackMeter; MAX_TRACKS]` plus a `track_count`, because a `Vec` here
  would be an allocation per block.
- **DTO side becomes a list** of exactly the tracks that exist: a JSON array of sixteen entries
  with fourteen of them zero is fourteen entries of nothing crossing the boundary thirty times a
  second.
- **Cadence is unchanged** at 30 Hz, and so is the master half of the frame — the existing
  `meters` panel keeps working as the master meter and was updated only where the type demanded it.
- **Pre-master on purpose:** pull a channel fader and its own meter follows; pull the master and it
  does not. That is what a channel strip shows.

`crates/ipc-bridge` gained a dependency on `crates/rt-core` so `MAX_TRACKS` has one definition
rather than two that can drift.

---

## 5. The project model, and what rebuilds the graph

`app/src/project.rs` holds the editable side — `TrackDef { id, name, source, gain_db, pan, muted,
soloed }` and `Project { tracks, master_gain_db }` — in the same relationship to `MixerParams`
that `Pattern` has to `CompiledPattern`. `app/src/tracks.rs` (`TrackHost`) is the machine that
keeps the audio thread agreeing with it.

**Default project:** track 0 "Drums" (drum kit), track 1 "Synth" (poly synth). Two rather than one
because the point of the step is that they sum; two rather than four because a mixer full of
silent tracks is a worse first impression than one you add to. Drums first so the sequencer's four
default rows — on channel 0, and a kick, a snare and two hats — address the kit that can play them.

Three kinds of edit, and only one of them rebuilds anything:

| Edit | Mechanism | Rebuilds? |
| --- | --- | --- |
| gain, pan, mute, solo, master | stores into `MixerParams` | no |
| a track's **source** | node swap through that track's `SourceSlot` (step 3's mechanism, per track) | no |
| **add / remove** a track | `GraphSpec` recompiled and installed | yes |

Mixer and source edits work with **no audio device open** where they can: a fader and a project are
data, and editing them on a machine with no sound card has to work. (A source change needs the
stream's sample rate to build a node, so it reports `NotRunning`.)

Strips are constructed *already sitting at* their target, so a rebuild picks the mixer up exactly
where the old graph left it — no fade-in, no gap. `app/tests/mixer_graph.rs::adding_a_track_mid_play_does_not_interrupt_the_others`
and `graph::tests::a_rebuild_mid_play_leaves_the_untouched_track_continuous` measure that.

---

## 6. CLAP: where it goes, and what it cannot do

### Where the plugins go

| Environment variable | Placement |
| --- | --- |
| `MYDAW_CLAP_INSTRUMENT` | source of **track 0**, installed through that track's `SourceSlot` after the graph is up. Track 0 is the one the keyboard starts on. |
| `MYDAW_CLAP_FX` | a **master insert** — on the summed bus, after every track strip and before the master fader. |

The effect goes on the master rather than on a track because that is where a master-bus effect
belongs and because it is the only insert point that does not have to pick a track for it.
Per-track inserts are a later step's work: the graph already carries a per-track insert chain
(`TrackSpec::inserts`), so that is a change to the builder, not to the engine. They need a plugin
browser and a project file to record them in first.

### The structural limitation

**Adding or removing a track is refused while a hosted CLAP plugin is in the graph**
(`TrackError::ClapInGraph`; `MixerState.canEditLayout` is false, and the panel says so).

The reason is step 3's, unchanged: a hosted CLAP plugin can only be **activated once at a time**.
A rebuild compiles a whole new graph, which would need new CLAP nodes — but the old ones are not
retired until the old graph is dropped, which cannot happen until the new graph is installed. The
requirements are circular. Worse, only the main thread can build a CLAP node at all
(`LoadedClapPlugin` is `!Send`), so a rebuild on a command thread could not recreate one even if
the timing worked.

Everything *else* keeps working with a CLAP plugin loaded: faders, pans, mutes, solo, the master,
selecting a track, and changing a **non-CLAP** track's source — none of which rebuild the graph.
Lifting the restriction means a rebuild protocol that hands existing nodes across a swap, which is
the same machinery a project loader will want; it belongs with that work, not here.

`ClapSourceBridge::request` now names the track it is for. There is one plugin instance, so a burst
of requests naming different tracks cannot all be honoured: the first queued one wins and the rest
are told why, which beats silently activating the wrong track.

---

## 7. Multicore: deliberately not yet

Tracks are rendered one after another on the audio thread. Rendering them in parallel is a real win
once tracks carry heavyweight plugins, and it is deliberately not attempted here — it needs:

- a worker pool promoted to real-time priority (each worker with its own MMCSS registration);
- a dependency-ordered schedule, which only becomes non-trivial once sends and buses exist;
- a join that cannot block the callback — a spin-then-help scheme, not a condvar;
- and a way to keep the result bit-identical, because determinism is what makes every spectral
  assertion in this workspace a number rather than a tolerance.

None of that pays for itself at two tracks of synthesis, and all of it is cheaper to add once the
graph has the shape it will keep. The seam is the loop in `CompiledGraph::process`; nothing outside
that file depends on it being serial. Per-track scratch buffers — the thing parallelism actually
needs — already exist.

---

## 8. UI

The **mixer panel** is contributed by the *shell*, next to the kernel-status view, rather than by a
plugin: a strip is a view of the project, and the project lives in `app`. The metering plugin owns
the meter queue and the sequencer owns the pattern, but neither owns a track, so a plugin
contributing this panel would have to reach back into the shell for everything it draws.

- One strip per track plus a master strip: name (click to select), source label, a two-bar peak
  meter, a fader, a pan slider, M/S buttons, and a remove button.
- **DOM, not canvas** — same reasoning as the sequencer grid. The meters are two `<div>`s per
  channel whose height is written from a `requestAnimationFrame` loop reading the `MeterBus`;
  levels never touch a Solid signal.
- **`<Index>`, not `<For>`.** Every mixer command answers with a whole new `MixerState`, so every
  `TrackDto` is a new object; `<For>` keys by reference and would rebuild the strips on every edit,
  which mid-drag replaces the very `<input>` the pointer is captured on and ends the gesture.
- **Fader curve:** `db = −60 + travel^0.4 × 66`. Unity lands at ~79 % of the throw, half travel at
  −10 dB, a quarter at −22 dB. A linear map would spend half the throw between −60 and −30 dB.
- Every mixer command returns the whole `MixerState` rather than an acknowledgement: a strip is a
  handful of numbers, the spike-4 measurements make a message this size free, and one shape back
  means the panel never merges a partial update into what it already had.

The **transport bar's source selector** is kept and now sets the source of the *selected track*; it
shows that track's name in place of the "Source" label, so the target is never a surprise. The
mixer is where you change which track that is. Removing the selector would have cost the fastest
path to "make this track a drum kit" for no gain.

The existing **meters panel** is unchanged in behaviour and remains the master meter.

---

## 9. Verification

### Gates

| Gate | Result |
| --- | --- |
| `cargo test --workspace` | **670 passed, 0 failed** (597 before this step) |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo fmt --all` | clean |
| `pnpm --dir ui build` (`tsc --noEmit && vite build`) | clean |

New coverage, by claim:

| Claim | Where |
| --- | --- |
| sum correctness, faders, master | `rt-core/graph.rs`, `app/tests/mixer_graph.rs` |
| equal-power law at L/C/R, constant power across the sweep, −3.01 dB centre | `rt-core/mixer.rs` |
| hard pan leaves ≤ −80 dBFS in the opposite channel | `rt-core/graph.rs`, `app/tests/mixer_graph.rs` |
| mute/gain ramps never step by more than 1/`ramp_frames` | `rt-core/mixer.rs`, `rt-core/graph.rs`, `app/tests/mixer_graph.rs` |
| solo silences the rest, un-solo restores, solo overrides mute | `app/src/project.rs`, `app/src/tracks.rs`, `app/tests/mixer_graph.rs` |
| a note reaches only the track its channel names | `rt-core/graph.rs`, `app/tests/mixer_graph.rs` |
| per-track meters are independent and pre-master | `rt-core/graph.rs`, `plugins/audio-io/src/rt.rs`, `app/tests/mixer_graph.rs` |
| rebuild mid-play leaves the other tracks continuous | `rt-core/graph.rs`, `app/tests/mixer_graph.rs` |
| **zero allocation** with 16 tracks, a full event block and controls moving | `crates/rt-core/tests/mixer_rt_alloc.rs` |
| a 16-track graph swap frees nothing on the audio thread | `crates/rt-core/tests/mixer_rt_alloc.rs` |
| determinism: two identical runs are bit-identical | `rt-core/graph.rs` |
| layout edits refused with a CLAP plugin in the graph | `app/src/tracks.rs` |

### Hardware verification

`cargo test -p mydaw-app --test mixer_audio -- --ignored --nocapture --test-threads=1`

Speakers (Focusrite USB Audio) @ 44 100 Hz, 2 ch, max block 4096. Kick-only pattern at 130 BPM on
track 0 (drum kit) plus a held C-major triad (60/64/67) on track 1 (poly synth), four captures of
1.5 s each, 32768-point FFT (1.35 Hz/bin), **0 dropped samples** in every capture.

Measured as band energy in dBFS: **kick** 30–140 Hz (the kit's kick sweeps 150 → 50 Hz),
**chord** 240–420 Hz (C4 261.63, E4 329.63, G4 392.00). The pattern is kick-only because a hi-hat
is broadband and would put energy in the chord's band.

| Capture | kick L | chord L | chord R |
| --- | --- | --- | --- |
| beat only (synth muted) — the reference floor | −20.10 | −42.53 | −42.53 |
| **both tracks** | −20.27 | **−21.86** (+20.67 over floor) | −21.86 |
| **track 1 muted** | −20.96 | **−43.17** (−0.64 from floor) | −43.17 |
| **track 1 hard left** | L −20.79 / R −21.93 | **−18.98** (+23.55 over floor) | **−43.98** (−1.45 from floor, L−R = 25.00 dB) |

Read as claims:

1. **Both present.** The chord's band stands 20.67 dB above the beat's own floor, and all three
   partials (261.63 / 329.63 / 392.00 Hz, ±4 Hz) appear as spectral peaks. Nothing clipped; no
   non-finite samples.
2. **Mute removes the chord and leaves the beat.** The chord's band returns to within 0.64 dB of
   the beat-only floor and none of the three partials survives as a peak, while the kick moves
   0.69 dB — measurement noise on a transient, not a level change.
3. **Hard-left pan empties the right channel of the chord and keeps it in the left.** The right
   channel's chord band sits 1.45 dB *below* the floor while the left gains 2.9 dB — which is the
   equal-power law's +3.01 dB edge, measured. All three partials are peaks in L and absent from R.
   The centred kit stays in both channels to within 1.14 dB.

A kick drum's attack is broadband, so it puts a floor of its own into the chord's band; "the chord
is gone" can only mean "that band is back down to the beat's own floor", which is why the test
takes a beat-only reference capture first rather than comparing against an absolute number.

WAV, annotated PNG and `stats.json` for each capture are written under
`.debug/captures/<timestamp>/` and the paths are printed, so a failure can be looked at rather than
guessed at.

---

## 10. Limitations, and what step 6 inherits

- **No per-track inserts from the UI.** The graph carries the chain; nothing fills it but the
  master CLAP effect. Needs a plugin browser and somewhere to record the choice.
- **Layout edits are refused with a CLAP plugin loaded.** See §6.
- **Removing a track renumbers note channels.** See §3.
- **No sends, no buses, no groups.** The sum is flat: every track goes to the master.
- **No automation.** `MixerParams` is exactly the surface automation will write to, and the ramp
  is exactly the smoothing it will need; nothing about the RT side has to change for it.
- **No project file.** `Project` is the shape one will serialise. Nothing in it is derived, and the
  ids are already stable.
- **Serial track rendering.** See §7.
- **Master strip has no pan.** A "pan" on a stereo bus is a balance control, which is a different
  thing and not one this step needs.
