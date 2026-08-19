//! The pattern: what the user clicks, and what it is allowed to be.
//!
//! This is the **editable** shape -- a `Vec` of rows, each a fixed array of
//! steps -- and it never crosses the real-time wall. What the audio thread sees
//! is [`CompiledPattern`](crate::CompiledPattern), which this module's
//! [`Pattern::compile`] produces on a non-RT thread.
//!
//! ```text
//!   webview            app (non-RT)                     audio thread
//!   -------            ------------                     ------------
//!   PatternDto --> Pattern::validate --> compile --> Owned<CompiledPattern>
//!                  (typed errors)     (fixed arrays)   -- rtrb --> PatternSlot
//! ```
//!
//! # Why the limits are constants and not opinions
//! [`STEPS`] is 16 because one bar of 4/4 in sixteenths is 16, and step 4 is the
//! whole point of the exercise: an FL-style grid where the bar *is* the pattern.
//! [`MAX_ROWS`] is 16 because the compiled form is a fixed-size array and the
//! audio thread must never be handed something it has to allocate for. Both are
//! checked at the IPC boundary, so a webview bug is a status-bar sentence rather
//! than a truncated pattern or a panicking `RtNode`.

use mydaw_rt_core::MAX_KEY;

use crate::compiled::{CompiledPattern, CompiledRow};

/// Steps in a pattern. One bar of 4/4 in sixteenth notes.
pub const STEPS: usize = 16;

/// Steps per beat. 16 steps = 4 beats = one bar.
pub const STEPS_PER_BEAT: usize = 4;

/// Most rows a pattern may have.
///
/// The compiled form is `[CompiledRow; MAX_ROWS]`, so this is a hard bound on
/// the audio thread's side rather than a policy: raising it is a recompile, and
/// exceeding it is a rejected IPC call.
pub const MAX_ROWS: usize = 16;

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

/// One row of the grid: a key, the channel it speaks on, and its 16 cells.
#[derive(Debug, Clone, PartialEq)]
pub struct PatternRow {
    /// MIDI key number this row triggers, e.g. 36 for a General MIDI kick.
    pub key: u8,
    /// Note channel, `0..=15`.
    pub channel: u8,
    pub steps: [Step; STEPS],
}

impl PatternRow {
    /// An empty row on `key`, channel 0.
    pub fn empty(key: u8) -> Self {
        Self {
            key,
            channel: 0,
            steps: [Step::OFF; STEPS],
        }
    }

    /// An empty row with the steps in `indices` switched on.
    ///
    /// Test and demo affordance -- "kick on every fourth step" is one call
    /// rather than four assignments. Indices past [`STEPS`] are ignored.
    #[must_use]
    pub fn with_steps(mut self, indices: &[usize]) -> Self {
        for index in indices {
            if let Some(step) = self.steps.get_mut(*index) {
                *step = Step::on();
            }
        }
        self
    }

    /// Whether any cell in this row is switched on.
    pub fn is_empty(&self) -> bool {
        !self.steps.iter().any(|step| step.on)
    }
}

/// The whole grid.
///
/// `steps` is carried explicitly even though it is always [`STEPS`] today: it is
/// what the IPC boundary validates against, and it is the field that changes
/// when patterns grow to 32 or 64 steps.
#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    pub rows: Vec<PatternRow>,
    pub steps: u8,
}

impl Default for Pattern {
    /// Four empty drum rows: kick, snare, closed hat, open hat.
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
            steps: STEPS as u8,
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
    #[error("a pattern must have exactly {STEPS} steps, got {0}")]
    WrongStepCount(usize),
    #[error("row {row} declares {got} steps; every row must have exactly {STEPS}")]
    WrongRowLength { row: usize, got: usize },
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
}

impl Pattern {
    /// An empty pattern with no rows at all. Produces no events.
    pub fn silent() -> Self {
        Self {
            rows: Vec::new(),
            steps: STEPS as u8,
        }
    }

    /// Check every bound the compiled form depends on.
    ///
    /// # Errors
    /// See [`PatternError`]. The first problem found is reported; a UI that has
    /// sent one bad value has usually sent one bad value.
    pub fn validate(&self) -> Result<(), PatternError> {
        if self.steps as usize != STEPS {
            return Err(PatternError::WrongStepCount(self.steps as usize));
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
        Ok(())
    }

    /// Freeze this pattern into the fixed-size form the audio thread runs.
    ///
    /// **Non-RT.** Rows past [`MAX_ROWS`] are dropped, which
    /// [`validate`](Self::validate) has already refused -- compiling is
    /// infallible on purpose so the publication path has one failure point
    /// (validation) rather than two.
    pub fn compile(&self) -> CompiledPattern {
        let mut compiled = CompiledPattern::EMPTY;
        for row in self.rows.iter().take(MAX_ROWS) {
            let mut compiled_row = CompiledRow::silent(row.key, row.channel);
            for (index, step) in row.steps.iter().enumerate() {
                if step.on {
                    compiled_row.set(index, step.velocity.clamp(0.0, 1.0));
                }
            }
            compiled.push(compiled_row);
        }
        compiled
    }

    /// Whether any row has any step switched on.
    pub fn is_empty(&self) -> bool {
        self.rows.iter().all(PatternRow::is_empty)
    }

    /// Total steps switched on, across every row. Diagnostics and tests.
    pub fn hit_count(&self) -> usize {
        self.rows
            .iter()
            .map(|row| row.steps.iter().filter(|s| s.on).count())
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_pattern_is_four_empty_general_midi_drum_rows() {
        let pattern = Pattern::default();
        assert_eq!(pattern.steps as usize, STEPS);
        assert_eq!(
            pattern.rows.iter().map(|r| r.key).collect::<Vec<_>>(),
            vec![36, 38, 42, 46],
            "kick, snare, closed hat, open hat -- the drum kit's own keys"
        );
        assert!(pattern.rows.iter().all(|r| r.channel == 0));
        assert!(
            pattern.is_empty(),
            "a fresh session greets you with silence"
        );
        assert_eq!(pattern.hit_count(), 0);
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
        let row = PatternRow::empty(36).with_steps(&[0, 15, 16, 999]);
        assert!(row.steps[0].on && row.steps[15].on);
        assert_eq!(row.steps.iter().filter(|s| s.on).count(), 2);
    }

    #[test]
    fn a_pattern_with_too_many_rows_is_refused_by_name() {
        let pattern = Pattern {
            rows: (0..MAX_ROWS + 1)
                .map(|i| PatternRow::empty(i as u8))
                .collect(),
            steps: STEPS as u8,
        };
        assert_eq!(
            pattern.validate(),
            Err(PatternError::TooManyRows(MAX_ROWS + 1))
        );
        // Exactly at the limit is fine.
        let full = Pattern {
            rows: (0..MAX_ROWS).map(|i| PatternRow::empty(i as u8)).collect(),
            steps: STEPS as u8,
        };
        assert!(full.validate().is_ok());
    }

    #[test]
    fn a_pattern_that_is_not_sixteen_steps_is_refused() {
        for steps in [0_u8, 8, 15, 17, 32] {
            let pattern = Pattern {
                rows: Vec::new(),
                steps,
            };
            assert_eq!(
                pattern.validate(),
                Err(PatternError::WrongStepCount(steps as usize)),
                "steps = {steps}"
            );
        }
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

    #[test]
    fn the_error_messages_name_the_limit_the_reader_has_to_fix() {
        let message = PatternError::TooManyRows(99).to_string();
        assert!(message.contains("16"), "{message}");
        assert!(message.contains("99"), "{message}");
        let message = PatternError::WrongStepCount(8).to_string();
        assert!(message.contains("16") && message.contains('8'), "{message}");
    }

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
        assert_eq!(compiled.steps(), STEPS);
        let compiled_row = compiled.row(0).unwrap();
        assert_eq!((compiled_row.key(), compiled_row.channel()), (38, 3));
        assert_eq!(compiled_row.velocity_at(4), Some(ACCENT_VELOCITY));
        assert_eq!(compiled_row.velocity_at(12), Some(DEFAULT_STEP_VELOCITY));
        assert_eq!(compiled_row.velocity_at(0), None);
        assert_eq!(compiled_row.velocity_at(STEPS), None, "past the end");
    }

    #[test]
    fn compiling_the_empty_default_produces_a_pattern_that_emits_nothing() {
        let compiled = Pattern::default().compile();
        assert_eq!(compiled.row_count(), 4);
        assert!(compiled.is_silent(), "four rows, no hits");
        assert!(Pattern::silent().compile().is_silent());
        assert_eq!(Pattern::silent().compile().row_count(), 0);
    }

    #[test]
    fn compiling_never_exceeds_the_fixed_row_array() {
        // Validation refuses this first; compile still has to be total, because
        // it is infallible and the fixed array is the audio thread's contract.
        let pattern = Pattern {
            rows: (0..MAX_ROWS + 8)
                .map(|i| PatternRow::empty(i as u8).with_steps(&[0]))
                .collect(),
            steps: STEPS as u8,
        };
        assert!(pattern.validate().is_err());
        assert_eq!(pattern.compile().row_count(), MAX_ROWS);
    }

    #[test]
    fn the_grid_geometry_is_the_documented_one() {
        assert_eq!(STEPS, 16);
        assert_eq!(STEPS_PER_BEAT, 4);
        assert_eq!(STEPS / STEPS_PER_BEAT, 4, "one bar of 4/4");
        assert_eq!(MAX_ROWS, 16);
    }
}
