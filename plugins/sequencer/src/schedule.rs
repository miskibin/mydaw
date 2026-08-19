//! The scheduler: song plus transport window in, note events out.
//!
//! This is the timing-critical part of the DAW, and it is deliberately a *pure*
//! piece of arithmetic over a fixed-size state machine: no audio, no device, no
//! graph, no clock of its own. [`Scheduler::schedule`] is handed the song, the
//! block's transport window and the block's event buffer, and everything it does
//! is visible in what lands in that buffer. That is what makes the tempo grid
//! testable at 999 BPM and 4096-frame blocks without a sound card.
//!
//! # Two modes, one loop
//! ```text
//!   PAT   |<-- active pattern -->|<-- active pattern -->|<-- ...   forever
//!
//!   SONG  |A....|A....|     |A....|          |B..|B..|B..|
//!         0     4     8     12                          (bars)
//! ```
//!
//! [`PlayMode::Pattern`] loops the set's active pattern from tick 0, which is
//! what the step grid and the piano roll are editing. [`PlayMode::Song`] walks
//! the playlist's clips against the same clock. Both go through the *same*
//! emitter ([`Scheduler::emit_run`]): a pattern clip is "this pattern, from this
//! tick, until this tick", and pattern mode is the same thing with a tick of
//! zero and no end. One loop, one set of note-off rules, one thing to get right.
//!
//! # Drift-free by construction
//! A step boundary is computed from the **absolute** tick, never accumulated:
//!
//! ```text
//!   step_len = sample_rate * 60 / bpm / 4          (f64, fractional)
//!   boundary(n) = round(n * step_len)              (u64, exact for every n)
//!   tick_boundary(t) = round((t / TICKS_PER_STEP) * step_len)
//! ```
//!
//! Nothing anywhere adds `step_len` to a running total, so there is no error to
//! accumulate. After ten minutes at 174.3 BPM the 10 000th boundary is still
//! `round(10000 * step_len)` -- within half a sample of the ideal position, and
//! that half sample is the rounding, not drift. The tests assert exactly this.
//!
//! `tick_boundary` is written as a division *before* the multiplication for one
//! reason: when `t` is a whole number of steps, `t / 240` is *exactly* the
//! integer step index (an exact multiple of 240 divided by 240 is exact in IEEE
//! 754), so the expression becomes `round(n * step_len)` -- character for
//! character [`step_boundary`]. A note written on the grid therefore lands on
//! the very same sample as a step there, not within a sample of it. The equality
//! is asserted over ten thousand steps at four tempos.
//!
//! Loop `k` of a pattern `L` steps long starts at tick `k * L * TICKS_PER_STEP`,
//! so loop points are step boundaries too and a wrap is a non-event: nothing is
//! reset, nothing is phase-locked, and a seek needs no cooperation at all.
//!
//! # The pending note-off state machine
//! A note's off is due at `start + length`, which is usually inside the same
//! block -- but at a small block size, a slow tempo, or when the off lands in
//! the next bar, it is not. So a note-off that falls past the end of the block
//! is parked in a fixed-size array and emitted by a later block:
//!
//! ```text
//!   block k        |----------|                  on(step n) at +12
//!                       off due at +900 -> park
//!   block k+1      |----------|                  off emitted at +388
//! ```
//!
//! Drums ignore note-offs (a one-shot is a one-shot), but the poly synth does
//! not, and a sequencer that leaks note-ons into a synth is a sequencer that ends
//! the session with sixteen stuck voices. Two events that must never be lost,
//! therefore, get a state machine rather than a hope:
//!
//! - **Stop** flushes every parked note-off at frame 0 of the stopping block.
//! - **Seek** does the same: the notes belong to a bar that is no longer under
//!   the playhead.
//! - **Switching mode** is a seek in every sense that matters here.
//! - The array is [`MAX_PENDING`] long, and anything past it is *counted*
//!   ([`Scheduler::overflowed`]) rather than silently dropped.
//!
//! # Ordering at the same frame offset
//! [`NoteEventBuffer`] is a stable sort, so the order events are *pushed* is the
//! order the plugin sees them at any one offset. The engine pushes **live input
//! first, then the sequencer**. Live-first because a key the user is pressing
//! right now should not be pre-empted by the grid.
//!
//! # RT safety
//! Every field is a fixed-size array or a scalar. `schedule` allocates nothing,
//! locks nothing and calls nothing that can. It **binary-searches** the sorted
//! note array for the first note due in the block rather than walking all
//! thousand of them, so the per-block cost is `O(clips + log n + emitted)`.
//! `tests/rt_alloc.rs` measures the allocation claim with a counting allocator.

use mydaw_rt_core::{NoteEvent, NoteEventBuffer};

use crate::compiled::{CompiledPattern, CompiledSong};
use crate::pattern::{MAX_NOTES, MAX_PATTERN_CHANNELS, STEPS_PER_BEAT, TICKS_PER_STEP};

/// What the transport is playing. Owned by `rt-core`, because it is transport
/// state and not sequencer state -- see [`mydaw_rt_core::PlayMode`].
pub use mydaw_rt_core::PlayMode;

/// Parked note-offs the scheduler can hold at once.
///
/// Four thousand. In pattern mode the true worst case is one per sounding note
/// ([`MAX_NOTES`] per channel is the *editing* limit, and a pattern that hit it
/// on every channel could not be played by any instrument), and in song mode
/// overlapping clips can stack several patterns' worth. This is sized past both
/// with room to spare, at 64 kB of a structure that is allocated once per
/// stream; running out is counted rather than assumed away.
pub const MAX_PENDING: usize = 4 * 1_024;

const _: () = assert!(MAX_PENDING >= MAX_NOTES * 2);
const _: () = assert!(MAX_PENDING >= MAX_PATTERN_CHANNELS * crate::pattern::MAX_STEPS);

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

/// The tick a sample position falls on, rounding down.
///
/// The inverse of [`tick_boundary`], and only ever used *outside* the scheduler
/// -- to tell a UI where the playhead is in musical time. The scheduler itself
/// never goes this way round, because rounding a position back into ticks and
/// forward into samples again is exactly the accumulation the module refuses.
pub fn ticks_at_sample(sample: u64, step_len: f64) -> u64 {
    if !(step_len.is_finite() && step_len >= 1.0) {
        return 0;
    }
    ((sample as f64 / step_len) * TICKS_PER_STEP as f64).floor() as u64
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
    /// Whether the transport is running. The scheduler emits sequenced events
    /// only while it is -- live input is a separate path and is never gated.
    pub playing: bool,
    /// Pattern or song.
    pub mode: PlayMode,
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

/// Turns a song and a transport window into note events.
///
/// Owns exactly one piece of state: the note-offs it still owes. Everything else
/// is derived from the window, which is why a seek needs no cooperation and a
/// loop wrap needs no code.
pub struct Scheduler {
    /// Heap-allocated: 64 kB inline blew the main thread's 1 MB stack in debug
    /// builds, where every move of the enclosing `RtState` into the cpal closure
    /// is a real memcpy on the stack.
    pending: Box<[PendingOff; MAX_PENDING]>,
    pending_len: usize,
    /// Transport state as of the previous block, for edge detection.
    playing: bool,
    /// Mode as of the previous block. A change is a seek.
    mode: PlayMode,
    /// Sample position the next block is expected to start at. A mismatch is a
    /// seek.
    next_sample: u64,
    /// Note-offs that did not fit [`MAX_PENDING`]. Always zero in practice; a
    /// non-zero value would mean stuck notes, so it is counted rather than
    /// assumed away.
    overflowed: u64,
    /// Set while the last block was in song mode at or past the last clip.
    song_ended: bool,
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl Scheduler {
    /// A scheduler with nothing owed, stopped at sample 0, in pattern mode.
    pub fn new() -> Self {
        // Build the pending store on the heap from the start: `Box::new([..])`
        // or a by-value array literal would construct all 64 kB on the stack
        // first, which is the exact overflow this layout exists to avoid.
        let pending: Box<[PendingOff; MAX_PENDING]> = vec![
            PendingOff {
                at: 0,
                key: 0,
                channel: 0,
            };
            MAX_PENDING
        ]
        .into_boxed_slice()
        .try_into()
        .expect("vec length is MAX_PENDING by construction");
        Self {
            pending,
            pending_len: 0,
            playing: false,
            mode: PlayMode::Pattern,
            next_sample: 0,
            overflowed: 0,
            song_ended: false,
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

    /// Whether the last scheduled block was in song mode with the playhead at or
    /// past the end of the last clip.
    ///
    /// The engine reads this to stop the transport. The scheduler cannot do it
    /// itself: it has no clock, deliberately, and a scheduler that could stop
    /// playback would be a scheduler you could not run offline over a fixed
    /// window. FL's default and this one: the song plays to the end and stops.
    /// A playlist with no clips at all has no end, so it free-runs.
    pub const fn song_ended(&self) -> bool {
        self.song_ended
    }

    /// Emit this block's sequenced events into `out`.
    ///
    /// Returns how many events `out` had no room for -- the same currency as
    /// [`drain_notes`](mydaw_rt_core::drain_notes), so the engine can add the two
    /// and report one number.
    ///
    /// Call **after** live notes have been drained into `out`, so that live input
    /// wins the tie at a shared frame offset (see the module docs).
    ///
    /// # RT safety
    /// Allocation-free, lock-free, bounded: every loop advances by at least one
    /// pattern loop (which `is_usable` has checked is at least a sample) or one
    /// clip, and the clip list is bounded by
    /// [`MAX_CLIPS`](crate::MAX_CLIPS).
    pub fn schedule(
        &mut self,
        song: Option<&CompiledSong>,
        window: &TransportWindow,
        out: &mut NoteEventBuffer,
    ) -> usize {
        let mut dropped = 0;

        // --- transport edges, before anything is emitted ---------------------
        //
        // Stop, seek and a mode switch all mean "the notes I still owe belong to
        // a bar that is no longer playing". Release them at the top of this
        // block; anything else is a stuck voice.
        let stopped = self.playing && !window.playing;
        let seeked = window.playing && window.start_sample != self.next_sample;
        let switched = window.mode != self.mode;
        if stopped || seeked || switched {
            dropped += self.flush(out);
        }
        self.playing = window.playing;
        self.mode = window.mode;
        // While stopped the clock does not advance, so the next block starts
        // where this one did; that must not read as a seek.
        self.next_sample = if window.playing {
            window.end_sample()
        } else {
            window.start_sample
        };
        self.song_ended = false;

        if !window.playing || window.frames == 0 || !window.is_usable() {
            return dropped;
        }

        let start = window.start_sample;
        let end = window.end_sample();

        // --- note-offs already owed that come due inside this block ----------
        dropped += self.emit_due(start, end, window.frames, out);

        let Some(song) = song else {
            return dropped;
        };
        let step_len = step_len_samples(window.sample_rate, window.bpm);

        match window.mode {
            PlayMode::Pattern => {
                let Some(pattern) = song.active_pattern() else {
                    return dropped;
                };
                dropped += self.emit_run(pattern, 0, None, start, end, step_len, out);
            }
            PlayMode::Song => {
                let song_end = song.end_ticks();
                if song_end > 0 && start >= tick_boundary(song_end, step_len) {
                    // Past the last clip. Nothing left to emit, and the engine
                    // is told so it can stop the transport.
                    self.song_ended = true;
                    return dropped;
                }
                // A linear pass over the clips. They are sorted by start tick
                // but may overlap, so an earlier clip can still be sounding
                // under a later one and there is nothing to binary-search past;
                // two comparisons per clip against a list bounded at
                // `MAX_CLIPS` is a few hundred nanoseconds, and the alternative
                // (an interval tree) would be a data structure to get wrong in
                // exchange for nothing measurable.
                for clip in song.clips() {
                    let Some(pattern) = song.pattern(clip.pattern) else {
                        continue;
                    };
                    let clip_start = tick_boundary(clip.start_ticks, step_len);
                    if clip_start >= end {
                        // Sorted by start tick, so every later clip is later
                        // still.
                        break;
                    }
                    if tick_boundary(clip.end_ticks(), step_len) <= start {
                        continue;
                    }
                    dropped += self.emit_run(
                        pattern,
                        clip.start_ticks,
                        Some(clip.end_ticks()),
                        start,
                        end,
                        step_len,
                        out,
                    );
                }
            }
        }

        dropped
    }

    /// Emit one pattern, looping from `base_ticks`, into the block
    /// `[start, end)`.
    ///
    /// `limit_ticks` is one past the last tick this run may emit at -- a
    /// pattern clip's end. `None` loops forever, which is pattern mode.
    ///
    /// The shape is:
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
    #[allow(clippy::too_many_arguments)]
    fn emit_run(
        &mut self,
        pattern: &CompiledPattern,
        base_ticks: u64,
        limit_ticks: Option<u64>,
        start: u64,
        end: u64,
        step_len: f64,
        out: &mut NoteEventBuffer,
    ) -> usize {
        let notes = pattern.notes();
        if notes.is_empty() {
            return 0;
        }
        let loop_ticks = pattern.loop_ticks();
        if loop_ticks == 0 {
            return 0;
        }
        // From the *fractional* loop length, not the rounded one: dividing by a
        // rounded loop length would drift into the wrong iteration after a few
        // hundred bars, which is exactly the bug this whole module is written to
        // avoid.
        let loop_len = loop_ticks as f64 / f64::from(TICKS_PER_STEP) * step_len;
        if !loop_len.is_finite() || loop_len < 1.0 {
            return 0;
        }

        let mut dropped = 0;
        let base_sample = tick_boundary(base_ticks, step_len);
        // Jump straight to the iteration the block is in rather than counting up
        // from zero -- a four-minute clip of a one-bar pattern is a hundred
        // iterations, every block. One iteration early, because the rounding can
        // put the boundary just below `start`.
        let mut iteration = if start > base_sample {
            (((start - base_sample) as f64 / loop_len).floor() as u64).saturating_sub(1)
        } else {
            0
        };

        loop {
            let iteration_tick = base_ticks + iteration * loop_ticks;
            if limit_ticks.is_some_and(|limit| iteration_tick >= limit) {
                break;
            }
            if tick_boundary(iteration_tick, step_len) >= end {
                break;
            }

            // The notes are sorted by start tick and `tick_boundary` is
            // monotonic, so "is this note before the block?" is monotonic too --
            // which is what makes a binary search correct here.
            let first = notes.partition_point(|note| {
                tick_boundary(iteration_tick + u64::from(note.start_ticks), step_len) < start
            });
            for note in &notes[first..] {
                let note_tick = iteration_tick + u64::from(note.start_ticks);
                // Sorted by start tick, so the first note past the clip's end is
                // the last note worth looking at in this iteration.
                if limit_ticks.is_some_and(|limit| note_tick >= limit) {
                    break;
                }
                let at = tick_boundary(note_tick, step_len);
                if at >= end {
                    break;
                }
                let offset = (at - start) as u32;
                if !out.push(
                    NoteEvent::note_on(offset, note.key, note.velocity).on_channel(note.channel),
                ) {
                    dropped += 1;
                }

                // A note is cut off at the end of the clip it is in: that is
                // what a clip boundary means, and letting a tail ring past a
                // clip the user shortened would be the clip not doing anything.
                let off_tick = match limit_ticks {
                    Some(limit) => (iteration_tick + note.end_ticks()).min(limit),
                    None => iteration_tick + note.end_ticks(),
                };
                // A zero-length note would put its own note-off at its note-on;
                // `compile` clamps the length to 1 tick, but at a slow enough
                // tempo one tick still rounds to zero samples, and a note-off
                // *before* the note-on would sort ahead of it.
                let off_at = tick_boundary(off_tick, step_len).max(at + 1);
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
    /// [`schedule`](Self::schedule) call it for stop, seek and a mode switch.
    /// Returns how many events did not fit.
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
        self.mode = PlayMode::Pattern;
        self.next_sample = 0;
        self.song_ended = false;
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

impl std::fmt::Debug for Scheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scheduler")
            .field("mode", &self.mode)
            .field("playing", &self.playing)
            .field("pending", &self.pending_len)
            .field("overflowed", &self.overflowed)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    /// Regression guard: the scheduler once carried its 64 kB pending store
    /// inline, and the enclosing RtState's move into the audio callback
    /// overflowed the main thread's 1 MB stack in debug builds on boot.
    #[test]
    fn scheduler_stays_small_on_the_stack() {
        assert!(
            std::mem::size_of::<super::Scheduler>() < 256,
            "Scheduler is {} bytes; keep bulk storage boxed",
            std::mem::size_of::<super::Scheduler>()
        );
    }

    use super::*;
    use crate::compiled::CompiledPatternClip;
    use crate::pattern::{
        ChannelClip, MelodicNote, Pattern, StepRow, DEFAULT_STEPS, STEP_GATE_TICKS, TICKS_PER_BAR,
    };
    use crate::set::PatternSet;
    use mydaw_rt_core::{NoteKind, DEFAULT_BPM};

    const SR: f64 = 48_000.0;
    const BAR: u64 = TICKS_PER_BAR as u64;

    fn window(
        start: u64,
        frames: usize,
        bpm: f64,
        playing: bool,
        mode: PlayMode,
    ) -> TransportWindow {
        TransportWindow {
            start_sample: start,
            frames,
            sample_rate: SR,
            bpm,
            playing,
            mode,
        }
    }

    /// A one-channel pattern whose step row hits the given steps.
    fn stepped(id: u64, key: u8, channel: u8, steps: &[usize]) -> Pattern {
        let mut pattern = Pattern::silent(id, "P");
        let mut clip = ChannelClip::stepped(channel, key);
        clip.steps = Some(StepRow::empty(key).with_steps(steps));
        pattern.channels.push(clip);
        pattern
    }

    /// A song of one pattern, with no arrangement.
    fn pattern_song(pattern: &Pattern) -> CompiledSong {
        CompiledSong::new(vec![pattern.compile()], 0, Vec::new())
    }

    /// Run `blocks` blocks of `frames` from sample 0 and collect
    /// `(absolute sample, event)`.
    fn run(
        scheduler: &mut Scheduler,
        song: &CompiledSong,
        mode: PlayMode,
        blocks: usize,
        frames: usize,
        bpm: f64,
        sample_rate: f64,
    ) -> Vec<(u64, NoteEvent)> {
        run_from(scheduler, song, mode, 0, blocks, frames, bpm, sample_rate)
    }

    #[allow(clippy::too_many_arguments)]
    fn run_from(
        scheduler: &mut Scheduler,
        song: &CompiledSong,
        mode: PlayMode,
        from: u64,
        blocks: usize,
        frames: usize,
        bpm: f64,
        sample_rate: f64,
    ) -> Vec<(u64, NoteEvent)> {
        let mut buffer = NoteEventBuffer::new(8_192);
        let mut collected = Vec::new();
        for block in 0..blocks {
            let start = from + (block * frames) as u64;
            buffer.clear();
            let window = TransportWindow {
                start_sample: start,
                frames,
                sample_rate,
                bpm,
                playing: true,
                mode,
            };
            assert_eq!(
                scheduler.schedule(Some(song), &window, &mut buffer),
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

    /// Blocks of `frames` needed to cover `samples`, plus a couple for luck.
    ///
    /// Written out rather than guessed at: a bar at 130 BPM is 88 615 samples,
    /// which is 173 blocks of 512, and a test that ran "40 blocks" would be
    /// asserting about the first half-bar while claiming to assert about four.
    fn blocks_for(samples: u64, frames: usize) -> usize {
        (samples / frames as u64) as usize + 2
    }

    /// Samples in one bar of 4/4 at `bpm`.
    fn bar_samples(bpm: f64) -> u64 {
        (step_len_samples(SR, bpm) * DEFAULT_STEPS as f64).ceil() as u64
    }

    /// Absolute sample positions of the note-ons in `events`.
    fn onsets(events: &[(u64, NoteEvent)]) -> Vec<u64> {
        events
            .iter()
            .filter(|(_, e)| e.kind.is_note_on())
            .map(|(at, _)| *at)
            .collect()
    }

    /// Onsets on one channel.
    fn onsets_on(events: &[(u64, NoteEvent)], channel: u8) -> Vec<u64> {
        events
            .iter()
            .filter(|(_, e)| e.kind.is_note_on() && e.channel == channel)
            .map(|(at, _)| *at)
            .collect()
    }

    // ------------------------------------------------------------- the clock

    #[test]
    fn the_step_length_is_a_sixteenth_note_at_the_stated_tempo() {
        // 130 BPM: a beat is 48000*60/130 = 22153.8 samples, a step a quarter.
        let step = step_len_samples(SR, 130.0);
        assert!((step - 5538.4615).abs() < 1e-3, "{step}");
        assert!((step * 16.0 - SR * 60.0 / 130.0 * 4.0).abs() < 1e-6);
        assert_eq!(step_len_samples(48_000.0, 120.0), 6_000.0);
        assert_eq!(step_len_samples(44_100.0, 60.0), 11_025.0);
    }

    #[test]
    fn a_tick_on_a_step_line_lands_on_exactly_the_step_boundary() {
        // The identity the whole two-grids-are-one-grid claim rests on.
        for bpm in [60.0_f64, 130.0, 174.3, 999.0] {
            let step_len = step_len_samples(SR, bpm);
            for n in 0..10_000_u64 {
                assert_eq!(
                    tick_boundary(n * u64::from(TICKS_PER_STEP), step_len),
                    step_boundary(n, step_len),
                    "step {n} at {bpm} BPM"
                );
            }
        }
    }

    #[test]
    fn a_sample_position_converts_back_to_the_tick_it_is_in() {
        // Only ever used to tell a UI where the playhead is; the scheduler never
        // goes this way round. Still has to be right at the boundaries.
        let step_len = step_len_samples(SR, 130.0);
        for tick in [0_u64, 240, 480, 3_840, 15_360] {
            let sample = tick_boundary(tick, step_len);
            let back = ticks_at_sample(sample, step_len);
            assert!(
                back.abs_diff(tick) <= 1,
                "tick {tick} -> sample {sample} -> tick {back}"
            );
        }
        // A step length the engine would never produce must not panic or wrap.
        assert_eq!(ticks_at_sample(1_000, 0.0), 0);
        assert_eq!(ticks_at_sample(1_000, f64::NAN), 0);
    }

    // ----------------------------------------------------------- pattern mode

    #[test]
    fn an_empty_song_produces_no_events_at_all() {
        let mut scheduler = Scheduler::new();
        let song = CompiledSong::empty();
        assert!(run(&mut scheduler, &song, PlayMode::Pattern, 8, 512, 130.0, SR).is_empty());
        assert!(run(&mut scheduler, &song, PlayMode::Song, 8, 512, 130.0, SR).is_empty());
        assert_eq!(scheduler.pending(), 0);

        // Channels but no hits: same answer, different branch.
        let silent = pattern_song(&Pattern::with_channels(1, "P", &[0, 1, 2, 3], |_| 36));
        assert!(run(
            &mut scheduler,
            &silent,
            PlayMode::Pattern,
            8,
            512,
            130.0,
            SR
        )
        .is_empty());
    }

    #[test]
    fn no_song_installed_at_all_produces_no_events() {
        let mut scheduler = Scheduler::new();
        let mut buffer = NoteEventBuffer::new(64);
        assert_eq!(
            scheduler.schedule(
                None,
                &window(0, 512, 130.0, true, PlayMode::Pattern),
                &mut buffer
            ),
            0
        );
        assert!(buffer.is_empty());
    }

    #[test]
    fn a_stopped_transport_emits_nothing_however_full_the_pattern_is() {
        let mut scheduler = Scheduler::new();
        let song = pattern_song(&stepped(1, 36, 0, &(0..DEFAULT_STEPS).collect::<Vec<_>>()));
        let mut buffer = NoteEventBuffer::new(64);
        for block in 0..8 {
            buffer.clear();
            scheduler.schedule(
                Some(&song),
                &window(block * 512, 512, 130.0, false, PlayMode::Pattern),
                &mut buffer,
            );
            assert!(buffer.is_empty(), "block {block} emitted while stopped");
        }
        assert!(!scheduler.is_playing());
    }

    #[test]
    fn every_step_lands_exactly_on_its_boundary_at_every_tempo_rate_and_block_size() {
        // The core timing claim. A pattern that hits every step, run for one
        // bar, must produce boundaries identical to `round(n*step_len)` --
        // independent of how the blocks happen to be cut.
        for sample_rate in [44_100.0_f64, 48_000.0] {
            for bpm in [60.0_f64, 130.0, 174.3, 999.0] {
                let step_len = step_len_samples(sample_rate, bpm);
                let expected: Vec<u64> = (0..DEFAULT_STEPS as u64)
                    .map(|n| step_boundary(n, step_len))
                    .collect();
                let bar = step_len * DEFAULT_STEPS as f64;

                for frames in [32_usize, 256, 441, 4_096] {
                    let mut scheduler = Scheduler::new();
                    let song =
                        pattern_song(&stepped(1, 36, 0, &(0..DEFAULT_STEPS).collect::<Vec<_>>()));
                    let blocks = (bar / frames as f64).ceil() as usize + 1;
                    let events = run(
                        &mut scheduler,
                        &song,
                        PlayMode::Pattern,
                        blocks,
                        frames,
                        bpm,
                        sample_rate,
                    );
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
        // 174.3 BPM is deliberately not a round number: a tempo whose step
        // length is an integer would hide exactly the bug this looks for.
        let bpm = 174.3;
        let step_len = step_len_samples(SR, bpm);
        let mut scheduler = Scheduler::new();
        let song = pattern_song(&stepped(1, 36, 0, &[0]));

        let frames = 512;
        let ten_minutes = (SR * 600.0) as u64;
        let blocks = (ten_minutes / frames as u64) as usize;
        let events = run(
            &mut scheduler,
            &song,
            PlayMode::Pattern,
            blocks,
            frames,
            bpm,
            SR,
        );

        let got = onsets(&events);
        assert!(got.len() > 400, "only {} bars in ten minutes", got.len());
        for (bar_index, at) in got.iter().enumerate() {
            let expected = step_boundary(bar_index as u64 * DEFAULT_STEPS as u64, step_len);
            assert_eq!(*at, expected, "bar {bar_index} drifted");
        }
    }

    #[test]
    fn the_loop_wraps_with_no_state_and_no_gap() {
        // A hit on the last step and one on the first: the gaps across the bar
        // line must be exactly one step, and the others fifteen.
        let mut scheduler = Scheduler::new();
        let song = pattern_song(&stepped(1, 36, 0, &[0, 15]));
        let step_len = step_len_samples(SR, DEFAULT_BPM);
        let events = run(
            &mut scheduler,
            &song,
            PlayMode::Pattern,
            blocks_for(3 * bar_samples(DEFAULT_BPM), 512),
            512,
            DEFAULT_BPM,
            SR,
        );
        let got = onsets(&events);
        assert!(got.len() >= 4, "{got:?}");
        for pair in got.windows(2) {
            let steps = ((pair[1] - pair[0]) as f64 / step_len).round();
            assert!(
                (steps - 1.0).abs() < 0.001 || (steps - 15.0).abs() < 0.001,
                "a wrap must be one step, got {steps} steps"
            );
        }
    }

    #[test]
    fn a_step_compiles_to_a_note_whose_off_is_half_a_step_later() {
        // The steps-are-notes decision, measured in the output rather than in
        // the compiler: the gate is `STEP_GATE_TICKS`, on the tick grid.
        let mut scheduler = Scheduler::new();
        let song = pattern_song(&stepped(1, 36, 0, &[0]));
        let step_len = step_len_samples(SR, DEFAULT_BPM);
        let events = run(
            &mut scheduler,
            &song,
            PlayMode::Pattern,
            blocks_for(bar_samples(DEFAULT_BPM), 512),
            512,
            DEFAULT_BPM,
            SR,
        );

        let on = events.iter().find(|(_, e)| e.kind.is_note_on()).unwrap();
        let off = events
            .iter()
            .find(|(at, e)| matches!(e.kind, NoteKind::NoteOff { .. }) && *at > on.0)
            .expect("a step's note-off");
        assert_eq!(on.0, 0);
        assert_eq!(off.0, tick_boundary(u64::from(STEP_GATE_TICKS), step_len));
    }

    #[test]
    fn a_note_off_that_falls_past_the_block_is_parked_and_emitted_later() {
        let mut scheduler = Scheduler::new();
        let mut pattern = Pattern::silent(1, "P");
        let mut clip = ChannelClip::melodic(2);
        // A whole bar long, so its off is many blocks away.
        clip.notes.push(MelodicNote::new(60, 0, BAR as u32));
        pattern.channels.push(clip);
        let song = pattern_song(&pattern);

        let mut buffer = NoteEventBuffer::new(64);
        scheduler.schedule(
            Some(&song),
            &window(0, 512, DEFAULT_BPM, true, PlayMode::Pattern),
            &mut buffer,
        );
        assert_eq!(buffer.as_slice().len(), 1, "the on, and nothing else yet");
        assert_eq!(scheduler.pending(), 1, "the off is owed");

        let events = run_from(
            &mut scheduler,
            &song,
            PlayMode::Pattern,
            512,
            blocks_for(2 * bar_samples(DEFAULT_BPM), 512),
            512,
            DEFAULT_BPM,
            SR,
        );
        let offs: Vec<u64> = events
            .iter()
            .filter(|(_, e)| matches!(e.kind, NoteKind::NoteOff { .. }))
            .map(|(at, _)| *at)
            .collect();
        let step_len = step_len_samples(SR, DEFAULT_BPM);
        assert_eq!(offs[0], tick_boundary(BAR, step_len));
    }

    #[test]
    fn stopping_flushes_every_owed_note_off_rather_than_leaving_a_stuck_voice() {
        let mut scheduler = Scheduler::new();
        let mut pattern = Pattern::silent(1, "P");
        let mut clip = ChannelClip::melodic(2);
        for key in [60_u8, 64, 67] {
            clip.notes.push(MelodicNote::new(key, 0, BAR as u32));
        }
        pattern.channels.push(clip);
        let song = pattern_song(&pattern);

        let mut buffer = NoteEventBuffer::new(64);
        scheduler.schedule(
            Some(&song),
            &window(0, 512, DEFAULT_BPM, true, PlayMode::Pattern),
            &mut buffer,
        );
        assert_eq!(scheduler.pending(), 3);

        buffer.clear();
        scheduler.schedule(
            Some(&song),
            &window(512, 512, DEFAULT_BPM, false, PlayMode::Pattern),
            &mut buffer,
        );
        assert_eq!(scheduler.pending(), 0, "nothing may still be owed");
        let offs = buffer
            .as_slice()
            .iter()
            .filter(|e| matches!(e.kind, NoteKind::NoteOff { .. }))
            .count();
        assert_eq!(offs, 3);
        assert!(buffer.as_slice().iter().all(|e| e.frame_offset == 0));
    }

    #[test]
    fn a_seek_and_a_mode_switch_both_release_what_the_old_position_owed() {
        for (label, next) in [
            (
                "seek",
                window(96_000, 512, DEFAULT_BPM, true, PlayMode::Pattern),
            ),
            ("mode", window(512, 512, DEFAULT_BPM, true, PlayMode::Song)),
        ] {
            let mut scheduler = Scheduler::new();
            let mut pattern = Pattern::silent(1, "P");
            let mut clip = ChannelClip::melodic(2);
            clip.notes.push(MelodicNote::new(60, 0, BAR as u32));
            pattern.channels.push(clip);
            let song = CompiledSong::new(vec![pattern.compile()], 0, Vec::new());

            let mut buffer = NoteEventBuffer::new(64);
            scheduler.schedule(
                Some(&song),
                &window(0, 512, DEFAULT_BPM, true, PlayMode::Pattern),
                &mut buffer,
            );
            assert_eq!(scheduler.pending(), 1, "{label}");

            buffer.clear();
            scheduler.schedule(Some(&song), &next, &mut buffer);
            assert!(
                buffer
                    .as_slice()
                    .iter()
                    .any(|e| matches!(e.kind, NoteKind::NoteOff { .. }) && e.frame_offset == 0),
                "{label}: the owed note-off must be released at frame 0"
            );
        }
    }

    #[test]
    fn several_channels_in_one_pattern_are_emitted_on_their_own_channels() {
        let mut scheduler = Scheduler::new();
        let mut pattern = Pattern::silent(1, "P");
        for (channel, key, steps) in [(0_u8, 36_u8, vec![0, 4, 8, 12]), (1, 38, vec![4, 12])] {
            let mut clip = ChannelClip::stepped(channel, key);
            clip.steps = Some(StepRow::empty(key).with_steps(&steps));
            pattern.channels.push(clip);
        }
        let mut melody = ChannelClip::melodic(4);
        melody.notes.push(MelodicNote::new(72, 480, 240));
        pattern.channels.push(melody);

        let song = pattern_song(&pattern);
        let events = run(
            &mut scheduler,
            &song,
            PlayMode::Pattern,
            blocks_for(2 * bar_samples(DEFAULT_BPM), 512),
            512,
            DEFAULT_BPM,
            SR,
        );
        let step_len = step_len_samples(SR, DEFAULT_BPM);

        assert_eq!(
            onsets_on(&events, 0)[..8],
            (0..8)
                .map(|hit| step_boundary(hit / 4 * 16 + (hit % 4) * 4, step_len))
                .collect::<Vec<_>>()[..],
            "two bars of four kicks, on the grid"
        );
        assert_eq!(onsets_on(&events, 1)[0], step_boundary(4, step_len));
        assert_eq!(onsets_on(&events, 4)[0], tick_boundary(480, step_len));
        // The keys travel too.
        let melodic = events
            .iter()
            .find(|(_, e)| e.channel == 4 && e.kind.is_note_on())
            .unwrap();
        assert!(matches!(melodic.1.kind, NoteKind::NoteOn { key: 72, .. }));
    }

    #[test]
    fn pattern_mode_plays_the_active_pattern_and_only_that_one() {
        let mut set = PatternSet::single(stepped(1, 36, 0, &[0]));
        let second = set.add(None, &[], |_| 36).unwrap();
        *set.get_mut(second).unwrap() = stepped(second, 38, 1, &[8]);

        // Active is still the first.
        let song = CompiledSong::new(set.compile(), set.active_index(), Vec::new());
        let mut scheduler = Scheduler::new();
        let events = run(
            &mut scheduler,
            &song,
            PlayMode::Pattern,
            blocks_for(bar_samples(DEFAULT_BPM), 512),
            512,
            DEFAULT_BPM,
            SR,
        );
        assert!(!onsets_on(&events, 0).is_empty());
        assert!(onsets_on(&events, 1).is_empty(), "pattern 2 is not active");

        // Switch, and the other one plays.
        set.set_active(second).unwrap();
        let song = CompiledSong::new(set.compile(), set.active_index(), Vec::new());
        let mut scheduler = Scheduler::new();
        let events = run(
            &mut scheduler,
            &song,
            PlayMode::Pattern,
            blocks_for(bar_samples(DEFAULT_BPM), 512),
            512,
            DEFAULT_BPM,
            SR,
        );
        assert!(onsets_on(&events, 0).is_empty());
        assert!(!onsets_on(&events, 1).is_empty());
    }

    // -------------------------------------------------------------- song mode

    /// A song of `pattern` placed at each `(start_bar, length_bars)`.
    fn arranged(pattern: Pattern, clips: &[(u64, u64)]) -> CompiledSong {
        CompiledSong::new(
            vec![pattern.compile()],
            0,
            clips
                .iter()
                .map(|(start, length)| CompiledPatternClip {
                    pattern: 0,
                    start_ticks: start * BAR,
                    length_ticks: length * BAR,
                })
                .collect(),
        )
    }

    #[test]
    fn a_clip_sounds_at_its_own_tick_and_nowhere_else() {
        // The core song-mode claim, over a matrix of tempos and block sizes: a
        // one-bar pattern with one hit, placed at bar 4, must produce exactly
        // one onset, at bar 4's sample.
        for bpm in [60.0_f64, 130.0, 174.3] {
            for frames in [64_usize, 256, 512, 1_024] {
                let step_len = step_len_samples(SR, bpm);
                let song = arranged(stepped(1, 36, 0, &[0]), &[(4, 1)]);
                let mut scheduler = Scheduler::new();
                let blocks = (tick_boundary(6 * BAR, step_len) / frames as u64) as usize + 2;
                let events = run(
                    &mut scheduler,
                    &song,
                    PlayMode::Song,
                    blocks,
                    frames,
                    bpm,
                    SR,
                );
                assert_eq!(
                    onsets(&events),
                    vec![tick_boundary(4 * BAR, step_len)],
                    "{bpm} BPM / {frames} frames"
                );
            }
        }
    }

    #[test]
    fn nothing_sounds_before_the_first_clip_or_between_two_of_them() {
        let song = arranged(stepped(1, 36, 0, &[0]), &[(0, 1), (4, 1)]);
        let mut scheduler = Scheduler::new();
        let step_len = step_len_samples(SR, DEFAULT_BPM);
        let events = run(
            &mut scheduler,
            &song,
            PlayMode::Song,
            blocks_for(6 * bar_samples(DEFAULT_BPM), 512),
            512,
            DEFAULT_BPM,
            SR,
        );
        assert_eq!(
            onsets(&events),
            vec![0, tick_boundary(4 * BAR, step_len)],
            "bars 1, 2 and 3 are empty"
        );
    }

    #[test]
    fn a_clip_longer_than_its_pattern_loops_it() {
        // Four bars of a one-bar pattern is one clip, not four.
        let song = arranged(stepped(1, 36, 0, &[0]), &[(0, 4)]);
        let mut scheduler = Scheduler::new();
        let step_len = step_len_samples(SR, DEFAULT_BPM);
        let events = run(
            &mut scheduler,
            &song,
            PlayMode::Song,
            blocks_for(5 * bar_samples(DEFAULT_BPM), 512),
            512,
            DEFAULT_BPM,
            SR,
        );
        assert_eq!(
            onsets(&events),
            (0..4)
                .map(|bar| tick_boundary(bar * BAR, step_len))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_clip_shorter_than_its_pattern_trims_it() {
        // A four-bar pattern hitting every bar, placed as a two-bar clip: only
        // the first two hits survive.
        let mut pattern = stepped(1, 36, 0, &[0, 16, 32, 48]);
        pattern.length_steps = 64;
        let song = arranged(pattern, &[(0, 2)]);
        let mut scheduler = Scheduler::new();
        let step_len = step_len_samples(SR, DEFAULT_BPM);
        let events = run(
            &mut scheduler,
            &song,
            PlayMode::Song,
            blocks_for(3 * bar_samples(DEFAULT_BPM), 512),
            512,
            DEFAULT_BPM,
            SR,
        );
        assert_eq!(
            onsets(&events),
            vec![0, tick_boundary(BAR, step_len)],
            "the clip ends at bar 2 and so does the pattern"
        );
    }

    #[test]
    fn a_note_that_would_ring_past_its_clip_is_cut_off_at_the_clip_edge() {
        // What a clip boundary means. A whole-bar note in a clip trimmed to half
        // a bar has to stop at the trim, or shortening a clip would do nothing.
        let mut pattern = Pattern::silent(1, "P");
        let mut clip = ChannelClip::melodic(2);
        clip.notes.push(MelodicNote::new(60, 0, BAR as u32));
        pattern.channels.push(clip);

        let song = CompiledSong::new(
            vec![pattern.compile()],
            0,
            vec![CompiledPatternClip {
                pattern: 0,
                start_ticks: 0,
                length_ticks: BAR / 2,
            }],
        );
        let mut scheduler = Scheduler::new();
        let step_len = step_len_samples(SR, DEFAULT_BPM);
        let events = run(
            &mut scheduler,
            &song,
            PlayMode::Song,
            blocks_for(bar_samples(DEFAULT_BPM), 512),
            512,
            DEFAULT_BPM,
            SR,
        );
        let off = events
            .iter()
            .find(|(_, e)| matches!(e.kind, NoteKind::NoteOff { .. }))
            .expect("the note has to end");
        assert_eq!(off.0, tick_boundary(BAR / 2, step_len));
    }

    #[test]
    fn two_clips_on_the_same_bar_both_sound() {
        // Pattern A and pattern B stacked, which is what an arrangement is for.
        let a = stepped(1, 36, 0, &[0]);
        let b = stepped(2, 38, 1, &[8]);
        let song = CompiledSong::new(
            vec![a.compile(), b.compile()],
            0,
            vec![
                CompiledPatternClip {
                    pattern: 0,
                    start_ticks: 0,
                    length_ticks: BAR,
                },
                CompiledPatternClip {
                    pattern: 1,
                    start_ticks: 0,
                    length_ticks: BAR,
                },
            ],
        );
        let mut scheduler = Scheduler::new();
        let step_len = step_len_samples(SR, DEFAULT_BPM);
        let events = run(
            &mut scheduler,
            &song,
            PlayMode::Song,
            blocks_for(bar_samples(DEFAULT_BPM), 512),
            512,
            DEFAULT_BPM,
            SR,
        );
        assert_eq!(onsets_on(&events, 0), vec![0]);
        assert_eq!(onsets_on(&events, 1), vec![step_boundary(8, step_len)]);
    }

    #[test]
    fn seeking_into_the_middle_of_a_clip_resumes_from_the_right_step() {
        // A one-bar pattern hitting every fourth step, in a clip from bar 4 to
        // bar 8. Seek to just before a hit in bar 5 and the next onset must be
        // that hit -- not the clip's origin, and not a step of its own bar.
        let song = arranged(stepped(1, 36, 0, &[0, 4, 8, 12]), &[(4, 4)]);
        let step_len = step_len_samples(SR, DEFAULT_BPM);

        let target = tick_boundary(5 * BAR + 8 * u64::from(TICKS_PER_STEP), step_len);
        let from = target - 100;

        let mut scheduler = Scheduler::new();
        let events = run_from(
            &mut scheduler,
            &song,
            PlayMode::Song,
            from,
            4,
            512,
            DEFAULT_BPM,
            SR,
        );
        assert_eq!(
            onsets(&events)[0],
            target,
            "a seek must land on the grid, not on the clip's origin"
        );

        // And a run from the top produces the same absolute position, so the
        // seek did not shift the arrangement.
        let mut fresh = Scheduler::new();
        let whole = run(
            &mut fresh,
            &song,
            PlayMode::Song,
            blocks_for(9 * bar_samples(DEFAULT_BPM), 512),
            512,
            DEFAULT_BPM,
            SR,
        );
        assert!(onsets(&whole).contains(&target));
    }

    #[test]
    fn seeking_backwards_over_a_clip_boundary_still_lines_up() {
        let song = arranged(stepped(1, 36, 0, &[0]), &[(0, 1), (4, 1), (8, 1)]);
        let step_len = step_len_samples(SR, DEFAULT_BPM);
        let mut scheduler = Scheduler::new();

        // Forwards to bar 8...
        let late = run_from(
            &mut scheduler,
            &song,
            PlayMode::Song,
            tick_boundary(8 * BAR, step_len),
            4,
            512,
            DEFAULT_BPM,
            SR,
        );
        assert_eq!(onsets(&late), vec![tick_boundary(8 * BAR, step_len)]);

        // ...then backwards to bar 4, on the same scheduler.
        let early = run_from(
            &mut scheduler,
            &song,
            PlayMode::Song,
            tick_boundary(4 * BAR, step_len),
            4,
            512,
            DEFAULT_BPM,
            SR,
        );
        assert_eq!(onsets(&early), vec![tick_boundary(4 * BAR, step_len)]);
    }

    #[test]
    fn the_song_ends_after_its_last_clip_and_says_so() {
        let song = arranged(stepped(1, 36, 0, &[0]), &[(0, 2)]);
        let step_len = step_len_samples(SR, DEFAULT_BPM);
        let end = tick_boundary(2 * BAR, step_len);

        let mut scheduler = Scheduler::new();
        let mut buffer = NoteEventBuffer::new(64);
        scheduler.schedule(
            Some(&song),
            &window(end - 512, 512, DEFAULT_BPM, true, PlayMode::Song),
            &mut buffer,
        );
        assert!(!scheduler.song_ended(), "not yet");

        buffer.clear();
        scheduler.schedule(
            Some(&song),
            &window(end, 512, DEFAULT_BPM, true, PlayMode::Song),
            &mut buffer,
        );
        assert!(
            scheduler.song_ended(),
            "the last clip is behind the playhead"
        );
        assert!(buffer.is_empty());

        // Pattern mode never ends.
        buffer.clear();
        scheduler.schedule(
            Some(&song),
            &window(end, 512, DEFAULT_BPM, true, PlayMode::Pattern),
            &mut buffer,
        );
        assert!(!scheduler.song_ended());
    }

    #[test]
    fn a_playlist_with_no_clips_at_all_free_runs_rather_than_stopping_at_once() {
        // There is no end to reach, so pressing play must not immediately stop.
        let song = CompiledSong::new(vec![stepped(1, 36, 0, &[0]).compile()], 0, Vec::new());
        let mut scheduler = Scheduler::new();
        let mut buffer = NoteEventBuffer::new(64);
        scheduler.schedule(
            Some(&song),
            &window(0, 512, DEFAULT_BPM, true, PlayMode::Song),
            &mut buffer,
        );
        assert!(!scheduler.song_ended());
        assert!(
            buffer.is_empty(),
            "and nothing sounds, because nothing is placed"
        );
    }

    #[test]
    fn a_clip_far_down_the_song_costs_no_more_than_one_at_the_start() {
        // The iteration estimate rather than counting up from zero: a clip four
        // hundred bars in, of a one-bar pattern, must still produce its hits.
        let song = arranged(stepped(1, 36, 0, &[0]), &[(400, 2)]);
        let step_len = step_len_samples(SR, DEFAULT_BPM);
        let mut scheduler = Scheduler::new();
        let events = run_from(
            &mut scheduler,
            &song,
            PlayMode::Song,
            tick_boundary(400 * BAR, step_len),
            blocks_for(3 * bar_samples(DEFAULT_BPM), 512),
            512,
            DEFAULT_BPM,
            SR,
        );
        assert_eq!(
            onsets(&events),
            vec![
                tick_boundary(400 * BAR, step_len),
                tick_boundary(401 * BAR, step_len)
            ]
        );
    }

    #[test]
    fn a_full_pending_array_is_counted_rather_than_silently_dropping_note_offs() {
        // Unreachable with any pattern a person can write: it takes more
        // simultaneously-sounding long notes than MAX_PENDING. Reachable here,
        // and the point is that it is *counted*.
        let mut scheduler = Scheduler::new();
        let mut pattern = Pattern::silent(1, "P");
        for channel in 0..16_u8 {
            let mut clip = ChannelClip::melodic(channel);
            clip.notes = (0..MAX_NOTES)
                .map(|i| MelodicNote::new((i % 128) as u8, 0, BAR as u32))
                .collect();
            pattern.channels.push(clip);
        }
        let song = pattern_song(&pattern);

        let mut buffer = NoteEventBuffer::new(64 * 1_024);
        scheduler.schedule(
            Some(&song),
            &window(0, 512, DEFAULT_BPM, true, PlayMode::Pattern),
            &mut buffer,
        );
        assert_eq!(scheduler.pending(), MAX_PENDING);
        assert!(
            scheduler.overflowed() > 0,
            "the overflow has to be visible, not invisible"
        );
    }

    #[test]
    fn resetting_forgets_everything_owed_without_emitting_it() {
        let mut scheduler = Scheduler::new();
        let mut pattern = Pattern::silent(1, "P");
        let mut clip = ChannelClip::melodic(0);
        clip.notes.push(MelodicNote::new(60, 0, BAR as u32));
        pattern.channels.push(clip);
        let song = pattern_song(&pattern);

        let mut buffer = NoteEventBuffer::new(64);
        scheduler.schedule(
            Some(&song),
            &window(0, 512, DEFAULT_BPM, true, PlayMode::Pattern),
            &mut buffer,
        );
        assert_eq!(scheduler.pending(), 1);
        scheduler.reset();
        assert_eq!(scheduler.pending(), 0);
        assert!(!scheduler.is_playing());
    }

    #[test]
    fn a_tempo_the_engine_would_never_produce_emits_nothing_rather_than_hanging() {
        let mut scheduler = Scheduler::new();
        let song = pattern_song(&stepped(1, 36, 0, &[0]));
        let mut buffer = NoteEventBuffer::new(64);
        for bpm in [0.0_f64, f64::NAN, f64::INFINITY, 1e12] {
            buffer.clear();
            let window = window(0, 512, bpm, true, PlayMode::Pattern);
            assert!(!window.is_usable(), "{bpm}");
            assert_eq!(scheduler.schedule(Some(&song), &window, &mut buffer), 0);
            assert!(buffer.is_empty(), "{bpm} produced events");
        }
    }

    #[test]
    fn a_note_can_never_be_emitted_after_its_own_note_off() {
        // A one-tick note at a tempo fast enough that a tick rounds to zero
        // samples: the off must still be at least a sample later, or the buffer
        // would sort it ahead of the on.
        let mut pattern = Pattern::silent(1, "P");
        let mut clip = ChannelClip::melodic(0);
        clip.notes.push(MelodicNote::new(60, 0, 1));
        pattern.channels.push(clip);
        let song = pattern_song(&pattern);

        let mut scheduler = Scheduler::new();
        let events = run(&mut scheduler, &song, PlayMode::Pattern, 4, 512, 999.0, SR);
        let on = events
            .iter()
            .position(|(_, e)| e.kind.is_note_on())
            .unwrap();
        let off = events
            .iter()
            .position(|(_, e)| matches!(e.kind, NoteKind::NoteOff { .. }))
            .unwrap();
        assert!(off > on, "the off must come after the on");
        assert!(events[off].0 > events[on].0);
    }

    #[test]
    fn the_debug_line_says_what_the_scheduler_is_doing() {
        let scheduler = Scheduler::new();
        let text = format!("{scheduler:?}");
        assert!(text.contains("Pattern"), "{text}");
        assert!(text.contains("pending: 0"), "{text}");
        assert!(text.len() < 200);
    }
}
