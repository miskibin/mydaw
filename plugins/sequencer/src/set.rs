//! The pattern set: every pattern in the project, and which one is being edited.
//!
//! ```text
//!   PatternSet { patterns: [Pattern; ..], active: 0, next_id: 4 }
//!                   |  |  |
//!                   |  |  +-- id 3 "Lead"       <- playlist clips name these
//!                   |  +----- id 2 "Bass"          by id, never by position
//!                   +-------- id 1 "Drums"  (active: what PAT mode loops)
//! ```
//!
//! # Ids, not indices
//! Every pattern has a `u64` that is allocated once and never reused, survives a
//! save and load, and is what a playlist clip stores. Indices are for the
//! scheduler, which resolves them at compile time; nothing a user or a file can
//! touch is addressed positionally. Reordering or deleting a pattern would
//! otherwise silently repoint every clip in the arrangement -- the same class of
//! bug that makes a track index a bad key, and the reason
//! [`TrackDef`](../../app/src/project.rs) has an id too.
//!
//! Ids start at 1. Zero is left free so that "no pattern" has an obvious
//! spelling in a wire format that cannot use `Option`.
//!
//! # The active pattern
//! One pattern is *active*: it is what the step grid and the piano roll edit,
//! and what the transport loops in PAT mode. It is stored as an index because it
//! is a cursor rather than a reference -- it always names a pattern that exists,
//! deleting the active pattern moves it rather than dangling, and an empty set
//! is not representable ([`remove`](PatternSet::remove) refuses the last one).

use mydaw_rt_core::MAX_KEY;

use crate::pattern::{Pattern, PatternError};

/// Most patterns one project may hold.
///
/// Sixty-four is far past any arrangement anyone writes by hand and small enough
/// that compiling the whole set on every edit stays a sub-millisecond operation
/// -- which is what lets the engine publish the *whole* song rather than a diff.
pub const MAX_PATTERNS: usize = 64;

/// The id "no pattern". Never allocated to a real one.
pub const NO_PATTERN: u64 = 0;

/// Why a pattern-set edit was refused.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum PatternSetError {
    #[error("there is no pattern {0}")]
    NoSuchPattern(u64),
    #[error("a project may have at most {MAX_PATTERNS} patterns")]
    TooManyPatterns,
    #[error("a project must keep at least one pattern")]
    LastPattern,
    #[error(transparent)]
    Pattern(#[from] PatternError),
}

/// Every pattern in the project, plus which one is being edited.
#[derive(Debug, Clone, PartialEq)]
pub struct PatternSet {
    patterns: Vec<Pattern>,
    active: usize,
    next_id: u64,
}

impl Default for PatternSet {
    /// One empty pattern named "Pattern 1", with no channels.
    fn default() -> Self {
        Self::single(Pattern::silent(1, "Pattern 1"))
    }
}

impl PatternSet {
    /// A set holding exactly `pattern`.
    pub fn single(pattern: Pattern) -> Self {
        let next_id = pattern.id.saturating_add(1).max(1);
        Self {
            patterns: vec![pattern],
            active: 0,
            next_id,
        }
    }

    /// The set a fresh project starts with: one pattern with an empty grid on
    /// each of `channels`.
    ///
    /// `key_for` gives each channel its step key -- see
    /// [`Pattern::with_channels`].
    pub fn starter(channels: &[u8], key_for: impl Fn(u8) -> u8) -> Self {
        Self::single(Pattern::with_channels(1, "Pattern 1", channels, key_for))
    }

    /// A set read back from a file.
    ///
    /// Ids are restored rather than regenerated -- playlist clips reference
    /// them, so regenerating would detach the arrangement from its patterns.
    /// The next id continues past the highest one in the file, so a pattern
    /// added after a load cannot collide with one that was in it.
    ///
    /// An empty `patterns` is replaced by the default single pattern: a set with
    /// nothing in it has no active pattern, and every caller would then have to
    /// handle an `Option` that only a corrupt file can produce.
    pub fn restored(patterns: Vec<Pattern>, active: usize) -> Self {
        if patterns.is_empty() {
            return Self::default();
        }
        let next_id = patterns
            .iter()
            .map(|pattern| pattern.id)
            .max()
            .map_or(1, |highest| highest.saturating_add(1));
        Self {
            active: active.min(patterns.len() - 1),
            patterns,
            next_id,
        }
    }

    /// Every pattern, in the order the picker shows them.
    pub fn patterns(&self) -> &[Pattern] {
        &self.patterns
    }

    /// Patterns in the set. Never zero.
    pub fn len(&self) -> usize {
        self.patterns.len()
    }

    /// Always false: a set keeps at least one pattern. Present because clippy
    /// asks for it next to `len`, and because saying so is cheaper than making
    /// every caller wonder.
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// The id the next added pattern will get. Diagnostics and tests.
    pub const fn next_id(&self) -> u64 {
        self.next_id
    }

    /// Index of the pattern being edited.
    pub const fn active_index(&self) -> usize {
        self.active
    }

    /// The pattern being edited, and what PAT mode loops.
    pub fn active(&self) -> &Pattern {
        &self.patterns[self.active.min(self.patterns.len() - 1)]
    }

    /// Id of the pattern being edited.
    pub fn active_id(&self) -> u64 {
        self.active().id
    }

    /// Position of `id` in the set, or `None`.
    pub fn index_of(&self, id: u64) -> Option<usize> {
        self.patterns.iter().position(|pattern| pattern.id == id)
    }

    /// The pattern with `id`.
    pub fn get(&self, id: u64) -> Option<&Pattern> {
        self.patterns.iter().find(|pattern| pattern.id == id)
    }

    /// The pattern with `id`, mutably.
    ///
    /// # Errors
    /// [`PatternSetError::NoSuchPattern`].
    pub fn get_mut(&mut self, id: u64) -> Result<&mut Pattern, PatternSetError> {
        self.patterns
            .iter_mut()
            .find(|pattern| pattern.id == id)
            .ok_or(PatternSetError::NoSuchPattern(id))
    }

    /// Make `id` the pattern being edited.
    ///
    /// # Errors
    /// [`PatternSetError::NoSuchPattern`].
    pub fn set_active(&mut self, id: u64) -> Result<(), PatternSetError> {
        let index = self
            .index_of(id)
            .ok_or(PatternSetError::NoSuchPattern(id))?;
        self.active = index;
        Ok(())
    }

    /// Append an empty pattern with a grid on each of `channels`, and return its
    /// id.
    ///
    /// The new pattern is **not** made active. Adding one and switching to it
    /// are two things a UI may want separately (duplicating a pattern to edit it
    /// wants the switch; building a set does not), and a command that did both
    /// could not express the other.
    ///
    /// # Errors
    /// [`PatternSetError::TooManyPatterns`].
    pub fn add(
        &mut self,
        name: Option<String>,
        channels: &[u8],
        key_for: impl Fn(u8) -> u8,
    ) -> Result<u64, PatternSetError> {
        if self.patterns.len() >= MAX_PATTERNS {
            return Err(PatternSetError::TooManyPatterns);
        }
        let id = self.next_id;
        self.next_id += 1;
        let name = name.unwrap_or_else(|| format!("Pattern {id}"));
        self.patterns
            .push(Pattern::with_channels(id, name, channels, key_for));
        Ok(id)
    }

    /// Copy `id`, everything in it, and insert the copy straight after it.
    ///
    /// After it rather than at the end because a duplicate is an edit of *this*
    /// pattern, and a picker that scrolled to the bottom to show it would have
    /// lost the user's place.
    ///
    /// # Errors
    /// [`PatternSetError::NoSuchPattern`] or [`PatternSetError::TooManyPatterns`].
    pub fn clone_pattern(&mut self, id: u64, name: Option<String>) -> Result<u64, PatternSetError> {
        if self.patterns.len() >= MAX_PATTERNS {
            return Err(PatternSetError::TooManyPatterns);
        }
        let index = self
            .index_of(id)
            .ok_or(PatternSetError::NoSuchPattern(id))?;
        let new_id = self.next_id;
        self.next_id += 1;
        let mut copy = self.patterns[index].clone();
        copy.name = name.unwrap_or_else(|| format!("{} copy", copy.name));
        copy.id = new_id;
        self.patterns.insert(index + 1, copy);
        // Inserting before the cursor would move the pattern out from under the
        // editor.
        if self.active > index {
            self.active += 1;
        }
        Ok(new_id)
    }

    /// Remove `id`.
    ///
    /// The caller is responsible for the playlist clips that referenced it --
    /// see [`Playlist::drop_pattern`](crate::Playlist::drop_pattern). This type
    /// does not know the arrangement exists, and a pattern set that silently
    /// reached into one would be the wrong shape of coupling.
    ///
    /// # Errors
    /// [`PatternSetError::NoSuchPattern`], or [`PatternSetError::LastPattern`]
    /// -- a project with no patterns has nothing for the grid to edit, and
    /// "delete everything" is not a pattern-picker gesture.
    pub fn remove(&mut self, id: u64) -> Result<Pattern, PatternSetError> {
        if self.patterns.len() == 1 {
            return Err(PatternSetError::LastPattern);
        }
        let index = self
            .index_of(id)
            .ok_or(PatternSetError::NoSuchPattern(id))?;
        let removed = self.patterns.remove(index);
        // The cursor follows: past the removed one it shifts down, and on it it
        // stays put, which lands on the pattern that took its place (or on the
        // new last one).
        if self.active > index {
            self.active -= 1;
        }
        self.active = self.active.min(self.patterns.len() - 1);
        Ok(removed)
    }

    /// Rename `id`.
    ///
    /// # Errors
    /// [`PatternSetError::NoSuchPattern`], or [`PatternError::EmptyName`]
    /// through [`PatternSetError::Pattern`] -- a blank name would show as a
    /// blank row in the picker.
    pub fn rename(&mut self, id: u64, name: impl Into<String>) -> Result<(), PatternSetError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(PatternError::EmptyName.into());
        }
        self.get_mut(id)?.name = name;
        Ok(())
    }

    /// Set `id`'s accent colour, or clear it with `None`.
    ///
    /// # Errors
    /// [`PatternSetError::NoSuchPattern`].
    pub fn set_color(&mut self, id: u64, color: Option<String>) -> Result<(), PatternSetError> {
        self.get_mut(id)?.color = color;
        Ok(())
    }

    /// Set `id`'s loop length, in steps.
    ///
    /// Shortening is **not** destructive: cells and notes past the new end are
    /// kept and simply not played, so lengthening it again gives them back. A
    /// note that would then hang past the bar line is the one exception -- it is
    /// refused, because validation cannot express "this note is fine as long as
    /// nobody looks".
    ///
    /// # Errors
    /// [`PatternSetError::NoSuchPattern`], or a [`PatternError`] when the length
    /// is not one of [`PATTERN_LENGTHS`](crate::PATTERN_LENGTHS) or a note would
    /// no longer fit.
    pub fn set_length(&mut self, id: u64, steps: u8) -> Result<(), PatternSetError> {
        let pattern = self.get_mut(id)?;
        let previous = pattern.length_steps;
        pattern.length_steps = steps;
        if let Err(err) = pattern.validate() {
            pattern.length_steps = previous;
            return Err(err.into());
        }
        Ok(())
    }

    /// Forget everything every pattern said to `channel`, and shift the channels
    /// above it down.
    ///
    /// Called when a rack channel is deleted. A channel *is* a track index, so
    /// not doing this would leave every pattern playing the wrong instrument.
    pub fn retarget_after_removing(&mut self, channel: u8) {
        for pattern in &mut self.patterns {
            pattern.retarget_after_removing(channel);
        }
    }

    /// Give every pattern an empty step grid on `channel`.
    ///
    /// Called when a rack channel is added, so the new channel has a row to
    /// click in rather than appearing only once something is drawn into it.
    /// Patterns already at [`MAX_PATTERN_CHANNELS`](crate::MAX_PATTERN_CHANNELS)
    /// are left alone rather than failing the whole edit: the channel exists
    /// either way, and it is the pattern that has run out of room.
    pub fn add_channel(&mut self, channel: u8, key: u8) {
        for pattern in &mut self.patterns {
            if pattern.channel(channel).is_none() {
                let _ = pattern
                    .channel_mut(channel)
                    .map(|clip| clip.steps = Some(crate::StepRow::empty(key)));
            }
        }
    }

    /// Move every step-row key and piano-roll note on `channel` by `to - from`
    /// semitones, clamped to the MIDI range.
    ///
    /// Used when a channel's instrument root changes — replacing a GM kick
    /// (key 36) with a one-shot rooted at C4 (key 60) — so the grid keeps its
    /// rhythm and the sample plays at recorded pitch.
    pub fn shift_channel_keys(&mut self, channel: u8, from: u8, to: u8) {
        if from == to {
            return;
        }
        let delta = i16::from(to) - i16::from(from);
        for pattern in &mut self.patterns {
            let Some(clip) = pattern
                .channels
                .iter_mut()
                .find(|clip| clip.channel == channel)
            else {
                continue;
            };
            if let Some(row) = clip.steps.as_mut() {
                row.key = shift_key(row.key, delta);
            }
            for note in &mut clip.notes {
                note.key = shift_key(note.key, delta);
            }
        }
    }

    /// Check every pattern.
    ///
    /// # Errors
    /// The first [`PatternError`] any pattern reports.
    pub fn validate(&self) -> Result<(), PatternError> {
        for pattern in &self.patterns {
            pattern.validate()?;
        }
        Ok(())
    }

    /// Compile every pattern, in set order. **Non-RT.**
    ///
    /// The whole set rather than the active one: SONG mode can be playing any of
    /// them, and a scheduler that had to ask the non-RT side for a pattern it
    /// did not have would be a scheduler that allocates.
    pub fn compile(&self) -> Vec<crate::CompiledPattern> {
        self.patterns.iter().map(Pattern::compile).collect()
    }
}

fn shift_key(key: u8, delta: i16) -> u8 {
    (i16::from(key) + delta).clamp(0, i16::from(MAX_KEY)) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern::{MelodicNote, Step, StepRow, DEFAULT_STEP_KEY, MAX_STEPS};

    fn drum_key(channel: u8) -> u8 {
        crate::DEFAULT_ROW_KEYS
            .get(channel as usize)
            .copied()
            .unwrap_or(DEFAULT_STEP_KEY)
    }

    fn starter() -> PatternSet {
        PatternSet::starter(&[0, 1, 2, 3, 4], drum_key)
    }

    #[test]
    fn a_fresh_set_is_one_named_pattern_that_is_already_active() {
        let set = starter();
        assert_eq!(set.len(), 1);
        assert!(!set.is_empty());
        assert_eq!(set.active().name, "Pattern 1");
        assert_eq!(set.active_id(), 1);
        assert_eq!(set.active_index(), 0);
        assert_eq!(set.next_id(), 2);
        assert_eq!(set.active().channels.len(), 5);
        set.validate().unwrap();

        // Ids start at one so that zero can mean "no pattern" on the wire.
        assert_eq!(NO_PATTERN, 0);
        assert!(set.patterns().iter().all(|p| p.id != NO_PATTERN));
    }

    #[test]
    fn adding_a_pattern_gives_it_a_fresh_id_and_leaves_the_cursor_alone() {
        let mut set = starter();
        let id = set.add(None, &[0, 1], drum_key).unwrap();
        assert_eq!(id, 2);
        assert_eq!(set.get(id).unwrap().name, "Pattern 2");
        assert_eq!(set.get(id).unwrap().channels.len(), 2);
        assert_eq!(
            set.active_id(),
            1,
            "adding is not switching; a UI that wants both says both"
        );

        let named = set.add(Some("Bridge".to_owned()), &[], drum_key).unwrap();
        assert_eq!(set.get(named).unwrap().name, "Bridge");
        assert_eq!(set.len(), 3);
    }

    #[test]
    fn ids_are_never_reused_so_a_playlist_clip_cannot_be_repointed() {
        // The whole reason ids exist. Deleting a pattern and adding another must
        // not resurrect an arrangement that referenced the first.
        let mut set = starter();
        let second = set.add(None, &[], drum_key).unwrap();
        set.remove(second).unwrap();
        let third = set.add(None, &[], drum_key).unwrap();
        assert_ne!(third, second);
        assert_eq!(third, 3);
    }

    #[test]
    fn the_set_is_capped_and_says_so_rather_than_growing() {
        let mut set = starter();
        while set.len() < MAX_PATTERNS {
            set.add(None, &[], drum_key).unwrap();
        }
        assert_eq!(
            set.add(None, &[], drum_key),
            Err(PatternSetError::TooManyPatterns)
        );
        assert_eq!(
            set.clone_pattern(1, None),
            Err(PatternSetError::TooManyPatterns)
        );
    }

    #[test]
    fn cloning_copies_the_content_and_lands_next_to_the_original() {
        let mut set = starter();
        set.get_mut(1).unwrap().channels[0].steps =
            Some(StepRow::empty(36).with_steps(&[0, 4, 8, 12]));
        set.add(None, &[], drum_key).unwrap(); // id 2, after the original

        let copy = set.clone_pattern(1, None).unwrap();
        assert_eq!(copy, 3);
        assert_eq!(
            set.patterns().iter().map(|p| p.id).collect::<Vec<_>>(),
            vec![1, 3, 2],
            "the duplicate sits next to what it duplicates"
        );
        assert_eq!(set.get(copy).unwrap().name, "Pattern 1 copy");
        assert_eq!(set.get(copy).unwrap().hit_count(), 4, "content came too");

        // Editing the copy must not touch the original.
        set.get_mut(copy).unwrap().channels[0].steps = Some(StepRow::empty(36));
        assert_eq!(set.get(1).unwrap().hit_count(), 4);
        assert_eq!(set.get(copy).unwrap().hit_count(), 0);

        let named = set.clone_pattern(1, Some("Verse B".to_owned())).unwrap();
        assert_eq!(set.get(named).unwrap().name, "Verse B");
    }

    #[test]
    fn cloning_before_the_cursor_moves_the_cursor_with_its_pattern() {
        let mut set = starter();
        let second = set.add(None, &[], drum_key).unwrap();
        set.set_active(second).unwrap();
        assert_eq!(set.active_index(), 1);

        set.clone_pattern(1, None).unwrap();
        assert_eq!(
            set.active_id(),
            second,
            "the editor must still be on the pattern it was on"
        );
        assert_eq!(set.active_index(), 2);
    }

    #[test]
    fn the_cursor_follows_a_removal_rather_than_dangling() {
        let mut set = starter();
        let b = set.add(None, &[], drum_key).unwrap();
        let c = set.add(None, &[], drum_key).unwrap();

        // Removing before the cursor shifts it down onto the same pattern.
        set.set_active(c).unwrap();
        set.remove(b).unwrap();
        assert_eq!(set.active_id(), c);

        // Removing *the* cursor lands on whatever took its place.
        set.remove(c).unwrap();
        assert_eq!(set.active_id(), 1);
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn the_last_pattern_cannot_be_removed_and_an_unknown_id_is_named() {
        let mut set = starter();
        assert_eq!(set.remove(1), Err(PatternSetError::LastPattern));
        set.add(None, &[], drum_key).unwrap();
        assert_eq!(set.remove(99), Err(PatternSetError::NoSuchPattern(99)));
        assert_eq!(set.set_active(99), Err(PatternSetError::NoSuchPattern(99)));
        assert!(matches!(
            set.get_mut(99),
            Err(PatternSetError::NoSuchPattern(99))
        ));
        assert!(set.get(99).is_none());
        assert!(set.index_of(99).is_none());
    }

    #[test]
    fn renaming_refuses_a_blank_because_a_picker_would_show_an_empty_row() {
        let mut set = starter();
        set.rename(1, "Drums").unwrap();
        assert_eq!(set.get(1).unwrap().name, "Drums");
        assert_eq!(
            set.rename(1, "   "),
            Err(PatternSetError::Pattern(PatternError::EmptyName))
        );
        assert_eq!(set.get(1).unwrap().name, "Drums", "unchanged");
        assert_eq!(set.rename(99, "x"), Err(PatternSetError::NoSuchPattern(99)));
    }

    #[test]
    fn a_colour_is_the_uis_and_the_engine_only_stores_it() {
        let mut set = starter();
        assert_eq!(set.active().color, None);
        set.set_color(1, Some("#4f8ef7".to_owned())).unwrap();
        assert_eq!(set.get(1).unwrap().color.as_deref(), Some("#4f8ef7"));
        set.set_color(1, None).unwrap();
        assert_eq!(set.get(1).unwrap().color, None);
    }

    #[test]
    fn changing_the_length_keeps_the_tail_and_refuses_one_that_would_hang() {
        let mut set = starter();
        set.get_mut(1).unwrap().channels[0].steps =
            Some(StepRow::empty(36).with_steps(&[0, 20, 40]));

        set.set_length(1, 64).unwrap();
        assert_eq!(set.get(1).unwrap().hit_count(), 3);
        set.set_length(1, 16).unwrap();
        assert_eq!(set.get(1).unwrap().hit_count(), 1, "not played");
        set.set_length(1, 64).unwrap();
        assert_eq!(set.get(1).unwrap().hit_count(), 3, "not lost either");

        // A length the grid cannot be.
        assert!(matches!(
            set.set_length(1, 24),
            Err(PatternSetError::Pattern(PatternError::WrongStepCount(24)))
        ));
        assert_eq!(set.get(1).unwrap().length_steps, 64, "rolled back");

        // A note that would hang past the new bar line is refused, and the
        // length is put back -- a half-applied edit is worse than a rejected one.
        set.get_mut(1).unwrap().channel_mut(4).unwrap().notes =
            vec![MelodicNote::new(60, 7_680, 960)];
        assert!(matches!(
            set.set_length(1, 16),
            Err(PatternSetError::Pattern(
                PatternError::NoteOutOfPattern { .. }
            ))
        ));
        assert_eq!(set.get(1).unwrap().length_steps, 64);
    }

    #[test]
    fn adding_a_rack_channel_gives_every_pattern_a_row_to_click_in() {
        let mut set = PatternSet::starter(&[0], drum_key);
        set.add(None, &[0], drum_key).unwrap();
        set.add_channel(1, 38);
        for pattern in set.patterns() {
            let clip = pattern
                .channel(1)
                .expect("every pattern gained the channel");
            assert_eq!(clip.steps.as_ref().unwrap().key, 38);
            assert_eq!(clip.steps.as_ref().unwrap().steps.len(), MAX_STEPS);
        }

        // Idempotent: adding it again must not wipe what was drawn into it.
        set.get_mut(1).unwrap().channel_mut(1).unwrap().steps =
            Some(StepRow::empty(38).with_steps(&[3]));
        set.add_channel(1, 38);
        assert_eq!(set.get(1).unwrap().hit_count(), 1);
    }

    #[test]
    fn removing_a_rack_channel_retargets_every_pattern_in_the_set() {
        let mut set = PatternSet::starter(&[0, 1, 2], drum_key);
        set.add(None, &[0, 1, 2], drum_key).unwrap();
        set.retarget_after_removing(1);
        for pattern in set.patterns() {
            assert_eq!(
                pattern
                    .channels
                    .iter()
                    .map(|c| c.channel)
                    .collect::<Vec<_>>(),
                vec![0, 1],
                "every pattern, not only the active one"
            );
        }
    }

    #[test]
    fn compiling_produces_one_compiled_pattern_per_pattern_in_order() {
        let mut set = starter();
        set.get_mut(1).unwrap().channels[0].steps = Some(StepRow::empty(36).with_steps(&[0, 8]));
        let second = set.add(None, &[0], drum_key).unwrap();
        set.get_mut(second).unwrap().channels[0].steps = Some({
            let mut row = StepRow::empty(38);
            row.steps[4] = Step::accent();
            row
        });

        let compiled = set.compile();
        assert_eq!(compiled.len(), 2);
        assert_eq!(compiled[0].id(), 1);
        assert_eq!(compiled[0].note_count(), 2);
        assert_eq!(compiled[1].id(), second);
        assert_eq!(compiled[1].note_count(), 1);
        assert_eq!(compiled[1].note(0).unwrap().velocity, 1.0);
    }

    #[test]
    fn a_restored_set_keeps_its_ids_and_continues_past_the_highest() {
        let set = PatternSet::restored(
            vec![
                Pattern::silent(9, "Nine"),
                Pattern::silent(4, "Four"),
                Pattern::silent(17, "Seventeen"),
            ],
            2,
        );
        assert_eq!(
            set.patterns().iter().map(|p| p.id).collect::<Vec<_>>(),
            vec![9, 4, 17]
        );
        assert_eq!(set.next_id(), 18);
        assert_eq!(set.active_id(), 17);

        // A selection past the end is clamped rather than panicking.
        assert_eq!(
            PatternSet::restored(vec![Pattern::silent(1, "P")], 9).active_index(),
            0
        );
        // ...and a file with no patterns at all still gives an editable set.
        assert_eq!(PatternSet::restored(Vec::new(), 0).len(), 1);
    }

    #[test]
    fn shifting_a_channel_moves_its_step_key_and_notes_by_the_same_interval() {
        let mut set = PatternSet::starter(&[0, 1], |_| 36);
        set.patterns[0].channels[0]
            .notes
            .push(MelodicNote::new(48, 0, 240));
        set.shift_channel_keys(0, 36, 60);
        let clip = &set.active().channels[0];
        assert_eq!(clip.steps.as_ref().unwrap().key, 60);
        assert_eq!(clip.notes[0].key, 72);
        assert_eq!(
            set.active().channels[1].steps.as_ref().unwrap().key,
            36,
            "the other channel is not moved"
        );
        set.shift_channel_keys(0, 60, 60);
        assert_eq!(set.active().channels[0].steps.as_ref().unwrap().key, 60);
    }
}
