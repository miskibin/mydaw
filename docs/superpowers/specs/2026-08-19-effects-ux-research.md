# Effects UX research — what the most-loved plugins get right

**Status:** research input for the internal effects pack (EQ, compressor, delay,
saturator, utility). Written before a line of DSP, so the parameter list is a
consequence of the findings rather than a rationalisation of them.

**Method.** Five parallel surveys of vendor manuals, published measurements and
the places users actually argue: KVR Audio forums, Gearspace, Sound On Sound,
Production Expert, Tape Op, and the manuals for Pro-Q 4, Pro-C 2, Saturn 2,
EchoBoy, Ableton Live 12, Bitwig, TDR Nova/Kotelnikov and Valhalla Delay. Where a
site blocked automated fetching (Gearspace `/board/`, Reddit, vi-control,
valhalladsp.com), claims come through search excerpts and are marked as weaker
evidence by being paired with a primary source wherever one exists.

**Bias declared up front.** The user's instruction was *UX over UI*. So this
document is mostly about **what a parameter means, what range it has, what curve
it sits on, and what it does when you move it** — decisions that live in the
engine and that a UI cannot fix later. The interaction specs in each section are
written for whoever builds the shell, not built here.

---

## The five findings that changed our design

Before the per-effect detail, the things that came out of the research strongly
enough to be treated as rules rather than preferences.

1. **Level-matched A/B is the whole game.** The single loudest complaint in the
   saturation research is that drive makes things louder and louder sounds
   better: the KVR thread *"is a saturator without autogain worth it in 2023?"*
   has its author saying the loudness bump "fool[s] me every time into thinking
   the new saturated sound is better". Softube shipped the most-used free
   saturator in existence with no output control and **retrofitted gain staging
   under user pressure**. → Our saturator gets drive-compensated output, on by
   default. Our EQ and compressor get honest, level-matched bypass.

2. **Auto-gain must be signal-independent, or users switch it off.** 47% of that
   same KVR poll said they would rather trim by hand, and every stated reason is
   a description of a *level-tracking* auto-gain behaving like a slow compressor.
   FabFilter's own manual concedes its EQ auto-gain "is an educated guess based
   on the current EQ settings, and is **not** a dynamic process based on actually
   measured levels" — and nobody complains about that one. → Every compensation
   we compute is a function of the **parameters**, never of the live signal.

3. **A parameter is a level; a switch is not.** Research on both compressors and
   delays converges on the same implementation note: smoothing state must survive
   block boundaries (the tell for getting it wrong is "the bigger your buffer,
   the worse it is"), and discrete values must never be interpolated — "you can't
   smooth it while it's discrete". → `ParamDef::smoothing_ms` is per parameter,
   and a switch declares `0.0`.

4. **The gesture disambiguates the intent.** EchoBoy repitches when you *drag*
   the time knob and crossfades when you *click* a different note division. Both
   behaviours are correct; the user never has to choose. → Our delay crossfades
   on a division change (an edit) and is specified to repitch on a continuous
   free-time drag (a performance).

5. **Filter quality beats brute force.** The circulated comparative measurement
   from the aliasing threads is "apShaper at x4 oversampling outperforms
   StandardClip at x64", and developers in the same threads admit they cannot ABX
   4x against 8x. → We spend the budget on a measured halfband, not on a bigger
   ratio, and `tests/oversample.rs` measures the stopband rather than trusting a
   tabulated number.

---

## 1. Parametric EQ

### Top plugins, and the one-line reason each is loved

| Plugin | Why | Source |
| --- | --- | --- |
| FabFilter Pro-Q 3/4 | Direct manipulation of the curve on top of the analyser — "its graphical interface makes it a joy to use". The workflow is the moat, not the sound. | [SOS](https://www.soundonsound.com/reviews/fabfilter-pro-q-3), [Pro-Q 4 manual](https://www.fabfilter.com/downloads/pdf/help/ffproq4-manual.pdf) |
| TDR Nova / Nova GE | Free, dynamics on *every* band, and genuinely good "equal loudness" auto-gain. | [Nova manual](https://docs.tokyodawn.net/nova-manual/) |
| Three-Body Kirchhoff-EQ | Analog-matched curves with no HF cramping; "a little more fluid and smoother in operation". | [threebodytech](https://www.threebodytech.com/en/products/kirchhoffeq) |
| Sonnox Oxford EQ | Selectable gain/Q-interaction "Types" — Type 1 has "very little Gain/Q interaction… most like the original 4000 Series SSLs". | [Production Expert](https://www.production-expert.com/production-expert-1/sonnox-oxford-eq-gml-option) |
| Waves Q10 | Any filter type on any band, with a Q and gain range wide enough for surgery. | [Sweetwater](https://www.sweetwater.com/store/detail/Q10EQ--waves-q10-equalizer-plug-in) |
| Crave EQ | The cheap-workflow darling: band solo, a scalable dB axis, "smoother workflow, larger fonts". | [KVR](https://www.kvraudio.com/forum/viewtopic.php?t=545665) |

### What users praise

- **Dragging bands on the spectrum, not turning knobs.** Pro-Q's manual leads
  with "the easiest way to adjust them is simply by dragging them around". The
  criticism levelled at a new competitor on KVR was that users expect "not even
  having the panel with the knobs and just drag and modify-drag on the dots"
  ([KVR t=629380](https://www.kvraudio.com/forum/viewtopic.php?t=629380)).
- **Band creation infers the shape from where you clicked.** "Do you need a Notch
  curve? Just double-click in the far low area of the display… Shelving filter?
  Drag the yellow curve at the left or right end." Pro-Q's reported heuristic is
  low shelf below 30 Hz, high shelf above 10 kHz, bell in between, **Q 1.0 for
  bells and ~0.3–0.7 for shelves**, gain 0.
- **Band solo auditions the *source*, not the effect.** "In solo mode, you don't
  hear the effect of the EQ band itself, but instead you will hear the part of
  the frequency spectrum that is being affected by that band."
- **Gain–Q interaction, as an explicit toggle, on bells only.** "the Q
  automatically gets a bit narrower when gain is increased… Note: Gain-Q
  interaction only affects the Bell filter shape."
- **Zipper-free automation is now advertised.** Pro-Q markets "no zipper effects
  whatsoever" while dragging frequency, even in linear-phase mode.

### What users hate

- Zipper noise on automation. Named against Waves Linear Phase EQ.
- HF cramping: bilinear-transform filters are forced to unity at Nyquist, so a
  high bell gets "squash[ed]" above its centre
  ([Production Expert](https://www.production-expert.com/production-expert-1/what-is-eq-cramping-and-should-you-care)).
- Floating panels that occlude the band you are editing — "a major design flaw
  that I can't believe is still around after all these years".
- Bands renumbering on delete, which breaks host automation.

### Decisions we adopt

Five bands, fixed: **HP → low shelf → bell 1 → bell 2 → high shelf**. Fixed
rather than "add a band" because a fixed chain is a fixed allocation, and the
chain a mixer strip needs is this one.

| Parameter | Range | Curve | Default | Why |
| --- | --- | --- | --- | --- |
| `hp.enabled` | switch | — | **off** | A freshly inserted effect must be bit-transparent. |
| `hp.freq` | 20 Hz – 1 kHz | log | 80 Hz | |
| `hp.slope` | 12 / 24 dB/oct | switch | 24 dB/oct | The two everybody expects; 24 is the default a mix engineer reaches for. |
| `low.enabled`, `high.enabled` | switch | — | on | Shelves at 0 dB are already transparent. |
| `*.freq` | 20 Hz – 20 kHz | **log** | 120 Hz / 8 kHz | Pitch perception is logarithmic and the display axis is; a pixel of drag must be a constant musical interval everywhere. |
| `*.gain` | ±18 dB | **linear in dB** | 0 dB | dB is *already* the logarithmic amplitude taper. Tapering it again is the classic mistake. |
| `*.q` | 0.1 – 10 | **log** | 1.0 (bells), 0.7 (shelves) | 0.5→1.0 must feel like the same gesture as 5→10. **Q 1.0 for bells** specifically because that is Pro-Q's default and therefore the value users' hands are trained on; 0.707 reads as "too wide to be surgical". |
| `output` | ±18 dB | dB | 0 dB | |

- **Butterworth-staggered 24 dB/oct.** Two cascaded 12 dB sections at the *same*
  Q would put the corner 6 dB down and ripple in the passband. The correct pair
  is `1/(2 cos 22.5°)` and `1/(2 cos 67.5°)` = 0.5412 and 1.3066; this is
  asserted in `plugins/effects/src/biquad.rs`.
- **Coefficients recomputed per sample while a parameter is ramping**, and frozen
  otherwise. Recomputing per block would make an 18 dB gain change a staircase of
  block-sized steps, which is exactly the zipper users name. It costs two
  transcendentals per sample for the 10 ms after a knob moves and nothing at all
  the rest of the time.
- **Magnitude response is exposed as a non-RT function** so the future curve
  display and the tests read the *same* numbers the audio path uses, rather than
  a second implementation that can drift.

### Deliberately deferred, with the evidence for why they are wanted

**Gain–Q interaction.** Research is decisive that it should live in the
*parameter* layer rather than the gesture layer — FabFilter links the knobs, not
the parameters, and mastering engineer Rich Breen reported the resulting
inconsistency ("if I enter -20 into the numeric gain field, Q does not change"),
to which FabFilter confirmed "When you enter a value manually for the gain, the Q
will indeed not be adjusted automatically". Deferred from v1 only so that the
magnitude-response test can assert the plain RBJ response to ±0.1 dB; when it
lands it should be `Q_eff = Q_user · (1 + 0.7·|gain_dB| / 18)`, bells only, a
toggle defaulting off, and it must never write back to `Q_user`.

**Static auto-gain, dynamic bands, EQ match, linear phase, mid/side bands.** All
praised, all real work. Linear phase in particular is *not* a quality upgrade:
FabFilter's own guidance is "linear-phase processing is not better or more
transparent than normal processing, it is different! Linear-phase EQing is a
problem-solving tool".

### UI interaction spec (for the shell owners)

The canvas is log-frequency on X, linear-dB on Y, analyser behind the curve.

| Gesture | Action |
| --- | --- |
| Hover empty canvas | Ghost preview of the band that would be created here |
| **Double-click** canvas | Create/enable a band at that frequency, **gain 0**, shape inferred by zone: far left → HP, below ~30 Hz → low shelf, above ~10 kHz → high shelf, otherwise the nearest free bell |
| **Drag the composite curve** | Set that band's gain in the same gesture that created it |
| **Drag a band dot** | Frequency and gain together |
| **Alt + drag** | Constrain to one axis |
| **Wheel over a band** | Q (for the HP: step the slope) |
| **Ctrl/Cmd + wheel** | Gain |
| **Shift + anything** | Fine-tune |
| **Alt + click a dot** | Toggle that band's `enabled`; the dot dims and stays in place |
| **Double-click a dot** | Numeric entry; Tab steps freq → gain → Q; accept `1k`, `2.5k`, `A4` |
| Band solo (hold) | Audition the *source* band-passed to that band's range — not the EQ's effect |
| Drag the frequency ruler | Zoom; double-click resets |
| dB range selector | ±3 / ±6 / ±12 / ±24, auto-expanding when a drag exceeds it |

Hard requirements that came out of the complaints: **the floating band panel must
never occlude a band dot**; every band keeps a **stable index and colour**; a band
dragged out of view must remain reachable (Kirchhoff's unrecoverable-state bug).

---

## 2. Compressor

### Top plugins, and the one-line reason each is loved

| Plugin | Why | Source |
| --- | --- | --- |
| FabFilter Pro-C 2 | You can *see* the compression — level display, transfer curve and GR meter on **one shared dB scale**. | [Pro-C displays](https://www.fabfilter.com/help/pro-c/using/displays) |
| TDR Kotelnikov GE | Split peak/RMS release with a Crest control — "stunning to tailor the compression, and super fast to dial once you're used to it". | [Kotelnikov GE manual](https://docs.tokyodawn.net/kotelnikov-ge-manual/) |
| 1176 emulations | 20 µs attack, feedback topology, and "all-buttons-in" as a one-gesture character preset. | [Wikipedia 1176](https://en.wikipedia.org/wiki/1176_Peak_Limiter) |
| LA-2A emulations | Two-stage program-dependent release (~60 ms for the first 50%, then 0.5–5 s) behind two knobs. | [Wikipedia LA-2A](https://en.wikipedia.org/wiki/LA-2A_Leveling_Amplifier) |
| Cytomic The Glue | Auto-release plus Range — "makes it easy to apply compression that just tightens up the sound". | [SOS](https://www.soundonsound.com/reviews/cytomic-glue) |
| Klanghelm DC8C | A continuous feedback↔feed-forward morph: "the compressor of my wet/dry feed forward/backward dreams". | [KVR reviews](https://www.kvraudio.com/product/dc8c-by-klanghelm/reviews) |

### What users praise, and what they complain about

- **The gain-reduction meter is the primary readout, and the complaint is about
  *numbers*, not existence.** "If I'm using 6db of compression on a track, I
  wanna know somehow!"; "A GR meter (a normal one, something with visible
  numbers)". Ableton's Compressor is criticised specifically because its GR bar
  has no dB markings.
- **Meter ballistics miss real compression.** "even though the meter doesn't
  react at all there is already compression happening"; "sometimes the screen
  never displays any change at all" — because the whole attack and recovery falls
  between two UI frames. The fix named by developers in that thread is to feed
  *every sample* into the display smoother and **take the max GR within each
  buffer**.
- **Useful GR resolution is 0–12 dB, not 0–60.** "Most engineers are looking for
  an average of about 3 dB to 5-6 dB of gain reduction"; 20 dB is "a special
  effect".
- **Fast-attack distortion is understood and has a known fix.** "with short
  release times, say 5 ms, the compressor can be triggered by the peaks of low
  frequency waves", and "a 50 ms hold time will prevent distortion on signals
  down to 20 Hz". The other named remedy is to do the ballistics **in the dB
  domain**.
- **Sidechain HPF is the pumping fix**, and the recurring misconception is worth
  designing against: "The filter only affects what frequencies trigger the
  compression action" — users repeatedly think it EQs the output.
- **Auto-makeup is polarising and leans negative.** "I find auto makeup gain flat
  out obnoxious and a guaranteed way to screw up a mix". What users say they want
  instead is level-matched bypass: "You set the make up gain by bypassing the
  unit and checking that over-all levels are same."
- **A mix knob must be internal and must be one knob.** "The wet/dry option in
  FabFilter's Pro-C is essential and there's almost no time I use fully wet".
  Even Pro-C got the *interaction* wrong: its split-knob mix drew "This makes it
  slow and clumsy to do parallel compression".

### The topology decision, with a citation

Giannoulis, Massberg & Reiss, *"Digital Dynamic Range Compressor Design — A
Tutorial and Analysis"*, JAES 60(6):399–408 (2012)
([AES](https://aes2.org/publications/elibrary-page/?id=16354)):

> the authors recommend **feed-forward** algorithms because they are stable and
> predictable. The **detector is placed in the log domain after the gain
> computer** since this generates a smooth envelope, no attack lag, and a
> variable knee width.

That is exactly what we build. Users' preference for feedback "musicality" is
real and consistently reported — feedback is "smoother and more musically
satisfying", feed-forward "tighter and more punchy" ([SOS](https://www.soundonsound.com/sound-advice/compressor-topology))
— but the original justification for feedback was that analog gain cells were
imprecise, and a software gain cell is exact. DC8C shows the answer is a *blend*
rather than a switch; that is a v2 parameter, noted here so it is not
re-litigated.

### Decisions we adopt

| Parameter | Range | Curve | Default | Why |
| --- | --- | --- | --- | --- |
| `threshold` | −60 … 0 dB | dB | −18 dB | |
| `ratio` | 1:1 … 20:1 | **log** | 4:1 | Stored as ratio, used internally as **slope** `S = 1 − 1/R`, which is the standard formulation and is numerically stable towards ∞:1. |
| `attack` | 0.05 … 100 ms | **log** | 10 ms | Below 0.1 ms deliberately: the 1176 is 20 µs and The Glue starts at 0.01 ms. |
| `release` | 5 … 2000 ms | **log** | 100 ms | |
| `auto_release` | switch | — | off | The Glue's two-time-constant scheme: "a slow one as a base compression value, and a fast one to react to transients". |
| `knee` | 0 … 18 dB | linear | 6 dB | Total width, centred on the threshold. |
| `detector` | peak / RMS | switch | peak | Peak for drums and transients, RMS for vocals and levelling — the split the forums describe consistently. |
| `rms_ms` | 1 … 100 ms | log | 10 ms | Only meaningful in RMS mode. |
| `sc_hpf` | off (20) … 500 Hz | log | off | One biquad in the detector. The standard pumping fix, missing from most stock compressors. |
| `makeup` | ±24 dB | dB | 0 dB | |
| `auto_makeup` | switch | — | **off** | Deliberately off. See the quote above. |
| `mix` | 0 … 100 % | linear | 100 % | One knob, on the main panel, never split. |

**Time-constant convention: 10 % → 90 %**, i.e. `α = exp(−ln 9 / (τ·fs))`. This
matters more than it looks. There is no industry standard — "after decades of
designing sundry dynamics processors there is not a single common standard"
([GroupDIY](https://groupdiy.com/threads/calculating-attack-release-times-help-i-give-up.45371/))
— but the mainstream DSP-tutorial convention is 10→90, and the naive
`exp(−1/(τ·fs))` form is the 63 % one, which is **2.2× faster at the same label**.
A user A/B-ing our compressor against Pro-C at "10 ms" would conclude ours
"sounds different at the same settings", and they would be right.

**Gain reduction is published as an atomic**, both the instantaneous value and
the **per-block maximum**, because the per-block maximum is what a meter must
draw — see the "the meter never moves" complaint above.

Deferred with evidence: lookahead (the de-click value is 2 ms, and it needs
plugin delay compensation the engine does not have yet), Range/max-GR clamp,
Hold, feedback blend, Kotelnikov's dual peak/RMS release with a Crest offset,
delta listen, sidechain audition.

### UI interaction spec (for the shell owners)

1. **GR meter large and always visible**, scaled **0 to −12 dB** by default and
   auto-expanding to −24/−40 when exceeded (Kotelnikov: "Scale automatically
   grows with the amount of gain reduction").
2. **A numeric max-GR readout beside it, click to reset.** This is the specific
   thing users say is missing everywhere, and the engine already publishes it.
3. Meter ballistics: instant attack, decay ≈ **1 dB / 100 ms**, peak-hold ~1 s.
   Draw the per-block maximum the engine publishes — never sample the
   instantaneous value at frame rate.
4. **Transfer curve with a live operating point**: input dB on X, output dB on Y,
   threshold/ratio/knee visible on it, and the curve lit at the current input
   level.
5. **One shared dB scale** across the GR meter, the transfer curve and the level
   meters — FabFilter names this as the thing that "makes it easier than ever to
   find the proper compression settings".
6. **Level-matched bypass.** What users want instead of auto-makeup.
7. Mix as a single knob on the main panel.
8. Do **not** invert the attack/release knob direction. Users tolerate the 1176's
   backwards knobs only because it is the archetype; a corrected 1176 is
   described as "very confusing".

---

## 3. Tempo-synced delay

### Top plugins, and the one-line reason each is loved

| Plugin | Why | Source |
| --- | --- | --- |
| Soundtoys EchoBoy | Timing controls (Groove/Feel/Accent) that make repeats "feel musical rather than mechanical", plus 30 character Styles. | [EchoBoy manual](https://www.soundtoys.com/wp-content/uploads/EchoBoy-Manual.pdf) |
| Valhalla Delay | Enormous range with a GUI that has "plenty of available tweaking…without too much clutter", plus one-knob Duck. | [Tape Op](https://tapeop.com/reviews/gear/132/valhalladelay-plug-in) |
| FabFilter Timeless 3 | Deepest modulation, 6 filters in the delay path, explicit Tape-vs-Stretch time behaviour. | [FabFilter](https://www.fabfilter.com/help/timeless/using/delaycontrols) |
| Waves H-Delay | Speed: "loads fast, sounds good immediately, and doesn't require deep tweaking". | [MusicRadar](https://www.musicradar.com/reviews/tech/waves-h-delay-215344) |
| Ableton Echo | Everything in one device, with explicit Repitch/Fade/Jump semantics. | [Live manual](https://www.ableton.com/en/live-manual/12/live-audio-effect-reference/#echo) |
| Baby Audio Comeback Kid | Ducking as the headline — "I honestly don't understand why it's not on more delay plugins". | [Everything Recording](https://everythingrecording.com/review-baby-audio-comeback-kid/) |

### What users praise

- **Sync should be on by default, and the division picker is two widgets, not
  one.** Every vendor splits it: EchoBoy's `TIME | NOTE | DOT | TRIP` row,
  Ableton's "Notes, Triplet, Dotted", Comeback Kid's "Straight, Dotted, Triplet,
  or Free". `[base] × [straight|dotted|triplet]` gives 21 values from two small
  controls instead of a 21-item dropdown. The ms-first pain point is real: a KVR
  user asking for note-value display said "setting the timing in ms…can be quite
  hard".
- **Damping in the feedback loop is what makes a delay sit in a mix.** EchoBoy:
  the cuts "can be very beneficial in shaping delays and their **sense of
  distance to blend better with the original signal**". The frequencies mixers
  actually use: high-pass the repeats at **250–400 Hz**, low-pass at **4–8 kHz**.
- **Filter placement matters and is nearly free.** EchoBoy's Style Editor
  separates "Gain" (the tone of the first repeat) from "Decay" (how the tone
  changes per repeat) — and *placing the filter inside the loop gives you the
  Decay behaviour for nothing*. That is the praised tape-generation loss, at the
  cost of choosing where to put two one-poles.
- **Feedback past unity is a feature, and taming it is mandatory.** EchoBoy
  documents self-oscillation as a feature and *also* warns "High settings on the
  Feedback control can boost the output level from EchoBoy significantly"; it
  ships a Style literally called `Limited`. The counter-example is Voxengo Tempo
  Delay, where too much feedback "result[s] in an extremely clipped signal". A
  soft clipper inside the loop is not optional.
- **Ducking crossed from nice-to-have to expected.** Valhalla shipped without it
  and added it under user pressure; Baby Audio built a product around it. The
  decisive UX finding is *one knob, not threshold/attack/release*: Valhalla's
  duck gives "echo level tracking of the input signal volume **without
  threshold/attack/release controls**", and Ableton's threshold-and-release
  version is the less-praised one. And it should duck **both** the wet output and
  the feedback — that is what produces the praised behaviour where "the delayed
  signal **blooms** and becomes more apparent between the notes".

### What users hate

- Zipper noise and clicks when the delay time moves: "the stored data being
  recalled from the delay buffer jumps around as the delay time knob moves,
  causing clicks, pops and zips".
- Unwanted pitch shift on a tempo change — there is an entire VI-Control thread
  titled "Tape delay sound that doesn't pitch shift when delay time changes".
- Metallic resonance from short synced delays, which EchoBoy shipped a whole
  "Prime Numbers" feature to defeat.

### The time-change decision

All four major vendors implement **both** behaviours (Ableton Repitch/Fade/Jump,
FabFilter Tape/Stretch, Bitwig Repitch/Fade). EchoBoy's is the smartest and it is
*not* a mode switch — two sentences from its manual, four paragraphs apart:

> The Echo Time control also reacts like in hardware – when you turn the Echo
> Time knob in real-time, the **pitch will audibly change** just like in an old
> analog delay box.

> When programming Echo Time using one of the Note settings, EchoBoy will
> **smoothly crossfade** to any new note values.

**The gesture disambiguates the intent.** Dragging a time knob is a performance
gesture and should repitch; clicking 1/8 → dotted 1/8 is an *editing* gesture and
must be silent. → We crossfade on a division change. Repitch on a continuous free
drag is specified for the shell and left as an engine follow-up.

### Decisions we adopt

| Parameter | Range | Curve | Default | Why |
| --- | --- | --- | --- | --- |
| `sync` | switch | — | **on** | The pain point above. |
| `division` | 1/64 … 1/1 (7 positions) | switch | 1/8 | 1/64 is the real floor; below it is flanger territory. |
| `modifier` | straight / dotted / triplet | switch | straight | Separate control. Dotted 1/8 is the single most-cited musical value. |
| `time_ms` | 1 … 4000 ms | log | 375 ms | Free mode. |
| `feedback` | 0 … 110 % | linear | 35 % | Past unity is reachable and marked, never arrived at by accident. Soft-clipped in the loop. |
| `damp_lp` | 200 Hz … 20 kHz | log | 6.5 kHz | One pole, **inside** the loop → cumulative per-repeat darkening. |
| `damp_hp` | 20 … 2000 Hz | log | 150 Hz | Also inside the loop. |
| `mode` | stereo / ping-pong | switch | stereo | Ping-pong sums the input to mono and uses cross-only feedback, per EchoBoy and FabFilter. |
| `duck` | 0 … 100 % | linear | 0 % | **One knob.** Attenuates the wet output *and* the feedback. Internal attack/release fixed at 5 ms / 200 ms. |
| `mix` | 0 … 100 % | linear | 30 % | |

**Memory.** The line is allocated for the longest division at the lowest tempo we
guarantee. `MIN_BPM` is 20, and a dotted whole note is six beats, so honouring
every division at every tempo would need 18 s of stereo buffer — 6.9 MB per
instance at 48 kHz, 13.8 MB at 96 kHz, times sixteen tracks. We allocate **8
seconds** instead (3.1 MB at 48 kHz, 6.1 MB at 96 kHz) and halve a division that
does not fit, which keeps it on the grid. Eight seconds covers a dotted whole
note down to **45 BPM**; below that the longest two divisions fold. Documented,
tested, and a far better trade than a delay that costs 14 MB a track.

Deferred with evidence: freeze/infinite hold (universal, but a performance
control that wants a UI), modulation/wow (praised, and nearly free *once the
interpolator exists* — ours does, so this is the first thing to add), per-channel
independent divisions, continuous cross-feedback, time offset/swing, prime-number
detuning, diffusion.

### UI interaction spec (for the shell owners)

1. Division as **two segmented rows** — `[1/64][1/32][1/16][1/8][1/4][1/2][1/1]`
   and `[Straight][Dotted][Triplet]` — never a 21-item dropdown.
2. **Always show the resolved milliseconds** next to the division, and let the
   user type into it.
3. Feedback knob: a detent and tick at 100 %, with a distinct band from 100→110.
4. Duck is a **single large knob** near Mix. Do not build a threshold/attack/
   release panel; deleting those was the design win.
5. Two filter knobs with one response curve drawn over the wet spectrum, and a
   small always-visible **topology strip** showing that the filters are inside
   the loop — Baby Audio publishes its flowchart precisely because this is not
   obvious, and it is why the damping "sounds different than my EQ".
6. Wet-only is a **mode toggle**, not "turn Mix to 100": both EchoBoy and Ableton
   tell users to do this in prose, so make it a button.

---

## 4. Saturator

### Top plugins, and the one-line reason each is loved

| Plugin | Why | Source |
| --- | --- | --- |
| Soundtoys Decapitator | Five named analog models and one Drive knob — strong character with almost no decisions. | [manual](https://www.soundtoys.com/wp-content/uploads/Decapitator-Manual.pdf) |
| FabFilter Saturn 2 | 28 styles × 6 bands for people who want to *build* a saturation. | [SOS](https://www.soundonsound.com/reviews/fabfilter-saturn-2) |
| Softube Saturation Knob | One knob, three frequency-weighted modes, free, and it sounds good. | [Softube](https://www.softube.com/plug-ins/saturation-knob) |
| Klanghelm SDRR / IVGI | Four well-differentiated modes, dynamic input-dependent behaviour, clean anti-aliasing. | [Klanghelm](https://klanghelm.com/contents/products/SDRR) |
| Black Box HG-2 | Two stackable tube stages (pentode = even, triode = odd) you push against each other. | [SonicScoop](https://sonicscoop.com/new-software-review-black-box-hg-2-plugin-alliance/) |
| ToneProjects Kelvin, Cytomic The Scream | Picked *specifically* for anti-aliasing — "like a posh Decapitator to me". | [KVR t=618193](https://www.kvraudio.com/forum/viewtopic.php?t=618193) |

### What users praise, and what they complain about

- **Drive-compensated output is the highest-leverage feature in the category.**
  Decapitator: "Turning on the **Auto Output** switch will decrease the output
  level as you increase the drive". Saturn 2: "**While increasing the drive, the
  output level will be adjusted automatically**". And the market datapoint that
  settles it — Softube shipped Saturation Knob with no output control, drew a
  Gearspace thread titled "Why no gain compensation on Softube Saturation Knob?"
  ("it's impossible to tell the real effect of the plugin on a track when it gets
  so much louder"), and **added input gain, output gain, headroom and metering in
  a 2021 update**.
- **But it must be defeatable and signal-independent.** 47 % of the KVR poll
  preferred manual trim, with reasons like "It usually doesn't work 100 %
  accurately" and "I deactivate auto-gain immediately in any plugin that provides
  it" — all descriptions of level-tracking auto-gain acting like a slow
  compressor and eating the dynamic benefit. Decapitator's is drive-indexed and
  deterministic, and nobody complains about it.
- **Aliasing is a real, specific, named complaint.** There is a KVR thread whose
  title is the demand: *"saturation/distortion with excellent anti-aliasing"*.
  Decapitator is the most-named offender — "it's quite ok up to around 7-8k …
  **after 10k it gets to foldback heaven**". Users describe it as "extra
  uncorrelated '**false**' harmonic information" in the "3 to 12 kHz region", and
  the argument that persuades people is cumulative: "compounded aliasing" across
  many tracks means the "mix will sound awful".
- **The tone control people actually want is pre-emphasis.** Softube's
  three-position mode switch *is* a tilt in front of the shaper: "Keep High"
  "distorts the low end more, keeping the high frequencies relatively clean";
  "Keep Low" lets you "squash a drum loop but keep the bass drum fairly intact".
  It is the most-praised design decision on the most-used free saturator, and it
  is nothing but a tilt.
- **Multiband is a minority want and a majority tax.** Saturn 2 "still takes some
  experience to figure out"; Gearspace called the feature level "total overkill".

### The oversampling numbers

The practical consensus is explicitly *not* a fixed ratio:

> **For mild saturation, just 2x or 4x might be plenty. Once we start pushing
> sinewaves into squarewaves, even 16x or whatever might still result in
> significant aliasing.** ([KVR t=601534](https://www.kvraudio.com/forum/viewtopic.php?t=601534))

The rule of thumb is "the alias frequencies drop about 6 dB for every 2X you
oversample" — which is the *worst case*, corresponding to a `1/n` harmonic series
(hard clipping). A `tanh` decays far faster than `1/n`, so each doubling buys much
more. That is the same statement as the quote above, in two regimes.

The geometry is more useful than any single number. At 4x from 48 kHz the shaper
runs at 192 kHz with an internal Nyquist of 96 kHz, and the decimator removes
everything between 24 and 96 kHz. So only harmonics generated **above 96 kHz** can
fold back:

| Fundamental | First harmonic that can fold | Consequence |
| --- | --- | --- |
| 1 kHz | ~#96 | Unmeasurable for any soft shaper. |
| 5 kHz | ~#19 | Fine for tanh, marginal for hard clip. |
| 10 kHz | ~#9 | Where hard clipping audibly folds — matching the Decapitator report exactly. |

Published measurement: Kahles, Esqueda & Välimäki, *"Oversampling for Nonlinear
Waveshaping: Choosing the Right Filters"* (AES, 2019,
[e-lib 20485](https://www.aes.org/e-lib/browse.cfm?elib=20485)) evaluates filters
at 8x using **A-weighted noise-to-mask ratio** rather than raw stopband dB — the
right metric, and the reason "when anti-aliasing is basically just starting to
waste CPU" is a real threshold and not laziness.

### Decisions we adopt

| Parameter | Range | Curve | Default | Why |
| --- | --- | --- | --- | --- |
| `type` | tape / tube / clip | switch | tape | Three, named by **mechanism** and not by adjective. Tape = symmetric `tanh` (odd harmonics), tube = asymmetric (even *and* odd), clip = hard clip with a soft knee. |
| `drive` | 0 … 36 dB | dB | 0 dB | |
| `auto_gain` | switch | — | **on** | And its applied compensation is readable. |
| `tone` | ±12 dB tilt | dB | 0 dB | One tilt pair pivoting at 700 Hz, in front of the shaper. |
| `mix` | 0 … 100 % | linear | 100 % | |
| `output` | ±24 dB | dB | 0 dB | |
| `oversample` | off / 2x / 4x | switch | **4x** | "Off" is honest and zero-latency, and it is what `tests/saturator_dsp.rs` measures the 4x aliasing floor *against*. |

**Auto-gain is a static, drive-indexed table.** Built at node construction (non-RT)
by pushing a fixed deterministic noise sequence through each shaper at each drive
step and measuring the RMS change; interpolated at runtime. Signal-independent,
deterministic, automatable, bit-reproducible across renders, and matching the one
implementation nobody complains about. **RMS, not peak** — saturation raises RMS
while *lowering* peaks, so peak-matching compensates in the wrong direction.

**Linear-phase FIR halfbands, not the cheaper IIR.** An allpass IIR halfband is
dramatically cheaper for the same stopband and has single-digit latency, and for a
saturator alone it would win. It loses *here* because of the mix knob: an IIR
halfband's group delay is frequency-dependent, so the dry path cannot be
delay-matched with an integer and a 50 % blend would comb the top octave — which
is precisely the "the mix knob mysteriously thins the highs" failure. A
linear-phase FIR has an integer delay, so the dry path is one delay line.
Measured cost: 24 samples (0.5 ms) at 4x, which we spend deliberately.

**A DC blocker after the shaper, at the oversampled rate.** Asymmetric shapers
"create a DC offset proportional to bias even at silence, and if not subtracting
the offset at the output… you'll hear thumps at parameter changes"
([KVR t=502609](https://www.kvraudio.com/forum/viewtopic.php?t=502609)).

Deferred with evidence: a low cut on the *drive path only* (cheap, and it delivers
the single biggest multiband use case — "keep my kick intact" — for ~1 % of
multiband's cost; the first thing to add), de-emphasis toggle, 8x/16x, ADAA for
the clip shaper, multiband, modelled styles.

### UI interaction spec (for the shell owners)

1. **One oversized Drive knob**, visually dominant. The plugin should be usable by
   touching exactly one control.
2. Type as a **three-item segmented control**, never a dropdown.
3. Tone as a horizontal slider with a **centre detent**, drawn dark → bright.
4. **Draw the transfer curve for the current type and drive, with a live dot** on
   it. Nearly free to render, it *teaches* what drive does, and it is the
   signature element of every plugin in the category people call fun.
5. **Show the applied auto-gain in dB** (`AUTO −7.4 dB`). An auto-gain you cannot
   see is an auto-gain users switch off.
6. Show the oversampling factor and the reported latency in samples somewhere
   unobtrusive — the people who care about aliasing are exactly the people who
   will look, and telling them buys credibility.
7. **Bypass must compare against the compensated output**, or the auto-gain
   feature is decorative.

---

## 5. Utility

### Top plugins, and the one-line reason each is loved

| Plugin | Why | Source |
| --- | --- | --- |
| Ableton Utility | The reference "everything I need in one small device" — gain, balance, width, mono, per-channel polarity, DC filter, bass mono. | [Live manual](https://www.ableton.com/en/manual/live-audio-effect-reference/) |
| Bitwig Tool | Gain ±36 dB, width 0–200 %, per-channel polarity, high-resolution output meters. | [Bitwig](https://www.bitwig.com/userguide/latest/utility/) |
| Voxengo MSED | **Inline** M/S mode so you do not need two instances in series; free; a correlation meter you can trust; zero latency. | [Voxengo](https://www.voxengo.com/product/msed/) |
| Brainworx bx_control V2 | Mono Maker "with no loss of power or fidelity", and four solo buttons (M/S/L/R) "soloed in place". | [Plugin Alliance](https://www.plugin-alliance.com/products/bx_control-v2) |
| Melda MUtility | Pan law as an actual parameter — "Advanced Pan settings like Pan Law… not found in DAWs regularly". | [Melda](https://www.meldaproduction.com/MUtility) |

### What users praise

- **Width where 100 % is bit-transparent.** With `M = (L+R)/2`, `S = (L−R)/2`,
  `L = M+S`, `R = M−S`, the reconstruction is exact — no normalisation fudge, no
  half-decibel drift. Users null-test this and complain when it fails.
- **0 % is a plain mono sum, not a power-compensated one.** Engineers use width 0
  as a mono-compatibility check and expect centred content to keep its level.
- **DC blocking is table stakes**, and the cutoff matters: the practical band is
  1–10 Hz, "for 5 Hz we already get 80 ms" of settling, and "the **phase-shift**
  from the DC blockers is really the most important component here, as it extends
  much higher than the actual amplitude response losses". A blocker at 20 Hz is
  audible on a kick.
- **Polarity is two independent buttons**, Ø L and Ø R, never one global one.

### The pan-law question, and why we do not answer it here

The research is clear that the compromise answer is −4.5 dB centre attenuation —
"half-way between the two previous ideals", with 3 dB being right for correlated
speaker playback and 6 dB for mono summing — and that mismatched, undocumented
defaults are "one common reason why mixes constructed on different DAWs but with
apparently identical fader and pan control settings can sound subtly but
noticeably different"
([SOS](https://www.soundonsound.com/sound-advice/q-what-pan-law-setting-should-use)).

**We use MYDAW's existing pan law anyway**: equal power (−3 dB), normalised so
centre is unity, exactly as `crates/rt-core/src/mixer.rs` documents it and the
track strips implement it. The reason is the finding itself. Two pan laws inside
one DAW would be a worse version of the cross-DAW problem the article describes,
in a place where the user has even less reason to suspect it. A pan-law *choice*
is worth having later — as a project-wide setting that the mixer and the utility
both read, never as a per-effect parameter.

### Decisions we adopt

| Parameter | Range | Curve | Default | Why |
| --- | --- | --- | --- | --- |
| `gain` | ±36 dB | dB | 0 dB | Bitwig's range. ±12 is the most-complained-about DAW trim range. |
| `pan` | −1 … +1 | linear | 0 | The mixer's law. Balance on a stereo source. |
| `width` | 0 … 200 % | linear | 100 % | Side gain. 100 % bit-transparent, 0 % a plain mono sum. |
| `mono` | switch | — | off | A separate button from width 0 %, so a mono check does not destroy the width you dialled. |
| `invert_l`, `invert_r` | switch | — | off | Independent, per channel. |
| `dc_block` | switch | — | **off** | Transparent by default; 5 Hz, first order, coefficient derived from the sample rate so it does not slide an octave at 96 kHz. |

Deferred with evidence: bass mono via a highpass on the **side channel only** —
mid untouched, so zero crossover phase error and no level dip, which is literally
the "no loss of power or fidelity" claim bx_control makes; L/R swap; M/S/L/R solo
in place; mid and side gains; a correlation meter (which should sit next to any
width control).

### Cross-effect conventions we adopt everywhere

These came out of the utility survey but apply to all five.

- **A freshly inserted effect must be bit-transparent.** Users complain when a
  "default" carries leftovers, and "an init button [is] a feature that every
  plugin should have". Every one of our five is a null-test-clean pass-through at
  its defaults.
- **Bypass is a crossfade, not a switch.** The VST3 specification puts it on the
  plugin: it "must take care of **artifact-free switching (ramping/fade-in/
  fade-out, parallel processing or algorithm changes)**"
  ([Steinberg](https://steinbergmedia.github.io/vst3_dev_portal/pages/FAQ/Processing.html)),
  and FabFilter markets "**soft bypassing to avoid clicks**" across its whole
  line. Logic users report "horrible click sounds when bypassing/unbypassing".
  → Every effect has a `bypass` parameter that drives a 10 ms equal-gain
  crossfade, and is bit-transparent once settled.
- **Smoothing state must survive block boundaries.** The named tell for getting
  this wrong is "the bigger your buffer… the worse it is". → `ParamSmoother`
  keeps its position in the node, exactly as `TrackStrip` does for a fader.
- **Discrete values are never interpolated.** "You can't smooth discrete values,
  **it just doesn't work**". → `ParamDef::choice` declares `smoothing_ms: 0.0`,
  and a mode change lands on the block boundary.
- **Mix belongs on non-linear, dynamic and time-based effects; not on gain, pan
  or EQ.** Where the plugin lacks one, users demand it from the *host* — there
  are standing feature requests in Cubase and Studio One for exactly that, and
  REAPER puts a wet/dry knob on every plugin window.

---

## What this research did not settle

- **Whether users can hear 4x versus 8x oversampling.** Developers in the thread
  admit they cannot ABX it. We ship 4x and an honest "off", and measure the
  difference rather than asserting it.
- **Whether feedback-topology "musicality" survives in software.** The perceptual
  reports are consistent and the engineering justification is gone. DC8C's
  continuous blend suggests the answer is "let the user dial it", which is a v2
  parameter, not a v1 argument.
- **What pan law MYDAW should have project-wide.** −4.5 dB is the researched
  compromise; we are on −3 dB for consistency with the mixer, and changing it is
  a project-level decision with a migration attached, not an effect's business.

---

## Sources

EQ: [Pro-Q 4 manual](https://www.fabfilter.com/downloads/pdf/help/ffproq4-manual.pdf) ·
[SOS Pro-Q 3](https://www.soundonsound.com/reviews/fabfilter-pro-q-3) ·
[TDR Nova](https://docs.tokyodawn.net/nova-manual/) ·
[Nova GE](https://docs.tokyodawn.net/nova-ge-manual/) ·
[Kirchhoff-EQ](https://www.threebodytech.com/en/products/kirchhoffeq) ·
[Sonnox Oxford EQ](https://sonnox.com/products/oxford-eq) ·
[Production Expert on cramping](https://www.production-expert.com/production-expert-1/what-is-eq-cramping-and-should-you-care) ·
[Rane Note 154 (proportional Q)](https://www.ranecommercial.com/legacy/note154.html) ·
[KVR t=629380](https://www.kvraudio.com/forum/viewtopic.php?t=629380) ·
[KVR t=533282 (auto-gain)](https://www.kvraudio.com/forum/viewtopic.php?t=533282) ·
[FabFilter forum on gain-Q](https://prod.fabfilter.com/forum/topic/7329/pro-q3-gainq-dependency-squirrely-behavior)

Compressor: [Giannoulis, Massberg & Reiss, JAES 60(6) 2012](https://aes2.org/publications/elibrary-page/?id=16354) ·
[Pro-C 2 displays](https://www.fabfilter.com/help/pro-c/using/displays) ·
[Kotelnikov GE manual](https://docs.tokyodawn.net/kotelnikov-ge-manual/) ·
[The Glue manual](https://cytomic.com/files/TheGlue-Manual.pdf) ·
[SOS compressor topology](https://www.soundonsound.com/sound-advice/compressor-topology) ·
[GroupDIY on attack-time conventions](https://groupdiy.com/threads/calculating-attack-release-times-help-i-give-up.45371/) ·
[KVR t=466611 (GR meter ballistics)](https://www.kvraudio.com/forum/viewtopic.php?t=466611) ·
[KVR t=222445 (fast-attack LF distortion)](https://www.kvraudio.com/forum/viewtopic.php?t=222445) ·
[KVR t=490276 (sidechain HPF)](https://www.kvraudio.com/forum/viewtopic.php?t=490276) ·
[Zölzer-style tutorial](https://christiangaertner.github.io/uhh-digitalaudioprocessing/c/2018/01/29/compressor-explanation.html)

Delay: [EchoBoy manual](https://www.soundtoys.com/wp-content/uploads/EchoBoy-Manual.pdf) ·
[Ableton Live 12 effect reference](https://www.ableton.com/en/live-manual/12/live-audio-effect-reference/) ·
[FabFilter Timeless 3](https://www.fabfilter.com/help/timeless/using/delaycontrols) ·
[Bitwig Delay+](https://www.bitwig.com/userguide/latest/delay/) ·
[Tape Op on Valhalla Delay](https://tapeop.com/reviews/gear/132/valhalladelay-plug-in) ·
[Everything Recording on Comeback Kid](https://everythingrecording.com/review-baby-audio-comeback-kid/) ·
[KVR t=42849 (zipper noise in delay lines)](https://www.kvraudio.com/forum/viewtopic.php?t=42849) ·
[Nail The Mix delay FAQs](https://www.nailthemix.com/delay-faqs)

Saturator: [Decapitator manual](https://www.soundtoys.com/wp-content/uploads/Decapitator-Manual.pdf) ·
[Saturn 2 band controls](https://www.fabfilter.com/help/saturn/using/bandcontrols) ·
[Softube Saturation Knob](https://www.softube.com/plug-ins/saturation-knob) ·
[BPB on the 2021 gain-controls update](https://bedroomproducersblog.com/2021/10/11/free-saturation-knob/) ·
[Klanghelm SDRR](https://klanghelm.com/contents/products/SDRR) ·
[KVR t=618193 (anti-aliasing)](https://www.kvraudio.com/forum/viewtopic.php?t=618193) ·
[KVR t=596813 (autogain)](https://www.kvraudio.com/forum/viewtopic.php?t=596813) ·
[KVR t=601534 (oversampling)](https://www.kvraudio.com/forum/viewtopic.php?t=601534) ·
[KVR t=502609 (DC from asymmetric shapers)](https://www.kvraudio.com/forum/viewtopic.php?t=502609) ·
[Kahles/Esqueda/Välimäki AES 2019](https://www.aes.org/e-lib/browse.cfm?elib=20485) ·
[de Soras, Quest For The Perfect Resampler](https://ldesoras.fr/doc/articles/resampler-en.pdf)

Utility and cross-effect: [Ableton effect reference](https://www.ableton.com/en/manual/live-audio-effect-reference/) ·
[Bitwig Tool](https://www.bitwig.com/userguide/latest/utility/) ·
[Voxengo MSED](https://www.voxengo.com/product/msed/) ·
[bx_control V2](https://www.plugin-alliance.com/products/bx_control-v2) ·
[MUtility](https://www.meldaproduction.com/MUtility) ·
[SOS on pan law](https://www.soundonsound.com/sound-advice/q-what-pan-law-setting-should-use) ·
[VST3 processing FAQ (bypass)](https://steinbergmedia.github.io/vst3_dev_portal/pages/FAQ/Processing.html) ·
[FabFilter output options](https://www.fabfilter.com/help/pro-c/using/outputoptions) ·
[KVR t=535852 (DC blocker cutoff)](https://www.kvraudio.com/forum/viewtopic.php?t=535852) ·
[KVR t=439148 (smoothing times)](https://www.kvraudio.com/forum/viewtopic.php?t=439148)
