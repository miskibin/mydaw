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
//! # The pending note-off state machine
//! A step's note-on is emitted at its boundary; its note-off is due
//! [`GATE_FRACTION`] of a step later. That is usually inside the same block --
//! but at a small block size, or a slow tempo, or when the note-off lands in the
//! *next* bar, it is not. So a note-off that falls past the end of the block is
//! parked in a fixed-size array and emitted by a later block:
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
//! - The array is sized `MAX_ROWS * 2`, twice what a 50 % gate can produce.
//!
//! # Ordering at the same frame offset
//! [`NoteEventBuffer`] is a stable sort, so the order events are *pushed* is the
//! order the plugin sees them at any one offset. The engine pushes **live input
//! first, then the sequencer**, and within the sequencer **note-offs before
//! note-ons**. Live-first because a key the user is pressing right now should
//! not be pre-empted by the grid; offs-before-ons because that turns a step that
//! retriggers a still-sounding key into a retrigger rather than a stuck note.
//!
//! # RT safety
//! Every field is a fixed-size array or a scalar. `schedule` allocates nothing,
//! locks nothing and calls nothing that can. `tests/rt_alloc.rs` measures it.

use mydaw_rt_core::{NoteEvent, NoteEventBuffer};

use crate::compiled::CompiledPattern;
use crate::pattern::{MAX_ROWS, STEPS_PER_BEAT};

/// Fraction of a step a scheduled note sounds for.
///
/// A half-step gate is the classic drum-machine value: long enough that a synth
/// row plays a note rather than a click, short enough that consecutive steps do
/// not overlap. Per-step note length is a piano-roll feature, not a step-grid
/// one -- see the spec's limitations.
pub const GATE_FRACTION: f64 = 0.5;

/// Parked note-offs the scheduler can hold at once.
///
/// Two per row. A [`GATE_FRACTION`] of 0.5 can only ever leave one note per row
/// pending, so this is a doubling of the worst case rather than a guess -- and
/// it stays correct if the gate is ever raised towards a full step.
pub const MAX_PENDING: usize = MAX_ROWS * 2;

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
        if pattern.row_count() == 0 || pattern.steps() == 0 {
            return dropped;
        }

        // --- step boundaries falling inside [start, end) ---------------------
        let step_len = step_len_samples(window.sample_rate, window.bpm);
        let gate = (step_len * GATE_FRACTION).round().max(1.0) as u64;
        let steps = pattern.steps() as u64;

        // Start one step early: `step_boundary` rounds, so the boundary of
        // `floor(start / step_len)` can land just *below* `start` and the one
        // before it cannot. Boundaries below `start` are skipped, not emitted.
        let mut index = ((start as f64 / step_len).floor() as u64).saturating_sub(1);
        loop {
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
    use crate::pattern::{Pattern, PatternRow, STEPS};
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
        let pattern = one_row(36, &(0..STEPS).collect::<Vec<_>>());
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
                let expected: Vec<u64> = (0..STEPS as u64)
                    .map(|n| step_boundary(n, step_len))
                    .collect();
                let bar = step_len * STEPS as f64;

                for frames in [32_usize, 256, 441, 4_096] {
                    let mut scheduler = Scheduler::new();
                    let pattern = one_row(36, &(0..STEPS).collect::<Vec<_>>());
                    // Enough blocks to cover one whole bar, plus one.
                    let blocks = (bar / frames as f64).ceil() as usize + 1;
                    let events = run(&mut scheduler, &pattern, blocks, frames, bpm, sample_rate);
                    let got = onsets(&events);
                    assert!(
                        got.len() >= STEPS,
                        "{sample_rate} Hz / {bpm} BPM / {frames} frames: only {} onsets",
                        got.len()
                    );
                    assert_eq!(
                        &got[..STEPS],
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
            let expected = step_boundary(bar as u64 * STEPS as u64, step_len);
            assert_eq!(
                *onset, expected,
                "bar {bar} drifted: {onset} instead of {expected}"
            );
        }
        // And the last one is still within half a sample of the ideal position,
        // which is the rounding and nothing else.
        let last = *onsets.last().unwrap();
        let ideal = (onsets.len() - 1) as f64 * step_len * STEPS as f64;
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
        let pattern = one_row(60, &[STEPS - 1]);
        let step_len = step_len_samples(SR, DEFAULT_BPM);
        let gate = (step_len * GATE_FRACTION).round() as u64;
        let bar = step_boundary(STEPS as u64, step_len);
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
            step_boundary(STEPS as u64 - 1, step_len),
            "step 15 of bar 0"
        );
        assert_eq!(
            ons[1].1,
            step_boundary(2 * STEPS as u64 - 1, step_len),
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
        let pattern = one_row(60, &(0..STEPS).collect::<Vec<_>>());
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
        let pattern = one_row(36, &(0..STEPS).collect::<Vec<_>>());
        let events = run(&mut scheduler, &pattern, 4, 4_096, bpm, rate);
        let got = onsets(&events);
        for (index, onset) in got.iter().enumerate() {
            assert_eq!(
                *onset,
                step_boundary(index as u64, step_len),
                "step {index}"
            );
        }
        assert!(got.len() > STEPS, "more than a bar fits in four blocks");
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
        let pattern = one_row(36, &(0..STEPS).collect::<Vec<_>>());
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
}
