//! The pattern: what the user clicks, and what it is allowed to be.
//!
//! This is the **editable** shape and it never crosses the real-time wall. What
//! the audio thread sees is [`CompiledPattern`](crate::CompiledPattern), which
//! [`Pattern::compile`] produces on a non-RT thread.
//!
//! ```text
//!   webview            app (non-RT)                     audio thread
//!   -------            ------------                     ------------
//!   PatternDto --> Pattern::validate --> compile --> Owned<CompiledSong>
//!                  (typed errors)      (notes only)  -- rtrb --> SongSlot
//! ```
//!
//! # The shape: a pattern is a bag of channel clips
//! ```text
//!   Pattern { id, name, color, length_steps, channels: [
//!       ChannelClip { channel: 0, steps: Some(StepRow{key:36, ..}), notes: [] },
//!       ChannelClip { channel: 4, steps: None,                     notes: [..] },
//!   ]}
//! ```
//!
//! One [`ChannelClip`] per **channel** the pattern has anything to say about,
//! and a channel is a rack channel is a mixer strip is a note channel -- one
//! number, all the way down. A channel the pattern says nothing about has no
//! clip, which is why an empty pattern is a handful of bytes rather than
//! sixteen empty rows.
//!
//! Before this the shape was a flat `rows: Vec<PatternRow>` plus a flat
//! `notes: Vec<MelodicNote>`, each entry carrying its own `channel`. That made
//! three things awkward that a channel-keyed shape makes trivial: deleting a
//! channel (one `retain` rather than two), asking "what does channel 4 play in
//! this pattern" (a lookup rather than two filters), and drawing ghost notes
//! (the other clips of the same pattern, right there).
//!
//! # Steps are notes, written shorthand
//! **The decision, and the reason.** A [`StepRow`] is an *editing*
//! representation: sixteen or sixty-four cells at one fixed pitch, which is what
//! a drum grid wants and what a checkbox can toggle. A [`MelodicNote`] is a
//! pitch, a position and a length. FL Studio treats the two as one thing and so
//! does this: [`Pattern::compile`] turns every switched-on step into a note at
//! `index * TICKS_PER_STEP` lasting [`STEP_GATE_TICKS`], and the audio thread
//! **only ever sees notes**.
//!
//! One source of truth at the point where correctness is hardest to recover --
//! the audio thread -- and two representations only where the *user* is, which
//! is where two representations are a feature. The alternatives were both worse:
//! two scheduler paths (which is what the previous design had, and it duplicated
//! the loop-wrap and note-off machinery), or steps as the only form (which
//! cannot express a note length, so the piano roll would have nothing to write
//! into).
//!
//! The cost is one small behaviour change, and it is an improvement: a step's
//! gate used to be `round(step_len * 0.5)` samples added to the boundary, and is
//! now the sample [`STEP_GATE_TICKS`] rounds to. Those agree to within a sample,
//! and the new one is exact on the same tick grid everything else uses.
//!
//! A channel may carry **both** a step row and melodic notes; they sum. That is
//! not a state the UI offers, but it is a state the file format can express, and
//! refusing it would mean a lossy round trip through a mode switch.
//!
//! # Why the limits are constants and not opinions
//! Every one of them is checked at the IPC boundary so that a webview bug is a
//! status-bar sentence rather than a truncated pattern:
//!
//! - [`MAX_STEPS`] is 64 -- four bars of sixteenths, the longest
//!   [`PATTERN_LENGTHS`] offers.
//! - [`MAX_PATTERN_CHANNELS`] is 16, one clip per possible channel.
//! - [`MAX_NOTES`] is 1024 melodic notes per pattern.

use mydaw_rt_core::MAX_KEY;

use crate::compiled::{CompiledNote, CompiledPattern};

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

/// Ticks in one bar of 4/4.
pub const TICKS_PER_BAR: u32 = TICKS_PER_STEP * DEFAULT_STEPS as u32;

/// How long a note compiled from a switched-on step sounds.
///
/// Half a step -- the classic drum-machine gate: long enough that a synth row
/// plays a note rather than a click, short enough that consecutive steps do not
/// overlap. Expressed in **ticks** rather than as a fraction of the block-time
/// step length, so that it lands on the same grid as everything else and does
/// not move with the tempo's rounding.
pub const STEP_GATE_TICKS: u32 = TICKS_PER_STEP / 2;

/// Most channel clips one pattern may hold: one per note channel.
pub const MAX_PATTERN_CHANNELS: usize = 16;

/// Most melodic notes one channel clip may hold.
///
/// 1024 notes across four bars is a note every 3.75 ticks -- far past anything
/// a person will draw.
pub const MAX_NOTES: usize = 1024;

/// Highest note channel, mirroring MIDI 1.0 and
/// [`MAX_TRACKS`](mydaw_rt_core::MAX_TRACKS).
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

/// General MIDI keys a drum channel's step row defaults to, in rack order:
/// kick, snare, closed hat, open hat.
///
/// Written out rather than imported from `mydaw-plugin-instruments`: this crate
/// deliberately does not depend on it. A step row addresses a *key*, and whether
/// anything is listening on that key is the instrument's business. These numbers
/// are General MIDI's, not MYDAW's, so the two cannot drift apart -- and they
/// are the base keys the per-drum instruments are tuned to, so a row entered at
/// its default key plays its drum untransposed.
pub const DEFAULT_ROW_KEYS: [u8; 4] = [36, 38, 42, 46];

/// Key a step row uses when nothing says otherwise. Middle C.
///
/// A melodic channel's step row has no General MIDI number to inherit, and a
/// grid that triggered key 0 would be inaudible on most instruments.
pub const DEFAULT_STEP_KEY: u8 = 60;

/// Short display name for a step row on `key`.
///
/// Lives here rather than in the UI so the four drum rows are named once, in the
/// language that has the tests. Anything outside the General MIDI drum keys is
/// named by its number, which is the honest answer.
pub fn row_label(key: u8) -> String {
    match key {
        36 => "Kick".to_owned(),
        38 => "Snare".to_owned(),
        39 => "Clap".to_owned(),
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

/// One channel's step grid: a key, and the cells that trigger it.
///
/// The array is always [`MAX_STEPS`] long whatever the pattern's current length.
/// Shortening a pattern and lengthening it again therefore gives the tail back
/// rather than having thrown it away; only the first `length_steps` cells are
/// ever compiled.
#[derive(Debug, Clone, PartialEq)]
pub struct StepRow {
    /// MIDI key every switched-on cell triggers, e.g. 36 for a kick.
    pub key: u8,
    pub steps: [Step; MAX_STEPS],
}

impl StepRow {
    /// An empty row on `key`.
    pub fn empty(key: u8) -> Self {
        Self {
            key,
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

    /// Whether any cell is switched on, anywhere in the array.
    ///
    /// Ignores the pattern's length on purpose: this is a question about the
    /// row. "Is anything audible?" is [`Pattern::is_empty`]'s, which knows how
    /// long the pattern is.
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

    /// This row's label, from its key.
    pub fn label(&self) -> String {
        row_label(self.key)
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
/// **No channel field.** The note lives inside a [`ChannelClip`], and that clip
/// knows which channel it is. Carrying it on the note as well was a second copy
/// of the same fact, and the two could disagree.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MelodicNote {
    /// MIDI key, `0..=127`. 60 is middle C.
    pub key: u8,
    /// Offset from the start of the pattern, in ticks.
    pub start_ticks: u32,
    /// How long the note sounds, in ticks. At least 1.
    pub length_ticks: u32,
    /// Normalised velocity, `0.0..=1.0`.
    pub velocity: f32,
}

impl MelodicNote {
    /// A note on `key` at [`DEFAULT_STEP_VELOCITY`].
    pub const fn new(key: u8, start_ticks: u32, length_ticks: u32) -> Self {
        Self {
            key,
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

/// What one pattern says to one channel.
///
/// Either half may be empty; a clip with neither is legal and silent, which is
/// what an empty drum row looks like before anything is clicked.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelClip {
    /// The channel this plays: the rack channel, the mixer strip and the note
    /// channel are the same index.
    pub channel: u8,
    /// The step grid, when this channel has one.
    pub steps: Option<StepRow>,
    /// The piano roll's notes, in no particular order -- [`Pattern::compile`]
    /// sorts them on the way to the audio thread. Kept unsorted here because
    /// the editor addresses a note by its index, and re-sorting under an edit
    /// would renumber the note being dragged.
    pub notes: Vec<MelodicNote>,
}

impl ChannelClip {
    /// A clip with a step row on `key` and no notes.
    pub fn stepped(channel: u8, key: u8) -> Self {
        Self {
            channel,
            steps: Some(StepRow::empty(key)),
            notes: Vec::new(),
        }
    }

    /// A clip with notes and no step row.
    pub fn melodic(channel: u8) -> Self {
        Self {
            channel,
            steps: None,
            notes: Vec::new(),
        }
    }

    /// Whether this clip would produce no events inside `length` steps.
    pub fn is_empty(&self, length: usize) -> bool {
        self.notes.is_empty()
            && self
                .steps
                .as_ref()
                .is_none_or(|row| row.hits_within(length) == 0)
    }

    /// How the editor should draw this channel in this pattern.
    ///
    /// Derived rather than stored, and derived *this* way round -- notes win --
    /// because a channel that has notes must not be drawn as a grid that cannot
    /// show them. A channel with neither falls back to the grid, which is the
    /// thing you can click into.
    pub fn display_mode(&self) -> ChannelDisplay {
        if self.notes.is_empty() {
            ChannelDisplay::Steps
        } else {
            ChannelDisplay::Roll
        }
    }
}

/// Which editor a channel's content wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelDisplay {
    /// The step grid.
    Steps,
    /// The piano roll.
    Roll,
}

impl ChannelDisplay {
    /// Stable wire name.
    pub const fn id(self) -> &'static str {
        match self {
            Self::Steps => "steps",
            Self::Roll => "roll",
        }
    }
}

/// One pattern: an identity, a length, and what it says to each channel.
#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    /// Stable for this pattern's whole life, including across a save and load.
    /// Playlist clips reference it, so it must not be an index.
    pub id: u64,
    pub name: String,
    /// Accent colour, as a CSS hex string the UI draws clips in. `None` means
    /// "the UI picks", which is what a pattern created by the engine says --
    /// the engine has no palette and inventing one here would put a design
    /// decision in a file format.
    pub color: Option<String>,
    /// Steps in the loop: one of [`PATTERN_LENGTHS`].
    pub length_steps: u8,
    /// One entry per channel this pattern has anything to say about.
    pub channels: Vec<ChannelClip>,
}

/// Why a pattern was refused.
///
/// Every variant is reachable from the webview, which can send any JSON it
/// likes. A pattern that would not fit has to fail at the boundary with a
/// sentence, not be silently truncated on the audio thread.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum PatternError {
    #[error("a pattern may have at most {MAX_PATTERN_CHANNELS} channel clips, got {0}")]
    TooManyChannels(usize),
    #[error("a pattern must be 16, 32 or 64 steps long, got {0}")]
    WrongStepCount(usize),
    #[error("channel clip {clip} declares {got} steps; the pattern is {expected} long")]
    WrongRowLength {
        clip: usize,
        got: usize,
        expected: usize,
    },
    #[error("channel clip {clip} is on channel {channel}; note channels are 0..={MAX_CHANNEL}")]
    ChannelOutOfRange { clip: usize, channel: u16 },
    #[error("channel {channel} has two clips in the same pattern")]
    DuplicateChannel { channel: u8 },
    #[error("the step row on channel {channel} addresses key {key}; MIDI keys are 0..={MAX_KEY}")]
    KeyOutOfRange { channel: u8, key: u16 },
    #[error(
        "step {step} on channel {channel} has velocity {velocity}, \
         which is not a number between 0.0 and 1.0"
    )]
    InvalidVelocity {
        channel: u8,
        step: usize,
        velocity: f32,
    },
    #[error("channel {channel} has {got} notes; a channel may have at most {MAX_NOTES}")]
    TooManyNotes { channel: u8, got: usize },
    #[error("note {note} on channel {channel} addresses key {key}; MIDI keys are 0..={MAX_KEY}")]
    NoteKeyOutOfRange { channel: u8, note: usize, key: u16 },
    #[error("note {note} on channel {channel} has zero length; a note sounds for at least a tick")]
    NoteZeroLength { channel: u8, note: usize },
    #[error(
        "note {note} on channel {channel} runs to tick {end} \
         but the pattern is only {pattern_ticks} ticks long"
    )]
    NoteOutOfPattern {
        channel: u8,
        note: usize,
        end: u64,
        pattern_ticks: u32,
    },
    #[error(
        "note {note} on channel {channel} has velocity {velocity}, \
         which is not a number between 0.0 and 1.0"
    )]
    NoteInvalidVelocity {
        channel: u8,
        note: usize,
        velocity: f32,
    },
    #[error("a pattern's name must not be empty")]
    EmptyName,
}

impl Pattern {
    /// An empty pattern with no channel clips at all. Produces no events.
    pub fn silent(id: u64, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            color: None,
            length_steps: DEFAULT_STEPS as u8,
            channels: Vec::new(),
        }
    }

    /// A one-bar pattern with an empty step row on each of `channels`.
    ///
    /// The shape a fresh project's first pattern has, and the shape "new
    /// pattern" makes: a grid per channel, nothing switched on. Empty rather
    /// than pre-filled with a beat -- a DAW that greets you with somebody
    /// else's rhythm is showing off, and clicking in a four-on-the-floor is ten
    /// clicks, and those ten clicks are the feature.
    ///
    /// `key_for` supplies each channel's step key, so the caller (which knows
    /// what instrument is on the channel) can give a drum channel its General
    /// MIDI number and a melodic one middle C.
    pub fn with_channels(
        id: u64,
        name: impl Into<String>,
        channels: &[u8],
        key_for: impl Fn(u8) -> u8,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            color: None,
            length_steps: DEFAULT_STEPS as u8,
            channels: channels
                .iter()
                .map(|channel| ChannelClip::stepped(*channel, key_for(*channel)))
                .collect(),
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

    /// The clip for `channel`, if this pattern has one.
    pub fn channel(&self, channel: u8) -> Option<&ChannelClip> {
        self.channels.iter().find(|clip| clip.channel == channel)
    }

    /// The clip for `channel`, creating an empty one if there is none.
    ///
    /// # Errors
    /// [`PatternError::TooManyChannels`] when the pattern is already full, and
    /// [`PatternError::ChannelOutOfRange`] for a channel the engine has no
    /// track for.
    pub fn channel_mut(&mut self, channel: u8) -> Result<&mut ChannelClip, PatternError> {
        if channel > MAX_CHANNEL {
            return Err(PatternError::ChannelOutOfRange {
                clip: self.channels.len(),
                channel: u16::from(channel),
            });
        }
        if let Some(index) = self.channels.iter().position(|c| c.channel == channel) {
            return Ok(&mut self.channels[index]);
        }
        if self.channels.len() >= MAX_PATTERN_CHANNELS {
            return Err(PatternError::TooManyChannels(self.channels.len() + 1));
        }
        self.channels.push(ChannelClip::melodic(channel));
        Ok(self.channels.last_mut().expect("just pushed"))
    }

    /// Forget everything this pattern said to `channel`.
    pub fn remove_channel(&mut self, channel: u8) {
        self.channels.retain(|clip| clip.channel != channel);
    }

    /// Renumber the channels after a track was removed at `removed`.
    ///
    /// A channel *is* a track index, so deleting a track shifts every channel
    /// above it down by one -- and a clip addressed at the deleted track has
    /// nothing left to play, so it goes. Doing this here rather than leaving the
    /// pattern pointing at the wrong instrument is the difference between
    /// "delete a track" and "delete a track and quietly rewrite the song".
    pub fn retarget_after_removing(&mut self, removed: u8) {
        self.channels.retain(|clip| clip.channel != removed);
        for clip in &mut self.channels {
            if clip.channel > removed {
                clip.channel -= 1;
            }
        }
    }

    /// Check every bound the compiled form depends on.
    ///
    /// # Errors
    /// See [`PatternError`]. The first problem found is reported; a UI that has
    /// sent one bad value has usually sent one bad value.
    pub fn validate(&self) -> Result<(), PatternError> {
        if self.name.trim().is_empty() {
            return Err(PatternError::EmptyName);
        }
        if !Self::is_legal_length(self.length_steps as usize) {
            return Err(PatternError::WrongStepCount(self.length_steps as usize));
        }
        if self.channels.len() > MAX_PATTERN_CHANNELS {
            return Err(PatternError::TooManyChannels(self.channels.len()));
        }

        let pattern_ticks = self.ticks();
        for (index, clip) in self.channels.iter().enumerate() {
            if clip.channel > MAX_CHANNEL {
                return Err(PatternError::ChannelOutOfRange {
                    clip: index,
                    channel: u16::from(clip.channel),
                });
            }
            if self.channels[..index]
                .iter()
                .any(|earlier| earlier.channel == clip.channel)
            {
                return Err(PatternError::DuplicateChannel {
                    channel: clip.channel,
                });
            }

            if let Some(row) = &clip.steps {
                if row.key > MAX_KEY {
                    return Err(PatternError::KeyOutOfRange {
                        channel: clip.channel,
                        key: u16::from(row.key),
                    });
                }
                for (step, cell) in row.steps.iter().enumerate() {
                    if !cell.velocity.is_finite() || !(0.0..=1.0).contains(&cell.velocity) {
                        return Err(PatternError::InvalidVelocity {
                            channel: clip.channel,
                            step,
                            velocity: cell.velocity,
                        });
                    }
                }
            }

            if clip.notes.len() > MAX_NOTES {
                return Err(PatternError::TooManyNotes {
                    channel: clip.channel,
                    got: clip.notes.len(),
                });
            }
            for (note_index, note) in clip.notes.iter().enumerate() {
                if note.key > MAX_KEY {
                    return Err(PatternError::NoteKeyOutOfRange {
                        channel: clip.channel,
                        note: note_index,
                        key: u16::from(note.key),
                    });
                }
                if note.length_ticks == 0 {
                    return Err(PatternError::NoteZeroLength {
                        channel: clip.channel,
                        note: note_index,
                    });
                }
                // Notes may *touch* the bar line -- a note filling the last step
                // ends exactly at `pattern_ticks`, and its note-off is the first
                // sample of the next time round. They may not cross it: a
                // wrapped note would sound before its own note-on next loop.
                if note.end_ticks() > u64::from(pattern_ticks) {
                    return Err(PatternError::NoteOutOfPattern {
                        channel: clip.channel,
                        note: note_index,
                        end: note.end_ticks(),
                        pattern_ticks,
                    });
                }
                if !note.velocity.is_finite() || !(0.0..=1.0).contains(&note.velocity) {
                    return Err(PatternError::NoteInvalidVelocity {
                        channel: clip.channel,
                        note: note_index,
                        velocity: note.velocity,
                    });
                }
            }
        }
        Ok(())
    }

    /// Freeze this pattern into the form the audio thread runs.
    ///
    /// **Non-RT.** Every switched-on step inside the pattern's length becomes a
    /// note of [`STEP_GATE_TICKS`]; every melodic note comes across as itself.
    /// The result is sorted by start tick, which is what lets the scheduler
    /// binary-search into it instead of scanning. Ties break by channel then key
    /// so the order is total and a compile is deterministic: the same pattern
    /// always produces the same bytes.
    ///
    /// Compiling is **infallible** on purpose, so the publication path has one
    /// failure point ([`validate`](Self::validate)) rather than two. Anything
    /// past a limit is dropped here, which validation has already refused.
    pub fn compile(&self) -> CompiledPattern {
        let length = (self.length_steps as usize).min(MAX_STEPS);
        let mut notes: Vec<CompiledNote> = Vec::new();

        for clip in self.channels.iter().take(MAX_PATTERN_CHANNELS) {
            if clip.channel > MAX_CHANNEL {
                continue;
            }
            if let Some(row) = &clip.steps {
                for (index, cell) in row.steps[..length].iter().enumerate() {
                    if !cell.on {
                        continue;
                    }
                    notes.push(CompiledNote {
                        key: row.key,
                        channel: clip.channel,
                        start_ticks: index as u32 * TICKS_PER_STEP,
                        length_ticks: STEP_GATE_TICKS,
                        velocity: cell.velocity.clamp(0.0, 1.0),
                    });
                }
            }
            for note in clip.notes.iter().take(MAX_NOTES) {
                notes.push(CompiledNote {
                    key: note.key,
                    channel: clip.channel,
                    start_ticks: note.start_ticks,
                    length_ticks: note.length_ticks.max(1),
                    velocity: note.velocity.clamp(0.0, 1.0),
                });
            }
        }

        CompiledPattern::new(self.id, length, notes)
    }

    /// Whether this pattern would produce no events at all.
    ///
    /// Counts only what is inside the pattern's length: a cell past the loop
    /// point is remembered but never played, and calling a pattern non-empty for
    /// something nobody can hear would be a lie the UI would then have to draw.
    pub fn is_empty(&self) -> bool {
        let length = self.length_steps as usize;
        self.channels.iter().all(|clip| clip.is_empty(length))
    }

    /// Rack channels this pattern actually plays, in rack order.
    ///
    /// A channel with an all-off grid and no notes is silent and is not a
    /// voice. [`PatternSet::split_by_channel`] uses this list: one new pattern
    /// per entry.
    #[must_use]
    pub fn occupied_channels(&self) -> Vec<u8> {
        let length = self.length_steps as usize;
        let mut channels: Vec<u8> = self
            .channels
            .iter()
            .filter(|clip| !clip.is_empty(length))
            .map(|clip| clip.channel)
            .collect();
        channels.sort_unstable();
        channels
    }

    /// Drop every channel clip except `channel`.
    pub fn keep_only_channel(&mut self, channel: u8) {
        self.channels.retain(|clip| clip.channel == channel);
    }

    /// Steps switched on inside the loop, across every channel. Diagnostics and
    /// tests; melodic notes are counted by [`note_count`](Self::note_count).
    pub fn hit_count(&self) -> usize {
        let length = self.length_steps as usize;
        self.channels
            .iter()
            .filter_map(|clip| clip.steps.as_ref())
            .map(|row| row.hits_within(length))
            .sum()
    }

    /// Melodic notes across every channel.
    pub fn note_count(&self) -> usize {
        self.channels.iter().map(|clip| clip.notes.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drum_keys(channel: u8) -> u8 {
        DEFAULT_ROW_KEYS
            .get(channel as usize)
            .copied()
            .unwrap_or(DEFAULT_STEP_KEY)
    }

    fn starter() -> Pattern {
        Pattern::with_channels(1, "Pattern 1", &[0, 1, 2, 3, 4], drum_keys)
    }

    #[test]
    fn a_starter_pattern_is_one_empty_grid_per_channel_one_bar_long() {
        let pattern = starter();
        assert_eq!(pattern.length_steps as usize, DEFAULT_STEPS);
        assert_eq!(pattern.channels.len(), 5);
        assert_eq!(
            pattern
                .channels
                .iter()
                .map(|c| c.steps.as_ref().unwrap().key)
                .collect::<Vec<_>>(),
            vec![36, 38, 42, 46, DEFAULT_STEP_KEY],
            "the drums get their General MIDI keys, the melodic channel middle C"
        );
        assert!(
            pattern.is_empty(),
            "a fresh session greets you with silence"
        );
        assert_eq!(pattern.hit_count(), 0);
        assert_eq!(pattern.note_count(), 0);
        assert_eq!(pattern.ticks(), 16 * 240);
        assert_eq!(pattern.color, None, "the engine has no palette");
        pattern.validate().expect("the starter must be valid");
    }

    #[test]
    fn the_rows_are_labelled_for_the_grid_and_unknown_keys_name_themselves() {
        assert_eq!(row_label(36), "Kick");
        assert_eq!(row_label(38), "Snare");
        assert_eq!(row_label(42), "CH");
        assert_eq!(row_label(46), "OH");
        assert_eq!(row_label(60), "Key 60");
        assert_eq!(StepRow::empty(38).label(), "Snare");
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
        let mut pattern = starter();
        pattern.channels[0].steps = Some(StepRow::empty(36).with_steps(&[0, 4, 8, 12]));
        pattern.channels[1].steps = Some(StepRow::empty(38).with_steps(&[4, 12]));
        pattern.channels[2].steps = Some(StepRow::empty(42).with_steps(&[2, 6, 10, 14]));
        assert_eq!(pattern.hit_count(), 10);
        assert!(!pattern.is_empty());
        pattern.validate().unwrap();
    }

    #[test]
    fn a_cell_past_the_loop_point_is_remembered_but_not_counted() {
        // Shorten a four-bar pattern to one and the tail has to survive, or
        // every length change would be a destructive edit.
        let mut pattern = starter();
        pattern.length_steps = 64;
        pattern.channels[0].steps = Some(StepRow::empty(36).with_steps(&[0, 32, 48]));
        assert_eq!(pattern.hit_count(), 3);

        pattern.length_steps = 16;
        assert_eq!(pattern.hit_count(), 1, "only step 0 is inside the loop");
        assert!(!pattern.channels[0].steps.as_ref().unwrap().is_empty());
        assert!(pattern.channels[0].steps.as_ref().unwrap().steps[48].on);

        pattern.length_steps = 64;
        assert_eq!(pattern.hit_count(), 3, "and it comes back");
    }

    #[test]
    fn a_pattern_with_too_many_channel_clips_is_refused_by_name() {
        let mut pattern = Pattern::silent(1, "P");
        for channel in 0..=MAX_CHANNEL {
            pattern.channels.push(ChannelClip::melodic(channel));
        }
        assert_eq!(pattern.channels.len(), MAX_PATTERN_CHANNELS);
        assert!(pattern.validate().is_ok(), "exactly at the limit is fine");

        pattern.channels.push(ChannelClip::melodic(0));
        assert_eq!(
            pattern.validate(),
            Err(PatternError::TooManyChannels(MAX_PATTERN_CHANNELS + 1))
        );
    }

    #[test]
    fn one_channel_may_not_have_two_clips_in_the_same_pattern() {
        // Which is the whole point of keying by channel: two clips would be two
        // answers to "what does channel 3 play here", and the compile would
        // silently play both.
        let mut pattern = Pattern::silent(1, "P");
        pattern.channels.push(ChannelClip::melodic(3));
        pattern.channels.push(ChannelClip::stepped(3, 36));
        assert_eq!(
            pattern.validate(),
            Err(PatternError::DuplicateChannel { channel: 3 })
        );
    }

    #[test]
    fn only_one_two_or_four_bars_are_legal_lengths() {
        for length in PATTERN_LENGTHS {
            let pattern = Pattern {
                length_steps: length,
                ..Pattern::silent(1, "P")
            };
            assert!(pattern.validate().is_ok(), "{length} steps must be legal");
            assert_eq!(pattern.ticks(), u32::from(length) * TICKS_PER_STEP);
        }
        for steps in [0_u8, 8, 15, 17, 24, 48, 63, 65, 255] {
            let pattern = Pattern {
                length_steps: steps,
                ..Pattern::silent(1, "P")
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
    fn a_pattern_with_no_name_is_refused_because_a_picker_would_show_a_blank() {
        let mut pattern = Pattern::silent(1, "  ");
        assert_eq!(pattern.validate(), Err(PatternError::EmptyName));
        pattern.name = "Drums".to_owned();
        assert!(pattern.validate().is_ok());
    }

    #[test]
    fn out_of_range_keys_channels_and_velocities_are_refused_with_what_did_it() {
        let mut pattern = Pattern::silent(1, "P");
        pattern.channels.push(ChannelClip::stepped(0, 200));
        assert_eq!(
            pattern.validate(),
            Err(PatternError::KeyOutOfRange {
                channel: 0,
                key: 200
            })
        );

        let mut pattern = Pattern::silent(1, "P");
        pattern.channels.push(ChannelClip::melodic(16));
        assert_eq!(
            pattern.validate(),
            Err(PatternError::ChannelOutOfRange {
                clip: 0,
                channel: 16
            })
        );

        for bad in [-0.1_f32, 1.5, f32::NAN, f32::INFINITY] {
            let mut pattern = Pattern::silent(1, "P");
            let mut clip = ChannelClip::stepped(2, 36);
            clip.steps.as_mut().unwrap().steps[3] = Step::hit(bad);
            pattern.channels.push(clip);
            assert!(
                matches!(
                    pattern.validate(),
                    Err(PatternError::InvalidVelocity {
                        channel: 2,
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
        assert_eq!(TICKS_PER_BAR, 3_840);
        assert_eq!(STEP_GATE_TICKS, 120, "half a step, the drum-machine gate");
        // The factors that make 960 worth having: every one of these is exact.
        for division in [2_u32, 3, 4, 5, 6, 8, 12, 16, 24, 32, 48, 64, 96, 192, 480] {
            assert_eq!(PPQ % division, 0, "{division} must divide {PPQ} exactly");
        }
    }

    #[test]
    fn a_note_knows_where_it_ends_without_overflowing() {
        let note = MelodicNote::new(60, 480, 960);
        assert_eq!(note.end_ticks(), 1_440);
        assert_eq!(note.velocity, DEFAULT_STEP_VELOCITY);
        assert_eq!(note.at_velocity(1.0).velocity, 1.0);

        // Two u32s that would wrap a u32 must produce a number, not a wrap.
        let huge = MelodicNote::new(60, u32::MAX, u32::MAX);
        assert_eq!(huge.end_ticks(), 2 * u64::from(u32::MAX));
    }

    #[test]
    fn a_note_may_touch_the_bar_line_but_not_cross_it() {
        let mut pattern = starter();
        let bar = pattern.ticks();
        let clip = pattern.channel_mut(4).unwrap();

        // Exactly filling the last step: legal, and its note-off is the first
        // sample of the next loop.
        clip.notes = vec![MelodicNote::new(60, bar - TICKS_PER_STEP, TICKS_PER_STEP)];
        assert_eq!(clip.notes[0].end_ticks(), u64::from(bar));
        pattern.validate().expect("a note may end on the bar line");

        // One tick longer: refused, because on the next time round it would
        // sound before its own note-on.
        pattern.channel_mut(4).unwrap().notes[0].length_ticks += 1;
        assert_eq!(
            pattern.validate(),
            Err(PatternError::NoteOutOfPattern {
                channel: 4,
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
            let mut pattern = starter();
            pattern.channel_mut(4).unwrap().notes = vec![note];
            pattern.validate().unwrap_err()
        };

        assert_eq!(
            bad(MelodicNote::new(200, 0, 240)),
            PatternError::NoteKeyOutOfRange {
                channel: 4,
                note: 0,
                key: 200
            }
        );
        assert_eq!(
            bad(MelodicNote::new(60, 0, 0)),
            PatternError::NoteZeroLength {
                channel: 4,
                note: 0
            },
            "a note that sounds for no time is an editing accident, not a rest"
        );
        for velocity in [-0.1_f32, 1.5, f32::NAN, f32::INFINITY] {
            assert!(
                matches!(
                    bad(MelodicNote::new(60, 0, 240).at_velocity(velocity)),
                    PatternError::NoteInvalidVelocity { channel: 4, .. }
                ),
                "velocity {velocity} should have been refused"
            );
        }
    }

    #[test]
    fn a_channel_may_hold_exactly_the_notes_the_limit_allows() {
        let mut pattern = starter();
        pattern.length_steps = 64;
        pattern.channel_mut(4).unwrap().notes = (0..MAX_NOTES)
            .map(|i| MelodicNote::new(60, (i % 3_840) as u32, 1))
            .collect();
        pattern.validate().expect("exactly at the limit is fine");

        pattern
            .channel_mut(4)
            .unwrap()
            .notes
            .push(MelodicNote::new(60, 0, 1));
        assert_eq!(
            pattern.validate(),
            Err(PatternError::TooManyNotes {
                channel: 4,
                got: MAX_NOTES + 1
            })
        );
    }

    #[test]
    fn notes_may_overlap_because_that_is_what_a_chord_is() {
        let mut pattern = starter();
        pattern.channel_mut(4).unwrap().notes = vec![
            MelodicNote::new(60, 0, 960),
            MelodicNote::new(64, 0, 960),
            MelodicNote::new(67, 0, 960),
            // And the same key twice over, which is legal too -- the scheduler
            // emits two on/off pairs and the synth decides.
            MelodicNote::new(60, 240, 240),
        ];
        pattern.validate().expect("polyphony is the point");
        assert!(!pattern.is_empty());
        assert_eq!(pattern.hit_count(), 0, "notes are not drum hits");
        assert_eq!(pattern.note_count(), 4);
    }

    // -------------------------------------------------------- channel edits

    #[test]
    fn asking_for_a_channel_creates_it_and_asking_again_finds_the_same_one() {
        let mut pattern = Pattern::silent(1, "P");
        pattern
            .channel_mut(3)
            .unwrap()
            .notes
            .push(MelodicNote::new(60, 0, 240));
        assert_eq!(pattern.channels.len(), 1);
        assert_eq!(pattern.channel_mut(3).unwrap().notes.len(), 1);
        assert_eq!(pattern.channels.len(), 1, "no second clip appeared");
        assert!(pattern.channel(3).is_some());
        assert!(pattern.channel(4).is_none());

        assert!(matches!(
            pattern.channel_mut(200),
            Err(PatternError::ChannelOutOfRange { .. })
        ));
    }

    #[test]
    fn removing_a_track_shifts_the_channels_above_it_and_drops_its_own_clip() {
        // A channel *is* a track index. Leaving the clips pointing at the wrong
        // instrument would turn "delete a track" into "delete a track and
        // quietly rewrite the song".
        let mut pattern = Pattern::with_channels(1, "P", &[0, 1, 2, 3], |_| 36);
        pattern.retarget_after_removing(1);
        assert_eq!(
            pattern
                .channels
                .iter()
                .map(|c| c.channel)
                .collect::<Vec<_>>(),
            vec![0, 1, 2],
            "channel 1's clip is gone; 2 and 3 moved down"
        );
        // ...and the surviving clips kept their content.
        assert_eq!(pattern.channels[1].steps.as_ref().unwrap().key, 36);

        pattern.remove_channel(0);
        assert_eq!(
            pattern
                .channels
                .iter()
                .map(|c| c.channel)
                .collect::<Vec<_>>(),
            vec![1, 2],
            "removing a channel's content does not renumber anything"
        );
    }

    #[test]
    fn a_channel_with_notes_wants_the_roll_and_one_without_wants_the_grid() {
        let mut clip = ChannelClip::stepped(0, 36);
        assert_eq!(clip.display_mode(), ChannelDisplay::Steps);
        assert_eq!(ChannelDisplay::Steps.id(), "steps");

        clip.notes.push(MelodicNote::new(60, 0, 240));
        assert_eq!(
            clip.display_mode(),
            ChannelDisplay::Roll,
            "a grid cannot show a note length, so notes decide"
        );
        assert_eq!(ChannelDisplay::Roll.id(), "roll");

        // A channel with neither falls back to the grid: it is the thing you
        // can click into.
        assert_eq!(
            ChannelClip::melodic(0).display_mode(),
            ChannelDisplay::Steps
        );
    }

    // -------------------------------------------------------------- compile

    #[test]
    fn a_switched_on_step_compiles_to_a_note_at_its_tick() {
        // The decision this module documents: the audio thread sees notes only.
        let mut pattern = Pattern::silent(1, "P");
        let mut clip = ChannelClip::stepped(3, 38);
        clip.steps.as_mut().unwrap().steps[4] = Step::accent();
        clip.steps.as_mut().unwrap().steps[12] = Step::on();
        pattern.channels.push(clip);

        let compiled = pattern.compile();
        assert_eq!(compiled.note_count(), 2);
        assert_eq!(compiled.steps(), DEFAULT_STEPS);
        assert_eq!(compiled.id(), 1);

        let first = compiled.note(0).unwrap();
        assert_eq!(first.start_ticks, 4 * TICKS_PER_STEP);
        assert_eq!(first.length_ticks, STEP_GATE_TICKS);
        assert_eq!((first.key, first.channel), (38, 3));
        assert_eq!(first.velocity, ACCENT_VELOCITY);

        let second = compiled.note(1).unwrap();
        assert_eq!(second.start_ticks, 12 * TICKS_PER_STEP);
        assert_eq!(second.velocity, DEFAULT_STEP_VELOCITY);
    }

    #[test]
    fn a_step_past_the_loop_point_does_not_compile_to_anything() {
        // The compiled counterpart of `hit_count` being length-aware: a one-bar
        // pattern with a hit on step 48 is silent, and the engine has to agree
        // with the panel about that.
        let mut pattern = Pattern::silent(1, "P");
        let mut clip = ChannelClip::stepped(0, 36);
        clip.steps.as_mut().unwrap().steps[48] = Step::on();
        pattern.channels.push(clip);
        assert!(pattern.compile().is_silent(), "step 48 is outside one bar");

        pattern.length_steps = 64;
        assert!(!pattern.compile().is_silent());
    }

    #[test]
    fn steps_and_notes_on_one_channel_sum_rather_than_one_winning() {
        // Not a state the UI offers, but one the file format can express, and
        // refusing it would make a mode switch a lossy round trip.
        let mut pattern = Pattern::silent(1, "P");
        let mut clip = ChannelClip::stepped(0, 36);
        clip.steps.as_mut().unwrap().steps[0] = Step::on();
        clip.notes.push(MelodicNote::new(72, 480, 240));
        pattern.channels.push(clip);

        let compiled = pattern.compile();
        assert_eq!(compiled.note_count(), 2);
        assert_eq!(compiled.note(0).unwrap().key, 36);
        assert_eq!(compiled.note(1).unwrap().key, 72);
    }

    #[test]
    fn compiling_sorts_by_start_so_the_scheduler_can_search_and_is_deterministic() {
        let mut pattern = starter();
        pattern.channel_mut(4).unwrap().notes = vec![
            MelodicNote::new(72, 720, 240),
            MelodicNote::new(64, 240, 120),
            MelodicNote::new(67, 240, 480).at_velocity(0.5),
        ];
        pattern.channel_mut(0).unwrap().steps = Some(StepRow::empty(36).with_steps(&[0, 3]));

        let compiled = pattern.compile();
        let starts: Vec<u32> = compiled.notes().iter().map(|n| n.start_ticks).collect();
        assert!(starts.windows(2).all(|w| w[0] <= w[1]), "{starts:?}");
        assert_eq!(starts, vec![0, 240, 240, 720, 720]);
        // Ties break by channel then key, so the order is total.
        assert_eq!(compiled.note(1).unwrap().channel, 4);
        assert_eq!(compiled.note(1).unwrap().key, 64);
        assert_eq!(compiled.note(2).unwrap().key, 67);
        assert_eq!(compiled.note(2).unwrap().velocity, 0.5);

        // Deterministic: the same pattern compiles to the same bytes.
        assert_eq!(pattern.compile(), compiled);
    }

    #[test]
    fn compiling_the_empty_starter_produces_a_pattern_that_emits_nothing() {
        let compiled = starter().compile();
        assert_eq!(compiled.note_count(), 0);
        assert!(compiled.is_silent());
    }

    #[test]
    fn compiling_clamps_rather_than_emitting_a_note_of_no_length() {
        // Unreachable through `validate`, which refuses a zero length -- but
        // `compile` is infallible, and a zero-length note on the audio thread
        // would be a note-off at the same sample as its note-on.
        let mut pattern = starter();
        pattern.channel_mut(4).unwrap().notes = vec![MelodicNote::new(60, 0, 0)];
        assert_eq!(pattern.compile().note(0).unwrap().length_ticks, 1);
    }

    #[test]
    fn the_grid_geometry_is_the_documented_one() {
        assert_eq!(DEFAULT_STEPS, 16);
        assert_eq!(MAX_STEPS, 64);
        assert_eq!(STEPS_PER_BEAT, 4);
        assert_eq!(DEFAULT_STEPS / STEPS_PER_BEAT, 4, "one bar of 4/4");
        assert_eq!(MAX_STEPS / STEPS_PER_BEAT, 16, "four bars of it");
        assert_eq!(MAX_PATTERN_CHANNELS, 16);
        assert_eq!(MAX_PATTERN_CHANNELS, mydaw_rt_core::MAX_TRACKS);
        assert_eq!(MAX_CHANNEL as usize, MAX_PATTERN_CHANNELS - 1);
        assert_eq!(MAX_NOTES, 1_024);
        assert_eq!(PATTERN_LENGTHS, [16, 32, 64]);
        assert_eq!(
            *PATTERN_LENGTHS.last().unwrap() as usize,
            MAX_STEPS,
            "the longest legal pattern must fit the row arrays"
        );
    }
}
