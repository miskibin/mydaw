//! The pattern as the audio thread sees it, and how it gets there.
//!
//! [`CompiledPattern`] is [`Pattern`](crate::Pattern) with every `Vec` replaced
//! by a fixed array. That is the whole difference, and it is the difference that
//! matters: the audio thread may read this structure, index it and drop it, and
//! none of those touch the allocator.
//!
//! Publication is [`GraphHandle`](mydaw_rt_core::GraphHandle)'s mechanism, one
//! step down and for the same reasons (see `mydaw_rt_core::graph`): the new
//! pattern crosses the wall **by value** through an `rtrb` queue as a
//! [`basedrop::Owned`], and the superseded one is dropped on the audio thread,
//! which under `basedrop` means *queued for the collector* rather than freed.
//!
//! ```text
//!   non-RT thread                          audio thread
//!   ------------                           ------------
//!   Pattern::compile -> Owned<CompiledPattern>
//!                     -- rtrb push -->  PatternSlot::poll_swap
//!                                          |  (old pattern dropped)
//!   GcThread <-- basedrop collector queue --+
//! ```
//!
//! An `ArcSwap` would also work here -- unlike a graph, a pattern is read-only
//! on the audio thread -- but using the same mechanism as every other RT hand-off
//! in the engine is worth more than saving one queue. One publication story, one
//! set of tests, one place to be wrong.
//!
//! # Why this is `Clone` and not `Copy`
//! With a thousand notes and sixty-four steps per row it is about 21 kB. That is
//! nothing to allocate once per edit on the editing thread, and exactly the size
//! you do not want an implicit `memcpy` of appearing in an audio callback
//! because somebody wrote `*slot.current().unwrap()`. Dropping `Copy` makes
//! every such copy a visible `.clone()` that review can ask about.

use mydaw_rt_core::basedrop::{Handle, Owned};
use rtrb::{Consumer, Producer};

use crate::pattern::{DEFAULT_STEPS, MAX_NOTES, MAX_ROWS, MAX_STEPS};

/// In-flight pattern swaps.
///
/// Four is a user dragging across the grid faster than the audio thread runs a
/// block, which does not happen at 512 frames; the queue exists so that overflow
/// is a reported error rather than a possibility nobody thought about.
pub const PATTERN_QUEUE_CAPACITY: usize = 4;

/// One drum row, ready to read on the audio thread.
///
/// Two parallel arrays rather than `[Option<f32>; MAX_STEPS]`: the `on` array is
/// what the scheduler branches on, and keeping it separate means the hot loop
/// touches one byte per step instead of eight.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompiledRow {
    key: u8,
    channel: u8,
    on: [bool; MAX_STEPS],
    velocity: [f32; MAX_STEPS],
}

impl CompiledRow {
    /// A row on `key`/`channel` with no steps switched on.
    pub const fn silent(key: u8, channel: u8) -> Self {
        Self {
            key,
            channel,
            on: [false; MAX_STEPS],
            velocity: [0.0; MAX_STEPS],
        }
    }

    /// Switch step `index` on at `velocity`. Out-of-range indices are ignored.
    pub fn set(&mut self, index: usize, velocity: f32) {
        if index < MAX_STEPS {
            self.on[index] = true;
            self.velocity[index] = velocity;
        }
    }

    /// MIDI key this row triggers.
    pub const fn key(&self) -> u8 {
        self.key
    }

    /// Note channel this row speaks on.
    pub const fn channel(&self) -> u8 {
        self.channel
    }

    /// Velocity at `index`, or `None` when that step is off or out of range.
    ///
    /// The scheduler's inner loop. Returning `Option` rather than a `(bool, f32)`
    /// pair is what lets that loop be a `let ... else { continue }` with no way
    /// to read a velocity belonging to a step that is not playing.
    pub fn velocity_at(&self, index: usize) -> Option<f32> {
        if index < MAX_STEPS && self.on[index] {
            Some(self.velocity[index])
        } else {
            None
        }
    }

    /// Whether this row has no steps switched on within the first `length`.
    pub fn is_silent_within(&self, length: usize) -> bool {
        !self.on[..length.min(MAX_STEPS)].iter().any(|on| *on)
    }

    /// Whether this row has no steps switched on anywhere.
    pub fn is_silent(&self) -> bool {
        self.is_silent_within(MAX_STEPS)
    }
}

/// One melodic note, ready to read on the audio thread.
///
/// Plain public fields and no `Option` anywhere: the scheduler reads all five on
/// every note it emits, and there is no state a note can be in where one of them
/// is meaningless. Sixteen bytes, so a thousand of them is one 16 kB array the
/// callback walks a handful of entries of.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompiledNote {
    pub key: u8,
    pub channel: u8,
    /// Offset from the start of the pattern, in ticks ([`PPQ`](crate::PPQ)).
    pub start_ticks: u32,
    /// How long it sounds, in ticks. Always at least 1.
    pub length_ticks: u32,
    pub velocity: f32,
}

impl CompiledNote {
    /// The filler the fixed array is initialised with. Never played: only the
    /// first `note_count` entries are read.
    pub const SILENT: Self = Self {
        key: 0,
        channel: 0,
        start_ticks: 0,
        length_ticks: 1,
        velocity: 0.0,
    };

    /// One tick past the last one this note sounds in.
    pub const fn end_ticks(&self) -> u64 {
        self.start_ticks as u64 + self.length_ticks as u64
    }
}

/// A pattern the audio thread can run: fixed size, no allocation.
#[derive(Clone, PartialEq)]
pub struct CompiledPattern {
    rows: [CompiledRow; MAX_ROWS],
    row_count: usize,
    /// Steps in the loop. The drum grid's modulo *and*, times
    /// [`TICKS_PER_STEP`](crate::TICKS_PER_STEP), the piano roll's.
    steps: usize,
    /// Sorted by `start_ticks`, then key. See [`sort_notes`](Self::sort_notes).
    notes: [CompiledNote; MAX_NOTES],
    note_count: usize,
}

impl CompiledPattern {
    /// A pattern with no rows and no notes. Produces no events, ever.
    pub const EMPTY: Self = Self {
        rows: [CompiledRow::silent(0, 0); MAX_ROWS],
        row_count: 0,
        steps: DEFAULT_STEPS,
        notes: [CompiledNote::SILENT; MAX_NOTES],
        note_count: 0,
    };

    /// Set how many steps the loop is. Clamped to [`MAX_STEPS`].
    pub fn set_steps(&mut self, steps: usize) {
        self.steps = steps.min(MAX_STEPS);
    }

    /// Append a row, up to [`MAX_ROWS`]. Beyond that the row is dropped.
    ///
    /// Callers reach this through [`Pattern::compile`](crate::Pattern::compile),
    /// which is itself only reached after
    /// [`validate`](crate::Pattern::validate) has refused anything longer.
    pub fn push(&mut self, row: CompiledRow) {
        if self.row_count < MAX_ROWS {
            self.rows[self.row_count] = row;
            self.row_count += 1;
        }
    }

    /// Append a note, up to [`MAX_NOTES`]. Beyond that the note is dropped.
    pub fn push_note(&mut self, note: CompiledNote) {
        if self.note_count < MAX_NOTES {
            self.notes[self.note_count] = note;
            self.note_count += 1;
        }
    }

    /// Put the notes in the order the scheduler requires: by start tick, then by
    /// key.
    ///
    /// **Non-RT**, called once per compile. The scheduler relies on this
    /// absolutely -- it binary-searches for the first note due in a block, and
    /// stops at the first one that is too late. An unsorted array would not
    /// crash it; it would silently drop notes, which is worse.
    pub fn sort_notes(&mut self) {
        self.notes[..self.note_count]
            .sort_unstable_by(|a, b| a.start_ticks.cmp(&b.start_ticks).then(a.key.cmp(&b.key)));
    }

    /// Rows in this pattern.
    pub const fn row_count(&self) -> usize {
        self.row_count
    }

    /// Melodic notes in this pattern.
    pub const fn note_count(&self) -> usize {
        self.note_count
    }

    /// Steps per loop.
    pub const fn steps(&self) -> usize {
        self.steps
    }

    /// Row `index`, or `None` past the end.
    pub fn row(&self, index: usize) -> Option<&CompiledRow> {
        if index < self.row_count {
            Some(&self.rows[index])
        } else {
            None
        }
    }

    /// Note `index`, or `None` past the end.
    pub fn note(&self, index: usize) -> Option<&CompiledNote> {
        if index < self.note_count {
            Some(&self.notes[index])
        } else {
            None
        }
    }

    /// The notes, sorted by start tick. The scheduler's search space.
    pub fn notes(&self) -> &[CompiledNote] {
        &self.notes[..self.note_count]
    }

    /// Whether nothing inside the loop would produce an event.
    pub fn is_silent(&self) -> bool {
        self.note_count == 0
            && self.rows[..self.row_count]
                .iter()
                .all(|row| row.is_silent_within(self.steps))
    }
}

impl Default for CompiledPattern {
    fn default() -> Self {
        Self::EMPTY
    }
}

/// Counts rather than contents.
///
/// The derived version would print sixteen 64-element arrays and a thousand
/// notes, which is not a debug line, it is a denial of service on the reader.
impl std::fmt::Debug for CompiledPattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledPattern")
            .field("steps", &self.steps)
            .field("rows", &self.row_count)
            .field("notes", &self.note_count)
            .field("silent", &self.is_silent())
            .finish()
    }
}

/// Why a pattern could not be handed to the audio thread.
#[derive(Debug, thiserror::Error)]
pub enum PatternInstallError {
    /// The audio thread has not consumed previous patterns yet. Retryable; the
    /// rejected pattern is dropped here, on the non-RT thread.
    #[error("the pattern queue is full; the audio thread has not picked up previous edits")]
    QueueFull,
    /// The audio thread went away -- the stream stopped since this handle was
    /// created.
    #[error("the audio thread is gone; cannot install the pattern")]
    Disconnected,
}

/// Non-RT end of the pattern channel, for one open stream.
pub struct PatternHandle {
    tx: Producer<Owned<CompiledPattern>>,
    gc: Handle,
}

impl PatternHandle {
    /// Publish `pattern` to the audio thread. Takes effect on the next block.
    ///
    /// All allocation (the `Owned` box) happens here, on the calling non-RT
    /// thread.
    ///
    /// # Errors
    /// [`PatternInstallError::QueueFull`] or
    /// [`PatternInstallError::Disconnected`].
    pub fn install(&mut self, pattern: CompiledPattern) -> Result<(), PatternInstallError> {
        let owned = Owned::new(&self.gc, pattern);
        // On failure the rejected pattern comes back inside the error and is
        // dropped right here -- deliberately on this thread, never on the audio
        // thread.
        self.tx.push(owned).map_err(|_| {
            if self.tx.is_abandoned() {
                PatternInstallError::Disconnected
            } else {
                PatternInstallError::QueueFull
            }
        })
    }
}

/// Audio-thread end: owns the pattern currently playing.
pub struct PatternSlot {
    rx: Consumer<Owned<CompiledPattern>>,
    current: Option<Owned<CompiledPattern>>,
}

impl PatternSlot {
    /// Pick up the newest pending pattern, keeping only the last of a burst.
    ///
    /// Wait-free; call once per block *before* scheduling. Superseded patterns
    /// are dropped here, which under `basedrop` enqueues them on the collector
    /// rather than freeing them.
    ///
    /// Returns `true` if the current pattern was replaced.
    pub fn poll_swap(&mut self) -> bool {
        let mut swapped = false;
        while let Ok(next) = self.rx.pop() {
            self.current = Some(next);
            swapped = true;
        }
        swapped
    }

    /// The pattern currently playing, or `None` before the first install.
    pub fn current(&self) -> Option<&CompiledPattern> {
        self.current.as_deref()
    }

    /// Whether a pattern has been installed.
    pub fn has_pattern(&self) -> bool {
        self.current.is_some()
    }
}

/// Create the pattern channel for one stream.
///
/// `gc` comes from a [`basedrop::Collector`] -- in the engine, from the audio
/// service's [`GcThread`](mydaw_rt_core::GcThread).
pub fn pattern_channel(capacity: usize, gc: Handle) -> (PatternHandle, PatternSlot) {
    let (tx, rx) = mydaw_rt_core::rt_channel::<Owned<CompiledPattern>>(capacity.max(1));
    (PatternHandle { tx, gc }, PatternSlot { rx, current: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern::{MelodicNote, Pattern, PatternRow, Step};
    use mydaw_rt_core::basedrop::Collector;

    #[test]
    fn an_empty_compiled_pattern_has_no_rows_no_notes_and_no_hits() {
        let empty = CompiledPattern::EMPTY;
        assert_eq!(empty.row_count(), 0);
        assert_eq!(empty.note_count(), 0);
        assert_eq!(empty.steps(), DEFAULT_STEPS);
        assert!(empty.is_silent());
        assert!(empty.row(0).is_none());
        assert!(empty.note(0).is_none());
        assert!(empty.notes().is_empty());
        assert_eq!(CompiledPattern::default(), empty);
    }

    #[test]
    fn a_compiled_row_only_yields_velocities_for_steps_that_are_on() {
        let mut row = CompiledRow::silent(36, 2);
        assert!(row.is_silent());
        row.set(0, 0.9);
        row.set(MAX_STEPS, 1.0); // out of range: ignored, not a panic
        assert!(!row.is_silent());
        assert_eq!(row.velocity_at(0), Some(0.9));
        assert_eq!(row.velocity_at(1), None);
        assert_eq!(row.velocity_at(MAX_STEPS), None);
        assert_eq!((row.key(), row.channel()), (36, 2));
    }

    #[test]
    fn a_hit_past_the_loop_point_does_not_make_a_pattern_sound() {
        // The compiled counterpart of `Pattern::hit_count` being length-aware:
        // a one-bar pattern with a hit on step 48 is silent, and the engine has
        // to agree with the panel about that.
        let mut pattern = Pattern::default();
        pattern.rows[0] = PatternRow::empty(36).with_steps(&[48]);
        assert!(pattern.compile().is_silent(), "step 48 is outside one bar");

        pattern.length_steps = 64;
        assert!(!pattern.compile().is_silent());
    }

    #[test]
    fn pushing_past_the_row_and_note_limits_drops_rather_than_growing() {
        let mut pattern = CompiledPattern::EMPTY;
        for key in 0..(MAX_ROWS + 4) as u8 {
            pattern.push(CompiledRow::silent(key, 0));
        }
        assert_eq!(pattern.row_count(), MAX_ROWS);
        assert_eq!(
            pattern.row(MAX_ROWS - 1).unwrap().key(),
            (MAX_ROWS - 1) as u8
        );

        for index in 0..MAX_NOTES + 4 {
            pattern.push_note(CompiledNote {
                start_ticks: index as u32,
                ..CompiledNote::SILENT
            });
        }
        assert_eq!(pattern.note_count(), MAX_NOTES);
        assert_eq!(pattern.notes().len(), MAX_NOTES);
        assert_eq!(
            pattern.note(MAX_NOTES - 1).unwrap().start_ticks,
            (MAX_NOTES - 1) as u32
        );
    }

    #[test]
    fn the_step_count_is_clamped_to_what_the_row_arrays_can_hold() {
        let mut pattern = CompiledPattern::EMPTY;
        pattern.set_steps(999);
        assert_eq!(pattern.steps(), MAX_STEPS);
        pattern.set_steps(32);
        assert_eq!(pattern.steps(), 32);
    }

    #[test]
    fn the_debug_line_is_counts_rather_than_twenty_one_kilobytes_of_arrays() {
        let compiled = Pattern::default().compile();
        let text = format!("{compiled:?}");
        assert!(text.contains("rows: 4"), "{text}");
        assert!(text.contains("notes: 0"), "{text}");
        assert!(
            text.len() < 200,
            "{} characters is a wall, not a line",
            text.len()
        );
    }

    #[test]
    fn a_slot_starts_empty_and_picks_up_the_installed_pattern() {
        let collector = Collector::new();
        let (mut handle, mut slot) = pattern_channel(PATTERN_QUEUE_CAPACITY, collector.handle());
        assert!(!slot.has_pattern());
        assert!(slot.current().is_none());
        assert!(!slot.poll_swap(), "nothing pending");

        let mut pattern = Pattern::silent();
        pattern.rows.push(PatternRow::empty(36).with_steps(&[0, 8]));
        pattern.notes.push(MelodicNote::new(60, 1, 480, 240));
        handle.install(pattern.compile()).unwrap();

        assert!(slot.poll_swap());
        let current = slot.current().expect("installed");
        assert_eq!(current.row_count(), 1);
        assert!(current.row(0).unwrap().velocity_at(8).is_some());
        assert_eq!(current.note(0).unwrap().key, 60);
    }

    #[test]
    fn a_burst_of_edits_collapses_to_the_newest_pattern() {
        let collector = Collector::new();
        let (mut handle, mut slot) = pattern_channel(8, collector.handle());
        for key in [36_u8, 38, 42] {
            let mut pattern = Pattern::silent();
            pattern.rows.push(PatternRow::empty(key).with_steps(&[0]));
            handle.install(pattern.compile()).unwrap();
        }
        assert!(slot.poll_swap());
        assert_eq!(slot.current().unwrap().row(0).unwrap().key(), 42);
    }

    #[test]
    fn a_full_queue_is_reported_rather_than_dropped_silently() {
        let collector = Collector::new();
        // Never polled, so the queue fills and stays full.
        let (mut handle, _slot) = pattern_channel(2, collector.handle());
        handle.install(CompiledPattern::EMPTY).unwrap();
        handle.install(CompiledPattern::EMPTY).unwrap();
        assert!(matches!(
            handle.install(CompiledPattern::EMPTY),
            Err(PatternInstallError::QueueFull)
        ));
    }

    #[test]
    fn an_abandoned_queue_reports_a_dead_audio_thread_not_a_full_one() {
        let collector = Collector::new();
        let (mut handle, slot) = pattern_channel(1, collector.handle());
        drop(slot);
        // `rtrb` lets a push into an abandoned queue succeed while there is
        // room -- the pattern simply has nowhere to go. It is the push that
        // *fails* which has to say which of the two things went wrong, because
        // "retry in a moment" and "the stream stopped" need different answers
        // from the caller.
        handle.install(CompiledPattern::EMPTY).unwrap();
        assert!(matches!(
            handle.install(CompiledPattern::EMPTY),
            Err(PatternInstallError::Disconnected)
        ));
    }

    /// The point of `basedrop`: a superseded pattern is dropped on the audio
    /// thread, and dropping it there must not free it.
    #[test]
    fn a_superseded_pattern_is_freed_by_the_collector_not_the_audio_thread() {
        let mut collector = Collector::new();
        let (mut handle, mut slot) = pattern_channel(4, collector.handle());

        handle.install(CompiledPattern::EMPTY).unwrap();
        let mut second = Pattern::silent();
        second.rows.push(PatternRow::empty(36).with_steps(&[4]));
        handle.install(second.compile()).unwrap();
        assert_eq!(collector.alloc_count(), 2);

        slot.poll_swap();
        assert_eq!(
            collector.alloc_count(),
            2,
            "dropping on the audio thread must not deallocate"
        );
        collector.collect();
        assert_eq!(
            collector.alloc_count(),
            1,
            "the collector freed the old one"
        );

        drop(slot);
        collector.collect();
        assert_eq!(collector.alloc_count(), 0);
    }

    #[test]
    fn velocities_survive_the_compile_unchanged() {
        let mut pattern = Pattern::silent();
        let mut row = PatternRow::empty(38);
        row.steps[1] = Step::hit(0.125);
        pattern.rows.push(row);
        pattern
            .notes
            .push(MelodicNote::new(60, 0, 0, 240).at_velocity(0.375));
        let compiled = pattern.compile();
        assert_eq!(compiled.row(0).unwrap().velocity_at(1), Some(0.125));
        assert_eq!(compiled.note(0).unwrap().velocity, 0.375);
    }

    #[test]
    fn a_compiled_note_knows_where_it_ends() {
        let note = CompiledNote {
            start_ticks: 3_600,
            length_ticks: 240,
            ..CompiledNote::SILENT
        };
        assert_eq!(note.end_ticks(), 3_840);
        assert_eq!(CompiledNote::SILENT.length_ticks, 1, "never zero");
    }
}
