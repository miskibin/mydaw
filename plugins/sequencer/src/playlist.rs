//! The playlist: patterns and audio, laid out along the song.
//!
//! ```text
//!   bar      0     4     8    12    16    20
//!   Drums   [A....][A....][A....][A....]
//!   Bass                  [B....][B....]
//!   Audio                        [vox.wav.......]
//! ```
//!
//! A pattern is a loop; the playlist is where loops become a piece of music.
//! [`Clip`] is one of two things placed on a [`PlaylistTrack`] at an absolute
//! song tick: a **pattern clip**, which plays a pattern from
//! [`PatternSet`](crate::PatternSet), or an **audio clip**, which plays a file.
//!
//! # Playlist tracks are lanes, not mixer channels
//! A playlist track has a name, a mute, a solo and **nothing else**: no fader,
//! no pan, no insert chain, no output. It exists so that overlapping clips can
//! be drawn on separate rows and muted or soloed in groups -- which is what
//! FL's playlist tracks are, and it is why a project can have five rack
//! channels and twelve playlist lanes without either number meaning anything
//! to the other. Solo is the mixer's rule, resolved here: if any lane is
//! soloed, only soloed lanes compile. The audio thread never sees the flag.
//!
//! A pattern clip's audio goes wherever its pattern's channels go: to the rack
//! channels, and therefore to their mixer strips. An audio clip of file *N*
//! goes to the **Audio Clip channel** for that file (`Source::AudioClip`),
//! through that strip's inserts, fader, pan and mute. Playlist lanes stay
//! drawing rows; they are not mixer strips. Per-clip gain
//! ([`AudioClip::gain_db`]) is a trim on top of the channel.
//!
//! # Time is ticks, everywhere
//! `start_ticks` and `length_ticks` are in the same [`PPQ`](crate::PPQ) ticks a
//! note uses, not in bars and not in samples. Bars are a UI unit (the design
//! mock stores clip positions in bars and note positions in sixteenths, and then
//! has to convert at every boundary); samples are a device unit and would make a
//! project sound different on a 44.1 kHz interface. One tick domain, converted
//! exactly once, in [`tick_boundary`](crate::tick_boundary).
//!
//! # Clip length is not pattern length
//! A pattern clip has its own length. Longer than the pattern **loops** it --
//! four bars of a one-bar pattern is one clip, not four -- and shorter **trims**
//! it. Both are what a user dragging a clip edge expects, and both fall out of
//! the scheduler's arithmetic without a special case.

use crate::pattern::TICKS_PER_BAR;
use crate::set::PatternSet;

/// Most lanes a playlist may have.
pub const MAX_PLAYLIST_TRACKS: usize = 32;

/// Most clips a playlist may hold.
///
/// Five hundred is a dense four-minute arrangement and is bounded so that
/// compiling the song stays an operation with a known cost -- the whole song is
/// republished on every edit, and "how long does that take" must have an answer.
pub const MAX_CLIPS: usize = 512;

/// Furthest a clip may start or end, in ticks. 1024 bars of 4/4.
///
/// At 130 BPM that is over half an hour. The bound exists so that the
/// scheduler's loop over clip iterations is provably finite from data the IPC
/// boundary has already checked, rather than from an argument about what users
/// do.
pub const MAX_SONG_TICKS: u64 = 1_024 * TICKS_PER_BAR as u64;

/// Length a pattern clip gets when the UI does not say. Four bars.
pub const DEFAULT_CLIP_TICKS: u64 = 4 * TICKS_PER_BAR as u64;

/// One lane of the arrangement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistTrack {
    /// Stable across insertion and removal, and across a save and load. Clips
    /// reference lanes by index rather than by id (a clip is drawn *at* a row),
    /// but the UI keys its list on this so a removal animates correctly.
    pub id: u64,
    pub name: String,
    /// Silences every clip on this lane unless a solo overrides it. Applied
    /// when the song is compiled, so the audio thread never has to ask.
    pub muted: bool,
    /// Isolates this lane in SONG mode. Same rule as mixer solo: if any lane
    /// is soloed, only soloed lanes compile, and a soloed+muted lane stays
    /// audible.
    pub soloed: bool,
}

impl PlaylistTrack {
    /// An unmuted, unsoloed lane.
    pub fn new(id: u64, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            muted: false,
            soloed: false,
        }
    }
}

/// A pattern placed on the timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatternClip {
    pub id: u64,
    /// The pattern this plays, by [`Pattern::id`](crate::Pattern::id).
    pub pattern_id: u64,
    /// Which lane it is drawn on.
    pub track: usize,
    /// Where it starts, in absolute song ticks.
    pub start_ticks: u64,
    /// How long it plays for. Longer than the pattern loops it; shorter trims
    /// it. Never zero -- see [`PlaylistError::ZeroLength`].
    pub length_ticks: u64,
}

/// An audio file placed on the timeline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioClip {
    pub id: u64,
    /// The loaded file this plays, by
    /// [`AudioSource::id`](crate::audio::AudioSource::id).
    pub source_id: u64,
    pub track: usize,
    /// Where it starts, in absolute song ticks.
    pub start_ticks: u64,
    /// How far into the file playback begins, in **frames of the file's own
    /// sample rate**.
    ///
    /// Frames rather than ticks because this is a position in a recording, not
    /// in the song: it must not move when the tempo changes. Trimming the head
    /// of a clip and then changing the tempo would otherwise start the vocal on
    /// a different syllable.
    pub offset_frames: u64,
    /// How long it plays for, in song ticks. The file ending first ends it
    /// early; this ending first cuts it off.
    pub length_ticks: u64,
    /// Per-clip trim, in dB. `0.0` is unity.
    pub gain_db: f32,
}

/// One thing on the timeline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Clip {
    Pattern(PatternClip),
    Audio(AudioClip),
}

impl Clip {
    /// This clip's id, unique across both kinds.
    pub const fn id(&self) -> u64 {
        match self {
            Self::Pattern(clip) => clip.id,
            Self::Audio(clip) => clip.id,
        }
    }

    /// The lane it is on.
    pub const fn track(&self) -> usize {
        match self {
            Self::Pattern(clip) => clip.track,
            Self::Audio(clip) => clip.track,
        }
    }

    /// The loaded file an audio clip plays, if this is one.
    pub const fn audio_source_id(&self) -> Option<u64> {
        match self {
            Self::Audio(clip) => Some(clip.source_id),
            Self::Pattern(_) => None,
        }
    }

    /// Where it starts, in song ticks.
    pub const fn start_ticks(&self) -> u64 {
        match self {
            Self::Pattern(clip) => clip.start_ticks,
            Self::Audio(clip) => clip.start_ticks,
        }
    }

    /// How long it plays for, in song ticks.
    pub const fn length_ticks(&self) -> u64 {
        match self {
            Self::Pattern(clip) => clip.length_ticks,
            Self::Audio(clip) => clip.length_ticks,
        }
    }

    /// One tick past the last one it occupies.
    pub const fn end_ticks(&self) -> u64 {
        self.start_ticks() + self.length_ticks()
    }

    /// Stable wire name of the kind.
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Pattern(_) => "pattern",
            Self::Audio(_) => "audio",
        }
    }

    /// Move it, keeping everything else.
    pub fn moved(mut self, track: usize, start_ticks: u64) -> Self {
        match &mut self {
            Self::Pattern(clip) => {
                clip.track = track;
                clip.start_ticks = start_ticks;
            }
            Self::Audio(clip) => {
                clip.track = track;
                clip.start_ticks = start_ticks;
            }
        }
        self
    }

    /// Resize it, keeping everything else.
    pub fn resized(mut self, length_ticks: u64) -> Self {
        match &mut self {
            Self::Pattern(clip) => clip.length_ticks = length_ticks,
            Self::Audio(clip) => clip.length_ticks = length_ticks,
        }
        self
    }
}

/// Why a playlist edit was refused.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum PlaylistError {
    #[error("there is no clip {0}")]
    NoSuchClip(u64),
    #[error("there is no playlist track {0}")]
    NoSuchTrack(usize),
    #[error("a playlist may have at most {MAX_PLAYLIST_TRACKS} tracks")]
    TooManyTracks,
    #[error("a playlist must keep at least one track")]
    LastTrack,
    #[error("a playlist may hold at most {MAX_CLIPS} clips")]
    TooManyClips,
    #[error("a clip of zero length would occupy no time and never sound")]
    ZeroLength,
    #[error("a clip may not run past tick {MAX_SONG_TICKS}; this one ends at {0}")]
    PastTheEnd(u64),
    #[error("clip {clip} names pattern {pattern}, which is not in this project")]
    NoSuchPattern { clip: u64, pattern: u64 },
    #[error("a playlist track's name must not be empty")]
    EmptyName,
}

/// The arrangement.
#[derive(Debug, Clone, PartialEq)]
pub struct Playlist {
    tracks: Vec<PlaylistTrack>,
    /// In no particular order: the UI addresses a clip by id, and the scheduler
    /// gets a sorted copy at compile time. Sorting here would renumber a clip
    /// being dragged, for the same reason a pattern's notes are unsorted.
    clips: Vec<Clip>,
    next_track_id: u64,
    next_clip_id: u64,
}

impl Default for Playlist {
    /// Four empty lanes and no clips.
    ///
    /// Four rather than one because an arrangement is a stack -- drums, bass,
    /// keys, one for audio -- and rather than twelve because a screen of empty
    /// rows is a worse first impression than one you add to. Named by number:
    /// the engine does not know what the user is about to write.
    fn default() -> Self {
        let tracks = (1..=4)
            .map(|n| PlaylistTrack::new(n, format!("Track {n}")))
            .collect();
        Self {
            tracks,
            clips: Vec::new(),
            next_track_id: 5,
            next_clip_id: 1,
        }
    }
}

impl Playlist {
    /// An arrangement with no lanes and no clips. Tests, and the empty state a
    /// migration from a file with no playlist starts from.
    pub fn empty() -> Self {
        Self {
            tracks: Vec::new(),
            clips: Vec::new(),
            next_track_id: 1,
            next_clip_id: 1,
        }
    }

    /// One read back from a file. Ids are restored, not regenerated.
    pub fn restored(tracks: Vec<PlaylistTrack>, clips: Vec<Clip>) -> Self {
        let next_track_id = tracks
            .iter()
            .map(|track| track.id)
            .max()
            .map_or(1, |highest| highest.saturating_add(1));
        let next_clip_id = clips
            .iter()
            .map(Clip::id)
            .max()
            .map_or(1, |highest| highest.saturating_add(1));
        Self {
            tracks,
            clips,
            next_track_id,
            next_clip_id,
        }
    }

    /// The lanes, top to bottom.
    pub fn tracks(&self) -> &[PlaylistTrack] {
        &self.tracks
    }

    /// Every clip, in no particular order.
    pub fn clips(&self) -> &[Clip] {
        &self.clips
    }

    /// The clip with `id`.
    pub fn clip(&self, id: u64) -> Option<&Clip> {
        self.clips.iter().find(|clip| clip.id() == id)
    }

    /// One past the last tick any clip occupies. `0` for an empty arrangement.
    ///
    /// Where SONG mode stops. Muted lanes count: muting a lane silences it, it
    /// does not shorten the song, and a playhead that ran off the end because
    /// the last lane was muted would be a surprise every time.
    pub fn end_ticks(&self) -> u64 {
        self.clips.iter().map(Clip::end_ticks).max().unwrap_or(0)
    }

    /// The id the next added clip will get. Diagnostics and tests.
    pub const fn next_clip_id(&self) -> u64 {
        self.next_clip_id
    }

    /// Append a lane.
    ///
    /// # Errors
    /// [`PlaylistError::TooManyTracks`].
    pub fn add_track(&mut self, name: Option<String>) -> Result<u64, PlaylistError> {
        if self.tracks.len() >= MAX_PLAYLIST_TRACKS {
            return Err(PlaylistError::TooManyTracks);
        }
        let id = self.next_track_id;
        self.next_track_id += 1;
        let name = name.unwrap_or_else(|| format!("Track {}", self.tracks.len() + 1));
        self.tracks.push(PlaylistTrack::new(id, name));
        Ok(id)
    }

    /// Remove lane `index`, **and every clip on it**, shifting the lanes below.
    ///
    /// The clips go because a clip's lane is its position, not a reference: a
    /// clip left behind would have to move somewhere, and every choice of
    /// somewhere is a silent rewrite of the arrangement.
    ///
    /// # Errors
    /// [`PlaylistError::NoSuchTrack`], or [`PlaylistError::LastTrack`].
    pub fn remove_track(&mut self, index: usize) -> Result<PlaylistTrack, PlaylistError> {
        if index >= self.tracks.len() {
            return Err(PlaylistError::NoSuchTrack(index));
        }
        if self.tracks.len() == 1 {
            return Err(PlaylistError::LastTrack);
        }
        self.clips.retain(|clip| clip.track() != index);
        for clip in &mut self.clips {
            let track = clip.track();
            if track > index {
                *clip = clip.moved(track - 1, clip.start_ticks());
            }
        }
        Ok(self.tracks.remove(index))
    }

    /// Rename lane `index`.
    ///
    /// # Errors
    /// [`PlaylistError::NoSuchTrack`] or [`PlaylistError::EmptyName`].
    pub fn rename_track(
        &mut self,
        index: usize,
        name: impl Into<String>,
    ) -> Result<(), PlaylistError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(PlaylistError::EmptyName);
        }
        self.track_mut(index)?.name = name;
        Ok(())
    }

    /// Mute or unmute lane `index`.
    ///
    /// # Errors
    /// [`PlaylistError::NoSuchTrack`].
    pub fn set_track_muted(&mut self, index: usize, muted: bool) -> Result<(), PlaylistError> {
        self.track_mut(index)?.muted = muted;
        Ok(())
    }

    /// Solo or un-solo lane `index`. Additive: several lanes may be soloed.
    ///
    /// # Errors
    /// [`PlaylistError::NoSuchTrack`].
    pub fn set_track_soloed(&mut self, index: usize, soloed: bool) -> Result<(), PlaylistError> {
        self.track_mut(index)?.soloed = soloed;
        Ok(())
    }

    /// Whether any lane is soloed.
    pub fn any_soloed(&self) -> bool {
        self.tracks.iter().any(|track| track.soloed)
    }

    /// Whether lane `index` is silent right now, solo included.
    ///
    /// This is the whole of the solo rule, and it is the mixer's: with nothing
    /// soloed a lane is audible unless it is muted; with anything soloed a lane
    /// is audible only if it is itself soloed. A soloed *and* muted lane stays
    /// audible — solo is an override, not a vote. Unknown indices are silent.
    pub fn lane_silenced(&self, index: usize) -> bool {
        let Some(track) = self.tracks.get(index) else {
            return true;
        };
        if self.any_soloed() {
            !track.soloed
        } else {
            track.muted
        }
    }

    fn track_mut(&mut self, index: usize) -> Result<&mut PlaylistTrack, PlaylistError> {
        self.tracks
            .get_mut(index)
            .ok_or(PlaylistError::NoSuchTrack(index))
    }

    /// Place the extra voices of a split under each instance of `original_id`.
    ///
    /// `pattern_ids[0]` is the rewritten original (already on those clips).
    /// Each further id is painted on the next lane down, same start and length.
    /// Missing lanes are appended. A pattern that is not on the playlist is a
    /// no-op: splitting in the picker must not invent arrangement.
    ///
    /// Bounds are checked before any lane or clip is added, so a refusal leaves
    /// the playlist as it was.
    ///
    /// # Errors
    /// [`PlaylistError::TooManyTracks`] or [`PlaylistError::TooManyClips`].
    pub fn spread_split_clips(
        &mut self,
        original_id: u64,
        pattern_ids: &[u64],
    ) -> Result<(), PlaylistError> {
        if pattern_ids.len() < 2 {
            return Ok(());
        }
        let extra = pattern_ids.len() - 1;
        let instances: Vec<(usize, u64, u64)> = self
            .clips
            .iter()
            .filter_map(|clip| match clip {
                Clip::Pattern(placed) if placed.pattern_id == original_id => {
                    Some((placed.track, placed.start_ticks, placed.length_ticks))
                }
                _ => None,
            })
            .collect();
        if instances.is_empty() {
            return Ok(());
        }
        let needed = instances
            .iter()
            .map(|(track, _, _)| track + extra)
            .max()
            .unwrap_or(0);
        if needed >= MAX_PLAYLIST_TRACKS {
            return Err(PlaylistError::TooManyTracks);
        }
        let adding = instances.len() * extra;
        if self.clips.len() + adding > MAX_CLIPS {
            return Err(PlaylistError::TooManyClips);
        }
        while self.tracks.len() <= needed {
            self.add_track(None)?;
        }
        for (track, start_ticks, length_ticks) in instances {
            for (offset, &pattern_id) in pattern_ids.iter().enumerate().skip(1) {
                self.add_pattern_clip(pattern_id, track + offset, start_ticks, length_ticks)?;
            }
        }
        Ok(())
    }

    /// Place a pattern on the timeline, returning the new clip's id.
    ///
    /// # Errors
    /// [`PlaylistError::NoSuchTrack`], [`PlaylistError::TooManyClips`],
    /// [`PlaylistError::ZeroLength`] or [`PlaylistError::PastTheEnd`].
    pub fn add_pattern_clip(
        &mut self,
        pattern_id: u64,
        track: usize,
        start_ticks: u64,
        length_ticks: u64,
    ) -> Result<u64, PlaylistError> {
        let id = self.reserve(track, start_ticks, length_ticks)?;
        self.clips.push(Clip::Pattern(PatternClip {
            id,
            pattern_id,
            track,
            start_ticks,
            length_ticks,
        }));
        Ok(id)
    }

    /// Place an audio file on the timeline, returning the new clip's id.
    ///
    /// # Errors
    /// As [`add_pattern_clip`](Self::add_pattern_clip).
    pub fn add_audio_clip(
        &mut self,
        source_id: u64,
        track: usize,
        start_ticks: u64,
        length_ticks: u64,
        offset_frames: u64,
        gain_db: f32,
    ) -> Result<u64, PlaylistError> {
        let id = self.reserve(track, start_ticks, length_ticks)?;
        self.clips.push(Clip::Audio(AudioClip {
            id,
            source_id,
            track,
            start_ticks,
            offset_frames,
            length_ticks,
            gain_db: if gain_db.is_finite() { gain_db } else { 0.0 },
        }));
        Ok(id)
    }

    /// The bounds every placement shares, plus the id it gets.
    fn reserve(
        &mut self,
        track: usize,
        start_ticks: u64,
        length_ticks: u64,
    ) -> Result<u64, PlaylistError> {
        if track >= self.tracks.len() {
            return Err(PlaylistError::NoSuchTrack(track));
        }
        if self.clips.len() >= MAX_CLIPS {
            return Err(PlaylistError::TooManyClips);
        }
        if length_ticks == 0 {
            return Err(PlaylistError::ZeroLength);
        }
        let end = start_ticks.saturating_add(length_ticks);
        if end > MAX_SONG_TICKS {
            return Err(PlaylistError::PastTheEnd(end));
        }
        let id = self.next_clip_id;
        self.next_clip_id += 1;
        Ok(id)
    }

    /// Remove a clip.
    ///
    /// # Errors
    /// [`PlaylistError::NoSuchClip`].
    pub fn remove_clip(&mut self, id: u64) -> Result<Clip, PlaylistError> {
        let index = self
            .clips
            .iter()
            .position(|clip| clip.id() == id)
            .ok_or(PlaylistError::NoSuchClip(id))?;
        Ok(self.clips.remove(index))
    }

    /// Drop every audio clip of `source_id`. Pattern clips are left alone.
    ///
    /// What happens when the Audio Clip channel for that file is deleted: the
    /// arrangement must not keep playing a file whose channel is gone.
    pub fn remove_clips_of_source(&mut self, source_id: u64) -> usize {
        let before = self.clips.len();
        self.clips
            .retain(|clip| clip.audio_source_id() != Some(source_id));
        before - self.clips.len()
    }

    /// Move a clip to another lane and start tick.
    ///
    /// # Errors
    /// [`PlaylistError::NoSuchClip`], [`PlaylistError::NoSuchTrack`] or
    /// [`PlaylistError::PastTheEnd`].
    pub fn move_clip(
        &mut self,
        id: u64,
        track: usize,
        start_ticks: u64,
    ) -> Result<(), PlaylistError> {
        if track >= self.tracks.len() {
            return Err(PlaylistError::NoSuchTrack(track));
        }
        let index = self
            .clips
            .iter()
            .position(|clip| clip.id() == id)
            .ok_or(PlaylistError::NoSuchClip(id))?;
        let end = start_ticks.saturating_add(self.clips[index].length_ticks());
        if end > MAX_SONG_TICKS {
            return Err(PlaylistError::PastTheEnd(end));
        }
        self.clips[index] = self.clips[index].moved(track, start_ticks);
        Ok(())
    }

    /// Change a clip's length.
    ///
    /// # Errors
    /// [`PlaylistError::NoSuchClip`], [`PlaylistError::ZeroLength`] or
    /// [`PlaylistError::PastTheEnd`].
    pub fn resize_clip(&mut self, id: u64, length_ticks: u64) -> Result<(), PlaylistError> {
        if length_ticks == 0 {
            return Err(PlaylistError::ZeroLength);
        }
        let index = self
            .clips
            .iter()
            .position(|clip| clip.id() == id)
            .ok_or(PlaylistError::NoSuchClip(id))?;
        let end = self.clips[index].start_ticks().saturating_add(length_ticks);
        if end > MAX_SONG_TICKS {
            return Err(PlaylistError::PastTheEnd(end));
        }
        self.clips[index] = self.clips[index].resized(length_ticks);
        Ok(())
    }

    /// Set an audio clip's per-clip gain, in dB.
    ///
    /// # Errors
    /// [`PlaylistError::NoSuchClip`] -- including when the clip exists but is a
    /// pattern clip, which has no gain of its own (its channels have faders).
    pub fn set_clip_gain(&mut self, id: u64, gain_db: f32) -> Result<(), PlaylistError> {
        let clip = self
            .clips
            .iter_mut()
            .find(|clip| clip.id() == id)
            .ok_or(PlaylistError::NoSuchClip(id))?;
        match clip {
            Clip::Audio(audio) => {
                audio.gain_db = if gain_db.is_finite() { gain_db } else { 0.0 };
                Ok(())
            }
            Clip::Pattern(_) => Err(PlaylistError::NoSuchClip(id)),
        }
    }

    /// Set how far into its file an audio clip begins, in the file's own frames.
    ///
    /// Frames rather than ticks, for the reason
    /// [`AudioClip::offset_frames`] gives: this is a position in a recording,
    /// not in the song, and it must not move when the tempo does. An offset past
    /// the end of the file is *allowed* and simply silent -- the registry is the
    /// only thing that knows how long the file is, and a playlist that had to
    /// ask it would be a playlist that cannot be edited without one.
    ///
    /// # Errors
    /// [`PlaylistError::NoSuchClip`] -- including when the clip exists but is a
    /// pattern clip, which has no file to be offset into.
    pub fn set_clip_offset(&mut self, id: u64, offset_frames: u64) -> Result<(), PlaylistError> {
        let clip = self
            .clips
            .iter_mut()
            .find(|clip| clip.id() == id)
            .ok_or(PlaylistError::NoSuchClip(id))?;
        match clip {
            Clip::Audio(audio) => {
                audio.offset_frames = offset_frames;
                Ok(())
            }
            Clip::Pattern(_) => Err(PlaylistError::NoSuchClip(id)),
        }
    }

    /// Remove every clip that plays pattern `pattern_id`.
    ///
    /// Called when a pattern is deleted. The alternative -- leaving the clips
    /// pointing at nothing -- would be a silent gap in the arrangement that only
    /// shows up on playback.
    pub fn drop_pattern(&mut self, pattern_id: u64) -> usize {
        let before = self.clips.len();
        self.clips
            .retain(|clip| !matches!(clip, Clip::Pattern(p) if p.pattern_id == pattern_id));
        before - self.clips.len()
    }

    /// Point every clip that plays `from` at `to` instead. Returns how many moved.
    ///
    /// `lane` narrows it to one playlist lane, which is what a **destructive
    /// sample edit** wants: reversing the file on one channel produces a new
    /// source and must carry that channel's clips across, while leaving alone
    /// any other lane that legitimately still plays the original take.
    /// `None` retargets every clip, which is what a project-load fixup wants.
    ///
    /// Offsets are untouched: they are positions in a recording, and every
    /// operation that keeps the file's length (reverse, normalize, gain) keeps
    /// them meaningful. A crop moves them, so the caller that crops passes the
    /// shift itself -- see [`shift_audio_offsets`](Self::shift_audio_offsets).
    pub fn retarget_audio_source(&mut self, from: u64, to: u64, lane: Option<usize>) -> usize {
        if from == to {
            return 0;
        }
        let mut moved = 0;
        for clip in &mut self.clips {
            if let Clip::Audio(audio) = clip {
                if audio.source_id == from && lane.is_none_or(|index| audio.track == index) {
                    audio.source_id = to;
                    moved += 1;
                }
            }
        }
        moved
    }

    /// Move every audio clip of `source_id` on `lane` by `delta` frames inside
    /// its file. Returns how many moved.
    ///
    /// The other half of a destructive **Trim**: the file lost its first
    /// `start` frames, so a clip that began a bar into the take has to begin a
    /// bar *minus the crop* into the new one, or the arrangement would jump.
    /// Undoing the trim passes the same number the other way and lands exactly
    /// where it started.
    ///
    /// Saturating at zero: a clip whose offset was inside the cropped-away head
    /// now starts at the beginning, which is the nearest audio that still
    /// exists.
    pub fn shift_audio_offsets(&mut self, source_id: u64, lane: usize, delta: i64) -> usize {
        if delta == 0 {
            return 0;
        }
        let magnitude = delta.unsigned_abs();
        let mut moved = 0;
        for clip in &mut self.clips {
            if let Clip::Audio(audio) = clip {
                if audio.source_id == source_id && audio.track == lane {
                    audio.offset_frames = if delta < 0 {
                        audio.offset_frames.saturating_sub(magnitude)
                    } else {
                        audio.offset_frames.saturating_add(magnitude)
                    };
                    moved += 1;
                }
            }
        }
        moved
    }

    /// Remove every clip that plays audio source `source_id`.
    pub fn drop_audio_source(&mut self, source_id: u64) -> usize {
        let before = self.clips.len();
        self.clips
            .retain(|clip| !matches!(clip, Clip::Audio(a) if a.source_id == source_id));
        before - self.clips.len()
    }

    /// Check every clip against `patterns` and the engine's bounds.
    ///
    /// # Errors
    /// The first [`PlaylistError`] found.
    pub fn validate(&self, patterns: &PatternSet) -> Result<(), PlaylistError> {
        if self.tracks.len() > MAX_PLAYLIST_TRACKS {
            return Err(PlaylistError::TooManyTracks);
        }
        if self.clips.len() > MAX_CLIPS {
            return Err(PlaylistError::TooManyClips);
        }
        for track in &self.tracks {
            if track.name.trim().is_empty() {
                return Err(PlaylistError::EmptyName);
            }
        }
        for clip in &self.clips {
            if clip.track() >= self.tracks.len() {
                return Err(PlaylistError::NoSuchTrack(clip.track()));
            }
            if clip.length_ticks() == 0 {
                return Err(PlaylistError::ZeroLength);
            }
            if clip.end_ticks() > MAX_SONG_TICKS {
                return Err(PlaylistError::PastTheEnd(clip.end_ticks()));
            }
            if let Clip::Pattern(pattern) = clip {
                if patterns.get(pattern.pattern_id).is_none() {
                    return Err(PlaylistError::NoSuchPattern {
                        clip: pattern.id,
                        pattern: pattern.pattern_id,
                    });
                }
            }
        }
        Ok(())
    }

    /// Resolve the pattern clips into the shape the scheduler walks.
    ///
    /// **Non-RT.** Silenced lanes (mute, or another lane's solo) and clips
    /// naming a pattern that is not in the set are dropped here, so the
    /// callback never has to ask either question. Audio clips are not in the
    /// result: they are rendered rather than scheduled, and go through
    /// [`crate::audio`].
    pub fn compile_pattern_clips(&self, patterns: &PatternSet) -> Vec<crate::CompiledPatternClip> {
        self.clips
            .iter()
            .filter_map(|clip| match clip {
                Clip::Pattern(pattern) => {
                    if self.lane_silenced(pattern.track) {
                        return None;
                    }
                    Some(crate::CompiledPatternClip {
                        pattern: patterns.index_of(pattern.pattern_id)?,
                        start_ticks: pattern.start_ticks,
                        length_ticks: pattern.length_ticks,
                    })
                }
                Clip::Audio(_) => None,
            })
            .collect()
    }

    /// The audio clips, with silenced lanes dropped. **Non-RT.**
    pub fn audible_audio_clips(&self) -> Vec<AudioClip> {
        self.clips
            .iter()
            .filter_map(|clip| match clip {
                Clip::Audio(audio) => {
                    if self.lane_silenced(audio.track) {
                        return None;
                    }
                    Some(*audio)
                }
                Clip::Pattern(_) => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern::TICKS_PER_BAR;

    const BAR: u64 = TICKS_PER_BAR as u64;

    fn patterns() -> PatternSet {
        let mut set = PatternSet::default();
        set.add(Some("B".to_owned()), &[], |_| 36).unwrap();
        set
    }

    #[test]
    fn a_fresh_playlist_is_four_empty_lanes() {
        let playlist = Playlist::default();
        assert_eq!(playlist.tracks().len(), 4);
        assert_eq!(
            playlist
                .tracks()
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Track 1", "Track 2", "Track 3", "Track 4"]
        );
        assert!(playlist.tracks().iter().all(|t| !t.muted && !t.soloed));
        assert!(playlist.clips().is_empty());
        assert_eq!(playlist.end_ticks(), 0, "an empty song ends at zero");
        playlist.validate(&patterns()).unwrap();
    }

    #[test]
    fn a_pattern_clip_carries_where_it_is_and_how_long_it_plays() {
        let mut playlist = Playlist::default();
        let id = playlist
            .add_pattern_clip(1, 0, 0, DEFAULT_CLIP_TICKS)
            .unwrap();
        assert_eq!(id, 1);
        assert_eq!(playlist.next_clip_id(), 2);

        let clip = playlist.clip(id).expect("just added");
        assert_eq!(clip.kind(), "pattern");
        assert_eq!(clip.track(), 0);
        assert_eq!(clip.start_ticks(), 0);
        assert_eq!(clip.length_ticks(), 4 * BAR);
        assert_eq!(clip.end_ticks(), 4 * BAR);
        assert_eq!(playlist.end_ticks(), 4 * BAR);
        playlist.validate(&patterns()).unwrap();
    }

    #[test]
    fn an_audio_clip_measures_its_offset_in_frames_and_its_length_in_ticks() {
        // Frames because the offset is a position in a recording: trimming the
        // head of a vocal and then changing the tempo must not start it on a
        // different syllable.
        let mut playlist = Playlist::default();
        let id = playlist
            .add_audio_clip(7, 2, 8 * BAR, 4 * BAR, 12_000, -3.0)
            .unwrap();
        let Some(Clip::Audio(clip)) = playlist.clip(id) else {
            panic!("an audio clip");
        };
        assert_eq!(clip.source_id, 7);
        assert_eq!(clip.offset_frames, 12_000);
        assert_eq!(clip.gain_db, -3.0);
        assert_eq!(clip.start_ticks, 8 * BAR);
        assert_eq!(playlist.clip(id).unwrap().kind(), "audio");
    }

    #[test]
    fn a_gain_the_ui_should_never_have_sent_becomes_unity_rather_than_silence() {
        let mut playlist = Playlist::default();
        let id = playlist.add_audio_clip(1, 0, 0, BAR, 0, f32::NAN).unwrap();
        let Some(Clip::Audio(clip)) = playlist.clip(id) else {
            panic!()
        };
        assert_eq!(clip.gain_db, 0.0);

        playlist.set_clip_gain(id, f32::INFINITY).unwrap();
        let Some(Clip::Audio(clip)) = playlist.clip(id) else {
            panic!()
        };
        assert_eq!(clip.gain_db, 0.0);
        playlist.set_clip_gain(id, -6.0).unwrap();
        assert!(matches!(playlist.clip(id), Some(Clip::Audio(c)) if c.gain_db == -6.0));
    }

    #[test]
    fn a_pattern_clip_has_no_gain_of_its_own_because_its_channels_have_faders() {
        let mut playlist = Playlist::default();
        let id = playlist.add_pattern_clip(1, 0, 0, BAR).unwrap();
        assert_eq!(
            playlist.set_clip_gain(id, -6.0),
            Err(PlaylistError::NoSuchClip(id))
        );
        assert_eq!(
            playlist.set_clip_offset(id, 1_000),
            Err(PlaylistError::NoSuchClip(id)),
            "and no file to be offset into"
        );
    }

    #[test]
    fn an_offset_is_in_the_files_own_frames_and_survives_a_move_and_a_resize() {
        // The property the whole field exists for: trimming the head of a take
        // and then dragging it, or stretching it, must not start it on a
        // different syllable.
        let mut playlist = Playlist::default();
        let id = playlist.add_audio_clip(1, 0, 0, BAR, 22_050, 0.0).unwrap();
        playlist.set_clip_offset(id, 44_100).unwrap();
        playlist.move_clip(id, 1, 8 * BAR).unwrap();
        playlist.resize_clip(id, 2 * BAR).unwrap();

        let Some(Clip::Audio(clip)) = playlist.clip(id) else {
            panic!("still an audio clip")
        };
        assert_eq!(clip.offset_frames, 44_100);
        assert_eq!(clip.start_ticks, 8 * BAR);
        assert_eq!(clip.length_ticks, 2 * BAR);
    }

    #[test]
    fn an_offset_past_the_end_of_the_file_is_allowed_and_simply_silent() {
        // The playlist does not know how long the file is -- the registry does
        // -- and a playlist that had to ask one would be a playlist that cannot
        // be edited without a decoded file behind it. The compile drops the clip.
        let mut playlist = Playlist::default();
        let id = playlist.add_audio_clip(1, 0, 0, BAR, 0, 0.0).unwrap();
        playlist.set_clip_offset(id, u64::MAX).unwrap();
        assert!(playlist.validate(&patterns()).is_ok());
    }

    #[test]
    fn removing_a_file_channel_drops_every_clip_of_that_file() {
        let mut playlist = Playlist::default();
        playlist.add_audio_clip(1, 0, 0, BAR, 0, 0.0).unwrap();
        playlist.add_audio_clip(1, 1, BAR, BAR, 0, 0.0).unwrap();
        playlist.add_audio_clip(2, 0, 2 * BAR, BAR, 0, 0.0).unwrap();
        playlist.add_pattern_clip(1, 0, 3 * BAR, BAR).unwrap();
        assert_eq!(playlist.remove_clips_of_source(1), 2);
        assert_eq!(playlist.clips().len(), 2);
        assert_eq!(playlist.clips()[0].audio_source_id(), Some(2));
        assert_eq!(playlist.clips()[1].kind(), "pattern");
        assert_eq!(playlist.remove_clips_of_source(1), 0);
    }

    #[test]
    fn placements_the_engine_could_not_play_are_refused_by_name() {
        let mut playlist = Playlist::default();
        assert_eq!(
            playlist.add_pattern_clip(1, 9, 0, BAR),
            Err(PlaylistError::NoSuchTrack(9))
        );
        assert_eq!(
            playlist.add_pattern_clip(1, 0, 0, 0),
            Err(PlaylistError::ZeroLength)
        );
        assert_eq!(
            playlist.add_pattern_clip(1, 0, MAX_SONG_TICKS, BAR),
            Err(PlaylistError::PastTheEnd(MAX_SONG_TICKS + BAR))
        );
        // Exactly to the end is fine.
        playlist
            .add_pattern_clip(1, 0, MAX_SONG_TICKS - BAR, BAR)
            .unwrap();
    }

    #[test]
    fn the_clip_count_is_capped_and_says_so() {
        let mut playlist = Playlist::default();
        for index in 0..MAX_CLIPS {
            playlist
                .add_pattern_clip(1, 0, index as u64 * BAR, BAR)
                .unwrap();
        }
        assert_eq!(
            playlist.add_pattern_clip(1, 0, 0, BAR),
            Err(PlaylistError::TooManyClips)
        );
    }

    #[test]
    fn moving_and_resizing_check_the_same_bounds_a_placement_does() {
        let mut playlist = Playlist::default();
        let id = playlist.add_pattern_clip(1, 0, 0, 4 * BAR).unwrap();

        playlist.move_clip(id, 2, 8 * BAR).unwrap();
        assert_eq!(playlist.clip(id).unwrap().track(), 2);
        assert_eq!(playlist.clip(id).unwrap().start_ticks(), 8 * BAR);

        playlist.resize_clip(id, BAR).unwrap();
        assert_eq!(playlist.clip(id).unwrap().length_ticks(), BAR);

        assert_eq!(
            playlist.move_clip(id, 9, 0),
            Err(PlaylistError::NoSuchTrack(9))
        );
        assert_eq!(
            playlist.move_clip(99, 0, 0),
            Err(PlaylistError::NoSuchClip(99))
        );
        assert_eq!(playlist.resize_clip(id, 0), Err(PlaylistError::ZeroLength));
        assert!(matches!(
            playlist.move_clip(id, 0, MAX_SONG_TICKS),
            Err(PlaylistError::PastTheEnd(_))
        ));
        // Nothing changed under a refused edit.
        assert_eq!(playlist.clip(id).unwrap().start_ticks(), 8 * BAR);
    }

    #[test]
    fn removing_a_lane_takes_its_clips_and_shifts_the_lanes_below_it() {
        // A clip's lane is its position, not a reference: a clip left behind
        // would have to move somewhere, and every somewhere is a silent rewrite.
        let mut playlist = Playlist::default();
        let on_one = playlist.add_pattern_clip(1, 1, 0, BAR).unwrap();
        let on_two = playlist.add_pattern_clip(1, 2, 0, BAR).unwrap();

        playlist.remove_track(1).unwrap();
        assert_eq!(playlist.tracks().len(), 3);
        assert!(playlist.clip(on_one).is_none(), "its lane went");
        assert_eq!(
            playlist.clip(on_two).unwrap().track(),
            1,
            "the lane below moved up, and its clip came with it"
        );
    }

    #[test]
    fn the_last_lane_cannot_be_removed_and_an_unknown_one_is_named() {
        let mut playlist = Playlist::default();
        while playlist.tracks().len() > 1 {
            playlist.remove_track(0).unwrap();
        }
        assert_eq!(playlist.remove_track(0), Err(PlaylistError::LastTrack));
        assert_eq!(playlist.remove_track(4), Err(PlaylistError::NoSuchTrack(4)));
    }

    #[test]
    fn lanes_are_added_up_to_the_limit_renamed_and_muted() {
        let mut playlist = Playlist::default();
        let id = playlist.add_track(Some("Vocals".to_owned())).unwrap();
        assert_eq!(playlist.tracks()[4].name, "Vocals");
        assert_eq!(id, 5);

        playlist.rename_track(0, "Drums").unwrap();
        assert_eq!(playlist.tracks()[0].name, "Drums");
        assert_eq!(playlist.rename_track(0, " "), Err(PlaylistError::EmptyName));
        assert_eq!(
            playlist.rename_track(9, "x"),
            Err(PlaylistError::NoSuchTrack(9))
        );

        playlist.set_track_muted(0, true).unwrap();
        assert!(playlist.tracks()[0].muted);
        assert_eq!(
            playlist.set_track_muted(9, true),
            Err(PlaylistError::NoSuchTrack(9))
        );

        while playlist.tracks().len() < MAX_PLAYLIST_TRACKS {
            playlist.add_track(None).unwrap();
        }
        assert_eq!(playlist.add_track(None), Err(PlaylistError::TooManyTracks));
    }

    #[test]
    fn deleting_a_pattern_takes_the_clips_that_played_it() {
        let mut playlist = Playlist::default();
        playlist.add_pattern_clip(1, 0, 0, BAR).unwrap();
        playlist.add_pattern_clip(1, 0, BAR, BAR).unwrap();
        let other = playlist.add_pattern_clip(2, 1, 0, BAR).unwrap();
        playlist.add_audio_clip(1, 2, 0, BAR, 0, 0.0).unwrap();

        assert_eq!(playlist.drop_pattern(1), 2);
        assert_eq!(playlist.clips().len(), 2);
        assert!(playlist.clip(other).is_some());
        assert_eq!(playlist.drop_pattern(99), 0);

        assert_eq!(playlist.drop_audio_source(1), 1);
        assert_eq!(playlist.clips().len(), 1);
    }

    #[test]
    fn a_clip_naming_a_pattern_that_is_gone_fails_validation_by_name() {
        // Reachable from a project file whose playlist outlived a pattern.
        let mut playlist = Playlist::default();
        let clip = playlist.add_pattern_clip(99, 0, 0, BAR).unwrap();
        assert_eq!(
            playlist.validate(&patterns()),
            Err(PlaylistError::NoSuchPattern { clip, pattern: 99 })
        );
    }

    #[test]
    fn compiling_resolves_pattern_ids_to_indices_and_drops_muted_lanes() {
        let set = patterns();
        let second = set.patterns()[1].id;
        let mut playlist = Playlist::default();
        playlist.add_pattern_clip(1, 0, 0, 4 * BAR).unwrap();
        playlist
            .add_pattern_clip(second, 1, 4 * BAR, 4 * BAR)
            .unwrap();
        playlist.add_audio_clip(1, 2, 0, BAR, 0, 0.0).unwrap();

        let clips = playlist.compile_pattern_clips(&set);
        assert_eq!(clips.len(), 2, "the audio clip is rendered, not scheduled");
        assert_eq!(clips[0].pattern, 0);
        assert_eq!(clips[1].pattern, 1);
        assert_eq!(clips[1].start_ticks, 4 * BAR);
        assert_eq!(playlist.audible_audio_clips().len(), 1);

        // Muting a lane removes its clips from the compile, so the callback
        // never has to ask.
        playlist.set_track_muted(1, true).unwrap();
        playlist.set_track_muted(2, true).unwrap();
        assert_eq!(playlist.compile_pattern_clips(&set).len(), 1);
        assert!(playlist.audible_audio_clips().is_empty());
        assert_eq!(
            playlist.end_ticks(),
            8 * BAR,
            "muting silences a lane; it does not shorten the song"
        );
    }

    #[test]
    fn compiling_keeps_only_soloed_lanes_when_any_lane_is_soloed() {
        let set = patterns();
        let second = set.patterns()[1].id;
        let mut playlist = Playlist::default();
        playlist.add_pattern_clip(1, 0, 0, 4 * BAR).unwrap();
        playlist
            .add_pattern_clip(second, 1, 4 * BAR, 4 * BAR)
            .unwrap();
        playlist.add_audio_clip(1, 2, 0, BAR, 0, 0.0).unwrap();

        playlist.set_track_soloed(1, true).unwrap();
        assert_eq!(playlist.compile_pattern_clips(&set).len(), 1);
        assert_eq!(playlist.compile_pattern_clips(&set)[0].pattern, 1);
        assert!(playlist.audible_audio_clips().is_empty());
        assert_eq!(
            playlist.end_ticks(),
            8 * BAR,
            "soloing silences a lane; it does not shorten the song"
        );

        playlist.set_track_soloed(2, true).unwrap();
        assert_eq!(playlist.compile_pattern_clips(&set).len(), 1);
        assert_eq!(playlist.audible_audio_clips().len(), 1);

        playlist.set_track_muted(1, true).unwrap();
        assert_eq!(
            playlist.compile_pattern_clips(&set).len(),
            1,
            "a soloed muted lane stays audible, the way mixer solo does"
        );
    }

    #[test]
    fn compiling_drops_a_clip_whose_pattern_is_not_in_the_set() {
        let mut playlist = Playlist::default();
        playlist.add_pattern_clip(99, 0, 0, BAR).unwrap();
        assert!(playlist.compile_pattern_clips(&patterns()).is_empty());
    }

    #[test]
    fn a_restored_playlist_keeps_its_ids_and_continues_past_the_highest() {
        let playlist = Playlist::restored(
            vec![PlaylistTrack::new(9, "Nine")],
            vec![Clip::Pattern(PatternClip {
                id: 12,
                pattern_id: 1,
                track: 0,
                start_ticks: 0,
                length_ticks: BAR,
            })],
        );
        assert_eq!(playlist.next_clip_id(), 13);
        assert_eq!(playlist.tracks()[0].id, 9);
        assert_eq!(Playlist::empty().next_clip_id(), 1);
    }

    #[test]
    fn the_bounds_are_the_documented_ones() {
        assert_eq!(MAX_PLAYLIST_TRACKS, 32);
        assert_eq!(MAX_CLIPS, 512);
        assert_eq!(TICKS_PER_BAR, 3_840);
        assert_eq!(MAX_SONG_TICKS, 1_024 * 3_840);
        assert_eq!(DEFAULT_CLIP_TICKS, 4 * 3_840);
    }

    #[test]
    fn spreading_a_split_places_each_voice_on_consecutive_lanes_and_adds_lanes() {
        let mut playlist = Playlist::empty();
        playlist.add_track(Some("A".into())).unwrap();
        playlist.add_pattern_clip(1, 0, 0, BAR).unwrap();
        playlist.add_pattern_clip(1, 0, 4 * BAR, BAR).unwrap();

        playlist.spread_split_clips(1, &[1, 2, 3]).unwrap();
        assert_eq!(
            playlist.tracks().len(),
            3,
            "two extra voices need two extra lanes"
        );
        let clips: Vec<(u64, usize, u64)> = playlist
            .clips()
            .iter()
            .filter_map(|clip| match clip {
                Clip::Pattern(p) => Some((p.pattern_id, p.track, p.start_ticks)),
                Clip::Audio(_) => None,
            })
            .collect();
        assert_eq!(
            clips,
            vec![
                (1, 0, 0),
                (1, 0, 4 * BAR),
                (2, 1, 0),
                (3, 2, 0),
                (2, 1, 4 * BAR),
                (3, 2, 4 * BAR),
            ]
        );
    }

    #[test]
    fn spreading_a_split_does_not_add_lanes_that_already_exist() {
        let mut playlist = Playlist::default();
        playlist.add_pattern_clip(1, 0, 0, BAR).unwrap();
        let before = playlist.tracks().len();
        playlist.spread_split_clips(1, &[1, 2]).unwrap();
        assert_eq!(playlist.tracks().len(), before);
        assert_eq!(playlist.clips().len(), 2);
        assert_eq!(playlist.clips()[1].track(), 1);
    }
}
