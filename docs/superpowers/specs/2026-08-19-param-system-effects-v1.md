# Parameter system and internal effects v1

**Status.** The engine half has shipped: the parameter system in
`crates/rt-core/src/params.rs` and the five effects in `plugins/effects`. The
shell half — per-track insert chains, the command surface, the DTOs and the
`.mydaw` format bump — is **specified here and not yet built**; see
[What is not built yet](#what-is-not-built-yet) for exactly where the line is and
what the follow-up has to do.

The evidence behind every range, curve, default and behaviour is in
`2026-08-19-effects-ux-research.md`, committed before the code. This document is
about the machinery.

---

## What changed, in one sentence

Nodes can now have knobs: a parameter is `&'static` metadata plus an atomic the
audio thread reads and ramps towards, and five effects are built on it.

---

## 1. The parameter system

Three separate things, deliberately not one thing.

| Type | What it is | Who owns it |
| --- | --- | --- |
| `ParamDef` | Metadata: id, name, unit, min, max, default, curve, step, smoothing time | The node **type**, as `&'static` data |
| `ParamBlock` | The values, one `AtomicF32` per parameter | The **shell**, by `Arc` |
| `SmoothedParams` | One linear ramp per parameter | The **node**, on the audio thread |

### Why the metadata is static and lives on the type

The shell has to answer *"what knobs does an EQ have"* before an EQ exists — to
draw a panel for an effect being chosen from a menu, and to validate a project
file that names one. A method on the node could only answer for a node that
already exists. So the declaration is on the factory:

```rust
pub trait ParamNodeFactory: RtNodeFactory {
    fn params(&self) -> &'static [ParamDef];
    fn readouts(&self) -> &'static [ReadoutDef] { &[] }
    fn create_with_params(&self, sample_rate: f64, max_block: usize,
                          params: Arc<ParamBlock>) -> Box<dyn RtNode>;
}
```

This also keeps `RtNode` exactly as small as it was — one object-safe method,
with nothing on it an audio callback does not need. Object-safety was the
constraint that decided the shape: `params()` returning a `'static` slice is
object-safe, and so is `create_with_params`, so `Arc<dyn ParamNodeFactory>` works
and the registry can hold one per type.

### Why the shell owns the block and not the node

A node is rebuilt whenever the graph is. If the values lived *in* the node they
would reset on every structural edit — adding a track would reset every EQ in the
session. They live in an `Arc<ParamBlock>` the shell keeps instead, and a rebuilt
node is handed the same one. This is exactly the relationship `MixerParams` has
with `TrackStrip`, and it is the same argument: **a rebuild must not be audible.**

`SmoothedParams::new` starts every ramp *at* the block's current value rather
than at the default, so a node built into a new graph picks the parameters up
where the old one left them instead of sweeping to them over 10 ms.

### Why atomics and not a queue

Unchanged from `crates/rt-core/src/mixer.rs`: a parameter is a *level*, not an
event. A dropped queue entry would leave a filter at the wrong frequency for
ever, and a queue deep enough never to drop one is a queue sized for a mouse. One
relaxed load per parameter per block cannot overflow and costs nothing.

### Curves

`ParamCurve` is a *perceptual* statement, carried to the UI so the shell does not
keep a second, drifting copy of every effect's taper. The stored value is always
the real value in the parameter's own unit.

- **`Log`** — frequencies, times, Q, ratio. `t ↦ min·(max/min)^t`. Requires
  `min > 0`. This is what makes a pixel of horizontal drag a constant musical
  interval everywhere, and what makes 0.5→1.0 feel like the same gesture as 5→10.
- **`Db`** — the value *is* decibels, and a linear slider in decibels is already
  the logarithmic amplitude taper the ear wants. Tapering it again is the classic
  mistake.
- **`Linear`** — pan, width, percentages.

### Smoothing, and what must not be smoothed

`smoothing_ms` is **per parameter**, because "10 ms for everything" is wrong in
both directions.

- Continuous parameters default to 10 ms — the same figure as the mixer's fader
  ramp, and the figure the research found developers naming.
- **Switches declare `0.0`.** There is no meaningful value halfway between "peak"
  and "RMS", and ramping towards one would spend 10 ms at a setting that does not
  exist. `ParamDef::choice` and `ParamDef::toggle` set it automatically, and
  `is_switch()` is what a UI checks to know not to animate.

The smoother's state lives in the node and survives block boundaries. That is not
incidental: the named symptom of getting it wrong is *"the bigger your buffer,
the worse it is"*.

### Readouts: the other direction

`ReadoutDef` is the mirror image of `ParamDef` — a value the node publishes and
the shell draws. Kept separate on purpose: collapsing them would let a UI "set" a
gain-reduction meter and a project file try to save one. The compressor publishes
`gr` and `gr_max`; the saturator publishes `auto_gain_db` and `latency`.

`gr_max` exists because of a specific, well-attested failure: a whole attack and
recovery can fall between two UI frames, so meters miss real compression. The
node takes the maximum reduction anywhere in the block and publishes that. **A
meter should draw `gr_max`, not `gr`.**

---

## 2. The effects

`plugins/effects`, five nodes plus an `EffectRegistry` service, contributed by
`EffectsPlugin` under the kernel id `effects`.

| `type_id` | Params | Readouts |
| --- | --- | --- |
| `effects.eq` | 20 | — |
| `effects.compressor` | 14 | `gr`, `gr_max` |
| `effects.delay` | 11 | — |
| `effects.saturator` | 8 | `auto_gain_db`, `latency` |
| `effects.utility` | 8 | — |

Every `type_id` and every parameter `id` is persisted, so renaming one
invalidates saved sessions. `plugins/effects/src/lib.rs` has a test that pins
them.

### Five properties all five hold

1. **Transparent when fresh.** Null-test clean at the defaults. (The delay is the
   documented exception: its mix defaults to 30 %, because a delay whose mix
   started at zero would appear broken. Its dry path is exactly 70 % of the input
   and its line is empty.)
2. **Click-free bypass**, bit-transparent once settled. A 10 ms equal-gain
   crossfade against a dry tap taken before the effect ran. Equal gain and not
   equal power because the two signals are the same signal.
3. **Zero allocation** on the audio thread, measured by a counting global
   allocator over swept parameters and bypass transitions.
4. **Bit-determinism**, and block-size independence from 1 to 512 frames.
5. **Ramps for parameters, block boundaries for switches.**

### EQ

Five fixed bands: HP (12/24 dB/oct) → low shelf → bell → bell → high shelf.
Fixed because a fixed chain is a fixed allocation, and this particular five is
what a mixer strip needs. Every band has an `enabled` switch, so "add a band" is
"turn one on" — and band indices are **stable by construction**, which is the
complaint about deleting a band breaking host automation, designed out rather
than worked around.

- RBJ cookbook coefficients, transposed direct form II, **`f64` state**. A 20 Hz
  highpass at 48 kHz has poles at radius ≈ 0.9974; in `f32` the coefficient
  quantisation there is comparable to the distance from the pole to the unit
  circle. Ten biquads in `f64` at 48 kHz is under half a megaflop per second.
- 24 dB/oct is **two staggered sections** at `1/(2 cos 22.5°)` and
  `1/(2 cos 67.5°)`, not two at the same Q. Asserted in `biquad.rs`.
- **Coefficients are recomputed per sample while a knob is ramping, and frozen
  otherwise.** Per-block recomputation is exactly the zipper users complain
  about: an 18 dB gain change becomes a staircase whose steps are the block size.
  The bill for doing it properly is two transcendentals per sample for one band
  for a hundredth of a second per gesture.
- `eq::magnitude_db_at` and `eq::magnitude_curve` evaluate the **same** coefficient
  set the audio path runs, so the future curve display and the tests read one
  implementation rather than two that can drift.

**Measured:** the audio path matches the curve to within 0.1 dB at 60 Hz, 200 Hz,
800 Hz, 2 kHz and 12 kHz for a four-band setting; a bell boosts its stated gain
at its centre to within 0.1 dB and leaves ±0.3 dB two decades away; the slope
switch measures −12.0 and −24.0 dB/octave.

### Compressor

Feed-forward, gain computer in dB, **smooth decoupled peak detector after the
gain computer, in the log domain** — Giannoulis, Massberg & Reiss (JAES 60(6),
2012), whose recommendation this is verbatim. Doing the ballistics in dB is also
the named fix for fast-attack distortion.

**Time constants are 10 % → 90 %**: `α = exp(−ln 9 / (τ·fs))`. There is no
industry standard, but 10→90 is the mainstream convention and the naive
`exp(−1/(τ·fs))` form is the 63 % one, which is **2.2× faster at the same
label**. Documented in the module and pinned by a test that asserts the two
conventions are different numbers.

Ratio is stored as a ratio and used as the **slope** `S = 1 − 1/R`, the standard
formulation and the one that stays finite as R grows. Auto-release runs two
release branches (0.25× and 4× the setting) and takes whichever is producing more
reduction — The Glue's "a slow one as a base compression value, and a fast one to
react to transients".

Auto-makeup is `reduction_db(0 dBFS)`, a function of threshold, ratio and knee
and **of nothing else** — signal-independent by construction — and is **off by
default**.

**Measured:** the static curve matches the threshold/ratio arithmetic to 0.5 dB
at −12, −6 and 0 dBFS; the soft knee is continuous in value and first derivative
across its whole width; a peak detector catches a 1 ms spike 3.0 dB harder than a
100 ms RMS window; a 200 Hz sidechain filter takes gain reduction from 12.6 dB to
under 6 dB on a bass-heavy signal; crest factor **15.86 dB → 12.59 dB** on a
drum-like pulse train.

### Delay

Tempo-synced by default, seven divisions × straight/dotted/triplet — two small
controls rather than a 21-item menu, which is what every vendor does.

- **The one-pole HP and LP are inside the feedback loop.** That is the whole
  feature: each pass filters again, so repeats darken and thin the way a tape
  generation does. Post-loop filtering would only colour the first repeat.
  6 dB/octave and not steeper, because a resonant filter inside a loop compounds
  its own ringing.
- Feedback reaches 110 % and is **soft-clipped in the loop**, so self-oscillation
  saturates instead of diverging.
- **Ducking is one knob** and attenuates the wet output *and* the feedback, which
  is what produces the blooming between notes. Its attack and release are fixed
  at 5 ms / 200 ms and deliberately not exposed — deleting those controls was the
  design win.
- A division change **crossfades** over 25 ms. Clicking a different note value is
  an *edit* and must be silent; repitching a continuous free-time drag is a
  performance gesture and is specified for the follow-up (the cubic interpolator
  it needs is already there).

**Memory.** The line is allocated for `MAX_DELAY_SECONDS = 8.0` and never grows:
3.1 MB per instance at 48 kHz, 6.1 MB at 96 kHz. Honouring every division at
every tempo the transport allows would mean a dotted whole note at 20 BPM —
eighteen seconds, 6.9 MB, times sixteen tracks. Eight seconds covers a dotted
whole note down to **45 BPM**; below that `fit_to_capacity` halves the division
until it fits, which keeps it on the grid.

**Measured:** a 1/8 echo at 120 BPM lands within 4 samples of 12 000; dotted 1/8
within 4 samples of 18 000; the tempo the transport reports is the tempo used, at
90, 130 and 174 BPM; repeats decay by a constant ratio; the fourth repeat is more
than 6 dB darker than the first at 6 kHz relative to 500 Hz; ping-pong puts the
first echo in the left channel at more than 20 dB of separation.

### Saturator

Three shapers named by mechanism — tape (symmetric, odd harmonics), tube
(asymmetric, even *and* odd), clip (hard with a quadratic knee).

- **Drive compensation is a static table**, built once in the factory by pushing
  a fixed deterministic noise sequence through each shaper at each whole decibel
  of drive and measuring the RMS change. A function of `(type, drive)` and
  nothing else: signal-independent, automatable, bit-reproducible. **RMS and not
  peak**, because saturation raises RMS while *lowering* peaks, so peak-matching
  would compensate in the wrong direction. The applied figure is published as a
  readout, because an auto-gain a user cannot see is one they switch off.
- **4x oversampling as two cascaded Kaiser-windowed halfband FIR stages.** The
  outer stage is 65 taps (it has to be clean from 20 kHz to 28 kHz, a narrow
  transition); the inner is 25 (nothing above 20 kHz is left to protect, so its
  transition may run to 76 kHz). Both are *designed* in the factory rather than
  tabulated, so the stopband is measurable — and measured.
- **FIR and not the cheaper polyphase IIR**, entirely because of the mix knob: an
  IIR halfband's group delay is frequency-dependent, so the dry path could not be
  delay-matched with an integer and a 50 % blend would comb the top octave. A
  linear-phase FIR's delay *is* an integer, so the dry path is one delay line.
- The DC blocker sits **after the shaper, at the oversampled rate**, so the
  decimation filter is never handed a DC step.

**Latency: 38 samples (0.79 ms) at 4x, 32 at 2x, and zero when off.** The engine
has no plugin delay compensation, so this is a documented cost of the quality and
is why the factor is a parameter. The internal dry path *is* aligned, so the mix
knob is phase-correct at every setting.

**Measured:** the round trip with no shaper is flat to 0.15 dB from 100 Hz to
18 kHz; the halfband stopband rejects more than 80 dB; the impulse peak lands
exactly at the reported latency; 4x buys **more than 20 dB** of alias suppression
against "off" on a 7 kHz tone through a hard clipper, measured at 1 kHz where
nothing harmonic can be; the tape shaper's second harmonic is 58 dB below the
tube shaper's at the same drive; drive compensation holds the level within
**1.5 dB** across all three shapers at 0, 6, 12, 24 and 36 dB of drive.

### Utility

Gain ±36 dB, pan, width 0–200 %, mono, per-channel polarity, DC blocker.

- **The pan law is the mixer's pan law** — equal power, centre at unity, the same
  `pan_gains` the track strips call. The research says the compromise answer is
  −4.5 dB and that mismatched laws are a known cause of mixes translating badly
  between DAWs; that finding is precisely why this does not introduce a *second*
  one. A pan-law choice belongs to the project, read by the mixer and the utility
  alike.
- **Width 100 % is bit-transparent** because the mid/side stage is *skipped*, not
  configured to an identity: `(L+R)/2 + (L−R)/2` is exact in real arithmetic and
  not in `f32`, and users null-test this.
- **Width 0 % is a plain mono sum at −6 dB per channel**, not power-compensated:
  engineers use it as a mono-compatibility check and expect centred content to
  keep its level.
- The DC blocker's coefficient is derived from the sample rate, so its corner
  stays at 5 Hz at 96 kHz instead of sliding down an octave.

---

## 3. What is not built yet

The line is clean: **nothing in the shell references `plugins/effects`.** The
crate builds, tests and lints on its own, and the app is exactly as it was.

The follow-up has to do five things. A partial implementation of the first two is
preserved at
`…/scratchpad/wip-phase4/` (an `app/src/effects.rs` and a diff against
`app/src/project.rs`); it is unwired and does not compile against the rest of the
shell.

### 3.1 The session model

`TrackDef` grows `inserts: Vec<InsertDef>` and `Project` grows
`master_inserts: Vec<InsertDef>`, where

```rust
pub struct InsertDef {
    pub id: u32,                  // stable across reordering; key a UI list on it
    pub type_id: String,          // "effects.eq"
    pub params: Arc<ParamBlock>,  // shared with the node the graph is running
}
```

`InsertDef: PartialEq` must be hand-written (compare id, `type_id` and the
parameter snapshot): `ParamBlock` holds atomics and cannot derive it, and
comparing the `Arc`s by pointer would make a loaded project unequal to the one
that saved it. Cap a chain at 8 inserts, so a webview sending "add" in a loop
gets a sentence rather than an allocation failure.

### 3.2 Graph assembly

`graph::track_chain` takes `Vec<Box<dyn RtNode>>` instead of `Option<...>`;
`TrackHost::build_spec` builds each chain from the registry against the blocks
the project owns. On the master, **the internal chain runs first and the hosted
`InsertSlot` last** — that keeps the hosted slot a single unconditional node at a
fixed position, which is what makes its rebuild-free swap work
(`app/src/insert.rs`). An insert naming an effect this build does not have is
**skipped with a warning**, not fatal: the missing-plugin policy, one level down.

### 3.3 Commands and DTOs

Two kinds of edit, and only one rebuilds:

| Command | Cost |
| --- | --- |
| `set_insert_param(target, index, param_id, value) -> f32` | One atomic store. No rebuild, no allocation. Returns what was **actually stored**, so a UI can snap a knob to the truth. |
| `add_insert` / `remove_insert` / `move_insert` | Graph rebuild, through the same routing `add_track` uses. |
| `get_effects() -> Vec<EffectInfoDto>` | Static catalogue, fetched once. |
| `get_insert_readouts(target, index)` | Polled at frame rate for meters — **separate** from the parameter values, or the IPC would send twenty knob positions sixty times a second to animate one bar. |

A structural edit leaves every hosted plugin without a node, exactly as a track
add does, so `AppState::edit_inserts` must route through the VST3 host, then the
CLAP host, then inline — the same three-way choice `AppState::add_track` makes,
and for the same reason. That means a new `ClapCommand::EditInserts` and
`Vst3Command::EditInserts` alongside `AddTrack`/`RemoveTrack`, each applying the
edit and then re-attaching.

**The property to test:** a CLAP instance placed on a track survives adding an
insert to a *different* track, and every other insert's parameters survive too.

### 3.4 `.mydaw` version 3

`TrackEntry` grows `inserts: Vec<InsertEntry>` and `ProjectFile` grows
`master_inserts`, both `#[serde(default)]`. An entry is

```json
{ "type_id": "effects.eq", "params": { "bell1.freq": 500.0, ... } }
```

A `BTreeMap` for the parameters, so the key order is deterministic and the
byte-identical round trip still holds. Write **every** parameter rather than only
the non-defaults, matching the precedent set by the v1→v2 migration test
(`"notes": []` is written explicitly rather than omitted): a default that changes
in a later build must not silently change saved projects.

**Why this bumps `format_version` to 3.** Step 8's rule is *do not* bump for a new
optional field — rule 1 already handles it. But step 8's own bump rule includes
"when a new field is *required* for correct playback", and an insert chain is: a
v3 project whose track carries a +12 dB shelf, opened by a build that ignores the
field, plays back wrong. Refusing it by name is the correct outcome.

Migration from v2 is *nothing* — `#[serde(default)]` gives empty chains, which is
precisely what a v2 file meant — plus the standard "stamp the current version on
load". It needs `app/tests/fixtures/v2.mydaw`, a real file written by this build
**before** the bump, committed verbatim, with a test asserting it still says
`"format_version": 2` (so nobody "helpfully" updates it) and that opening and
saving it writes version 3 with `"inserts": []` present.

Unknown parameter ids inside an entry are **ignored, not fatal** —
`ParamBlock::apply` already does this — which is the same forward-compatibility
rule at parameter granularity.

### 3.5 Hardware verification

Not run, because it needs the shell wiring. The four captures to take through
`debug_capture`, with the PNGs kept:

- drum + piano pattern, EQ low shelf +12 dB → the band boost shows in the capture
  spectrum;
- compressor on drums → `gr_max` non-zero and crest factor down (report both);
- dotted 1/8 delay on the synth at a known BPM → onsets on the grid, via the
  existing `debug-tap` onset machinery;
- saturator on the bass → the harmonic series appears.

The DSP claims behind all four are already measured offline in the crate's own
tests; the hardware run is what proves the *wiring* — that the chain is where the
project says it is, at the tempo the transport says.

---

## 4. Limitations

- **No plugin delay compensation.** The saturator's 0.79 ms at 4x is real and
  uncompensated across tracks. "Off" is offered and is genuinely free.
- **No sidechain input.** The compressor's detector is its own input; the
  sidechain filter shapes what it hears, not where it comes from.
- **No lookahead.** The research names 2 ms as the de-click value; it needs PDC
  first.
- **Stereo only**, like everything else in the engine.
- **Deferred but specified**, each with the evidence in the research document:
  the EQ's gain–Q interaction and static auto-gain; the compressor's lookahead,
  range clamp, hold and feedback blend; the delay's freeze and modulation (nearly
  free now that the cubic interpolator exists — the first thing to add); the
  saturator's drive-path low cut (the cheapest large win in the pack) and
  de-emphasis toggle; the utility's bass-mono, L/R swap and M/S solo.
