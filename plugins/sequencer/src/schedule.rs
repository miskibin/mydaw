//! The scheduler: pattern plus transport window in, note events out.
//!
//! This is the timing-critical part of the DAW, and it is deliberately a *pure*
//! piece of arithmetic over a fixed-size state machine: no audio, no device, no
//! graph, no clock of its own. [`Scheduler::schedule`] is handed the pattern,
//! the block's transport window and the block's event buffer, and everything it
//! does is visible in what lands in that buffer. That is what makes the tempo
//! grid testable at 999 BPM and 4096-frame blocks without a sound card.
//!
//! # Drift-free by construction
//! A step boundary is computed from the **absolute** step index, never
//! accumulated:
//!
//! ```text
//!   step_len = sample_rate * 60 / bpm / 4          (f64, fractional)
//!   boundary(n) = round(n * step_len)              (u64, exact for every n)
//! ```
//!
//! Nothing anywhere adds `step_len` to a running total, so there is no error to
//! accumulate. After ten minutes at 174.3 BPM the 10 000th boundary is still
//! `round(10000 * step_len)` -- within half a sample of the ideal position, and
//! that half sample is the rounding, not drift. The tests assert exactly this.
//!
//! The pattern position is `n % steps`, so **looping is free**: a bar wrap is
//! not an event, it is a modulo. Nothing has to be reset at the loop point,
//! which is why a note that started on step 15 and ends after the wrap needs no
//! special case at all.
//!
//! # The two clocks are one clock
//! A melodic note is positioned in **ticks** ([`PPQ`](crate::PPQ) per quarter)
//! rather than in steps, and its sample position is
//! [`tick_boundary`]:
//!
//! ```text
//!   tick_boundary(t) = round((t / TICKS_PER_STEP) * step_len)
//! ```
//!
//! Written that way, and not as `round(t * tick_len)`, for one reason: when `t`
//! is a whole number of steps, `t / 240` is *exactly* the integer step index
//! (an exact multiple of 240 divided by 240 is exact in IEEE 754), so the
//! expression becomes `round(n * step_len)` -- character for character the drum
//! grid's own [`step_boundary`]. A note written on the grid therefore lands on
//! the very same sample as a drum hit on that step, not within a sample of it.
//! The equality is asserted over ten thousand steps at four tempos.
//!
//! Positions between the grid lines round to the nearest sample and are exact to
//! within that half sample, from the absolute tick, for the same
//! never-accumulate reason as above. Loop `k` of a pattern `L` steps long starts
//! at tick `k * L * TICKS_PER_STEP`, so the loop points are step boundaries too
//! and a wrap remains a non-event.
//!
//! # The pending note-off state machine
//! A step's note-on is emitted at its boundary; its note-off is due
//! [`GATE_FRACTION`] of a step later. A melodic note's is due at its own
//! `start + length`. Either is usually inside the same block -- but at a small
//! block size, or a slow tempo, or when the note-off lands in the *next* bar, it
//! is not. So a note-off that falls past the end of the block is parked in a
//! fixed-size array and emitted by a later block:
//!
//! ```text
//!   block k        |----------|                  on(step n) at +12
//!                       off due at +900 -> park
//!   block k+1      |----------|                  off emitted at +388
//! ```
//!
//! Drums ignore note-offs (a one-shot is a one-shot), but the poly synth does
//! not, and a sequencer that leaks note-ons into a synth is a sequencer that
//! ends the session with sixteen stuck voices. Two events that must never be
//! lost, therefore, get a state machine rather than a hope:
//!
//! - **Stop** flushes every parked note-off at frame 0 of the stopping block.
//! - **Seek** does the same: the notes belong to a bar that is no longer under
//!   the playhead.
//! - The array is sized [`MAX_PENDING`]: twice what a 50 % gate can leave owed
//!   across the drum rows, plus one slot for every melodic note there can be --
//!   because a thousand whole notes starting together is a legal, if unmusical,
//!   pattern, and "cannot happen" is not a bounds check.
//!
//! # Ordering at the same frame offset
//! [`NoteEventBuffer`] is a stable sort, so the order events are *pushed* is the
//! order the plugin sees them at any one offset. The engine pushes **live input
//! first, then the sequencer**, and within the sequencer **note-offs before
//! note-ons**. Live-first because a key the user is pressing right now should
//! not be pre-empted by the grid; offs-before-ons because that turns a step that
//! retriggers a still-sounding key into a retrigger rather than a stuck note.
//!
//! The drum rows are emitted before the melodic notes. Nothing depends on that
//! order -- they address different keys in every pattern anyone will write, and
//! the buffer sorts by offset regardless -- but it is fixed so that a capture is
//! reproducible.
//!
//! # RT safety
//! Every field is a fixed-size array or a scalar. `schedule` allocates nothing,
//! locks nothing and calls nothing that can. The melodic path **binary-searches**
//! the sorted note array for the first note due in the block rather than walking
//! all thousand of them, so the per-block cost is `O(log n + emitted)`.
//! `tests/rt_alloc.rs` measures the allocation claim with a counting allocator.

use mydaw_rt_core::{NoteEvent, NoteEventBuffer};

use crate::compiled::CompiledPattern;
use crate::pattern::{MAX_NOTES, MAX_ROWS, STEPS_PER_BEAT, TICKS_PER_STEP};

/// Fraction of a step a scheduled note sounds for.
///
/// A half-step gate is the classic drum-machine value: long enough that a synth
/// row plays a note rather than a click, short enough that consecutive steps do
/// not overlap. Per-step note length is a piano-roll feature, not a step-grid
/// one -- see the spec's limitations.
pub const GATE_FRACTION: f64 = 0.5;

/// Parked note-offs the scheduler can hold at once.
///
/// Two per drum row -- a [`GATE_FRACTION`] of 0.5 can only ever leave one note
/// per row pending, so that is a doubling of the worst case rather than a guess,
/// and it stays correct if the gate is ever raised towards a full step -- plus
/// one per melodic note, because a note may be as long as the whole pattern and
/// a thousand of them may sound at once.
pub const MAX_PENDING: usize = MAX_ROWS * 2 + MAX_NOTES;

/// Samples per step at `sample_rate` and `bpm`.
///
/// `sample_rate * 60 / bpm` is one beat; a step is a sixteenth, so a quarter of
/// that. Fractional on purpose: rounding here rather than at each boundary is
/// precisely how a sequencer acquires drift.
pub fn step_len_samples(sample_rate: f64, bpm: f64) -> f64 {
    sample_rate * 60.0 / bpm / STEPS_PER_BEAT as f64
}

/// Absolute sample position of step `index`, counted from sample 0.
///
/// Rounded to the nearest sample, from the absolute index. See the module docs.
pub fn step_boundary(index: u64, step_len: f64) -> u64 {
    (index as f64 * step_len).round() as u64
}

/// Absolute sample position of `tick`, counted from tick 0.
///
/// `step_len` rather than a tick length, and the division before the
/// multiplication rather than after, so that a tick on a step boundary reduces
/// to [`step_boundary`] *exactly* -- see the module docs. The tests assert the
/// identity rather than trusting the argument.
pub fn tick_boundary(tick: u64, step_len: f64) -> u64 {
    ((tick as f64 / TICKS_PER_STEP as f64) * step_len).round() as u64
}

/// One block's worth of transport, as the scheduler needs it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransportWindow {
    /// Absolute sample position of the block's first frame.
    pub start_sample: u64,
    /// Frames in the block.
    pub frames: usize,
    pub sample_rate: f64,
    pub bpm: f64,
    /// Whether the transport is running. The scheduler emits pattern events only
    /// while it is -- live input is a separate path and is never gated.
    pub playing: bool,
}

impl TransportWindow {
    /// One past the last frame of this block.
    pub fn end_sample(&self) -> u64 {
        self.start_sample + self.frames as u64
    }

    /// Whether the tempo and rate give a step longer than one sample.
    ///
    /// False means the arithmetic below would be nonsense (a zero or NaN step
    /// length), and the scheduler emits nothing rather than looping forever.
    /// Unreachable through the engine, which clamps the tempo -- but this
    /// function is public, and "unreachable" is not a bounds check.
    pub fn is_usable(&self) -> bool {
        let step = step_len_samples(self.sample_rate, self.bpm);
        step.is_finite() && step >= 1.0
    }
}

/// A note-off waiting for the block it belongs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingOff {
    /// Absolute sample position the note-off is due at.
    at: u64,
    key: u8,
    channel: u8,
}

/// Turns a pattern and a transport window into note events.
///
/// Owns exactly one piece of state: the note-offs it still owes. Everything else
/// is derived from the window, which is why a seek needs no cooperation and a
/// loop wrap needs no code.
#[derive(Debug)]
pub struct Scheduler {
    pending: [PendingOff; MAX_PENDING],
    pending_len: usize,
    /// Transport state as of the previous block, for edge detection.
    playing: bool,
    /// Sample position the next block is expected to start at. A mismatch is a
    /// seek.
    next_sample: u64,
    /// Note-offs that did not fit [`MAX_PENDING`]. Always zero in practice; a
    /// non-zero value would mean stuck notes, so it is counted rather than
    /// assumed away.
    overflowed: u64,
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl Scheduler {
    /// A scheduler with nothing owed, stopped at sample 0.
    pub const fn new() -> Self {
        Self {
            pending: [PendingOff {
                at: 0,
                key: 0,
                channel: 0,
            }; MAX_PENDING],
            pending_len: 0,
            playing: false,
            next_sample: 0,
            overflowed: 0,
        }
    }

    /// Note-offs currently owed. Diagnostics and tests.
    pub const fn pending(&self) -> usize {
        self.pending_len
    }

    /// Note-offs dropped because [`MAX_PENDING`] was exceeded. Should be zero.
    pub const fn overflowed(&self) -> u64 {
        self.overflowed
    }

    /// Whether the scheduler believes the transport is running.
    pub const fn is_playing(&self) -> bool {
        self.playing
    }

    /// Emit this block's pattern events into `out`.
    ///
    /// Returns how many events `out` had no room for -- the same currency as
    /// [`drain_notes`](mydaw_rt_core::drain_notes), so the engine can add the two
    /// and report one number.
    ///
    /// Call **after** live notes have been drained into `out`, so that live input
    /// wins the tie at a shared frame offset (see the module docs).
    ///
    /// # RT safety
    /// Allocation-free, lock-free, bounded: the boundary loop runs at most
    /// `frames / step_len + 2` times, and `step_len >= 1` is checked.
    pub fn schedule(
        &mut self,
        pattern: Option<&CompiledPattern>,
        window: &TransportWindow,
        out: &mut NoteEventBuffer,
    ) -> usize {
        let mut dropped = 0;

        // --- transport edges, before anything is emitted ---------------------
        //
        // Stop and seek both mean "the notes I still owe belong to a bar that is
        // no longer playing". Release them at the top of this block; anything
        // else is a stuck voice.
        let stopped = self.playing && !window.playing;
        let seeked = window.playing && window.start_sample != self.next_sample;
        if stopped || seeked {
            dropped += self.flush(out);
        }
        self.playing = window.playing;
        // While stopped the clock does not advance, so the next block starts
        // where this one did; that must not read as a seek.
        self.next_sample = if window.playing {
            window.end_sample()
        } else {
            window.start_sample
        };

        if !window.playing || window.frames == 0 || !window.is_usable() {
            return dropped;
        }

        let start = window.start_sample;
        let end = window.end_sample();

        // --- note-offs already owed that come due inside this block ----------
        dropped += self.emit_due(start, end, window.frames, out);

        let Some(pattern) = pattern else {
            return dropped;
        };
        // A pattern of no length has no loop to be at a position within, which
        // both halves below would divide by.
        if pattern.steps() == 0 {
            return dropped;
        }

        let step_len = step_len_samples(window.sample_rate, window.bpm);

        // --- step boundaries falling inside [start, end) ---------------------
        //
        // Skipped entirely when there are no drum rows -- a piano-roll-only
        // pattern is an ordinary pattern, and the melodic half below is not
        // behind this gate.
        let gate = (step_len * GATE_FRACTION).round().max(1.0) as u64;
        let steps = pattern.steps() as u64;

        // Start one step early: `step_boundary` rounds, so the boundary of
        // `floor(start / step_len)` can land just *below* `start` and the one
        // before it cannot. Boundaries below `start` are skipped, not emitted.
        let mut index = ((start as f64 / step_len).floor() as u64).saturating_sub(1);
        while pattern.row_count() > 0 {
            let boundary = step_boundary(index, step_len);
            if boundary >= end {
                break;
            }
            if boundary >= start {
                let slot = (index % steps) as usize;
                let offset = (boundary - start) as u32;
                for row_index in 0..pattern.row_count() {
                    let Some(row) = pattern.row(row_index) else {
                        continue;
                    };
                    let Some(velocity) = row.velocity_at(slot) else {
                        continue;
                    };
                    if !out.push(
                        NoteEvent::note_on(offset, row.key(), velocity).on_channel(row.channel()),
                    ) {
                        dropped += 1;
                    }

                    let off_at = boundary + gate;
                    if off_at < end {
                        // The gate closes inside this same block. Push it now;
                        // the buffer sorts by offset, so order is preserved.
                        let off_offset = (off_at - start) as u32;
                        if !out.push(
                            NoteEvent::note_off(off_offset, row.key()).on_channel(row.channel()),
                        ) {
                            dropped += 1;
                        }
                    } else if !self.park(PendingOff {
                        at: off_at,
                        key: row.key(),
                        channel: row.channel(),
                    }) {
                        self.overflowed += 1;
                    }
                }
            }
            index += 1;
        }

        // --- melodic notes falling inside [start, end) -----------------------
        dropped += self.schedule_notes(pattern, start, end, step_len, out);

        dropped
    }

    /// Emit the melodic notes due in `[start, end)`.
    ///
    /// Split out of [`schedule`](Self::schedule) because it is a different loop
    /// over a different structure and inlining the two would produce a function
    /// nobody could read. The shape is:
    ///
    /// ```text
    ///   for each loop iteration overlapping the block:
    ///       binary-search the sorted notes for the first one at or after `start`
    ///       walk forward, emitting, until one lands at or after `end`
    /// ```
    ///
    /// The search is what keeps a thousand-note pattern from costing a thousand
    /// multiplications per block; the early break is what keeps it from costing
    /// them again on the way out.
    ///
    /// # RT safety
    /// No allocation and no unbounded loop: `partition_point` is a binary search
    /// over a slice, and the outer loop advances by at least
    /// `16 * step_len >= 16` samples per iteration.
    fn schedule_notes(
        &mut self,
        pattern: &CompiledPattern,
        start: u64,
        end: u64,
        step_len: f64,
        out: &mut NoteEventBuffer,
    ) -> usize {
        let notes = pattern.notes();
        if notes.is_empty() {
            return 0;
        }
        let loop_ticks = pattern.steps() as u64 * u64::from(TICKS_PER_STEP);
        // A pattern of no length would make the loop below never advance.
        let loop_len = loop_ticks as f64 / f64::from(TICKS_PER_STEP) * step_len;
        if !loop_len.is_finite() || loop_len < 1.0 {
            return 0;
        }

        let mut dropped = 0;
        // From the *fractional* loop length, not the rounded one: dividing by a
        // rounded loop length would drift into the wrong iteration after a few
        // hundred bars, which is exactly the bug this whole module is written to
        // avoid. One iteration early, because the rounding can put the boundary
        // just below `start`.
        let mut iteration = ((start as f64 / loop_len).floor() as u64).saturating_sub(1);
        loop {
            let base_tick = iteration * loop_ticks;
            let loop_start = tick_boundary(base_tick, step_len);
            if loop_start >= end {
                break;
            }

            // The notes are sorted by start tick and `tick_boundary` is
            // monotonic, so "is this note before the block?" is monotonic too --
            // which is what makes a binary search correct here.
            let first = notes.partition_point(|note| {
                tick_boundary(base_tick + u64::from(note.start_ticks), step_len) < start
            });
            for note in &notes[first..] {
                let at = tick_boundary(base_tick + u64::from(note.start_ticks), step_len);
                if at >= end {
                    break;
                }
                let offset = (at - start) as u32;
                if !out.push(
                    NoteEvent::note_on(offset, note.key, note.velocity).on_channel(note.channel),
                ) {
                    dropped += 1;
                }

                let off_at = tick_boundary(base_tick + note.end_ticks(), step_len);
                // A zero-length note would put its own note-off at its note-on;
                // `compile` clamps the length to 1 tick, but at a slow enough
                // tempo one tick still rounds to zero samples, and a note-off
                // *before* the note-on would sort ahead of it.
                let off_at = off_at.max(at + 1);
                if off_at < end {
                    if !out.push(
                        NoteEvent::note_off((off_at - start) as u32, note.key)
                            .on_channel(note.channel),
                    ) {
                        dropped += 1;
                    }
                } else if !self.park(PendingOff {
                    at: off_at,
                    key: note.key,
                    channel: note.channel,
                }) {
                    self.overflowed += 1;
                }
            }
            iteration += 1;
        }
        dropped
    }

    /// Release every owed note-off at frame 0 of the current block.
    ///
    /// Public so the engine can force it on teardown; the transport edges inside
    /// [`schedule`](Self::schedule) call it for stop and seek. Returns how many
    /// events did not fit.
    pub fn flush(&mut self, out: &mut NoteEventBuffer) -> usize {
        let mut dropped = 0;
        for index in 0..self.pending_len {
            let owed = self.pending[index];
            if !out.push(NoteEvent::note_off(0, owed.key).on_channel(owed.channel)) {
                dropped += 1;
            }
        }
        self.pending_len = 0;
        dropped
    }

    /// Forget every owed note-off without emitting it.
    ///
    /// For a stream teardown, where there is no buffer left to emit into and the
    /// instrument is about to be dropped anyway.
    pub fn reset(&mut self) {
        self.pending_len = 0;
        self.playing = false;
        self.next_sample = 0;
    }

    /// Emit the owed note-offs due before `end`, keeping the rest.
    fn emit_due(
        &mut self,
        start: u64,
        end: u64,
        frames: usize,
        out: &mut NoteEventBuffer,
    ) -> usize {
        let mut dropped = 0;
        let mut kept = 0;
        for index in 0..self.pending_len {
            let owed = self.pending[index];
            if owed.at >= end {
                // Compacted in place, preserving order, so a later block emits
                // them in the order they were parked.
                self.pending[kept] = owed;
                kept += 1;
                continue;
            }
            // `at` below `start` can only happen after a backwards seek that
            // somehow escaped the flush; clamping is cheaper than trusting it.
            let offset = owed
                .at
                .saturating_sub(start)
                .min((frames as u64).saturating_sub(1)) as u32;
            if !out.push(NoteEvent::note_off(offset, owed.key).on_channel(owed.channel)) {
                dropped += 1;
            }
        }
        self.pending_len = kept;
        dropped
    }

    /// Park a note-off for a later block. `false` when the array is full.
    fn park(&mut self, owed: PendingOff) -> bool {
        if self.pending_len >= MAX_PENDING {
            return false;
        }
        self.pending[self.pending_len] = owed;
        self.pending_len += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern::{Pattern, PatternRow, DEFAULT_STEPS};
    use mydaw_rt_core::{NoteKind, DEFAULT_BPM};

    const SR: f64 = 48_000.0;

    fn window(start: u64, frames: usize, bpm: f64, playing: bool) -> TransportWindow {
        TransportWindow {
            start_sample: start,
            frames,
            sample_rate: SR,
            bpm,
            playing,
        }
    }

    /// A pattern with one row on `key`, hitting the given steps.
    fn one_row(key: u8, steps: &[usize]) -> CompiledPattern {
        let mut pattern = Pattern::silent();
        pattern.rows.push(PatternRow::empty(key).with_steps(steps));
        pattern.compile()
    }

    /// Run `blocks` blocks of `frames` and collect `(absolute sample, event)`.
    fn run(
        scheduler: &mut Scheduler,
        pattern: &CompiledPattern,
        blocks: usize,
        frames: usize,
        bpm: f64,
        sample_rate: f64,
    ) -> Vec<(u64, NoteEvent)> {
        let mut buffer = NoteEventBuffer::new(512);
        let mut collected = Vec::new();
        for block in 0..blocks {
            let start = (block * frames) as u64;
            buffer.clear();
            let window = TransportWindow {
                start_sample: start,
                frames,
                sample_rate,
                bpm,
                playing: true,
            };
            assert_eq!(
                scheduler.schedule(Some(pattern), &window, &mut buffer),
                0,
                "the buffer is large enough that nothing may be dropped"
            );
            assert!(
                buffer.is_sorted(),
                "the block's events must be in time order"
            );
            for event in buffer.as_slice() {
                assert!(
                    (event.frame_offset as usize) < frames,
                    "offset {} is outside a {frames}-frame block",
                    event.frame_offset
                );
                collected.push((start + u64::from(event.frame_offset), *event));
            }
        }
        collected
    }

    /// Absolute sample positions of the note-ons in `events`.
    fn onsets(events: &[(u64, NoteEvent)]) -> Vec<u64> {
        events
            .iter()
            .filter(|(_, e)| e.kind.is_note_on())
            .map(|(at, _)| *at)
            .collect()
    }

    #[test]
    fn the_step_length_is_a_sixteenth_note_at_the_stated_tempo() {
        // 130 BPM: a beat is 48000*60/130 = 22153.8 samples, a step a quarter of
        // that.
        let step = step_len_samples(SR, 130.0);
        assert!((step - 5538.4615).abs() < 1e-3, "{step}");
        // Sixteen steps is one bar, which is four beats.
        assert!((step * 16.0 - SR * 60.0 / 130.0 * 4.0).abs() < 1e-6);
        // And the round numbers, so a reader can check the formula by hand.
        assert_eq!(step_len_samples(48_000.0, 120.0), 6_000.0);
        assert_eq!(step_len_samples(44_100.0, 60.0), 11_025.0);
    }

    #[test]
    fn an_empty_pattern_produces_no_events_at_all() {
        let mut scheduler = Scheduler::new();
        let events = run(&mut scheduler, &CompiledPattern::EMPTY, 8, 512, 130.0, SR);
        assert!(events.is_empty());
        assert_eq!(scheduler.pending(), 0);

        // Rows present but no hits: same answer, different branch.
        let silent = Pattern::default().compile();
        assert_eq!(silent.row_count(), 4);
        assert!(run(&mut scheduler, &silent, 8, 512, 130.0, SR).is_empty());
    }

    #[test]
    fn no_pattern_installed_at_all_produces_no_events() {
        let mut scheduler = Scheduler::new();
        let mut buffer = NoteEventBuffer::new(64);
        assert_eq!(
            scheduler.schedule(None, &window(0, 512, 130.0, true), &mut buffer),
            0
        );
        assert!(buffer.is_empty());
    }

    #[test]
    fn a_stopped_transport_emits_nothing_however_full_the_pattern_is() {
        let mut scheduler = Scheduler::new();
        let pattern = one_row(36, &(0..DEFAULT_STEPS).collect::<Vec<_>>());
        let mut buffer = NoteEventBuffer::new(64);
        for block in 0..8 {
            buffer.clear();
            scheduler.schedule(
                Some(&pattern),
                &window(block * 512, 512, 130.0, false),
                &mut buffer,
            );
            assert!(buffer.is_empty(), "block {block} emitted while stopped");
        }
        assert!(!scheduler.is_playing());
    }

    #[test]
    fn every_step_lands_exactly_on_its_boundary_at_every_tempo_rate_and_block_size() {
        // The core claim of the whole step. A pattern that hits every step, run
        // for one bar, must produce boundaries identical to `round(n*step_len)`
        // -- independent of how the blocks happen to be cut.
        for sample_rate in [44_100.0_f64, 48_000.0] {
            for bpm in [60.0_f64, 130.0, 174.3, 999.0] {
                let step_len = step_len_samples(sample_rate, bpm);
                let expected: Vec<u64> = (0..DEFAULT_STEPS as u64)
                    .map(|n| step_boundary(n, step_len))
                    .collect();
                let bar = step_len * DEFAULT_STEPS as f64;

                for frames in [32_usize, 256, 441, 4_096] {
                    let mut scheduler = Scheduler::new();
                    let pattern = one_row(36, &(0..DEFAULT_STEPS).collect::<Vec<_>>());
                    // Enough blocks to cover one whole bar, plus one.
                    let blocks = (bar / frames as f64).ceil() as usize + 1;
                    let events = run(&mut scheduler, &pattern, blocks, frames, bpm, sample_rate);
                    let got = onsets(&events);
                    assert!(
                        got.len() >= DEFAULT_STEPS,
                        "{sample_rate} Hz / {bpm} BPM / {frames} frames: only {} onsets",
                        got.len()
                    );
                    assert_eq!(
                        &got[..DEFAULT_STEPS],
                        &expected[..],
                        "{sample_rate} Hz / {bpm} BPM / {frames} frames"
                    );
                }
            }
        }
    }

    #[test]
    fn ten_minutes_of_playback_does_not_drift_by_a_single_sample() {
        // Accumulating `step_len` instead of multiplying would be off by ~4
        // samples here at 174.3 BPM; asserting exactness is what makes the
        // difference visible.
        let bpm = 174.3;
        let step_len = step_len_samples(SR, bpm);
        let mut scheduler = Scheduler::new();
        let pattern = one_row(36, &[0]);

        let frames = 441; // a deliberately awkward block size
        let total_samples = (SR * 600.0) as u64; // ten minutes
        let blocks = (total_samples / frames as u64) as usize;

        let mut buffer = NoteEventBuffer::new(64);
        let mut onsets = Vec::new();
        for block in 0..blocks {
            let start = (block * frames) as u64;
            buffer.clear();
            scheduler.schedule(
                Some(&pattern),
                &window(start, frames, bpm, true),
                &mut buffer,
            );
            for event in buffer.as_slice() {
                if event.kind.is_note_on() {
                    onsets.push(start + u64::from(event.frame_offset));
                }
            }
        }

        // A bar at 174.3 BPM is 66 093 samples, so ten minutes is ~435 of them.
        assert!(
            onsets.len() > 400,
            "ten minutes should be hundreds of bars, got {}",
            onsets.len()
        );
        // Step 0 of bar b is global step index b*16.
        for (bar, onset) in onsets.iter().enumerate() {
            let expected = step_boundary(bar as u64 * DEFAULT_STEPS as u64, step_len);
            assert_eq!(
                *onset, expected,
                "bar {bar} drifted: {onset} instead of {expected}"
            );
        }
        // And the last one is still within half a sample of the ideal position,
        // which is the rounding and nothing else.
        let last = *onsets.last().unwrap();
        let ideal = (onsets.len() - 1) as f64 * step_len * DEFAULT_STEPS as f64;
        assert!(
            (last as f64 - ideal).abs() <= 0.5,
            "after ten minutes the error is {} samples",
            last as f64 - ideal
        );
    }

    #[test]
    fn a_note_off_lands_half_a_step_after_its_note_on() {
        let mut scheduler = Scheduler::new();
        let pattern = one_row(60, &[0]);
        let step_len = step_len_samples(SR, DEFAULT_BPM);
        let gate = (step_len * GATE_FRACTION).round() as u64;

        let events = run(&mut scheduler, &pattern, 20, 512, DEFAULT_BPM, SR);
        let on = events
            .iter()
            .find(|(_, e)| e.kind.is_note_on())
            .expect("the step fires");
        let off = events
            .iter()
            .find(|(_, e)| matches!(e.kind, NoteKind::NoteOff { .. }))
            .expect("and it stops");
        assert_eq!(on.0, 0);
        assert_eq!(off.0, gate, "gate is half a step");
        assert_eq!(off.1.kind, NoteKind::NoteOff { key: 60 });
    }

    #[test]
    fn a_note_off_crossing_a_block_boundary_is_parked_and_emitted_by_the_next_block() {
        // 64-frame blocks at 130 BPM: a step is 5538 samples and the gate 2769,
        // so the note-off is 43 blocks after the note-on.
        let mut scheduler = Scheduler::new();
        let pattern = one_row(60, &[0]);
        let step_len = step_len_samples(SR, DEFAULT_BPM);
        let gate = (step_len * GATE_FRACTION).round() as u64;
        let frames = 64;

        let mut buffer = NoteEventBuffer::new(64);
        let mut off_at = None;
        for block in 0..64 {
            let start = (block * frames) as u64;
            buffer.clear();
            scheduler.schedule(
                Some(&pattern),
                &window(start, frames, DEFAULT_BPM, true),
                &mut buffer,
            );
            if block == 0 {
                assert_eq!(buffer.len(), 1, "only the note-on fits in block 0");
                assert_eq!(scheduler.pending(), 1, "the note-off is owed");
            }
            for event in buffer.as_slice() {
                if let NoteKind::NoteOff { key } = event.kind {
                    assert_eq!(key, 60);
                    off_at = Some(start + u64::from(event.frame_offset));
                }
            }
        }
        assert_eq!(
            off_at,
            Some(gate),
            "parked, then emitted at the right sample"
        );
        assert_eq!(scheduler.pending(), 0, "nothing still owed");
    }

    #[test]
    fn a_note_parked_across_the_loop_wrap_is_unaffected_by_it() {
        // A hit on the *last* step of the bar, at a block size small enough that
        // its note-off is parked for several blocks. In between, the bar wraps.
        //
        // The claim is that the wrap is a non-event: the scheduler holds an
        // absolute sample position, and the pattern position is a modulo, so
        // there is no phase to reset and nothing for the parked note-off to be
        // corrupted by. That is why this test asserts *ordinary* results either
        // side of a bar line rather than a special case.
        let mut scheduler = Scheduler::new();
        let pattern = one_row(60, &[DEFAULT_STEPS - 1]);
        let step_len = step_len_samples(SR, DEFAULT_BPM);
        let gate = (step_len * GATE_FRACTION).round() as u64;
        let bar = step_boundary(DEFAULT_STEPS as u64, step_len);
        let frames = 2_048;

        // Long enough to reach step 15 of the second bar (~172 000 samples).
        let mut buffer = NoteEventBuffer::new(64);
        let mut ons: Vec<(usize, u64)> = Vec::new();
        let mut offs: Vec<(usize, u64)> = Vec::new();
        for block in 0..100 {
            let start = (block * frames) as u64;
            buffer.clear();
            scheduler.schedule(
                Some(&pattern),
                &window(start, frames, DEFAULT_BPM, true),
                &mut buffer,
            );
            for event in buffer.as_slice() {
                let at = start + u64::from(event.frame_offset);
                match event.kind {
                    NoteKind::NoteOff { .. } => offs.push((block, at)),
                    _ => ons.push((block, at)),
                }
            }
        }

        assert_eq!(
            ons[0].1,
            step_boundary(DEFAULT_STEPS as u64 - 1, step_len),
            "step 15 of bar 0"
        );
        assert_eq!(
            ons[1].1,
            step_boundary(2 * DEFAULT_STEPS as u64 - 1, step_len),
            "step 15 of bar 1: the loop is a modulo, not a reset"
        );
        assert!(
            ons[1].1 > bar,
            "the second hit really is in the next bar ({} > {bar})",
            ons[1].1
        );

        assert_eq!(offs[0].1, ons[0].1 + gate, "half a step after the hit");
        assert_eq!(offs[1].1, ons[1].1 + gate);
        assert!(
            offs[0].0 > ons[0].0,
            "the note-off was parked across at least one block boundary \
             (on in block {}, off in block {})",
            ons[0].0,
            offs[0].0
        );
        assert_eq!(scheduler.pending(), 0, "everything owed was paid");
    }

    #[test]
    fn stopping_releases_every_note_the_scheduler_still_owes() {
        let mut scheduler = Scheduler::new();
        // Four rows all hitting step 0: four note-offs owed at once.
        let mut pattern = Pattern::silent();
        for key in [36_u8, 38, 42, 46] {
            pattern.rows.push(PatternRow::empty(key).with_steps(&[0]));
        }
        let compiled = pattern.compile();

        let mut buffer = NoteEventBuffer::new(64);
        scheduler.schedule(
            Some(&compiled),
            &window(0, 64, DEFAULT_BPM, true),
            &mut buffer,
        );
        assert_eq!(buffer.len(), 4, "four note-ons");
        assert_eq!(scheduler.pending(), 4);

        // Stop: the same block that turns playback off must release them.
        buffer.clear();
        scheduler.schedule(
            Some(&compiled),
            &window(64, 64, DEFAULT_BPM, false),
            &mut buffer,
        );
        assert_eq!(buffer.len(), 4, "four note-offs");
        assert!(buffer.as_slice().iter().all(|e| e.frame_offset == 0));
        let mut keys: Vec<u8> = buffer
            .as_slice()
            .iter()
            .filter_map(|e| e.kind.key())
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, vec![36, 38, 42, 46]);
        assert_eq!(scheduler.pending(), 0, "nothing may stay stuck");

        // And a second stopped block adds nothing.
        buffer.clear();
        scheduler.schedule(
            Some(&compiled),
            &window(64, 64, DEFAULT_BPM, false),
            &mut buffer,
        );
        assert!(buffer.is_empty());
    }

    #[test]
    fn seeking_releases_owed_notes_and_resumes_on_the_grid_at_the_new_position() {
        let mut scheduler = Scheduler::new();
        let pattern = one_row(60, &(0..DEFAULT_STEPS).collect::<Vec<_>>());
        let step_len = step_len_samples(SR, DEFAULT_BPM);
        let mut buffer = NoteEventBuffer::new(64);

        scheduler.schedule(
            Some(&pattern),
            &window(0, 64, DEFAULT_BPM, true),
            &mut buffer,
        );
        assert_eq!(scheduler.pending(), 1);

        // Jump a long way forward, which is not where the last block ended.
        let target = step_boundary(40, step_len);
        buffer.clear();
        scheduler.schedule(
            Some(&pattern),
            &window(target, 64, DEFAULT_BPM, true),
            &mut buffer,
        );

        let offs: Vec<&NoteEvent> = buffer
            .as_slice()
            .iter()
            .filter(|e| matches!(e.kind, NoteKind::NoteOff { .. }))
            .collect();
        assert_eq!(offs.len(), 1, "the pre-seek note is released");
        assert_eq!(offs[0].frame_offset, 0);
        // And the block at the seek target fires the step that lives there:
        // global step 40 is 40 % 16 = 8 of the pattern.
        assert!(
            buffer
                .as_slice()
                .iter()
                .any(|e| e.kind.is_note_on() && e.frame_offset == 0),
            "the step at the seek target must fire"
        );
    }

    #[test]
    fn velocity_travels_from_the_grid_cell_to_the_note_event_untouched() {
        let mut pattern = Pattern::silent();
        let mut row = PatternRow::empty(38);
        row.channel = 5;
        row.steps[0] = crate::pattern::Step::hit(0.31);
        row.steps[4] = crate::pattern::Step::accent();
        pattern.rows.push(row);
        let compiled = pattern.compile();

        let mut scheduler = Scheduler::new();
        // Step 4 lands at sample 22 154, so the run has to be longer than that.
        let events = run(&mut scheduler, &compiled, 64, 512, DEFAULT_BPM, SR);
        let ons: Vec<&NoteEvent> = events
            .iter()
            .filter(|(_, e)| e.kind.is_note_on())
            .map(|(_, e)| e)
            .collect();
        assert_eq!(
            ons[0].kind,
            NoteKind::NoteOn {
                key: 38,
                velocity: 0.31
            }
        );
        assert_eq!(
            ons[1].kind,
            NoteKind::NoteOn {
                key: 38,
                velocity: 1.0
            }
        );
        assert!(
            ons.iter().all(|e| e.channel == 5),
            "the row's channel travels with it"
        );
    }

    #[test]
    fn live_notes_come_first_at_a_shared_frame_offset_and_scheduled_offs_precede_ons() {
        // The documented tie-break, asserted rather than assumed.
        let mut scheduler = Scheduler::new();
        let pattern = one_row(36, &[0]);
        let mut buffer = NoteEventBuffer::new(64);

        // The engine drains live input into the buffer first; here, by hand.
        buffer.push(NoteEvent::note_on(0, 69, 0.5));
        scheduler.schedule(
            Some(&pattern),
            &window(0, 512, DEFAULT_BPM, true),
            &mut buffer,
        );

        let events = buffer.as_slice();
        assert_eq!(events[0].kind.key(), Some(69), "live input first");
        assert_eq!(events[1].kind.key(), Some(36), "then the grid");

        // Now the other half: a flush (note-offs) and a step both at offset 0.
        // Stop-then-play at the same position puts an owed note-off at 0 with a
        // fresh step boundary, and the off must be pushed first.
        let mut scheduler = Scheduler::new();
        let mut buffer = NoteEventBuffer::new(64);
        scheduler.schedule(
            Some(&pattern),
            &window(0, 64, DEFAULT_BPM, true),
            &mut buffer,
        );
        assert_eq!(scheduler.pending(), 1);
        buffer.clear();
        // A seek back to 0 while playing: flush at 0, then step 0 fires at 0.
        scheduler.schedule(
            Some(&pattern),
            &window(0, 64, DEFAULT_BPM, true),
            &mut buffer,
        );
        let events = buffer.as_slice();
        assert_eq!(events.len(), 2);
        assert!(
            matches!(events[0].kind, NoteKind::NoteOff { .. }),
            "the flushed note-off must precede the retrigger: {events:?}"
        );
        assert!(events[1].kind.is_note_on());
    }

    #[test]
    fn a_pattern_at_the_row_limit_schedules_every_row() {
        let mut pattern = Pattern::silent();
        for row in 0..MAX_ROWS {
            pattern
                .rows
                .push(PatternRow::empty(36 + row as u8).with_steps(&[0]));
        }
        let compiled = pattern.compile();
        assert_eq!(compiled.row_count(), MAX_ROWS);

        let mut scheduler = Scheduler::new();
        let mut buffer = NoteEventBuffer::new(128);
        scheduler.schedule(
            Some(&compiled),
            &window(0, 64, DEFAULT_BPM, true),
            &mut buffer,
        );
        assert_eq!(buffer.len(), MAX_ROWS, "one note-on per row");
        assert_eq!(scheduler.pending(), MAX_ROWS);
        assert!(scheduler.pending() <= MAX_PENDING);
        assert_eq!(scheduler.overflowed(), 0);
    }

    #[test]
    fn a_full_event_buffer_is_reported_rather_than_silently_losing_steps() {
        let mut pattern = Pattern::silent();
        for row in 0..MAX_ROWS {
            pattern
                .rows
                .push(PatternRow::empty(36 + row as u8).with_steps(&[0]));
        }
        let compiled = pattern.compile();

        let mut scheduler = Scheduler::new();
        let mut buffer = NoteEventBuffer::new(4);
        let dropped = scheduler.schedule(
            Some(&compiled),
            &window(0, 64, DEFAULT_BPM, true),
            &mut buffer,
        );
        assert_eq!(buffer.len(), 4);
        assert_eq!(dropped, MAX_ROWS - 4);
    }

    #[test]
    fn a_block_larger_than_a_whole_bar_still_places_every_step() {
        // 999 BPM at 44.1 kHz: a step is 662 samples, so a 4096-frame block
        // spans six of them and a bar is barely two blocks.
        let bpm = 999.0;
        let rate = 44_100.0;
        let step_len = step_len_samples(rate, bpm);
        let mut scheduler = Scheduler::new();
        let pattern = one_row(36, &(0..DEFAULT_STEPS).collect::<Vec<_>>());
        let events = run(&mut scheduler, &pattern, 4, 4_096, bpm, rate);
        let got = onsets(&events);
        for (index, onset) in got.iter().enumerate() {
            assert_eq!(
                *onset,
                step_boundary(index as u64, step_len),
                "step {index}"
            );
        }
        assert!(
            got.len() > DEFAULT_STEPS,
            "more than a bar fits in four blocks"
        );
    }

    #[test]
    fn a_nonsense_tempo_or_rate_emits_nothing_instead_of_looping_forever() {
        let mut scheduler = Scheduler::new();
        let pattern = one_row(36, &[0]);
        let mut buffer = NoteEventBuffer::new(64);
        for (rate, bpm) in [
            (0.0, 130.0),
            (48_000.0, 0.0),
            (48_000.0, f64::NAN),
            (f64::NAN, 130.0),
            (48_000.0, f64::INFINITY),
        ] {
            buffer.clear();
            let window = TransportWindow {
                start_sample: 0,
                frames: 512,
                sample_rate: rate,
                bpm,
                playing: true,
            };
            assert!(!window.is_usable(), "{rate} Hz / {bpm} BPM");
            assert_eq!(scheduler.schedule(Some(&pattern), &window, &mut buffer), 0);
            assert!(buffer.is_empty());
        }
    }

    #[test]
    fn a_zero_frame_block_is_a_no_op() {
        let mut scheduler = Scheduler::new();
        let pattern = one_row(36, &[0]);
        let mut buffer = NoteEventBuffer::new(64);
        scheduler.schedule(
            Some(&pattern),
            &window(0, 0, DEFAULT_BPM, true),
            &mut buffer,
        );
        assert!(buffer.is_empty());
    }

    #[test]
    fn a_tempo_change_mid_bar_re_grids_from_absolute_position_rather_than_drifting() {
        // Changing tempo moves the grid: boundaries are always
        // `round(n * step_len)` for the *current* step length, computed from
        // absolute zero. That is a deliberate property -- the alternative
        // (carrying a phase across the change) is what a tempo *ramp* needs, and
        // there is no automation yet to ramp with.
        let mut scheduler = Scheduler::new();
        let pattern = one_row(36, &(0..DEFAULT_STEPS).collect::<Vec<_>>());
        let mut buffer = NoteEventBuffer::new(64);

        let fast = step_len_samples(SR, 260.0);
        let start = step_boundary(4, fast);
        buffer.clear();
        scheduler.schedule(Some(&pattern), &window(start, 64, 260.0, true), &mut buffer);
        assert!(
            buffer
                .as_slice()
                .iter()
                .any(|e| e.kind.is_note_on() && e.frame_offset == 0),
            "at 260 BPM, sample {start} is exactly a step boundary"
        );
    }

    #[test]
    fn reset_forgets_everything_without_emitting() {
        let mut scheduler = Scheduler::new();
        let pattern = one_row(36, &[0]);
        let mut buffer = NoteEventBuffer::new(64);
        scheduler.schedule(
            Some(&pattern),
            &window(0, 64, DEFAULT_BPM, true),
            &mut buffer,
        );
        assert_eq!(scheduler.pending(), 1);
        scheduler.reset();
        assert_eq!(scheduler.pending(), 0);
        assert!(!scheduler.is_playing());
    }

    #[test]
    fn parking_past_the_limit_is_counted_rather_than_growing_the_array() {
        let mut scheduler = Scheduler::new();
        for index in 0..MAX_PENDING {
            assert!(scheduler.park(PendingOff {
                at: 1_000 + index as u64,
                key: 36,
                channel: 0
            }));
        }
        assert_eq!(scheduler.pending(), MAX_PENDING);
        assert!(!scheduler.park(PendingOff {
            at: 9_999,
            key: 36,
            channel: 0
        }));
    }

    // ----------------------------------------------------------- piano roll

    use crate::pattern::{MelodicNote, MAX_NOTES, PATTERN_LENGTHS, TICKS_PER_STEP};

    /// A pattern with no drum rows and the given notes, `length` steps long.
    fn notes_only(length: u8, notes: &[MelodicNote]) -> CompiledPattern {
        Pattern {
            rows: Vec::new(),
            length_steps: length,
            notes: notes.to_vec(),
        }
        .compile()
    }

    /// `(absolute sample, key)` of the note-ons in `events`.
    fn on_keys(events: &[(u64, NoteEvent)]) -> Vec<(u64, u8)> {
        events
            .iter()
            .filter(|(_, e)| e.kind.is_note_on())
            .map(|(at, e)| (*at, e.kind.key().unwrap()))
            .collect()
    }

    /// `(absolute sample, key)` of the note-offs in `events`.
    fn off_keys(events: &[(u64, NoteEvent)]) -> Vec<(u64, u8)> {
        events
            .iter()
            .filter(|(_, e)| matches!(e.kind, NoteKind::NoteOff { .. }))
            .map(|(at, e)| (*at, e.kind.key().unwrap()))
            .collect()
    }

    #[test]
    fn a_tick_on_a_step_line_is_the_very_same_sample_as_that_step() {
        // The claim the whole tick representation rests on. Not "within a
        // sample": *equal*, for ten thousand steps -- which is four hundred bars
        // -- at every rate and tempo the engine allows. If this ever fails, a
        // melody written on the grid will flam against the drums, quietly.
        for sample_rate in [44_100.0_f64, 48_000.0, 96_000.0] {
            for bpm in [20.0_f64, 60.0, 130.0, 174.3, 999.0] {
                let step_len = step_len_samples(sample_rate, bpm);
                for step in 0..10_000_u64 {
                    assert_eq!(
                        tick_boundary(step * u64::from(TICKS_PER_STEP), step_len),
                        step_boundary(step, step_len),
                        "{sample_rate} Hz / {bpm} BPM, step {step}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_tick_between_the_grid_lines_is_within_half_a_sample_of_the_ideal() {
        // Off-grid positions get no exactness guarantee, only the rounding one.
        // A triplet eighth (320 ticks) and a 64th (60) are the cases that matter,
        // because they are the ones a step grid cannot express at all.
        let step_len = step_len_samples(48_000.0, 174.3);
        for tick in [60_u64, 320, 640, 1_500, 3_839, 15_359, 100_000] {
            let ideal = tick as f64 / f64::from(TICKS_PER_STEP) * step_len;
            let got = tick_boundary(tick, step_len) as f64;
            assert!(
                (got - ideal).abs() <= 0.5,
                "tick {tick}: {got} is {} samples from {ideal}",
                got - ideal
            );
        }
    }

    #[test]
    fn a_melodic_note_emits_an_on_and_an_off_at_the_samples_its_ticks_name() {
        let step_len = step_len_samples(SR, DEFAULT_BPM);
        // C4 on the second beat, a quarter note long.
        let pattern = notes_only(16, &[MelodicNote::new(60, 1, 960, 960).at_velocity(0.5)]);

        let mut scheduler = Scheduler::new();
        // Long enough to reach tick 1920, which is 44 308 samples in.
        let events = run(&mut scheduler, &pattern, 128, 512, DEFAULT_BPM, SR);

        let ons = on_keys(&events);
        let offs = off_keys(&events);
        // Tick 960 is step 4; tick 1920 is step 8.
        assert_eq!(ons[0], (step_boundary(4, step_len), 60));
        assert_eq!(offs[0], (step_boundary(8, step_len), 60));

        let on_event = events.iter().find(|(_, e)| e.kind.is_note_on()).unwrap().1;
        assert_eq!(
            on_event.kind,
            NoteKind::NoteOn {
                key: 60,
                velocity: 0.5
            }
        );
        assert_eq!(on_event.channel, 1, "the note's channel is its track");
        assert_eq!(scheduler.pending(), 0, "nothing left owed");
    }

    #[test]
    fn a_note_written_on_the_grid_sounds_with_the_drum_hit_on_that_step() {
        // The property a musician would notice if it were false. Kick on step 4,
        // bass note on tick 960 -- the same instant -- and the two note-ons must
        // share a frame offset, not merely a block.
        let mut pattern = Pattern::default();
        pattern.rows[0] = PatternRow::empty(36).with_steps(&[0, 4, 8, 12]);
        pattern.notes = vec![
            MelodicNote::new(36, 1, 0, 240),
            MelodicNote::new(43, 1, 960, 240),
            MelodicNote::new(48, 1, 1_920, 240),
            MelodicNote::new(43, 1, 2_880, 240),
        ];
        let compiled = pattern.compile();

        let mut scheduler = Scheduler::new();
        // One whole bar is 88 615 samples at 130 BPM.
        let events = run(&mut scheduler, &compiled, 200, 512, DEFAULT_BPM, SR);

        // The run is a little longer than a bar, so the loop has begun again;
        // the first four of each are the bar under test.
        let drum_hits: Vec<u64> = events
            .iter()
            .filter(|(_, e)| e.kind.is_note_on() && e.kind.key() == Some(36) && e.channel == 0)
            .map(|(at, _)| *at)
            .take(4)
            .collect();
        let melody: Vec<u64> = events
            .iter()
            .filter(|(_, e)| e.kind.is_note_on() && e.channel == 1)
            .map(|(at, _)| *at)
            .take(4)
            .collect();
        assert_eq!(drum_hits.len(), 4);
        assert_eq!(
            drum_hits, melody,
            "the bass line and the kick must land on identical samples"
        );
    }

    #[test]
    fn the_notes_loop_with_the_pattern_however_long_it_is() {
        for length in PATTERN_LENGTHS {
            let step_len = step_len_samples(SR, DEFAULT_BPM);
            // One note on the very first tick, so its onsets are the loop points.
            let pattern = notes_only(length, &[MelodicNote::new(60, 0, 0, 240)]);
            let bar = step_boundary(u64::from(length), step_len);

            let mut scheduler = Scheduler::new();
            let blocks = (bar as usize * 3) / 2_048 + 2;
            let events = run(&mut scheduler, &pattern, blocks, 2_048, DEFAULT_BPM, SR);
            let ons = on_keys(&events);
            assert!(ons.len() >= 3, "{length} steps: only {} onsets", ons.len());
            for (index, (at, _)) in ons.iter().take(3).enumerate() {
                assert_eq!(
                    *at,
                    step_boundary(index as u64 * u64::from(length), step_len),
                    "{length} steps, loop {index}"
                );
            }
        }
    }

    #[test]
    fn a_note_ending_on_the_bar_line_has_its_off_parked_across_the_wrap() {
        // The melodic counterpart of the drum test above, and the case the spec
        // calls out: a note that starts near the end of the loop and whose
        // note-off is due *after* the pattern has wrapped. There is no phase to
        // reset, so the claim is that nothing special happens -- the off arrives
        // at the loop point, several blocks after the on, and the next time round
        // the note starts again.
        let step_len = step_len_samples(SR, DEFAULT_BPM);
        let pattern = notes_only(
            16,
            &[MelodicNote::new(72, 0, 15 * 240, 240)], // the last step, filled
        );
        let bar = step_boundary(16, step_len);
        let frames = 512;

        let mut scheduler = Scheduler::new();
        let mut buffer = NoteEventBuffer::new(64);
        let mut ons: Vec<(usize, u64)> = Vec::new();
        let mut offs: Vec<(usize, u64)> = Vec::new();
        // Two whole bars, so the second time round is really reached.
        for block in 0..350 {
            let start = (block * frames) as u64;
            buffer.clear();
            scheduler.schedule(
                Some(&pattern),
                &window(start, frames, DEFAULT_BPM, true),
                &mut buffer,
            );
            for event in buffer.as_slice() {
                let at = start + u64::from(event.frame_offset);
                match event.kind {
                    NoteKind::NoteOff { .. } => offs.push((block, at)),
                    _ => ons.push((block, at)),
                }
            }
        }

        assert_eq!(ons[0].1, step_boundary(15, step_len), "step 15 of bar 0");
        assert_eq!(
            offs[0].1, bar,
            "the note-off is the first sample of the next time round"
        );
        assert!(
            offs[0].0 > ons[0].0,
            "the off was parked across at least one block boundary \
             (on in block {}, off in block {})",
            ons[0].0,
            offs[0].0
        );
        assert_eq!(ons[1].1, step_boundary(31, step_len), "step 15 of bar 1");
        assert_eq!(offs[1].1, step_boundary(32, step_len));
        assert_eq!(scheduler.pending(), 0, "everything owed was paid");
    }

    #[test]
    fn a_chord_is_three_note_ons_at_one_offset_and_three_offs_at_another() {
        let step_len = step_len_samples(SR, DEFAULT_BPM);
        let pattern = notes_only(
            16,
            &[
                MelodicNote::new(60, 2, 0, 480),
                MelodicNote::new(64, 2, 0, 480),
                MelodicNote::new(67, 2, 0, 480),
            ],
        );

        let mut scheduler = Scheduler::new();
        let mut buffer = NoteEventBuffer::new(64);
        // One block long enough to hold the whole chord: the note-off is 11 077
        // samples in, and parking it would prove nothing about ordering.
        scheduler.schedule(
            Some(&pattern),
            &window(0, 16_384, DEFAULT_BPM, true),
            &mut buffer,
        );

        let ons: Vec<&NoteEvent> = buffer
            .as_slice()
            .iter()
            .filter(|e| e.kind.is_note_on())
            .collect();
        assert_eq!(ons.len(), 3, "a triad is three notes, not one");
        assert!(ons.iter().all(|e| e.frame_offset == 0));
        let mut keys: Vec<u8> = ons.iter().filter_map(|e| e.kind.key()).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec![60, 64, 67]);

        let offs: Vec<&NoteEvent> = buffer
            .as_slice()
            .iter()
            .filter(|e| matches!(e.kind, NoteKind::NoteOff { .. }))
            .collect();
        assert_eq!(offs.len(), 3);
        let expected_off = step_boundary(2, step_len) as u32;
        assert!(offs.iter().all(|e| e.frame_offset == expected_off));
    }

    #[test]
    fn stopping_releases_the_melodic_notes_the_scheduler_still_owes() {
        // A chord of long notes: every one of them is owed a note-off, and Stop
        // is the moment they would otherwise be left sounding for ever.
        let pattern = notes_only(
            16,
            &[
                MelodicNote::new(60, 1, 0, 3_840),
                MelodicNote::new(64, 1, 0, 3_840),
                MelodicNote::new(67, 1, 0, 3_840),
            ],
        );

        let mut scheduler = Scheduler::new();
        let mut buffer = NoteEventBuffer::new(64);
        scheduler.schedule(
            Some(&pattern),
            &window(0, 64, DEFAULT_BPM, true),
            &mut buffer,
        );
        assert_eq!(buffer.len(), 3, "three note-ons");
        assert_eq!(scheduler.pending(), 3, "three note-offs owed");

        buffer.clear();
        scheduler.schedule(
            Some(&pattern),
            &window(64, 64, DEFAULT_BPM, false),
            &mut buffer,
        );
        assert_eq!(buffer.len(), 3, "three note-offs");
        assert!(buffer.as_slice().iter().all(|e| e.frame_offset == 0));
        assert!(buffer
            .as_slice()
            .iter()
            .all(|e| matches!(e.kind, NoteKind::NoteOff { .. }) && e.channel == 1));
        assert_eq!(scheduler.pending(), 0, "nothing may stay stuck");
    }

    #[test]
    fn seeking_releases_owed_melodic_notes_and_resumes_where_it_landed() {
        let step_len = step_len_samples(SR, DEFAULT_BPM);
        let pattern = notes_only(
            16,
            &[
                MelodicNote::new(60, 0, 0, 3_840),
                MelodicNote::new(72, 0, 1_920, 240),
            ],
        );

        let mut scheduler = Scheduler::new();
        let mut buffer = NoteEventBuffer::new(64);
        scheduler.schedule(
            Some(&pattern),
            &window(0, 64, DEFAULT_BPM, true),
            &mut buffer,
        );
        assert_eq!(scheduler.pending(), 1, "the whole note is owed an off");

        // Jump to the tick the second note starts on, in the third loop.
        let target = tick_boundary(2 * 3_840 + 1_920, step_len);
        buffer.clear();
        scheduler.schedule(
            Some(&pattern),
            &window(target, 64, DEFAULT_BPM, true),
            &mut buffer,
        );

        let events = buffer.as_slice();
        assert!(
            matches!(events[0].kind, NoteKind::NoteOff { key: 60 }) && events[0].frame_offset == 0,
            "the pre-seek note is released first: {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| e.kind.key() == Some(72) && e.kind.is_note_on() && e.frame_offset == 0),
            "the note at the seek target must fire: {events:?}"
        );
    }

    #[test]
    fn drums_notes_and_live_input_merge_into_one_ordered_stream() {
        // The whole point of one scheduler rather than two: at a shared offset
        // the order is live input, then the grid, then the piano roll, and the
        // buffer is sorted by offset throughout.
        let mut pattern = Pattern::default();
        pattern.rows[0] = PatternRow::empty(36).with_steps(&[0, 4]);
        pattern.notes = vec![
            MelodicNote::new(60, 1, 0, 240),
            MelodicNote::new(64, 1, 120, 240),
        ];
        let compiled = pattern.compile();

        let mut scheduler = Scheduler::new();
        let mut buffer = NoteEventBuffer::new(64);
        // The engine drains live input first; here, by hand.
        buffer.push(NoteEvent::note_on(0, 69, 0.5).on_channel(3));
        scheduler.schedule(
            Some(&compiled),
            &window(0, 4_096, DEFAULT_BPM, true),
            &mut buffer,
        );

        assert!(
            buffer.is_sorted(),
            "the merged stream must be in time order"
        );
        let at_zero: Vec<(u8, u8)> = buffer
            .as_slice()
            .iter()
            .filter(|e| e.frame_offset == 0 && e.kind.is_note_on())
            .map(|e| (e.kind.key().unwrap(), e.channel))
            .collect();
        assert_eq!(
            at_zero,
            vec![(69, 3), (36, 0), (60, 1)],
            "live input, then the grid, then the roll"
        );
    }

    #[test]
    fn a_thousand_notes_are_found_by_search_rather_than_by_walking_them_all() {
        // The binary search is only correct because the compiled notes are
        // sorted. This fills the array to the brim with notes one tick apart and
        // asks for a block in the middle of it: the right ones, and only the
        // right ones, have to come out.
        let notes: Vec<MelodicNote> = (0..MAX_NOTES)
            .map(|i| MelodicNote::new(36 + (i % 60) as u8, 0, i as u32 * 15, 15))
            .collect();
        let pattern = notes_only(64, &notes);
        assert_eq!(pattern.note_count(), MAX_NOTES);

        let step_len = step_len_samples(SR, DEFAULT_BPM);
        let mut scheduler = Scheduler::new();

        // Walk the whole loop in blocks and collect every note-on: the set has
        // to be exactly the thousand notes, in tick order.
        let loop_samples = step_boundary(64, step_len);
        let frames = 512;
        let blocks = (loop_samples as usize / frames) + 2;
        let mut buffer = NoteEventBuffer::new(4_096);
        let mut ons: Vec<(u64, u8)> = Vec::new();
        for block in 0..blocks {
            let start = (block * frames) as u64;
            buffer.clear();
            assert_eq!(
                scheduler.schedule(
                    Some(&pattern),
                    &window(start, frames, DEFAULT_BPM, true),
                    &mut buffer
                ),
                0,
                "nothing may be dropped"
            );
            for event in buffer.as_slice() {
                if event.kind.is_note_on() {
                    ons.push((
                        start + u64::from(event.frame_offset),
                        event.kind.key().unwrap(),
                    ));
                }
            }
        }

        assert!(ons.len() >= MAX_NOTES, "only {} notes fired", ons.len());
        for (index, (at, key)) in ons.iter().take(MAX_NOTES).enumerate() {
            assert_eq!(
                *at,
                tick_boundary(index as u64 * 15, step_len),
                "note {index} landed on the wrong sample"
            );
            assert_eq!(
                *key,
                36 + (index % 60) as u8,
                "note {index} is the wrong key"
            );
        }
        assert_eq!(scheduler.overflowed(), 0, "no note-off was ever lost");
    }

    #[test]
    fn ten_minutes_of_melodic_playback_does_not_drift_by_a_single_sample() {
        // The drum grid's drift test, for the tick clock. A note on the first
        // tick of a four-bar loop, run for ten minutes at an awkward tempo and
        // an awkward block size.
        let bpm = 174.3;
        let step_len = step_len_samples(SR, bpm);
        let pattern = notes_only(64, &[MelodicNote::new(60, 0, 0, 240)]);

        let frames = 441;
        let blocks = ((SR * 600.0) as u64 / frames as u64) as usize;
        let mut scheduler = Scheduler::new();
        let mut buffer = NoteEventBuffer::new(64);
        let mut onsets = Vec::new();
        for block in 0..blocks {
            let start = (block * frames) as u64;
            buffer.clear();
            scheduler.schedule(
                Some(&pattern),
                &window(start, frames, bpm, true),
                &mut buffer,
            );
            for event in buffer.as_slice() {
                if event.kind.is_note_on() {
                    onsets.push(start + u64::from(event.frame_offset));
                }
            }
        }

        // A four-bar loop at 174.3 BPM is 264 372 samples, so ten minutes is
        // about a hundred of them.
        assert!(
            onsets.len() > 90,
            "ten minutes should be ~100 loops, got {}",
            onsets.len()
        );
        for (loop_index, onset) in onsets.iter().enumerate() {
            assert_eq!(
                *onset,
                step_boundary(loop_index as u64 * 64, step_len),
                "loop {loop_index} drifted"
            );
        }
        let last = *onsets.last().unwrap();
        let ideal = (onsets.len() - 1) as f64 * step_len * 64.0;
        assert!(
            (last as f64 - ideal).abs() <= 0.5,
            "after ten minutes the error is {} samples",
            last as f64 - ideal
        );
    }

    #[test]
    fn every_note_lands_on_its_tick_at_every_tempo_rate_and_block_size() {
        // The melodic counterpart of the step-boundary sweep: how the blocks are
        // cut must make no difference at all to where a note sounds.
        let notes = [
            MelodicNote::new(60, 0, 0, 60),
            MelodicNote::new(62, 0, 320, 60),   // a triplet eighth
            MelodicNote::new(64, 0, 960, 60),   // beat two
            MelodicNote::new(65, 0, 1_501, 60), // deliberately off everything
            MelodicNote::new(67, 0, 3_600, 60),
        ];
        let pattern = notes_only(16, &notes);

        for sample_rate in [44_100.0_f64, 48_000.0] {
            for bpm in [60.0_f64, 130.0, 174.3, 999.0] {
                let step_len = step_len_samples(sample_rate, bpm);
                let expected: Vec<u64> = notes
                    .iter()
                    .map(|n| tick_boundary(u64::from(n.start_ticks), step_len))
                    .collect();
                let loop_samples = step_boundary(16, step_len) as f64;

                for frames in [32_usize, 256, 441, 4_096] {
                    let mut scheduler = Scheduler::new();
                    let blocks = (loop_samples / frames as f64).ceil() as usize + 1;
                    let events = run(&mut scheduler, &pattern, blocks, frames, bpm, sample_rate);
                    let got: Vec<u64> =
                        on_keys(&events).iter().map(|(at, _)| *at).take(5).collect();
                    assert_eq!(
                        got, expected,
                        "{sample_rate} Hz / {bpm} BPM / {frames} frames"
                    );
                }
            }
        }
    }

    #[test]
    fn a_pattern_with_notes_but_no_rows_still_plays_them() {
        // `schedule` returns early when there are no drum rows; the piano roll
        // must not be behind that gate.
        let pattern = notes_only(16, &[MelodicNote::new(60, 0, 0, 240)]);
        assert_eq!(pattern.row_count(), 0);

        let mut scheduler = Scheduler::new();
        let mut buffer = NoteEventBuffer::new(64);
        scheduler.schedule(
            Some(&pattern),
            &window(0, 512, DEFAULT_BPM, true),
            &mut buffer,
        );
        assert_eq!(buffer.len(), 1, "the note-on: {:?}", buffer.as_slice());
        assert!(buffer.as_slice()[0].kind.is_note_on());
    }

    #[test]
    fn a_stopped_transport_emits_no_notes_either() {
        let pattern = notes_only(16, &[MelodicNote::new(60, 0, 0, 240)]);
        let mut scheduler = Scheduler::new();
        let mut buffer = NoteEventBuffer::new(64);
        for block in 0..8 {
            buffer.clear();
            scheduler.schedule(
                Some(&pattern),
                &window(block * 512, 512, DEFAULT_BPM, false),
                &mut buffer,
            );
            assert!(buffer.is_empty(), "block {block} emitted while stopped");
        }
    }

    #[test]
    fn a_note_off_never_precedes_its_own_note_on_however_short_the_note() {
        // One tick at 20 BPM is 3.1 samples; at 999 BPM it is 0.06, which rounds
        // to the same sample as the note-on. A note-off at or before its note-on
        // would sort ahead of it and leave the voice sounding for ever.
        for bpm in [20.0_f64, 130.0, 999.0] {
            let pattern = notes_only(16, &[MelodicNote::new(60, 0, 0, 1)]);
            let mut scheduler = Scheduler::new();
            let mut buffer = NoteEventBuffer::new(64);
            scheduler.schedule(Some(&pattern), &window(0, 512, bpm, true), &mut buffer);
            let events = buffer.as_slice();
            assert_eq!(events.len(), 2, "{bpm} BPM: {events:?}");
            assert!(events[0].kind.is_note_on(), "{bpm} BPM: {events:?}");
            assert!(
                matches!(events[1].kind, NoteKind::NoteOff { .. })
                    && events[1].frame_offset > events[0].frame_offset,
                "{bpm} BPM: the off must be strictly after the on: {events:?}"
            );
        }
    }
}
