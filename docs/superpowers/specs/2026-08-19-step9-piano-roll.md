# Step 9 — Piano roll

**Status:** landed. **Scope:** melodic notes — pitch, position and *length* — in
the same pattern the step sequencer edits, on the same scheduler, in the same
project file; plus a canvas editor for them.

Step 4 gave MYDAW a machine for laying sounds out in time, and it could only say
one thing: "this drum, on this sixteenth". A drum grid cannot express a note that
lasts three beats, a note between two grid lines, or two notes at once on one
instrument — and those three omissions are the difference between a drum machine
and a sequencer. This step adds the second half.

---

## The shape of it

| Module | What |
| --- | --- |
| `plugins/sequencer/src/pattern.rs` | `MelodicNote`, `PPQ`/`TICKS_PER_STEP`, `Pattern::length_steps`, `Pattern::notes`, and the new rejections. |
| `plugins/sequencer/src/compiled.rs` | `CompiledNote`, the fixed `[CompiledNote; 1024]` array and its sort. |
| `plugins/sequencer/src/schedule.rs` | `tick_boundary`, `Scheduler::schedule_notes`, a `MAX_PENDING` that can hold a thousand owed note-offs. |
| `app/src/dto.rs` | `MelodicNoteDto`, `PatternDto.notes`, a variable step count on the wire. |
| `app/src/project_file.rs` | `FORMAT_VERSION` 2 and the v1 migration. |
| `ui/src/lib/pattern.ts` | Tick arithmetic and the pure editing functions the roll is built from. |
| `ui/src/panels/piano-roll/draw.ts` | Canvas geometry and rendering. No Solid, no pointers. |
| `ui/src/panels/piano-roll/PianoRollPanel.tsx` | Pointer, scroll, selection, redraw policy. |
| `app/tests/piano_roll_audio.rs` | The hardware verification. |

---

## The model

A pattern is now **two collections and a length**:

```rust
pub struct Pattern {
    pub rows: Vec<PatternRow>,      // the drum grid, as before
    pub length_steps: u8,           // 16, 32 or 64
    pub notes: Vec<MelodicNote>,    // the piano roll
}

pub struct MelodicNote {
    pub key: u8,            // 0..=127
    pub channel: u8,        // 0..=15 -- the track this note plays
    pub start_ticks: u32,   // from the start of the pattern
    pub length_ticks: u32,  // >= 1
    pub velocity: f32,      // 0.0..=1.0
}
```

### Why notes are not steps

They are not two representations of the same thing, and neither is a special case
of the other:

| | `PatternRow` | `MelodicNote` |
| --- | --- | --- |
| Position | a step index, on the sixteenth grid | a tick |
| Length | the scheduler's fixed 50 % gate | its own |
| Pitch | one key per row | one key per note |
| Polyphony | one hit per row per step | unlimited |

A drum grid wants "row 0, step 4 — is it on?", a question with a fixed-size
answer that a checkbox toggles. A melody wants "C4 from here to there", a length
the grid has no way to say. Unifying them would mean either giving every one of
the 64 cells in every one of the 16 rows a length field nobody sets, or throwing
away the grid — which is the affordance that makes a beat ten clicks.

### Ticks, and why 960

`PPQ = 960` ticks per quarter note; `TICKS_PER_STEP = 240`.

960 is chosen for its factors, not its size. It divides exactly by 2, 3, 4, 5, 6,
8, 10, 12, 16, 20, 24, 32, 48, 60, 64, 96, 120, 192, 240, 320 and 480 — so a
sixteenth (240), a triplet eighth (320), a quintuplet sixteenth (192) and a 64th
(60) are all integers, and quantising to any of them is arithmetic rather than
approximation. A power of two would be smaller and could not express a triplet at
all. It is also what MIDI files and most sequencers settled on, so an import path
later is a copy rather than a conversion.

### Pattern length

`length_steps` is 16, 32 or 64 — one, two or four bars. Not a free range: a
pattern is a loop, and a loop whose length is not a whole number of bars puts the
downbeat somewhere different every time round. That is a musical effect, not a
default, and there is no UI for choosing it deliberately yet.

Every `PatternRow` carries `MAX_STEPS` (64) cells whatever the current length, and
only the first `length_steps` are played. **Shortening a pattern is therefore not
destructive**: the tail is remembered, and lengthening it again gives the steps
back. Notes cannot work that way — the wire carries the notes that exist, not a
fixed array — so shortening a pattern in the UI drops the notes that no longer
fit, which is stated in `withPatternLength`'s doc comment and visible in the note
count.

### Limits

| Limit | Value | Why |
| --- | --- | --- |
| `MAX_NOTES` | 1024 | The size of `[CompiledNote; MAX_NOTES]`. Four bars at 1024 notes is a note every 3.75 ticks. |
| `MAX_STEPS` | 64 | Four bars of sixteenths, the longest `PATTERN_LENGTHS` offers. |
| `MAX_ROWS` | 16 | Unchanged. |

### Validation

Beyond the existing row checks, a pattern is refused when a note has a key or
channel out of range, a length of zero, a velocity that is not a number in
`0..=1`, or an end past the loop point. Notes **may touch** the bar line — a note
filling the last step ends exactly at `pattern_ticks`, and its note-off is the
first sample of the next time round — but may not cross it, because a wrapped
note would sound before its own note-on on the following loop.

Overlaps are *not* an error. Three notes at the same tick on the same channel is
a triad, and a roll that refused it would be a monophonic sequencer with extra
steps.

---

## Tick arithmetic

The step grid's rule was: never accumulate. A step boundary is
`round(n * step_len)` for the **absolute** step index `n`, so there is no running
total to drift and no phase to reset at the loop point. The tick clock has to
join that rule without becoming a second one.

```rust
pub fn tick_boundary(tick: u64, step_len: f64) -> u64 {
    ((tick as f64 / TICKS_PER_STEP as f64) * step_len).round() as u64
}
```

The division comes **before** the multiplication, and the argument is
`step_len` rather than a tick length. That is the whole design:

- When `tick` is a whole number of steps, `tick / 240` is *exactly* the integer
  step index. (An exact multiple of 240 divided by 240 is exact in IEEE 754: the
  true quotient is representable, and IEEE division returns the correctly-rounded
  true quotient.) The expression then reduces, character for character, to
  `step_boundary`'s `round(n * step_len)`.
- So **a note written on the grid lands on the very same sample as a drum hit on
  that step** — not within a sample of it. Written the other way round, as
  `round(tick * tick_len)` with `tick_len = step_len / 240`, it would be two
  roundings against the grid's one and the two could disagree by a sample. A
  one-sample flam is inaudible; a *sometimes* one-sample flam is the kind of bug
  that gets diagnosed as "the synth feels loose".

The identity is asserted, not argued: 10 000 steps × 5 tempos × 3 sample rates,
in `a_tick_on_a_step_line_is_the_very_same_sample_as_that_step`.

Positions **between** the grid lines get the ordinary guarantee: within half a
sample of the ideal, from the absolute tick, so there is nothing to accumulate.
Loop `k` of an `L`-step pattern starts at tick `k * L * 240`, which is a step
boundary, so a loop wrap remains a non-event.

### The rounding, stated plainly

`round` is round-half-away-from-zero (`f64::round`). Every boundary is therefore
within 0.5 samples of its ideal position — 11 µs at 44.1 kHz — and that half
sample is quantisation, not drift: the ten-minute tests assert the *n*th boundary
is bit-for-bit `round(n * step_len)`, and that the last one is still within half
a sample of `n * step_len`.

---

## Scheduling

`Scheduler::schedule` gained one call at the end:

```text
  transport edges (stop / seek)  -> flush owed note-offs
  note-offs already owed         -> emit_due
  drum step boundaries in block  -> note-on, and note-off or park
  melodic notes in block         -> schedule_notes
```

`schedule_notes` walks the loop iterations that overlap the block. For each one it
**binary-searches** the sorted note array for the first note at or after the
block start, then walks forward until one lands at or after the block end:

```rust
let first = notes.partition_point(|note|
    tick_boundary(base_tick + note.start_ticks, step_len) < start);
```

The search is what keeps a thousand-note pattern from costing a thousand
multiplications per block; the early break is what keeps it from costing them
again on the way out. It is correct because `Pattern::compile` sorts the notes by
start tick (ties broken by key, so a compile is deterministic) and
`tick_boundary` is monotonic — which is why `CompiledPattern::sort_notes` says in
its doc comment that an unsorted array would not crash the scheduler, it would
silently drop notes.

The loop index is derived from the *fractional* loop length, not a rounded one:
dividing by a rounded loop length would land in the wrong iteration after a few
hundred bars, which is exactly the class of bug the module exists to avoid.

### Note-offs

A melodic note's off is due at `tick_boundary(base + start + length)`, clamped to
at least one sample after its own on — at 999 BPM one tick rounds to zero samples,
and a note-off that sorted ahead of its note-on would leave the voice sounding.
If the off falls inside the block it is pushed; otherwise it is parked, exactly
as a drum row's is.

`MAX_PENDING` grew from `MAX_ROWS * 2` to `MAX_ROWS * 2 + MAX_NOTES` = 1056. A
thousand whole notes starting together is a legal if unmusical pattern, and
"cannot happen" is not a bounds check. The array costs ~17 kB inside `RtState`.

Stop and seek flush every owed note-off, melodic ones included, at frame 0 of the
block that noticed. That matters more here than for drums: the drum kit ignores
note-offs, but the poly synth does not, and an eight-note arpeggio that leaked
would end the session with eight voices sounding. It is asserted offline
(`stopping_releases_the_melodic_notes_the_scheduler_still_owes`) and through the
speakers (the silence check at the end of the hardware test).

### Ordering at a shared frame offset

Unchanged, and now with a third participant. `NoteEventBuffer` is a stable sort,
so push order is the tie-break:

1. **live input** — a key the user is holding is not pre-empted by the pattern;
2. **the drum grid**;
3. **the piano roll**.

Nothing depends on 2 before 3 — they address different keys in every pattern
anyone will write — but it is fixed so a capture is reproducible. Within the
sequencer, note-offs still precede note-ons, which turns a step that retriggers a
still-sounding key into a retrigger rather than a stuck note.

### RT safety

`schedule` still allocates nothing, locks nothing and calls nothing that can.
`plugins/sequencer/tests/rt_alloc.rs` measures it with a counting global allocator
against the densest pattern MYDAW can express: 16 rows × 64 steps all on, **plus
the full thousand notes**, across 2 sample rates × 4 tempos × 8 block sizes.
Zero allocator events.

`CompiledPattern` is ~21 kB and is deliberately **`Clone` but not `Copy`**: that
is nothing to allocate once per edit on the editing thread, and exactly the size
you do not want an implicit `memcpy` of appearing in an audio callback because
somebody wrote `*slot.current().unwrap()`.

---

## Persistence

`FORMAT_VERSION` is **2**.

| Version | Shipped in | What changed |
| --- | --- | --- |
| 1 | step 8 | The format. |
| 2 | step 9 | `pattern.notes`; `pattern.steps` may be 32 or 64. |

### The wire shape of a note

```json
{ "key": 60, "channel": 1, "start": 0, "length": 960, "velocity": 0.78 }
```

`start` and `length` are in ticks, and the unit is **not** in the field name. This
shape is written into the `.mydaw` file as well as sent to the webview, and
`startTicks` next to `master_gain_db` would be a wart on the format for the rest
of its life. The units are documented in `app/src/dto.rs`, in
`ui/src/api/types.ts`, and here.

`notes` is declared after `rows`, because field order is file order — so a
version 1 file is a version 2 file with the last field missing, which is what
makes the migration nothing at all.

### Migration policy

Three rules, all in `app/src/project_file.rs`:

1. **Unknown fields are ignored** (serde's default, deliberately not overridden).
2. **A newer `format_version` is refused by name.** Ignoring unknown fields is
   safe; misreading known ones is not.
3. **An older `format_version` is migrated, never rejected.** A project a user
   saved is a document, and a DAW that cannot open its own last release's files is
   a DAW people stop trusting with their work.

The v1 → v2 migration is `#[serde(default)]` on `notes` plus one line in
`parse_project`: the parsed file carries `FORMAT_VERSION` whatever the bytes said,
so a load-then-save writes version 2 and the byte-identity round trip holds from
the first save onwards. **Migrate on load, write current on save.**

`app/tests/fixtures/v1.mydaw` is a real version 1 project, checked in verbatim.
The point of a migration test is to read the *format*; a fixture generated by this
build would only prove that this build agrees with itself. The test asserts the
fixture is still version 1 and still has no `notes` key, so that a well-meaning
update to it fails loudly rather than quietly deleting the coverage.

### What a round trip loses

The wire carries `pattern.steps` cells per row, not the engine's full 64. So a
pattern that went out to the webview and came back has lost the steps past its
loop point. That is the right trade — the UI has no way to show them, and sending
64 cells to draw 16 of them invites a UI bug to become a data one — and it is
stated in `Pattern::try_from`'s doc comment.

---

## The canvas editor

### Why a canvas

The step sequencer is 64 buttons and is quite rightly DOM: it wants focus,
labels, hover and hit-testing, and it gets all of them free. A four-bar roll is
128 rows against 960 sixteenths of grid line — tens of thousands of elements
before a single note is drawn, restyled on every scroll. That is not a
performance opinion, it is a different order of magnitude, and it is why every
DAW draws its roll.

### The split

`draw.ts` owns the geometry and the painting and knows nothing about Solid;
`PianoRollPanel.tsx` owns the pointer, the scroll and the selection. That is what
gives "where is tick 960?" exactly one answer, shared by the code that paints a
note and the code that decides which note was clicked — the classic way a roll
ends up with a one-pixel disagreement between what you see and what you grab.

The edits themselves are pure functions in `ui/src/lib/pattern.ts`
(`withNote`, `withReplacedNote`, `withoutNote`, `withPatternLength`, `clampNote`,
`snapTicks`, `noteAt`), handed to a single new state action:

```ts
app.editPattern((pattern) => withReplacedNote(pattern, index, edited));
```

`editPattern` is the general form of `toggleStep` and `clearPattern`. `state.tsx`
stays the one place that knows *when* an edit reaches Rust — the same 50 ms
debounce, the same `set_pattern`, the same validation, the same `basedrop`
hand-off — and never grows a second copy of *what* an edit is. Every helper
returns the pattern **unchanged by reference** for an edit it refuses, and
`editPattern` short-circuits on that, so a drag that has not yet crossed a grid
line costs nothing.

### Layout and interaction

```text
 ┌────────┬──────────────────────────────────────┐
 │        │ 1        2        3        4         │  ruler: bars, beats
 ├────────┼──────────────────────────────────────┤
 │  C5 ▌  │      ▓▓▓▓▓▓▓▓                        │  ← white/black key rows
 │     ▌  │  ▓▓▓▓                    │           │
 │  C4 ▌  │▓▓▓▓                      │           │    │ = playhead
 └────────┴──────────────────────────────────────┘
```

| Gesture | Effect |
| --- | --- |
| Click empty | Place a note: one snap unit long (a sixteenth with snap off), velocity 0.78, on the selected track's channel |
| Drag on from that click | Size it while placing |
| Click a note | Select it (the footer names it) |
| Drag a note's body | Move it in pitch and time, snapped |
| Drag its right edge | Resize, snapped to the *end* of a cell |
| Right-click a note | Delete |
| `Delete` / `Backspace` | Delete the selection |
| Wheel / shift-wheel | Scroll vertically / horizontally |

Wheel is scroll, never zoom: a pinch-zoom trackpad sends the same event as a
scroll wheel, and a roll that zooms when you meant to scroll past middle C is the
single most irritating thing a roll can do. Zoom is two buttons over three levels
(12/24/48 px per sixteenth), and it holds the left edge of the view at the same
musical position so zooming does not also scroll.

The resize handle is 6 px, and is suppressed on notes narrower than 12 px —
otherwise a 60-tick note at the coarsest zoom would be all handle and impossible
to move.

On first layout the roll fits the pattern to the width (`fitZoom`) and centres
middle C. Once only: after that the zoom and the scroll are the user's.

### Rendering

- **HiDPI**: the backing store is `cssSize * devicePixelRatio`, resized from a
  `ResizeObserver` on the host element.
- **Theme**: every colour is read from a `--roll-*` custom property at
  construction, the same arrangement `panels/meters/draw.ts` uses. A note and a
  step are the same idea at two resolutions and should not be two different
  greens.
- **Redraw policy**: a `createEffect` repaints when the pattern, geometry,
  selection or playhead changes; an animation-frame loop runs **only while the
  transport does**, because the playhead is the only thing that moves on its own.
  An idle roll is a static canvas — no rAF, no wakeups, nothing on the profile.
- Only the notes inside the viewport are painted, and they are clipped to the
  grid so nothing runs under the key column.
- The area past the loop point is greyed, because the pattern repeats there and
  drawing it like ordinary space invites a note where none can go.

### Where the panel lives

The sequencer plugin contributes two panels now: `step-sequencer` and
`piano-roll`. A plugin says what it draws, not where it goes; the shell decides,
and `WORKSPACE_PANELS` in `ui/src/panels/registry.ts` routes the roll to the
tabbed main area instead of the 380 px left dock. A piano roll drawn 380 px wide
would be a piano roll nobody could work in. The dock skips workspace panels, so
the roll is not also drawn as a card, and its count reflects what is actually
there.

The step grid follows the pattern's length too: its column count comes from
`--steps`, and it scrolls sideways rather than shrinking 64 cells past hitting
size.

---

## Hardware verification

`app/tests/piano_roll_audio.rs`, `#[ignore]`d, run on this machine:

```sh
cargo test -p mydaw-app --test piano_roll_audio -- --ignored --nocapture --test-threads=1
```

A two-bar pattern programmed **through `set_pattern_dto`** — the same path a
click in the roll takes, validation included — with the drum kit on track 0 and
the poly synth on track 1:

- kick on every quarter, closed hats on every eighth;
- C4 · E4 · G4 · C5, quarter notes, twice, on channel 1.

Two bars and a bit are captured from the debug tap and measured twice over:

1. **Timing.** Kick onsets against the quarter-note grid, with the step-4
   detector.
2. **Pitch.** Those onsets *define* the grid — arming the tap and starting the
   transport are different events, so the capture's phase within the loop is
   unknowable — and the middle 40–80 % of each quarter is band-passed to
   200–1500 Hz and transformed. The band's high corner removes the kick and the
   synth's own sub-oscillator (a sine an octave down at −6 dB, which would
   otherwise be a plausible-looking wrong answer); the low corner removes the
   hats. The window avoids the previous note's 120 ms release tail and the next
   kick's transient.

The arpeggio's rotation within the capture is *recovered*, not assumed: the figure
repeats every four quarters, so the alignment with the smallest total error is
chosen before anything is asserted. On the run below the winner scored 1.15 Hz
and the runner-up 1045.87 Hz, which is what "unambiguous" looks like.

Device: Speakers (Focusrite USB Audio) @ 44.1 kHz, 2 ch, max block 4096.
130 BPM → 20 353.85 samples per quarter.

**Grid** — 10 kick onsets, contiguous, worst 0.221 ms, mean 0.132 ms
(tolerance 1 ms).

**Pitch** — bin width 10.77 Hz; tolerance ±1 bin:

| Window | Key | Expected Hz | Measured Hz | Error Hz | Error bins |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 67 (G4) | 392.00 | 392.12 | +0.13 | +0.01 |
| 2 | 72 (C5) | 523.25 | 523.12 | −0.13 | −0.01 |
| 3 | 60 (C4) | 261.63 | 261.80 | +0.17 | +0.02 |
| 4 | 64 (E4) | 329.63 | 329.48 | −0.14 | −0.01 |
| 5 | 67 (G4) | 392.00 | 392.12 | +0.13 | +0.01 |
| 6 | 72 (C5) | 523.25 | 523.12 | −0.13 | −0.01 |
| 7 | 60 (C4) | 261.63 | 261.80 | +0.17 | +0.02 |
| 8 | 64 (E4) | 329.63 | 329.48 | −0.14 | −0.01 |

Then the transport is stopped and a quarter-second is captured: peak exactly
0.0. Eight melodic note-offs, all paid.

Artifacts are kept under `app/.debug/captures/<timestamp>/` — `capture.wav`,
`capture.png`, `stats.json`, and a `pitch.json` carrying the table above.

### A note on the measurement

The onset detector's band filter starts from zero state, so a capture that begins
in the middle of a sounding note opens with an artificial rise the detector reads
as an attack. This capture always begins mid-loop, so that artefact is always
there. `trim_warm_up` removes it by the one thing that distinguishes it from a
drum — it is not a quarter note away from its neighbour — and nothing else. If
the engine's spacing were actually wrong, that function would trim everything and
the contiguity assertion would fail loudly rather than quietly passing on two
onsets.

---

## Limitations

Deliberate, and each one is a step of its own later:

- **Velocity editing is minimal.** A note is placed at 0.78 and the footer shows
  its velocity, but there is no velocity lane and no way to change it. Doing that
  well needs a second pane under the roll and a drag model of its own.
- **No dragging notes between tracks.** A note's `channel` is set from the
  selected track when it is placed and never changes. The roll shows one pattern
  across all channels, so a note on another track is drawn but cannot be re-homed
  without deleting and replacing it.
- **One pattern.** Still. The whole session has a single looping pattern; pattern
  banks and an arrangement timeline are Phase 2.
- **No per-note off-grid nudge without changing snap.** Snap off gives tick
  resolution, but there is no modifier to bypass snap for one gesture.
- **No copy, paste, duplicate or multi-select.** One note at a time.
- **Shortening a pattern drops the notes that no longer fit**, unlike the drum
  grid, whose tail is remembered. See [Pattern length](#pattern-length).
- **The drum grid still has a fixed 50 % gate.** Per-step note length is a
  piano-roll feature; a drum row is a one-shot trigger and does not want one.

---

## Gates

| Gate | Result |
| --- | --- |
| `cargo build --workspace --all-targets` | clean |
| `cargo test --workspace` | 901 passed, 0 failed, 14 ignored |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo fmt --all` | clean |
| `pnpm build` (`tsc --noEmit && vite build`) | clean |
| `piano_roll_audio` (hardware) | passed — see above |
| `sequencer_audio` (hardware, step 4 regression) | passed — kick worst 0.052 ms, hats worst 0.123 ms |
