# Warp

`warp` (`crates/engine/src/dsp/warp.rs`, editor `ui/src/views/fx/warp.ts`) plays the input back
through a drawn **time map** and **volume map** over a two-bar buffer, locked to the song's
bars: half-time, stutters, rolls, tape stops and starts, reverses and scratches (time); gates,
trance gates and sidechain pumps (volume). It answers the same need as Image-Line's Gross Beat
or Cableguys' TimeShaper; the design, names and visuals are our own.

## Model

`Fx.maps: { time: WarpSlot[], vol: WarpSlot[] }`, each bank up to 16 slots, a slot =
`{ name, points: AutoPoint[] }` over one bar (`t` 0..4 beats, `v` 0..1, the automation segment
shapes and tension). A new Warp holds the factory slots (`crates/engine/src/warp_factory.json`,
also `fx_catalog().warp` and the UI mock's copy, so the three never drift). `putFx` refuses a
Warp without maps, maps on another effect, more than 16 slots or 256 points, points outside
the bar or 0..1, and points out of time order.

* **Time maps.** `v` 1 = the live input; lower reads further back, `Y = (1 − v) · 8` beats.
  The read position is `now − Y`, so the slope is the speed: flat = normal, the dashed
  "safety line" (−1 beat per beat) = stopped, the grey guides (−2) = reverse at normal speed,
  steeper = faster reverse, rising = faster forward (catching up), a vertical step (two points
  at one time) = a jump. A half-sine segment from flat to slope −1 is a tape stop.
* **Volume maps.** `v` = gain. Attack / Release smooth rises and falls, Tension bends every
  curve segment further.

Segment shapes (shared with automation clips, `model::curve_at`, checked against
`tests/fixtures/curves.json`): curve, double (S), hold, stairs, and new for Warp: **pulse**
(square, 1–16 cycles from the tension), **wave** (cosine, an odd number of half cycles so it
ends on its point), **half sine** (tension −1 ease in = tape stop, +1 ease out = tape start).

## Map library

Beyond the 16 + 16 slots, a library of ready-made maps (`crates/engine/src/warp_library.json`,
catalog `warpLibrary`, also read by the UI mock) loads into any slot from its right-click menu
(*Library → category → map*; one undo step, and the editor switches to that slot). Time maps
(51): slow-down (half-time per bar / half bar / beat, quarter speed, a fifth down, sinking,
dive), stutter and repeats (beat 1 ×4, first half, beats 1 and 3, last beat, 1/8, 1/16,
triplet, end only, shrinking echo, 1/8 dub shift), rolls and build-ups (1/16, 1/32, triplet,
1/4 → 1/32), tape (stops over a bar / half / beat / eighth, start, stop-and-start, wow, wobbly
cassette), reverse (per beat / half bar / eighth, previous bar, last beat, forward–back, half
speed, 2×), scratch (baby, chirp, per beat, triplet), glitch and speed (shuffled sixteenths,
skips, catch-up ×2 / ×1.5, half-time + stutter). Volume maps (35): sidechain (1/4 hard /
classic / gentle, 1/8, 1/2, with off-beat), gates (1/4 … 1/32, triplet, dotted 3/16, half
bar), trance gates (four patterns, fading chop, 1/32 transformer), transitions (fades, swells),
mutes (last beat / eighth, on 2 and 4, first beat only, off-beats only, second half), tremolo
(1/4, 1/8, 1/16, triplet). The maps are our own, built from the musical idea (each time map is a
list of "play the source from beat s at speed k" pieces, so it never reads the future).

## Parameters

| id | | |
|---|---|---|
| `tslot`, `vslot` | slot 1–16 | the playing slots. Picked on the slot strip (a click edits and plays a slot, one undo step); over the song, automate them with hold points (slot menu → *Automate slot changes…*) |
| `trig` | Off, 1/16, 1/8, 1/4, 1/2, 1 bar | when a slot change takes effect. A change arriving within 1/32 beat after a boundary counts as on it (automation is evaluated per block) |
| `pos` | Song / Trigger | where in the map a slot plays: the song's bar phase, or from its start |
| `oneshot` | Off / On | a slot other than 1 plays one bar, then slot 1 |
| `hold` | Off / Previous bar / Next bar | loops the bar before the current one, or records the coming bar and loops it. The buffer stops writing while it loops (the bar survives); the volume map keeps playing; after a hold the buffer starts empty again |
| `tmix`, `vmix` | 0–100 % | how much of the time manipulation / of the volume map is heard; the dry path is delayed by the latency so a blend has no comb filtering (use these, not the slot's dry/wet) |
| `att`, `rel`, `tns` | 0–500 ms, 0–1000 ms, ±100 % | volume smoothing and global tension |
| `click`, `attack`, `dc` | On / On / On | de-click jumps; shorter crossfade that keeps attacks; DC blocker (5 Hz) |

## Real-time behaviour

* The phase comes from `ProcessCtx::beat` per sample, never a free-running counter. The pattern
  loop preview counts as playing. Stopped transport: the input passes through (delayed by the
  latency), nothing is recorded.
* The ring holds 12.5 s per channel (two bars down to 40 bpm), indexed by absolute sample. It
  only holds what was heard since playback started or the playhead last jumped: a map that
  reaches further back is **silent** there (the editor shades it as "not heard yet"), never
  stale audio. Validity changes fade over 1 ms.
* Hermite interpolation; the read is at least 2 samples behind the write, so the effect reports
  `LATENCY` = 2 samples for compensation.
* Click reduction: a read position more than 4 samples off its own trajectory is a jump. The
  old trajectory keeps playing and crossfades **linearly** (heads often read the same material,
  where equal power would bump by 3 dB) into the new one over 3 ms, or 0.5 ms with Attack
  protect. The speed carries over a jump (the jump is not a rate).
* Maps reach the audio thread through `Processor::swap_data` (`Cmd::FxData`): built off the
  audio thread, moved in, the old one sent back as garbage. Only changed maps are sent
  (`Core::sent_maps`); fresh processors get theirs in `build_graph`. No allocation in `process`.
* `tail_samples`: the ring length (anything in it can come back); `None` while a hold loops.

## Meters (`Processor::meters`)

`[0]` bar position 0..4 (−1 stopped), `[1]`/`[2]` playing time / volume slot, `[3]`/`[4]`
queued slot (−1 none), `[5]` time offset in beats (≤ 0), `[6]` read speed, `[7]` volume gain,
`[8]` buffered beats at 120 bpm, `[9]` hold (0 off, 1 recording, 2 looping), `[10]` buffered
seconds.

## Editor

Slot strip (Time / Volume tabs, 16 slots: filled = playing, outlined = queued, white frame =
edited, `*` after the name = a factory slot that was changed). The map: click empty space to
add a point and drag it; drag a point (Shift = value only, Ctrl = time only, Alt = no snap; the
grid is a 1/16 note in time and in depth); drag the round handle to bend a segment;
double-click or Alt+click a point deletes it, a handle straightens it. Right-click a point:
segment shape, and for time maps a musical speed for the segment (×1, ×½, −7 st, ±1 st, stop,
reverse ×1; slope = 2^(n/12) − 1). Right-click a slot: copy, paste, clear, restore factory,
automate. Hovering a time map shows the speed there ("×0.50 · −12.0 st", "stopped",
"reverse ×1.00"). Edits are `putFx` ops (a drag is one undo step), so the CLI edits maps the same
way (`wave apply` with a `putFx` carrying `maps`).

## Not yet

Triggering slots from MIDI keys (Key-held, keyboard mapping) needs MIDI routed to effects; it
is a follow-up. Slot names are not editable yet.

## Tests

`dsp::warp::tests`: a flat map is the input delayed by the latency; a half-time slope plays an
octave down (220 Hz from 440); a −2 slope plays the past backwards; the factory stutter repeats
beat 1; reading before the recorded past is silent; the volume map gates in time; click
reduction keeps jump steps near the signal's own (and without it they click); trigger sync
waits for the boundary; Hold loops the previous bar and never parks; stopped transport passes
through; factory slots are valid. `tests/warp_and_delay.rs`: a map edit is heard at once on the
running audio thread; ops refuse bad maps; latency is reported. UI: `ui/src/lib/warp.test.ts`.
