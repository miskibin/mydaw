//! The pattern: what the user clicks, and what it is allowed to be.
//!
//! This is the **editable** shape -- a `Vec` of drum rows, a `Vec` of melodic
//! notes, and a length -- and it never crosses the real-time wall. What the
//! audio thread sees is [`CompiledPattern`](crate::CompiledPattern), which this
//! module's [`Pattern::compile`] produces on a non-RT thread.
//!
//! ```text
//!   webview            app (non-RT)                     audio thread
//!   -------            ------------                     ------------
//!   PatternDto --> Pattern::validate --> compile --> Owned<CompiledPattern>
//!                  (typed errors)     (fixed arrays)   -- rtrb --> PatternSlot
//! ```
//!
//! # Two ways to say "play a note"
//! A pattern carries both halves of what MYDAW can program:
//!
//! | | [`PatternRow`] (the step grid) | [`MelodicNote`] (the piano roll) |
//! | --- | --- | --- |
//! | Position | a step index, on the sixteenth grid | a tick, [`PPQ`] per quarter |
//! | Length | the scheduler's fixed gate | its own, in ticks |
//! | Pitch | one key per row | one key per note |
//! | Polyphony | one hit per row per step | unlimited, up to [`MAX_NOTES`] |
//!
//! They are not two representations of the same thing and neither is a special
//! case of the other. A drum grid wants "row 0, step 4, is it on?" -- a question
//! with a fixed-size answer that a checkbox can toggle. A melody wants "C4 from
//! here to there" -- a length the grid has no way to express. Step 9 added the
//! second without disturbing the first, and the scheduler merges them.
//!
//! # Why the limits are constants and not opinions
//! Every one of them is the size of an array on the audio thread's side, checked
//! at the IPC boundary so that a webview bug is a status-bar sentence rather
//! than a truncated pattern or a panicking `RtNode`:
//!
//! - [`MAX_STEPS`] is 64 -- four bars of sixteenths, the longest
//!   [`PATTERN_LENGTHS`] offers.
//! - [`MAX_ROWS`] is 16, one drum row per possible track.
//! - [`MAX_NOTES`] is 1024, which at four bars is a note every 3.75 ticks.

use mydaw_rt_core::MAX_KEY;

use crate::compiled::{CompiledNote, CompiledPattern, CompiledRow};

/// Steps a pattern has unless something says otherwise. One bar of 4/4 in
/// sixteenth notes.
pub const DEFAULT_STEPS: usize = 16;

/// Longest pattern, in steps. Four bars of 4/4 in sixteenths.
///
/// Every row carries this many cells whatever the pattern's current length, so
/// shortening a pattern and lengthening it again gives the tail back rather than
/// having thrown it away. Only the first `length_steps` of them are ever played.
pub const MAX_STEPS: usize = 64;

/// The lengths a pattern may be: one, two or four bars.
///
/// Not an arbitrary range. A pattern is a loop, and a loop whose length is not a
/// whole number of bars puts the downbeat somewhere different every time round
/// -- which is a musical effect, not a default, and there is no UI for choosing
/// it deliberately yet.
pub const PATTERN_LENGTHS: [u8; 3] = [16, 32, 64];

/// Steps per beat. 16 steps = 4 beats = one bar.
pub const STEPS_PER_BEAT: usize = 4;

/// Ticks per quarter note -- the resolution a melodic note's position and length
/// are expressed in.
///
/// 960 is the value most sequencers and MIDI files settled on, and it is chosen
/// for its factors rather than its size: it divides by 2, 3, 4, 5, 6, 8, 10, 12,
/// 16, 20, 24, 32, 48, 60, 64, 96, 120, 192, 240, 320 and 480. So a sixteenth
/// (240), a triplet eighth (320), a quintuplet sixteenth (192) and a 64th (60)
/// are all *exact* integers, and quantising to any of them is arithmetic rather
/// than approximation. A power of two would be smaller and could not express a
/// triplet at all.
pub const PPQ: u32 = 960;

/// Ticks in one step of the grid, i.e. one sixteenth note.
///
/// The bridge between the two halves of a pattern: step `n` of the drum grid and
/// tick `n * TICKS_PER_STEP` of the piano roll are the *same instant*, exactly,
/// and `schedule`'s tick arithmetic is written so that they round to the same
/// sample. See [`tick_boundary`](crate::tick_boundary).
pub const TICKS_PER_STEP: u32 = PPQ / STEPS_PER_BEAT as u32;

/// Most drum rows a pattern may have.
///
/// The compiled form is `[CompiledRow; MAX_ROWS]`, so this is a hard bound on
/// the audio thread's side rather than a policy: raising it is a recompile, and
/// exceeding it is a rejected IPC call.
pub const MAX_ROWS: usize = 16;

/// Most melodic notes a pattern may hold.
///
/// The compiled form is `[CompiledNote; MAX_NOTES]`, for the same reason
/// [`MAX_ROWS`] is what it is. 1024 notes across four bars is a note every 3.75
/// ticks: far past anything a person will draw, and small enough that the
/// compiled pattern stays a ~21 kB heap allocation made on the *editing* thread.
pub const MAX_NOTES: usize = 1024;

/// Highest note channel, mirroring MIDI 1.0.
pub const MAX_CHANNEL: u8 = 15;

/// Velocity a step gets from an ordinary click.
///
/// Not 1.0: a grid where every hit is full scale has no accent to give, and a
/// drum machine without accents sounds like a metronome. 0.78 leaves obvious
/// headroom above it while still being a confident hit.
pub const DEFAULT_STEP_VELOCITY: f32 = 0.78;

/// Velocity a step gets from a shift-click.
pub const ACCENT_VELOCITY: f32 = 1.0;

/// Velocity at or above which a step counts as accented, for display.
pub const ACCENT_THRESHOLD: f32 = 0.95;

/// General MIDI keys the default pattern is built from, in row order: kick,
/// snare, closed hat, open hat.
///
/// Written out rather than imported from `mydaw-plugin-instruments`: this crate
/// deliberately does not depend on it. A pattern row addresses a *key*, and
/// whether anything is listening on that key is the instrument's business --
/// routing a row to a chosen instrument is a later step. These numbers are
/// General MIDI's, not MYDAW's, so the two cannot drift apart.
pub const DEFAULT_ROW_KEYS: [u8; 4] = [36, 38, 42, 46];

/// Short display name for a row addressing `key`.
///
/// Lives here rather than in the UI so the four drum rows are named once, in the
/// language that has the tests. Anything outside the General MIDI drum keys the
/// default pattern uses is named by its number, which is the honest answer.
pub fn row_label(key: u8) -> String {
    match key {
        36 => "Kick".to_owned(),
        38 => "Snare".to_owned(),
        42 => "CH".to_owned(),
        46 => "OH".to_owned(),
        other => format!("Key {other}"),
    }
}

/// One cell of the grid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Step {
    pub on: bool,
    /// Normalised velocity, `0.0..=1.0`. Meaningful only when `on`.
    pub velocity: f32,
}

impl Step {
    /// An empty cell.
    pub const OFF: Self = Self {
        on: false,
        velocity: DEFAULT_STEP_VELOCITY,
    };

    /// A hit at `velocity`.
    pub const fn hit(velocity: f32) -> Self {
        Self { on: true, velocity }
    }

    /// A hit at [`DEFAULT_STEP_VELOCITY`].
    pub const fn on() -> Self {
        Self::hit(DEFAULT_STEP_VELOCITY)
    }

    /// A hit at [`ACCENT_VELOCITY`].
    pub const fn accent() -> Self {
        Self::hit(ACCENT_VELOCITY)
    }
}

impl Default for Step {
    fn default() -> Self {
        Self::OFF
    }
}

/// One row of the grid: a key, the channel it speaks on, and its cells.
///
/// The array is always [`MAX_STEPS`] long; how many of those cells are *played*
/// is [`Pattern::length_steps`]. See [`MAX_STEPS`] for why the tail is kept.
#[derive(Debug, Clone, PartialEq)]
pub struct PatternRow {
    /// MIDI key number this row triggers, e.g. 36 for a General MIDI kick.
    pub key: u8,
    /// Note channel, `0..=15`.
    pub channel: u8,
    pub steps: [Step; MAX_STEPS],
}

impl PatternRow {
    /// An empty row on `key`, channel 0.
    pub fn empty(key: u8) -> Self {
        Self {
            key,
            channel: 0,
            steps: [Step::OFF; MAX_STEPS],
        }
    }

    /// An empty row with the steps in `indices` switched on.
    ///
    /// Test and demo affordance -- "kick on every fourth step" is one call
    /// rather than four assignments. Indices past [`MAX_STEPS`] are ignored.
    #[must_use]
    pub fn with_steps(mut self, indices: &[usize]) -> Self {
        for index in indices {
            if let Some(step) = self.steps.get_mut(*index) {
                *step = Step::on();
            }
        }
        self
    }

    /// Whether any cell in this row is switched on, anywhere in the array.
    ///
    /// Ignores the pattern's length on purpose: this is a question about the
    /// row, and "is anything audible?" is [`Pattern::is_empty`]'s, which knows
    /// how long the pattern is.
    pub fn is_empty(&self) -> bool {
        !self.steps.iter().any(|step| step.on)
    }

    /// Cells switched on within the first `length` steps.
    pub fn hits_within(&self, length: usize) -> usize {
        self.steps[..length.min(MAX_STEPS)]
            .iter()
            .filter(|step| step.on)
            .count()
    }
}

/// One note of the piano roll.
///
/// Position and length are in ticks ([`PPQ`] per quarter note) rather than in
/// steps, because a note's whole point is that it is *not* on the grid: an
/// eighth-note triplet, a note nudged two ticks late, and a note held for three
/// and a half beats all have to be expressible, and a step index cannot say any
/// of them.
///
/// `channel` addresses a **track** -- a track's position in the mixer is its
/// note channel -- so a pattern can drive several instruments at once.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MelodicNote {
    /// MIDI key, `0..=127`. 60 is middle C.
    pub key: u8,
    /// Note channel, `0..=15`; the index of the track this note plays.
    pub channel: u8,
    /// Offset from the start of the pattern, in ticks.
    pub start_ticks: u32,
    /// How long the note sounds, in ticks. At least 1.
    pub length_ticks: u32,
    /// Normalised velocity, `0.0..=1.0`.
    pub velocity: f32,
}

impl MelodicNote {
    /// A note on `key`/`channel` at [`DEFAULT_STEP_VELOCITY`].
    pub const fn new(key: u8, channel: u8, start_ticks: u32, length_ticks: u32) -> Self {
        Self {
            key,
            channel,
            start_ticks,
            length_ticks,
            velocity: DEFAULT_STEP_VELOCITY,
        }
    }

    /// The same note at `velocity`.
    #[must_use]
    pub const fn at_velocity(mut self, velocity: f32) -> Self {
        self.velocity = velocity;
        self
    }

    /// One tick past the last one this note sounds in.
    ///
    /// `u64` because `start + length` is two `u32`s and a pattern that would
    /// overflow has to be *rejected*, not wrapped into something plausible.
    pub const fn end_ticks(&self) -> u64 {
        self.start_ticks as u64 + self.length_ticks as u64
    }
}

/// The whole pattern: the drum grid, the melodic notes, and how long it loops.
#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    pub rows: Vec<PatternRow>,
    /// Steps in the loop: one of [`PATTERN_LENGTHS`].
    pub length_steps: u8,
    /// The piano roll's notes, in no particular order -- [`compile`](Self::compile)
    /// sorts them. Kept unsorted here because the editor addresses a note by its
    /// index, and re-sorting under an edit would renumber the note being dragged.
    pub notes: Vec<MelodicNote>,
}

impl Default for Pattern {
    /// Four empty drum rows -- kick, snare, closed hat, open hat -- one bar
    /// long, and no melodic notes.
    ///
    /// Empty rather than pre-filled with a beat. A DAW that greets you with
    /// somebody else's rhythm is showing off; clicking in a four-on-the-floor is
    /// ten clicks, and those ten clicks are the feature.
    fn default() -> Self {
        Self {
            rows: DEFAULT_ROW_KEYS
                .iter()
                .map(|k| PatternRow::empty(*k))
                .collect(),
            length_steps: DEFAULT_STEPS as u8,
            notes: Vec::new(),
        }
    }
}

/// Why a pattern was refused.
///
/// Every variant is reachable from the webview, which can send any JSON it
/// likes. A pattern that would not fit the compiled form has to fail at the
/// boundary with a sentence, not be silently truncated on the audio thread.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum PatternError {
    #[error("a pattern may have at most {MAX_ROWS} rows, got {0}")]
    TooManyRows(usize),
    #[error("a pattern must be 16, 32 or 64 steps long, got {0}")]
    WrongStepCount(usize),
    #[error("row {row} declares {got} steps; every row must have exactly {expected}")]
    WrongRowLength {
        row: usize,
        got: usize,
        expected: usize,
    },
    #[error("row {row} addresses key {key}; MIDI keys are 0..={MAX_KEY}")]
    KeyOutOfRange { row: usize, key: u16 },
    #[error("row {row} is on channel {channel}; note channels are 0..={MAX_CHANNEL}")]
    ChannelOutOfRange { row: usize, channel: u16 },
    #[error(
        "row {row} step {step} has velocity {velocity}, which is not a number between 0.0 and 1.0"
    )]
    InvalidVelocity {
        row: usize,
        step: usize,
        velocity: f32,
    },
    #[error("a pattern may have at most {MAX_NOTES} notes, got {0}")]
    TooManyNotes(usize),
    #[error("note {note} addresses key {key}; MIDI keys are 0..={MAX_KEY}")]
    NoteKeyOutOfRange { note: usize, key: u16 },
    #[error("note {note} is on channel {channel}; note channels are 0..={MAX_CHANNEL}")]
    NoteChannelOutOfRange { note: usize, channel: u16 },
    #[error("note {note} has zero length; a note has to sound for at least one tick")]
    NoteZeroLength { note: usize },
    #[error("note {note} runs to tick {end} but the pattern is only {pattern_ticks} ticks long")]
    NoteOutOfPattern {
        note: usize,
        end: u64,
        pattern_ticks: u32,
    },
    #[error("note {note} has velocity {velocity}, which is not a number between 0.0 and 1.0")]
    NoteInvalidVelocity { note: usize, velocity: f32 },
}

impl Pattern {
    /// An empty pattern with no rows and no notes. Produces no events.
    pub fn silent() -> Self {
        Self {
            rows: Vec::new(),
            length_steps: DEFAULT_STEPS as u8,
            notes: Vec::new(),
        }
    }

    /// Ticks in one time round the loop.
    ///
    /// The bar line, in the piano roll's own units. `u32` is generous: four bars
    /// is 15 360.
    pub const fn ticks(&self) -> u32 {
        self.length_steps as u32 * TICKS_PER_STEP
    }

    /// Whether `length` is one of the lengths a pattern may be.
    pub fn is_legal_length(length: usize) -> bool {
        length <= u8::MAX as usize && PATTERN_LENGTHS.contains(&(length as u8))
    }

    /// Check every bound the compiled form depends on.
    ///
    /// # Errors
    /// See [`PatternError`]. The first problem found is reported; a UI that has
    /// sent one bad value has usually sent one bad value.
    pub fn validate(&self) -> Result<(), PatternError> {
        if !Self::is_legal_length(self.length_steps as usize) {
            return Err(PatternError::WrongStepCount(self.length_steps as usize));
        }
        if self.rows.len() > MAX_ROWS {
            return Err(PatternError::TooManyRows(self.rows.len()));
        }
        for (index, row) in self.rows.iter().enumerate() {
            if row.key > MAX_KEY {
                return Err(PatternError::KeyOutOfRange {
                    row: index,
                    key: u16::from(row.key),
                });
            }
            if row.channel > MAX_CHANNEL {
                return Err(PatternError::ChannelOutOfRange {
                    row: index,
                    channel: u16::from(row.channel),
                });
            }
            for (step_index, step) in row.steps.iter().enumerate() {
                if !step.velocity.is_finite() || !(0.0..=1.0).contains(&step.velocity) {
                    return Err(PatternError::InvalidVelocity {
                        row: index,
                        step: step_index,
                        velocity: step.velocity,
                    });
                }
            }
        }

        if self.notes.len() > MAX_NOTES {
            return Err(PatternError::TooManyNotes(self.notes.len()));
        }
        let pattern_ticks = self.ticks();
        for (index, note) in self.notes.iter().enumerate() {
            if note.key > MAX_KEY {
                return Err(PatternError::NoteKeyOutOfRange {
                    note: index,
                    key: u16::from(note.key),
                });
            }
            if note.channel > MAX_CHANNEL {
                return Err(PatternError::NoteChannelOutOfRange {
                    note: index,
                    channel: u16::from(note.channel),
                });
            }
            if note.length_ticks == 0 {
                return Err(PatternError::NoteZeroLength { note: index });
            }
            // Notes may *touch* the bar line -- a note filling the last step
            // ends exactly at `pattern_ticks`, and its note-off is the first
            // sample of the next time round. They may not cross it: a wrapped
            // note would sound before its own note-on on the following loop.
            if note.end_ticks() > u64::from(pattern_ticks) {
                return Err(PatternError::NoteOutOfPattern {
                    note: index,
                    end: note.end_ticks(),
                    pattern_ticks,
                });
            }
            if !note.velocity.is_finite() || !(0.0..=1.0).contains(&note.velocity) {
                return Err(PatternError::NoteInvalidVelocity {
                    note: index,
                    velocity: note.velocity,
                });
            }
        }
        Ok(())
    }

    /// Freeze this pattern into the fixed-size form the audio thread runs.
    ///
    /// **Non-RT.** Rows past [`MAX_ROWS`] and notes past [`MAX_NOTES`] are
    /// dropped, which [`validate`](Self::validate) has already refused --
    /// compiling is infallible on purpose so the publication path has one
    /// failure point (validation) rather than two.
    ///
    /// Notes come out **sorted by start tick**, which is what lets the scheduler
    /// binary-search into them instead of scanning a thousand entries per block.
    /// Ties are broken by key so the order is total and a compile is
    /// deterministic: the same pattern always produces the same bytes.
    pub fn compile(&self) -> CompiledPattern {
        let mut compiled = CompiledPattern::EMPTY;
        compiled.set_steps(self.length_steps as usize);
        for row in self.rows.iter().take(MAX_ROWS) {
            let mut compiled_row = CompiledRow::silent(row.key, row.channel);
            for (index, step) in row.steps.iter().enumerate() {
                if step.on {
                    compiled_row.set(index, step.velocity.clamp(0.0, 1.0));
                }
            }
            compiled.push(compiled_row);
        }
        for note in self.notes.iter().take(MAX_NOTES) {
            compiled.push_note(CompiledNote {
                key: note.key,
                channel: note.channel,
                start_ticks: note.start_ticks,
                length_ticks: note.length_ticks.max(1),
                velocity: note.velocity.clamp(0.0, 1.0),
            });
        }
        compiled.sort_notes();
        compiled
    }

    /// Whether this pattern would produce no events at all.
    ///
    /// Counts only what is inside the pattern's length: a cell past the loop
    /// point is remembered but never played, and calling a pattern non-empty for
    /// something nobody can hear would be a lie the UI would then have to draw.
    pub fn is_empty(&self) -> bool {
        self.hit_count() == 0 && self.notes.is_empty()
    }

    /// Steps switched on inside the loop, across every row. Diagnostics and
    /// tests; melodic notes are counted by `notes.len()`.
    pub fn hit_count(&self) -> usize {
        let length = self.length_steps as usize;
        self.rows.iter().map(|row| row.hits_within(length)).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_pattern_is_four_empty_general_midi_drum_rows_one_bar_long() {
        let pattern = Pattern::default();
        assert_eq!(pattern.length_steps as usize, DEFAULT_STEPS);
        assert_eq!(
            pattern.rows.iter().map(|r| r.key).collect::<Vec<_>>(),
            vec![36, 38, 42, 46],
            "kick, snare, closed hat, open hat -- the drum kit's own keys"
        );
        assert!(pattern.rows.iter().all(|r| r.channel == 0));
        assert!(pattern.notes.is_empty(), "and no melody");
        assert!(
            pattern.is_empty(),
            "a fresh session greets you with silence"
        );
        assert_eq!(pattern.hit_count(), 0);
        assert_eq!(pattern.ticks(), 16 * 240);
        pattern.validate().expect("the default must be valid");
    }

    #[test]
    fn the_rows_are_labelled_for_the_grid_and_unknown_keys_name_themselves() {
        assert_eq!(row_label(36), "Kick");
        assert_eq!(row_label(38), "Snare");
        assert_eq!(row_label(42), "CH");
        assert_eq!(row_label(46), "OH");
        assert_eq!(row_label(60), "Key 60");
    }

    #[test]
    fn a_click_is_a_hit_and_a_shift_click_is_an_accent() {
        const { assert!(!Step::OFF.on) };
        assert_eq!(Step::on().velocity, DEFAULT_STEP_VELOCITY);
        assert_eq!(Step::accent().velocity, ACCENT_VELOCITY);
        // The threshold has to sit between the two, or the UI would draw every
        // ordinary hit as an accent (or none of them).
        const { assert!(DEFAULT_STEP_VELOCITY < ACCENT_THRESHOLD) };
        const { assert!(ACCENT_VELOCITY >= ACCENT_THRESHOLD) };
        assert_eq!(Step::default(), Step::OFF);
    }

    #[test]
    fn four_on_the_floor_is_the_ten_clicks_it_claims_to_be() {
        let mut pattern = Pattern::default();
        pattern.rows[0] = PatternRow::empty(36).with_steps(&[0, 4, 8, 12]);
        pattern.rows[1] = PatternRow::empty(38).with_steps(&[4, 12]);
        pattern.rows[2] = PatternRow::empty(42).with_steps(&[2, 6, 10, 14]);
        assert_eq!(pattern.hit_count(), 10);
        assert!(!pattern.is_empty());
        pattern.validate().unwrap();
    }

    #[test]
    fn with_steps_ignores_indices_past_the_end_rather_than_panicking() {
        let row = PatternRow::empty(36).with_steps(&[0, 63, 64, 999]);
        assert!(row.steps[0].on && row.steps[63].on);
        assert_eq!(row.steps.iter().filter(|s| s.on).count(), 2);
    }

    #[test]
    fn a_cell_past_the_loop_point_is_remembered_but_not_counted() {
        // Shorten a four-bar pattern to one and the tail has to survive, or
        // every length change would be a destructive edit.
        let mut pattern = Pattern {
            length_steps: 64,
            ..Pattern::default()
        };
        pattern.rows[0] = PatternRow::empty(36).with_steps(&[0, 32, 48]);
        assert_eq!(pattern.hit_count(), 3);

        pattern.length_steps = 16;
        assert_eq!(pattern.hit_count(), 1, "only step 0 is inside the loop");
        assert!(!pattern.rows[0].is_empty(), "the tail is still there");
        assert!(pattern.rows[0].steps[48].on);

        pattern.length_steps = 64;
        assert_eq!(pattern.hit_count(), 3, "and it comes back");
    }

    #[test]
    fn a_pattern_with_too_many_rows_is_refused_by_name() {
        let pattern = Pattern {
            rows: (0..MAX_ROWS + 1)
                .map(|i| PatternRow::empty(i as u8))
                .collect(),
            ..Pattern::silent()
        };
        assert_eq!(
            pattern.validate(),
            Err(PatternError::TooManyRows(MAX_ROWS + 1))
        );
        // Exactly at the limit is fine.
        let full = Pattern {
            rows: (0..MAX_ROWS).map(|i| PatternRow::empty(i as u8)).collect(),
            ..Pattern::silent()
        };
        assert!(full.validate().is_ok());
    }

    #[test]
    fn only_one_two_or_four_bars_are_legal_lengths() {
        for length in PATTERN_LENGTHS {
            let pattern = Pattern {
                length_steps: length,
                ..Pattern::silent()
            };
            assert!(pattern.validate().is_ok(), "{length} steps must be legal");
            assert_eq!(pattern.ticks(), u32::from(length) * TICKS_PER_STEP);
        }
        for steps in [0_u8, 8, 15, 17, 24, 48, 63, 65, 255] {
            let pattern = Pattern {
                length_steps: steps,
                ..Pattern::silent()
            };
            assert_eq!(
                pattern.validate(),
                Err(PatternError::WrongStepCount(steps as usize)),
                "steps = {steps}"
            );
        }
        assert!(!Pattern::is_legal_length(1_000));
    }

    #[test]
    fn out_of_range_keys_channels_and_velocities_are_refused_with_the_row_that_did_it() {
        let mut pattern = Pattern::silent();
        pattern.rows.push(PatternRow::empty(200));
        assert_eq!(
            pattern.validate(),
            Err(PatternError::KeyOutOfRange { row: 0, key: 200 })
        );

        let mut pattern = Pattern::silent();
        let mut row = PatternRow::empty(36);
        row.channel = 16;
        pattern.rows.push(row);
        assert_eq!(
            pattern.validate(),
            Err(PatternError::ChannelOutOfRange {
                row: 0,
                channel: 16
            })
        );

        for bad in [-0.1_f32, 1.5, f32::NAN, f32::INFINITY] {
            let mut pattern = Pattern::silent();
            let mut row = PatternRow::empty(36);
            row.steps[3] = Step::hit(bad);
            pattern.rows.push(row);
            assert!(
                matches!(
                    pattern.validate(),
                    Err(PatternError::InvalidVelocity {
                        row: 0,
                        step: 3,
                        ..
                    })
                ),
                "velocity {bad} should have been refused"
            );
        }
    }

    // ---------------------------------------------------------------- notes

    #[test]
    fn the_tick_grid_lines_up_with_the_step_grid_exactly() {
        assert_eq!(PPQ, 960);
        assert_eq!(TICKS_PER_STEP, 240);
        assert_eq!(TICKS_PER_STEP * STEPS_PER_BEAT as u32, PPQ, "four per beat");
        // The factors that make 960 worth having: every one of these is exact.
        for division in [2_u32, 3, 4, 5, 6, 8, 12, 16, 24, 32, 48, 64, 96, 192, 480] {
            assert_eq!(PPQ % division, 0, "{division} must divide {PPQ} exactly");
        }
    }

    #[test]
    fn a_note_knows_where_it_ends_without_overflowing() {
        let note = MelodicNote::new(60, 1, 480, 960);
        assert_eq!(note.end_ticks(), 1_440);
        assert_eq!(note.velocity, DEFAULT_STEP_VELOCITY);
        assert_eq!(note.at_velocity(1.0).velocity, 1.0);

        // Two u32s that would wrap a u32 must produce a number, not a wrap.
        let huge = MelodicNote::new(60, 0, u32::MAX, u32::MAX);
        assert_eq!(huge.end_ticks(), 2 * u64::from(u32::MAX));
    }

    #[test]
    fn a_note_may_touch_the_bar_line_but_not_cross_it() {
        let mut pattern = Pattern::default();
        let bar = pattern.ticks();

        // Exactly filling the last step: legal, and its note-off is the first
        // sample of the next loop.
        pattern.notes = vec![MelodicNote::new(
            60,
            0,
            bar - TICKS_PER_STEP,
            TICKS_PER_STEP,
        )];
        assert_eq!(pattern.notes[0].end_ticks(), u64::from(bar));
        pattern.validate().expect("a note may end on the bar line");

        // One tick longer: refused, because on the next time round it would
        // sound before its own note-on.
        pattern.notes[0].length_ticks += 1;
        assert_eq!(
            pattern.validate(),
            Err(PatternError::NoteOutOfPattern {
                note: 0,
                end: u64::from(bar) + 1,
                pattern_ticks: bar,
            })
        );

        // ...and lengthening the pattern makes the same note legal again.
        pattern.length_steps = 32;
        assert!(pattern.validate().is_ok());
    }

    #[test]
    fn a_note_the_engine_could_not_play_is_refused_by_name() {
        let bad = |note: MelodicNote| {
            Pattern {
                notes: vec![note],
                ..Pattern::default()
            }
            .validate()
            .unwrap_err()
        };

        assert_eq!(
            bad(MelodicNote::new(200, 0, 0, 240)),
            PatternError::NoteKeyOutOfRange { note: 0, key: 200 }
        );
        assert_eq!(
            bad(MelodicNote::new(60, 16, 0, 240)),
            PatternError::NoteChannelOutOfRange {
                note: 0,
                channel: 16
            }
        );
        assert_eq!(
            bad(MelodicNote::new(60, 0, 0, 0)),
            PatternError::NoteZeroLength { note: 0 },
            "a note that sounds for no time is an editing accident, not a rest"
        );
        for velocity in [-0.1_f32, 1.5, f32::NAN, f32::INFINITY] {
            assert!(
                matches!(
                    bad(MelodicNote::new(60, 0, 0, 240).at_velocity(velocity)),
                    PatternError::NoteInvalidVelocity { note: 0, .. }
                ),
                "velocity {velocity} should have been refused"
            );
        }
    }

    #[test]
    fn a_pattern_may_hold_exactly_the_notes_the_compiled_array_has_room_for() {
        let full = Pattern {
            notes: (0..MAX_NOTES)
                .map(|i| MelodicNote::new(60, 0, (i % 3_840) as u32, 1))
                .collect(),
            length_steps: 64,
            ..Pattern::default()
        };
        full.validate().expect("exactly at the limit is fine");

        let mut over = full.clone();
        over.notes.push(MelodicNote::new(60, 0, 0, 1));
        assert_eq!(
            over.validate(),
            Err(PatternError::TooManyNotes(MAX_NOTES + 1))
        );
    }

    #[test]
    fn notes_may_overlap_because_that_is_what_a_chord_is() {
        // Three notes starting at the same tick on the same channel: a triad.
        // Nothing about that is an error, and a piano roll that refused it
        // would be a monophonic sequencer with extra steps.
        let pattern = Pattern {
            notes: vec![
                MelodicNote::new(60, 1, 0, 960),
                MelodicNote::new(64, 1, 0, 960),
                MelodicNote::new(67, 1, 0, 960),
                // And the same key twice over, which is legal too -- the
                // scheduler emits two on/off pairs and the synth decides.
                MelodicNote::new(60, 1, 240, 240),
            ],
            ..Pattern::default()
        };
        pattern.validate().expect("polyphony is the point");
        assert!(!pattern.is_empty());
        assert_eq!(pattern.hit_count(), 0, "notes are not drum hits");
    }

    // -------------------------------------------------------------- compile

    #[test]
    fn compiling_keeps_the_hits_the_velocities_and_the_addressing() {
        let mut pattern = Pattern::silent();
        let mut row = PatternRow::empty(38);
        row.channel = 3;
        row.steps[4] = Step::accent();
        row.steps[12] = Step::on();
        pattern.rows.push(row);

        let compiled = pattern.compile();
        assert_eq!(compiled.row_count(), 1);
        assert_eq!(compiled.steps(), DEFAULT_STEPS);
        let compiled_row = compiled.row(0).unwrap();
        assert_eq!((compiled_row.key(), compiled_row.channel()), (38, 3));
        assert_eq!(compiled_row.velocity_at(4), Some(ACCENT_VELOCITY));
        assert_eq!(compiled_row.velocity_at(12), Some(DEFAULT_STEP_VELOCITY));
        assert_eq!(compiled_row.velocity_at(0), None);
        assert_eq!(compiled_row.velocity_at(MAX_STEPS), None, "past the end");
    }

    #[test]
    fn compiling_carries_the_length_the_pattern_loops_at() {
        for length in PATTERN_LENGTHS {
            let pattern = Pattern {
                length_steps: length,
                ..Pattern::default()
            };
            assert_eq!(pattern.compile().steps(), length as usize);
        }
    }

    #[test]
    fn compiling_sorts_the_notes_by_start_so_the_scheduler_can_search_them() {
        let pattern = Pattern {
            notes: vec![
                MelodicNote::new(72, 0, 720, 240),
                MelodicNote::new(60, 1, 0, 240),
                MelodicNote::new(67, 0, 240, 480).at_velocity(0.5),
                MelodicNote::new(64, 0, 240, 120),
            ],
            ..Pattern::default()
        };
        let compiled = pattern.compile();
        assert_eq!(compiled.note_count(), 4);
        let starts: Vec<u32> = (0..compiled.note_count())
            .map(|i| compiled.note(i).unwrap().start_ticks)
            .collect();
        assert_eq!(starts, vec![0, 240, 240, 720]);
        // Ties break by key, so the order is total and a compile is repeatable.
        assert_eq!(compiled.note(1).unwrap().key, 64);
        assert_eq!(compiled.note(2).unwrap().key, 67);
        assert_eq!(compiled.note(2).unwrap().velocity, 0.5);
        assert_eq!(compiled.note(0).unwrap().channel, 1);
        assert!(compiled.note(4).is_none());

        // Deterministic: the same pattern compiles to the same bytes.
        assert_eq!(pattern.compile(), compiled);
    }

    #[test]
    fn compiling_the_empty_default_produces_a_pattern_that_emits_nothing() {
        let compiled = Pattern::default().compile();
        assert_eq!(compiled.row_count(), 4);
        assert_eq!(compiled.note_count(), 0);
        assert!(compiled.is_silent(), "four rows, no hits, no notes");
        assert!(Pattern::silent().compile().is_silent());
        assert_eq!(Pattern::silent().compile().row_count(), 0);
    }

    #[test]
    fn compiling_never_exceeds_the_fixed_arrays() {
        // Validation refuses these first; compile still has to be total, because
        // it is infallible and the fixed arrays are the audio thread's contract.
        let pattern = Pattern {
            rows: (0..MAX_ROWS + 8)
                .map(|i| PatternRow::empty(i as u8).with_steps(&[0]))
                .collect(),
            notes: (0..MAX_NOTES + 8)
                .map(|i| MelodicNote::new(60, 0, i as u32, 1))
                .collect(),
            length_steps: 64,
        };
        assert!(pattern.validate().is_err());
        let compiled = pattern.compile();
        assert_eq!(compiled.row_count(), MAX_ROWS);
        assert_eq!(compiled.note_count(), MAX_NOTES);
    }

    #[test]
    fn compiling_clamps_rather_than_emitting_a_note_of_no_length() {
        // Unreachable through `validate`, which refuses a zero length -- but
        // `compile` is infallible, and a zero-length note on the audio thread
        // would be a note-off at the same sample as its note-on.
        let pattern = Pattern {
            notes: vec![MelodicNote::new(60, 0, 0, 0)],
            ..Pattern::default()
        };
        assert_eq!(pattern.compile().note(0).unwrap().length_ticks, 1);
    }

    #[test]
    fn the_grid_geometry_is_the_documented_one() {
        assert_eq!(DEFAULT_STEPS, 16);
        assert_eq!(MAX_STEPS, 64);
        assert_eq!(STEPS_PER_BEAT, 4);
        assert_eq!(DEFAULT_STEPS / STEPS_PER_BEAT, 4, "one bar of 4/4");
        assert_eq!(MAX_STEPS / STEPS_PER_BEAT, 16, "four bars of it");
        assert_eq!(MAX_ROWS, 16);
        assert_eq!(MAX_NOTES, 1_024);
        assert_eq!(PATTERN_LENGTHS, [16, 32, 64]);
        assert_eq!(
            *PATTERN_LENGTHS.last().unwrap() as usize,
            MAX_STEPS,
            "the longest legal pattern must fit the row arrays"
        );
    }
}
