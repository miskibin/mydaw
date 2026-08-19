//! The song as the audio thread sees it, and how it gets there.
//!
//! [`CompiledSong`] is the whole sequencer -- every pattern, which one the
//! pattern-mode transport loops, and the playlist's pattern clips -- flattened
//! into the one shape the scheduler can walk without asking a question it cannot
//! answer in a callback.
//!
//! Publication is [`GraphHandle`](mydaw_rt_core::GraphHandle)'s mechanism, one
//! step down and for the same reasons (see `mydaw_rt_core::graph`): the new song
//! crosses the wall **by value** through an `rtrb` queue as a
//! [`basedrop::Owned`], and the superseded one is dropped on the audio thread,
//! which under `basedrop` means *queued for the collector* rather than freed.
//!
//! ```text
//!   non-RT thread                            audio thread
//!   ------------                             ------------
//!   PatternSet + Playlist -> compile -> Owned<CompiledSong>
//!                          -- rtrb push -->  SongSlot::poll_swap
//!                                              |  (old song dropped)
//!   GcThread <-- basedrop collector queue -----+
//! ```
//!
//! # `Vec`, on the audio thread, on purpose
//! Everything here is a `Vec`, and none of it allocates in the callback. The
//! rule the engine keeps is **no allocation on the audio thread**, not "no
//! heap": a `Vec` built by [`Pattern::compile`](crate::Pattern::compile) on the
//! editing thread, moved across by value, and only ever *indexed* on the other
//! side costs the callback exactly what a fixed array would -- one bounds check
//! -- and costs the editing thread the size of the song rather than the size of
//! the largest song imaginable.
//!
//! The alternative was measured against, not assumed away. Thirty-two patterns
//! of a thousand notes as `[[CompiledNote; 1024]; 32]` is a megabyte of mostly
//! zeroes memcpy'd on every keystroke in the piano roll, and the fixed size buys
//! nothing the type system was not already giving: `notes()` returns a slice
//! either way. `tests/rt_alloc.rs` measures the claim with a counting allocator
//! rather than trusting this paragraph.
//!
//! # Notes all the way down
//! There is no compiled *step*. A step is an editing shorthand for a note and
//! [`Pattern::compile`](crate::Pattern::compile) expands it, so the scheduler
//! has one loop over one kind of event instead of two loops that both have to
//! get looping, seeking and note-off parking right. See `crate::pattern`.

use mydaw_rt_core::basedrop::{Handle, Owned};
use rtrb::{Consumer, Producer};

use crate::pattern::TICKS_PER_STEP;

/// In-flight song swaps.
///
/// Sixty-four, and it was four until a real device asked for 4096-frame blocks.
/// At 44.1 kHz that is 93 ms per callback, and building an arrangement is a
/// burst of commands -- add a pattern, edit it, place three clips -- every one
/// of which republishes the whole song. Five of them inside one block filled a
/// four-deep queue and the fifth came back as an error, which is a real bug and
/// not a theoretical one: it happened the first time this ran on hardware.
///
/// The queue only ever has to absorb a *burst*, because
/// [`SongSlot::poll_swap`] keeps the last entry and drops the rest -- so the
/// depth is "how many edits can a person make in one block", not "how far
/// behind may the audio thread fall". Sixty-four is far past a human hand at
/// any block size a driver offers, and it is sixty-four pointers.
pub const SONG_QUEUE_CAPACITY: usize = 64;

/// One note, ready to read on the audio thread.
///
/// Plain public fields and no `Option` anywhere: the scheduler reads all five on
/// every note it emits, and there is no state a note can be in where one of them
/// is meaningless. Sixteen bytes, so a thousand of them is one 16 kB run the
/// callback walks a handful of entries of.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompiledNote {
    pub key: u8,
    /// The channel -- rack channel, mixer strip and note channel are one index.
    pub channel: u8,
    /// Offset from the start of the pattern, in ticks ([`PPQ`](crate::PPQ)).
    pub start_ticks: u32,
    /// How long it sounds, in ticks. Always at least 1.
    pub length_ticks: u32,
    pub velocity: f32,
}

impl CompiledNote {
    /// One tick past the last one this note sounds in.
    pub const fn end_ticks(&self) -> u64 {
        self.start_ticks as u64 + self.length_ticks as u64
    }
}

/// One pattern the audio thread can run.
#[derive(Clone, PartialEq)]
pub struct CompiledPattern {
    id: u64,
    /// Steps in the loop. Times [`TICKS_PER_STEP`], the loop length in ticks.
    steps: usize,
    /// Sorted by `start_ticks`, then channel, then key. The scheduler relies on
    /// this absolutely -- it binary-searches for the first note due in a block
    /// and stops at the first one that is too late. An unsorted array would not
    /// crash it; it would silently drop notes, which is worse.
    notes: Vec<CompiledNote>,
}

impl CompiledPattern {
    /// A pattern with no notes. Produces no events, ever.
    pub fn empty(id: u64) -> Self {
        Self {
            id,
            steps: crate::pattern::DEFAULT_STEPS,
            notes: Vec::new(),
        }
    }

    /// Freeze `notes` into a playable pattern, sorting them.
    ///
    /// **Non-RT**: this is where the sort and the allocation happen, once per
    /// compile, on the editing thread.
    pub fn new(id: u64, steps: usize, mut notes: Vec<CompiledNote>) -> Self {
        notes.sort_unstable_by(|a, b| {
            a.start_ticks
                .cmp(&b.start_ticks)
                .then(a.channel.cmp(&b.channel))
                .then(a.key.cmp(&b.key))
        });
        Self {
            id,
            steps: steps.min(crate::pattern::MAX_STEPS),
            notes,
        }
    }

    /// The pattern's stable id, as playlist clips reference it.
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// Steps per loop.
    pub const fn steps(&self) -> usize {
        self.steps
    }

    /// Ticks in one time round the loop.
    pub const fn loop_ticks(&self) -> u64 {
        self.steps as u64 * TICKS_PER_STEP as u64
    }

    /// Notes in this pattern.
    pub fn note_count(&self) -> usize {
        self.notes.len()
    }

    /// Note `index`, or `None` past the end.
    pub fn note(&self, index: usize) -> Option<&CompiledNote> {
        self.notes.get(index)
    }

    /// The notes, sorted by start tick. The scheduler's search space.
    pub fn notes(&self) -> &[CompiledNote] {
        &self.notes
    }

    /// Whether this pattern would produce no events.
    pub fn is_silent(&self) -> bool {
        self.notes.is_empty()
    }
}

/// Counts rather than contents.
///
/// The derived version would print a thousand notes, which is not a debug line,
/// it is a denial of service on the reader.
impl std::fmt::Debug for CompiledPattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledPattern")
            .field("id", &self.id)
            .field("steps", &self.steps)
            .field("notes", &self.notes.len())
            .finish()
    }
}

/// One placement of a pattern on the playlist, in the shape the scheduler wants.
///
/// Positions are absolute song ticks and lengths are already resolved, so the
/// scheduler never has to look at the pattern to know where the clip ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompiledPatternClip {
    /// Index into [`CompiledSong::patterns`]. Resolved at compile time, because
    /// searching a pattern list by id in a callback would be a linear scan per
    /// clip per block.
    pub pattern: usize,
    /// Where the clip starts, in absolute song ticks.
    pub start_ticks: u64,
    /// How long the clip plays for. Longer than the pattern loops it; shorter
    /// trims it.
    pub length_ticks: u64,
}

impl CompiledPatternClip {
    /// One tick past the last one this clip occupies.
    pub const fn end_ticks(&self) -> u64 {
        self.start_ticks + self.length_ticks
    }
}

/// Everything the note scheduler needs, in one value.
#[derive(Clone, PartialEq)]
pub struct CompiledSong {
    patterns: Vec<CompiledPattern>,
    /// Index into `patterns` of the pattern PAT mode loops. Always in range when
    /// `patterns` is non-empty; the compiler clamps it, so the scheduler does
    /// not have to.
    active: usize,
    /// Sorted by `start_ticks`. Muted playlist tracks are already excluded --
    /// a mute is a recompile, which is a human-speed edit, and it keeps the
    /// callback from asking a question per clip per block.
    clips: Vec<CompiledPatternClip>,
    /// One past the last tick any clip occupies, so SONG mode knows where the
    /// song ends without scanning.
    end_ticks: u64,
}

impl CompiledSong {
    /// A song with no patterns and no clips. Produces no events, ever.
    pub fn empty() -> Self {
        Self {
            patterns: Vec::new(),
            active: 0,
            clips: Vec::new(),
            end_ticks: 0,
        }
    }

    /// Assemble a song. **Non-RT.**
    ///
    /// `clips` is sorted here rather than being trusted, and `active` is
    /// clamped: both are invariants the scheduler depends on, and the one place
    /// to establish an invariant is where the value is built.
    pub fn new(
        patterns: Vec<CompiledPattern>,
        active: usize,
        mut clips: Vec<CompiledPatternClip>,
    ) -> Self {
        clips.retain(|clip| clip.pattern < patterns.len() && clip.length_ticks > 0);
        clips.sort_unstable_by_key(|clip| (clip.start_ticks, clip.pattern));
        let end_ticks = clips
            .iter()
            .map(CompiledPatternClip::end_ticks)
            .max()
            .unwrap_or(0);
        Self {
            active: active.min(patterns.len().saturating_sub(1)),
            patterns,
            clips,
            end_ticks,
        }
    }

    /// Patterns in this song.
    pub fn pattern_count(&self) -> usize {
        self.patterns.len()
    }

    /// Pattern `index`, or `None` past the end.
    pub fn pattern(&self, index: usize) -> Option<&CompiledPattern> {
        self.patterns.get(index)
    }

    /// The pattern PAT mode loops, or `None` when the song has none.
    pub fn active_pattern(&self) -> Option<&CompiledPattern> {
        self.patterns.get(self.active)
    }

    /// Index of the pattern PAT mode loops.
    pub const fn active(&self) -> usize {
        self.active
    }

    /// The playlist's pattern clips, sorted by start tick.
    pub fn clips(&self) -> &[CompiledPatternClip] {
        &self.clips
    }

    /// One past the last tick any clip occupies. `0` for an empty playlist.
    pub const fn end_ticks(&self) -> u64 {
        self.end_ticks
    }

    /// Move the song's end no earlier than `ticks`.
    ///
    /// Audio clips are not in `clips` -- they are rendered rather than
    /// scheduled, and go through [`crate::clips`] -- but they are still part of
    /// the *arrangement*, and SONG mode stops at the end of the last thing in
    /// it. Without this, a song whose final item is a vocal would stop at the
    /// last pattern clip and cut the vocal off.
    pub fn extend_to(mut self, ticks: u64) -> Self {
        self.end_ticks = self.end_ticks.max(ticks);
        self
    }

    /// Whether the arrangement would produce no events at all.
    pub fn song_is_silent(&self) -> bool {
        self.clips.iter().all(|clip| {
            self.patterns
                .get(clip.pattern)
                .is_none_or(CompiledPattern::is_silent)
        })
    }
}

impl Default for CompiledSong {
    fn default() -> Self {
        Self::empty()
    }
}

impl std::fmt::Debug for CompiledSong {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledSong")
            .field("patterns", &self.patterns.len())
            .field("active", &self.active)
            .field("clips", &self.clips.len())
            .field("end_ticks", &self.end_ticks)
            .finish()
    }
}

/// Why a song could not be handed to the audio thread.
#[derive(Debug, thiserror::Error)]
pub enum SongInstallError {
    /// The audio thread has not consumed previous songs yet. Retryable; the
    /// rejected song is dropped here, on the non-RT thread.
    #[error("the song queue is full; the audio thread has not picked up previous edits")]
    QueueFull,
    /// The audio thread went away -- the stream stopped since this handle was
    /// created.
    #[error("the audio thread is gone; cannot install the song")]
    Disconnected,
}

/// Non-RT end of the song channel, for one open stream.
pub struct SongHandle {
    tx: Producer<Owned<CompiledSong>>,
    gc: Handle,
}

impl SongHandle {
    /// Publish `song` to the audio thread. Takes effect on the next block.
    ///
    /// All allocation (the `Owned` box) happens here, on the calling non-RT
    /// thread.
    ///
    /// # Errors
    /// [`SongInstallError::QueueFull`] or [`SongInstallError::Disconnected`].
    pub fn install(&mut self, song: CompiledSong) -> Result<(), SongInstallError> {
        let owned = Owned::new(&self.gc, song);
        // On failure the rejected song comes back inside the error and is
        // dropped right here -- deliberately on this thread, never on the audio
        // thread.
        self.tx.push(owned).map_err(|_| {
            if self.tx.is_abandoned() {
                SongInstallError::Disconnected
            } else {
                SongInstallError::QueueFull
            }
        })
    }
}

/// Audio-thread end: owns the song currently playing.
pub struct SongSlot {
    rx: Consumer<Owned<CompiledSong>>,
    current: Option<Owned<CompiledSong>>,
}

impl SongSlot {
    /// Pick up the newest pending song, keeping only the last of a burst.
    ///
    /// Wait-free; call once per block *before* scheduling. Superseded songs are
    /// dropped here, which under `basedrop` enqueues them on the collector
    /// rather than freeing them.
    ///
    /// Returns `true` if the current song was replaced.
    pub fn poll_swap(&mut self) -> bool {
        let mut swapped = false;
        while let Ok(next) = self.rx.pop() {
            self.current = Some(next);
            swapped = true;
        }
        swapped
    }

    /// The song currently playing, or `None` before the first install.
    pub fn current(&self) -> Option<&CompiledSong> {
        self.current.as_deref()
    }

    /// Whether a song has been installed.
    pub fn has_song(&self) -> bool {
        self.current.is_some()
    }
}

/// Create the song channel for one stream.
///
/// `gc` comes from a [`basedrop::Collector`] -- in the engine, from the audio
/// service's [`GcThread`](mydaw_rt_core::GcThread).
pub fn song_channel(capacity: usize, gc: Handle) -> (SongHandle, SongSlot) {
    let (tx, rx) = mydaw_rt_core::rt_channel::<Owned<CompiledSong>>(capacity.max(1));
    (SongHandle { tx, gc }, SongSlot { rx, current: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern::{ChannelClip, MelodicNote, Pattern, Step, StepRow};
    use mydaw_rt_core::basedrop::Collector;

    fn note(start: u32, channel: u8, key: u8) -> CompiledNote {
        CompiledNote {
            key,
            channel,
            start_ticks: start,
            length_ticks: 120,
            velocity: 1.0,
        }
    }

    #[test]
    fn an_empty_song_has_no_patterns_no_clips_and_no_end() {
        let song = CompiledSong::empty();
        assert_eq!(song.pattern_count(), 0);
        assert!(song.clips().is_empty());
        assert_eq!(song.end_ticks(), 0);
        assert!(song.active_pattern().is_none());
        assert!(song.song_is_silent());
        assert_eq!(CompiledSong::default(), song);
    }

    #[test]
    fn a_compiled_pattern_sorts_its_notes_and_knows_its_loop_length() {
        let pattern = CompiledPattern::new(
            7,
            32,
            vec![note(720, 0, 60), note(0, 2, 36), note(240, 1, 38)],
        );
        assert_eq!(pattern.id(), 7);
        assert_eq!(pattern.steps(), 32);
        assert_eq!(pattern.loop_ticks(), 32 * 240);
        assert_eq!(
            pattern
                .notes()
                .iter()
                .map(|n| n.start_ticks)
                .collect::<Vec<_>>(),
            vec![0, 240, 720]
        );
        assert_eq!(pattern.note_count(), 3);
        assert!(pattern.note(3).is_none());
        assert!(!pattern.is_silent());
        assert!(CompiledPattern::empty(1).is_silent());
    }

    #[test]
    fn a_compiled_note_knows_where_it_ends() {
        let note = CompiledNote {
            start_ticks: 3_600,
            length_ticks: 240,
            ..note(0, 0, 60)
        };
        assert_eq!(note.end_ticks(), 3_840);
    }

    #[test]
    fn the_step_count_is_clamped_to_what_a_pattern_may_be() {
        assert_eq!(CompiledPattern::new(1, 999, Vec::new()).steps(), 64);
    }

    #[test]
    fn a_song_sorts_its_clips_clamps_the_active_index_and_finds_its_end() {
        let patterns = vec![CompiledPattern::empty(1), CompiledPattern::empty(2)];
        let song = CompiledSong::new(
            patterns,
            9,
            vec![
                CompiledPatternClip {
                    pattern: 1,
                    start_ticks: 7_680,
                    length_ticks: 3_840,
                },
                CompiledPatternClip {
                    pattern: 0,
                    start_ticks: 0,
                    length_ticks: 3_840,
                },
            ],
        );
        assert_eq!(
            song.clips()
                .iter()
                .map(|c| c.start_ticks)
                .collect::<Vec<_>>(),
            vec![0, 7_680],
            "the scheduler binary-searches these"
        );
        assert_eq!(song.end_ticks(), 11_520);
        assert_eq!(song.active(), 1, "clamped to the last pattern");
        assert_eq!(song.active_pattern().unwrap().id(), 2);
        assert!(song.pattern(2).is_none());
    }

    #[test]
    fn a_clip_naming_a_pattern_that_is_not_there_is_dropped_rather_than_trusted() {
        // Reachable from a project file whose playlist outlived a pattern. The
        // callback must not index past the end, and the one place to establish
        // that is where the value is built.
        let song = CompiledSong::new(
            vec![CompiledPattern::empty(1)],
            0,
            vec![
                CompiledPatternClip {
                    pattern: 4,
                    start_ticks: 0,
                    length_ticks: 3_840,
                },
                CompiledPatternClip {
                    pattern: 0,
                    start_ticks: 0,
                    length_ticks: 0,
                },
                CompiledPatternClip {
                    pattern: 0,
                    start_ticks: 3_840,
                    length_ticks: 3_840,
                },
            ],
        );
        assert_eq!(song.clips().len(), 1, "the dangling and the empty both go");
        assert_eq!(song.clips()[0].start_ticks, 3_840);
        assert_eq!(song.end_ticks(), 7_680);
    }

    #[test]
    fn a_song_of_silent_patterns_is_a_silent_song() {
        let song = CompiledSong::new(
            vec![CompiledPattern::empty(1)],
            0,
            vec![CompiledPatternClip {
                pattern: 0,
                start_ticks: 0,
                length_ticks: 3_840,
            }],
        );
        assert!(song.song_is_silent());

        let loud = CompiledSong::new(
            vec![CompiledPattern::new(1, 16, vec![note(0, 0, 36)])],
            0,
            vec![CompiledPatternClip {
                pattern: 0,
                start_ticks: 0,
                length_ticks: 3_840,
            }],
        );
        assert!(!loud.song_is_silent());
    }

    #[test]
    fn the_debug_lines_are_counts_rather_than_kilobytes_of_notes() {
        let mut pattern = Pattern::silent(1, "P");
        let mut clip = ChannelClip::stepped(0, 36);
        clip.steps = Some(StepRow::empty(36).with_steps(&(0..16).collect::<Vec<_>>()));
        clip.notes = (0..200).map(|i| MelodicNote::new(60, i * 10, 5)).collect();
        pattern.channels.push(clip);

        let compiled = pattern.compile();
        let text = format!("{compiled:?}");
        assert!(text.contains("notes: 216"), "{text}");
        assert!(text.len() < 200, "{} characters is a wall", text.len());

        let song = CompiledSong::new(vec![compiled], 0, Vec::new());
        let text = format!("{song:?}");
        assert!(text.contains("patterns: 1"), "{text}");
        assert!(text.len() < 200);
    }

    // ------------------------------------------------------------ the channel

    #[test]
    fn a_slot_starts_empty_and_picks_up_the_installed_song() {
        let collector = Collector::new();
        let (mut handle, mut slot) = song_channel(SONG_QUEUE_CAPACITY, collector.handle());
        assert!(!slot.has_song());
        assert!(slot.current().is_none());
        assert!(!slot.poll_swap(), "nothing pending");

        let mut pattern = Pattern::silent(3, "P");
        let mut clip = ChannelClip::stepped(0, 36);
        clip.steps.as_mut().unwrap().steps[8] = Step::on();
        pattern.channels.push(clip);
        handle
            .install(CompiledSong::new(vec![pattern.compile()], 0, Vec::new()))
            .unwrap();

        assert!(slot.poll_swap());
        let current = slot.current().expect("installed");
        assert_eq!(current.pattern_count(), 1);
        assert_eq!(current.active_pattern().unwrap().id(), 3);
        assert_eq!(current.active_pattern().unwrap().note_count(), 1);
    }

    #[test]
    fn a_burst_of_edits_collapses_to_the_newest_song() {
        let collector = Collector::new();
        let (mut handle, mut slot) = song_channel(8, collector.handle());
        for id in [1_u64, 2, 3] {
            handle
                .install(CompiledSong::new(
                    vec![CompiledPattern::empty(id)],
                    0,
                    Vec::new(),
                ))
                .unwrap();
        }
        assert!(slot.poll_swap());
        assert_eq!(slot.current().unwrap().active_pattern().unwrap().id(), 3);
    }

    #[test]
    fn a_full_queue_is_reported_rather_than_dropped_silently() {
        let collector = Collector::new();
        // Never polled, so the queue fills and stays full.
        let (mut handle, _slot) = song_channel(2, collector.handle());
        handle.install(CompiledSong::empty()).unwrap();
        handle.install(CompiledSong::empty()).unwrap();
        assert!(matches!(
            handle.install(CompiledSong::empty()),
            Err(SongInstallError::QueueFull)
        ));
    }

    #[test]
    fn an_abandoned_queue_reports_a_dead_audio_thread_not_a_full_one() {
        let collector = Collector::new();
        let (mut handle, slot) = song_channel(1, collector.handle());
        drop(slot);
        // `rtrb` lets a push into an abandoned queue succeed while there is
        // room -- the song simply has nowhere to go. It is the push that
        // *fails* which has to say which of the two things went wrong, because
        // "retry in a moment" and "the stream stopped" need different answers.
        handle.install(CompiledSong::empty()).unwrap();
        assert!(matches!(
            handle.install(CompiledSong::empty()),
            Err(SongInstallError::Disconnected)
        ));
    }

    /// The point of `basedrop`: a superseded song is dropped on the audio
    /// thread, and dropping it there must not free it.
    #[test]
    fn a_superseded_song_is_freed_by_the_collector_not_the_audio_thread() {
        let mut collector = Collector::new();
        let (mut handle, mut slot) = song_channel(4, collector.handle());

        handle.install(CompiledSong::empty()).unwrap();
        handle
            .install(CompiledSong::new(
                vec![CompiledPattern::new(1, 16, vec![note(0, 0, 36)])],
                0,
                Vec::new(),
            ))
            .unwrap();
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
}
