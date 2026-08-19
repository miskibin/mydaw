//! Audio clips on the audio thread: the compiled arrangement, the voice pool,
//! and the node that sums them into the master bus.
//!
//! ```text
//!   non-RT                                        audio thread
//!   ------                                        ------------
//!   Playlist + AudioSourceRegistry
//!        |  compile_audio_clips
//!        v
//!   CompiledAudioSong { clips: [{Arc<AudioSource>, start, end, offset, gain}] }
//!        |
//!        +-- rtrb push (basedrop::Owned) --> AudioClipNode::process
//!                                                 |
//!            transport.sample_pos ----------------+-- start/stop voices
//!                                                 |
//!            32 voices: cubic-interpolate, 2 ms ramp, += into ctx.buffers
//! ```
//!
//! # Where this sits in the graph
//! An audio clip goes to the **master bus**, summed after every track strip and
//! before the master insert and the master fader (§6 of the playlist spec). That
//! position is not a new concept in the graph: a master insert already runs on
//! the summed bus before the fader, so this node is compiled in as the **first**
//! master insert and *adds* to what the tracks produced instead of replacing it.
//! A playlist lane is not a mixer strip -- it has no fader, pan or output -- so
//! there is no strip to send a clip to, and sending it to strip *n* because the
//! lane happens to be *n*-th would invent a relationship the model does not have.
//!
//! # The tempo does not stretch audio
//! A clip's *position* is in ticks and moves with the tempo; its *content* plays
//! at one source frame per `sample_rate / device_rate` output frames and does
//! not. Changing from 120 to 140 BPM moves the vocal earlier and leaves the
//! singer's pitch alone. That is the honest behaviour and it is the one FL, Live
//! and Logic all default to for an un-warped clip; **there is no time-stretch
//! here**, and adding one is a separate step with its own algorithm (and its own
//! artefacts) rather than a flag on this node.
//!
//! A consequence worth stating: a clip whose `length_ticks` outlasts its file
//! ends when the file does, and a tempo *increase* can make a clip's tick length
//! shorter than the audio it was drawn around, which cuts the tail. Both are
//! resolved the same way -- [`ClipVoice`] stops at whichever of the two ends
//! first.
//!
//! # Sample-accurate, from the absolute tick
//! A clip's first frame is `tick_boundary(start_ticks, step_len)` -- character
//! for character the arithmetic the note scheduler uses, so an audio clip on bar
//! three and a kick on bar three land on the *same sample*, not within a sample
//! of each other. Nothing accumulates: the boundary is recomputed from the
//! absolute tick every block, which is why a tempo change moves a running clip's
//! end and why a seek needs no cooperation.
//!
//! # Seeking into the middle of a clip
//! On a seek (or a Play that starts inside a clip), every clip covering the new
//! position starts a voice at the source frame that position corresponds to:
//!
//! ```text
//!   position = offset_frames + (sample_pos - clip_start_sample) * step
//! ```
//!
//! so dropping the playhead halfway through a vocal continues the vocal, rather
//! than restarting it or leaving silence until the next clip.
//!
//! # No clicks
//! Every voice fades in over [`CLIP_RAMP_MS`] and out over the same, and a stop,
//! a seek or a steal starts the fade rather than cutting. Two milliseconds is
//! short enough to be inaudible as a fade and long enough that the step it
//! replaces is not a click -- the same reasoning as the mixer's 10 ms parameter
//! ramp, at the shorter end because a clip boundary is a musical edit point and
//! ten milliseconds of it would be audible as a soft attack.
//!
//! # RT safety
//! A fixed [`AUDIO_CLIP_VOICES`]-long voice array, an `rtrb` pop, `Arc` reads and
//! arithmetic. No allocation, no locks, no syscalls. A superseded arrangement is
//! dropped on the audio thread, which under `basedrop` means *queued for the
//! collector* -- so the hundred megabytes an `Arc<AudioSource>` may be the last
//! reference to are never freed in a callback. Measured with a counting
//! allocator in `tests/rt_alloc.rs`, not asserted by eye.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use mydaw_audio_file::{cubic, taps};
use mydaw_rt_core::basedrop::{Handle, Owned};
use mydaw_rt_core::{db_to_gain, rt_channel, PlayMode, ProcessCtx, RtNode, CHANNELS};
use rtrb::{Consumer, Producer};

use crate::audio::AudioSource;
use crate::playlist::Playlist;
use crate::schedule::{step_len_samples, tick_boundary};

/// Audio clips that may sound at once.
///
/// Thirty-two, the same number the sampler gives one instrument. A clip voice is
/// four multiplies and two array reads per frame -- cheaper than a synth voice --
/// so the number that matters is musical: thirty-two simultaneous takes is a
/// dense arrangement, and the arrangements this step is for are a handful of
/// loops and a vocal.
pub const AUDIO_CLIP_VOICES: usize = 32;

/// Fade applied at every start, stop, steal and seek, in milliseconds.
///
/// See the module docs. Two milliseconds is 88 frames at 44.1 kHz.
pub const CLIP_RAMP_MS: f32 = 2.0;

/// In-flight arrangement swaps. Sized like the note song's queue and for the
/// same reason: a burst of edits inside one 4096-frame block is a real thing a
/// hand can produce, and the slot keeps only the last of a burst.
pub const AUDIO_CLIP_QUEUE_CAPACITY: usize = 64;

/// One audio clip, resolved for the audio thread.
///
/// Positions are still **ticks**, not samples: the sample position depends on
/// the tempo, and the tempo can change between the compile and the block that
/// plays it. Ticks are what the arrangement means; samples are what this block
/// happens to make of them.
///
/// `Clone` is an `Arc` bump per clip and nothing else, which is what lets the
/// shell keep the last published arrangement and re-install it after a graph
/// rebuild without recompiling it.
#[derive(Clone)]
pub struct CompiledAudioClip {
    /// The playlist clip's own id, so a voice can be rebound across an edit
    /// rather than restarted. See [`AudioClipNode::rebind`].
    pub clip_id: u64,
    /// The file. An `Arc`, so forty clips of one vocal carry forty pointers.
    pub source: Arc<AudioSource>,
    pub start_ticks: u64,
    pub length_ticks: u64,
    /// Where in the file playback begins, in the file's own frames.
    pub offset_frames: u64,
    /// Exclusive end in the file's own frames. `0` means the file's last frame.
    pub end_frames: u64,
    /// Playback-rate multiplier on top of the file/device ratio. `1.0` is
    /// recorded pitch; `2.0` is an octave up. Channel pitch is baked in here
    /// so the voice does one multiply, not a policy.
    pub pitch_ratio: f64,
    /// Linear, already converted from the clip's dB.
    pub gain: f32,
}

impl CompiledAudioClip {
    /// One tick past the last one this clip occupies.
    pub const fn end_ticks(&self) -> u64 {
        self.start_ticks + self.length_ticks
    }

    /// Whether this clip would produce no sound at all.
    pub fn is_silent(&self) -> bool {
        let end = self.playable_end();
        self.source.frames() == 0
            || self.gain == 0.0
            || self.offset_frames >= end
            || self.pitch_ratio <= 0.0
    }

    /// Exclusive end frame in the file, after the channel region is applied.
    pub fn playable_end(&self) -> u64 {
        let frames = self.source.frames() as u64;
        if self.end_frames == 0 || self.end_frames > frames {
            frames
        } else {
            self.end_frames
        }
    }
}

impl std::fmt::Debug for CompiledAudioClip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledAudioClip")
            .field("clip_id", &self.clip_id)
            .field("source", &self.source.id())
            .field("start_ticks", &self.start_ticks)
            .field("length_ticks", &self.length_ticks)
            .field("offset_frames", &self.offset_frames)
            .field("end_frames", &self.end_frames)
            .field("pitch_ratio", &self.pitch_ratio)
            .field("gain", &self.gain)
            .finish()
    }
}

/// Every audible audio clip, in the shape the node walks.
#[derive(Clone)]
pub struct CompiledAudioSong {
    /// Sorted by `start_ticks`, which is what lets the node stop scanning at the
    /// first clip that starts after this block.
    clips: Vec<CompiledAudioClip>,
}

impl CompiledAudioSong {
    /// Nothing to play.
    pub fn empty() -> Self {
        Self { clips: Vec::new() }
    }

    /// Assemble an arrangement. **Non-RT.**
    ///
    /// Sorted here rather than trusted, and silent clips (a missing file, a
    /// fader at the bottom, an offset past the end of the take) are dropped:
    /// each is a question the callback would otherwise ask once per clip per
    /// block, and the answer cannot change without a recompile.
    pub fn new(mut clips: Vec<CompiledAudioClip>) -> Self {
        clips.retain(|clip| clip.length_ticks > 0 && !clip.is_silent());
        clips.sort_unstable_by_key(|clip| (clip.start_ticks, clip.clip_id));
        Self { clips }
    }

    /// The clips, sorted by start tick.
    pub fn clips(&self) -> &[CompiledAudioClip] {
        &self.clips
    }

    /// How many clips will sound.
    pub fn len(&self) -> usize {
        self.clips.len()
    }

    /// The clips of one audio file, as their own arrangement.
    ///
    /// Each audio-clip channel's node only plays that file, so the bus filters
    /// the song rather than teaching the voice pool about mixer routing.
    pub fn for_source(&self, source_id: u64) -> Self {
        Self::new(
            self.clips
                .iter()
                .filter(|clip| clip.source.id() == source_id)
                .cloned()
                .collect(),
        )
    }

    /// Whether the arrangement has no audio in it.
    pub fn is_empty(&self) -> bool {
        self.clips.is_empty()
    }

    /// One past the last tick any clip occupies. `0` when there are none.
    pub fn end_ticks(&self) -> u64 {
        self.clips
            .iter()
            .map(CompiledAudioClip::end_ticks)
            .max()
            .unwrap_or(0)
    }
}

impl Default for CompiledAudioSong {
    fn default() -> Self {
        Self::empty()
    }
}

impl std::fmt::Debug for CompiledAudioSong {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledAudioSong")
            .field("clips", &self.clips.len())
            .finish()
    }
}

/// Channel-level playback for every clip of one file: pitch and the SMP region.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioClipTune {
    /// Fine + coarse pitch, in cents. `0` is recorded pitch. `±1200` is an octave.
    pub pitch_cents: f32,
    /// Frames skipped at the start of the file, added to each clip's own offset.
    pub start_frames: u64,
    /// Exclusive end in the file. `0` means the file's last frame.
    pub end_frames: u64,
}

impl Default for AudioClipTune {
    fn default() -> Self {
        Self {
            pitch_cents: 0.0,
            start_frames: 0,
            end_frames: 0,
        }
    }
}

/// Playback-rate multiplier for `cents` of channel pitch.
pub fn pitch_ratio(cents: f32) -> f64 {
    if !cents.is_finite() {
        return 1.0;
    }
    2f64.powf(f64::from(cents.clamp(-2_400.0, 2_400.0)) / 1_200.0)
}

/// Resolve a playlist's audio clips against `lookup`. **Non-RT.**
///
/// `lookup` answers "the source with this id", which is
/// [`AudioSourceRegistry::get`](crate::audio::AudioSourceRegistry::get) in the
/// shell and a closure in the tests. A clip whose source is not loaded is
/// **dropped with no error**: the arrangement is still valid (the file may be
/// found later), it simply has nothing to play, which is the same thing a
/// missing-file placeholder produces.
pub fn compile_audio_clips(
    playlist: &Playlist,
    lookup: impl Fn(u64) -> Option<Arc<AudioSource>>,
) -> CompiledAudioSong {
    compile_audio_clips_with(playlist, lookup, |_| AudioClipTune::default())
}

/// [`compile_audio_clips`], with per-file pitch and region from the channel.
pub fn compile_audio_clips_with(
    playlist: &Playlist,
    lookup: impl Fn(u64) -> Option<Arc<AudioSource>>,
    tune_of: impl Fn(u64) -> AudioClipTune,
) -> CompiledAudioSong {
    let clips = playlist
        .audible_audio_clips()
        .into_iter()
        .filter_map(|clip| {
            let source = lookup(clip.source_id)?;
            let tune = tune_of(clip.source_id);
            Some(CompiledAudioClip {
                clip_id: clip.id,
                source,
                start_ticks: clip.start_ticks,
                length_ticks: clip.length_ticks,
                offset_frames: clip.offset_frames.saturating_add(tune.start_frames),
                end_frames: tune.end_frames,
                pitch_ratio: pitch_ratio(tune.pitch_cents),
                gain: db_to_gain(clip.gain_db),
            })
        })
        .collect();
    CompiledAudioSong::new(clips)
}

/// Counters the clip node keeps for the rest of the app.
///
/// Atomics rather than logging, for the reason every counter on this side is:
/// `log::warn!` in a callback can allocate, take the logger's lock and touch a
/// file.
#[derive(Debug, Default)]
pub struct AudioClipStats {
    voices_started: AtomicU64,
    voices_stolen: AtomicU64,
    songs_swapped: AtomicU64,
}

impl AudioClipStats {
    /// Voices started since the stream came up.
    pub fn voices_started(&self) -> u64 {
        self.voices_started.load(Ordering::Relaxed)
    }

    /// Voices taken from a still-sounding clip because the pool was full.
    ///
    /// Audible: the oldest clip is faded out early. Non-zero means an
    /// arrangement with more than [`AUDIO_CLIP_VOICES`] clips overlapping.
    pub fn voices_stolen(&self) -> u64 {
        self.voices_stolen.load(Ordering::Relaxed)
    }

    /// Arrangements the audio thread has picked up.
    pub fn songs_swapped(&self) -> u64 {
        self.songs_swapped.load(Ordering::Relaxed)
    }
}

/// One clip playing back.
#[derive(Debug, Clone, Copy)]
struct ClipVoice {
    active: bool,
    /// Index into the current song's clips. Re-resolved on every swap.
    clip: usize,
    /// The playlist clip id, so a swap can find this voice's clip again.
    clip_id: u64,
    /// What the voice is reading, and from where. Compared on a swap: a clip
    /// that still plays the same file from the same tick at the same offset is
    /// the same playback and keeps its position, while anything else is a
    /// different sound and fades out. Gain and length are deliberately *not*
    /// part of the comparison -- both can change without moving a sample, and
    /// restarting a vocal because a fader moved is the glitch this avoids.
    source_id: u64,
    start_ticks: u64,
    offset_frames: u64,
    /// Absolute sample the voice's **first frame** is due at.
    ///
    /// This is what makes a clip sample-accurate rather than block-accurate: a
    /// clip whose start tick lands 22 frames into a 512-frame block starts on
    /// frame 22, not on frame 0. Kept as an absolute position rather than an
    /// offset into "the block" so that nothing has to be cleared between blocks.
    start_at: u64,
    /// Whether this voice is reading the *retiring* arrangement rather than the
    /// current one -- see [`AudioClipNode::retiring`].
    retiring: bool,
    /// Read position in *content* frames of the source. Fractional; the
    /// interpolator takes the remainder.
    position: f64,
    /// Source frames per output frame.
    step: f64,
    /// The clip's linear gain, ramped rather than applied as a step.
    gain: f32,
    /// Fade multiplier, `0.0`..`1.0`.
    env: f32,
    /// Added to `env` each frame while fading in, subtracted while fading out.
    env_step: f32,
    /// Whether the fade is running down (and the voice ends at the bottom).
    releasing: bool,
    /// Absolute sample position the fade-out must *begin* at, so that silence is
    /// reached exactly at the clip's end.
    release_at: u64,
    /// Voices started when this one did, so "oldest" is a number rather than a
    /// scan of positions.
    age: u64,
}

impl ClipVoice {
    const fn idle() -> Self {
        Self {
            active: false,
            clip: 0,
            clip_id: 0,
            source_id: 0,
            start_ticks: 0,
            offset_frames: 0,
            start_at: 0,
            retiring: false,
            position: 0.0,
            step: 1.0,
            gain: 0.0,
            env: 0.0,
            env_step: 0.0,
            releasing: false,
            release_at: u64::MAX,
            age: 0,
        }
    }

    /// Start the fade-out. A faster fade always wins, so a stop cannot be slowed
    /// down by a clip-end fade that was already running.
    fn release(&mut self, step: f32) {
        if !self.active {
            return;
        }
        self.releasing = true;
        self.env_step = self.env_step.max(step);
    }
}

/// The audio-clip player: [`AUDIO_CLIP_VOICES`] voices summed into the bus.
///
/// A **summing** node, not a generator: it adds to `ctx.buffers` and never
/// clears them, which is what lets it sit on the master bus alongside the track
/// sum. Compiled in unconditionally and transparent when there is nothing to
/// play, for the same reason the master insert slot is (see `app::insert`): an
/// arrangement must be able to gain its first audio clip with no graph rebuild.
pub struct AudioClipNode {
    rx: Consumer<Owned<CompiledAudioSong>>,
    song: Option<Owned<CompiledAudioSong>>,
    /// The arrangement the last swap replaced, held only while a voice is still
    /// fading out of it. See [`AudioClipNode::rebind`] and
    /// [`AudioClipNode::retire`].
    retiring: Option<Owned<CompiledAudioSong>>,
    voices: [ClipVoice; AUDIO_CLIP_VOICES],
    /// Monotonic, for "oldest".
    next_age: u64,
    /// Transport state as of the previous block, for edge detection.
    playing: bool,
    mode: PlayMode,
    /// Where the next block is expected to start. A mismatch is a seek.
    next_sample: u64,
    stats: Arc<AudioClipStats>,
}

impl AudioClipNode {
    /// Pick up the newest pending arrangement, keeping only the last of a burst.
    ///
    /// Returns whether anything was swapped. **RT-safe**: an `rtrb` pop and a
    /// move; the superseded arrangement is dropped as a `basedrop::Owned`, which
    /// queues it for the collector rather than freeing it here.
    fn poll_swap(&mut self) -> bool {
        let mut swapped = false;
        while let Ok(next) = self.rx.pop() {
            if swapped {
                // Two arrangements inside one block: the middle one never
                // played, so nothing can be reading it.
                self.song = Some(next);
                continue;
            }
            // Voices still finishing a *previous* retirement (two edits inside
            // one 2 ms fade) have nowhere to read from once it goes. Vanishingly
            // rare, and the honest thing is to stop them rather than to index a
            // buffer that is being collected.
            for voice in &mut self.voices {
                if voice.retiring {
                    voice.active = false;
                }
            }
            self.retiring = self.song.take();
            self.song = Some(next);
            swapped = true;
        }
        if swapped {
            self.stats.songs_swapped.fetch_add(1, Ordering::Relaxed);
        }
        swapped
    }

    /// Let go of the retiring arrangement once no voice is still reading it.
    ///
    /// The last `Arc<AudioSource>` in it may be the only reference to a hundred
    /// megabytes, so this drop matters: it is a `basedrop::Owned`, which means
    /// *queued for the collector*, never `free()` in the callback.
    fn retire(&mut self) {
        if self.retiring.is_some()
            && !self
                .voices
                .iter()
                .any(|voice| voice.active && voice.retiring)
        {
            self.retiring = None;
        }
    }

    /// Whether a voice is sounding. Diagnostics and tests.
    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|voice| voice.active).count()
    }

    /// Clips in the arrangement the audio thread currently holds.
    pub fn clip_count(&self) -> usize {
        self.song.as_ref().map_or(0, |song| song.len())
    }

    /// The counters.
    pub fn stats(&self) -> &Arc<AudioClipStats> {
        &self.stats
    }

    /// Re-resolve every voice against a freshly swapped arrangement.
    ///
    /// A voice whose clip is still there, unmoved, playing the same file from
    /// the same offset, keeps its position and picks up the new gain and length
    /// -- so moving *one* clip does not restart the vocal on another lane, and a
    /// gain change is a ramp rather than a retrigger.
    ///
    /// A voice whose clip is **gone** fades out of the *retiring* arrangement,
    /// which is why that one is held rather than dropped: deleting a clip that
    /// is sounding has to be a two-millisecond fade, and a fade needs samples to
    /// fade.
    fn rebind(&mut self, ramp: f32) {
        let Some(song) = self.song.as_deref() else {
            for voice in &mut self.voices {
                voice.release(ramp);
            }
            return;
        };
        for voice in &mut self.voices {
            if !voice.active || voice.retiring {
                continue;
            }
            // The only fields that can invalidate a read position are the file,
            // the clip's start and its offset into that file. Everything else
            // about the clip may change under a sounding voice.
            let found = song.clips().iter().position(|clip| {
                clip.clip_id == voice.clip_id
                    && clip.source.id() == voice.source_id
                    && clip.start_ticks == voice.start_ticks
                    && clip.offset_frames == voice.offset_frames
            });
            match found {
                Some(index) => {
                    voice.clip = index;
                    voice.gain = song.clips()[index].gain;
                }
                None => {
                    voice.retiring = true;
                    voice.release(ramp);
                }
            }
        }
    }

    /// Recompute where every sounding voice must begin its fade-out.
    ///
    /// Two things can end a clip and either may move under it: the clip's own
    /// end tick (which a **tempo change** moves, because a tick is not a sample)
    /// and the end of the file (which the voice walks towards at its own step).
    /// Both are recomputed here from the absolute tick and the voice's *current*
    /// position, every block -- which is why nothing accumulates and why a tempo
    /// change mid-clip lands the fade in the right place rather than the place
    /// the old tempo implied.
    fn refresh_ends(&mut self, block_start: u64, step_len: f64, ramp_frames: u64) {
        let Some(song) = self.song.as_deref() else {
            return;
        };
        for voice in &mut self.voices {
            if !voice.active || voice.releasing || voice.retiring {
                continue;
            }
            let Some(clip) = song.clips().get(voice.clip) else {
                continue;
            };
            let clip_end = tick_boundary(clip.end_ticks(), step_len);
            let source_end = if voice.step > 0.0 {
                let remaining = (clip.source.frames() as f64 - voice.position).max(0.0);
                block_start.saturating_add((remaining / voice.step).ceil() as u64)
            } else {
                u64::MAX
            };
            let end = clip_end.min(source_end);
            voice.release_at = end.saturating_sub(ramp_frames);
        }
    }

    /// Fade every voice out. Stop, and the half of a seek that comes first.
    fn release_all(&mut self, ramp: f32) {
        for voice in &mut self.voices {
            voice.release(ramp);
        }
    }

    /// Take a free voice, or the oldest sounding one.
    ///
    /// Pool exhaustion is *counted* and the oldest clip is faded rather than
    /// cut: dropping the newest instead would make an arrangement's most recent
    /// edit the one that does not play, which is the wrong thing to lose.
    fn allocate(&mut self, ramp: f32) -> usize {
        if let Some(index) = self.voices.iter().position(|voice| !voice.active) {
            return index;
        }
        let mut oldest = 0;
        for (index, voice) in self.voices.iter().enumerate() {
            if voice.age < self.voices[oldest].age {
                oldest = index;
            }
        }
        self.stats.voices_stolen.fetch_add(1, Ordering::Relaxed);
        // The stolen slot is reused immediately, so the fade cannot be heard --
        // the counter is what makes the loss visible instead.
        self.voices[oldest].release(ramp);
        oldest
    }

    /// Start `clip` at source frame `position`, first sounding at absolute
    /// sample `start_at` and beginning its fade-out at `release_at`.
    fn start(
        &mut self,
        clip: usize,
        position: f64,
        start_at: u64,
        release_at: u64,
        ramp: f32,
        ctx_rate: f64,
    ) {
        let Some(song) = self.song.as_deref() else {
            return;
        };
        let Some(compiled) = song.clips().get(clip) else {
            return;
        };
        let step = playback_step(compiled.source.sample_rate(), ctx_rate) * compiled.pitch_ratio;
        let gain = compiled.gain;
        let clip_id = compiled.clip_id;
        let source_id = compiled.source.id();
        let start_ticks = compiled.start_ticks;
        let offset_frames = compiled.offset_frames;
        let index = self.allocate(ramp);
        let age = self.next_age;
        self.next_age += 1;
        self.voices[index] = ClipVoice {
            active: true,
            clip,
            clip_id,
            source_id,
            start_ticks,
            offset_frames,
            start_at,
            retiring: false,
            position,
            step,
            gain,
            // The first frame is already one ramp step above silence rather
            // than at it, so that "the clip sounds here" and "the fade starts
            // here" are the same sample. A step of 1/96th of the clip's own
            // level is far below anything a click detector -- or an ear --
            // would call an edge.
            env: ramp.min(1.0),
            env_step: ramp,
            releasing: false,
            release_at,
            age,
        };
        self.stats.voices_started.fetch_add(1, Ordering::Relaxed);
    }
}

/// Source frames advanced per output frame.
///
/// The whole of the resampling: a file keeps its own rate and the ratio is one
/// multiply in the voice. A 44.1 kHz take on a 48 kHz device advances 0.91875
/// source frames per output frame, so a 441 Hz tone in the file is a 441 Hz tone
/// out of the device -- the *pitch is preserved*, which is the point.
///
/// A rate this cannot use (a missing file reports zero) gives a step of zero,
/// which holds the voice at its first frame; it never divides by zero and never
/// runs backwards.
#[inline]
pub fn playback_step(source_rate: f64, device_rate: f64) -> f64 {
    if !(source_rate.is_finite() && device_rate.is_finite()) || device_rate <= 0.0 {
        return 0.0;
    }
    (source_rate / device_rate).max(0.0)
}

/// Fade increment per frame for a ramp of `ms` at `sample_rate`.
///
/// At least a full step, so a nonsense rate degrades to an instant fade rather
/// than a voice that never finishes.
#[inline]
pub fn ramp_step(ms: f32, sample_rate: f64) -> f32 {
    if !sample_rate.is_finite() || sample_rate <= 0.0 || !ms.is_finite() || ms <= 0.0 {
        return 1.0;
    }
    let frames = (f64::from(ms) / 1_000.0 * sample_rate).round().max(1.0);
    (1.0 / frames) as f32
}

impl RtNode for AudioClipNode {
    fn process(&mut self, ctx: &mut ProcessCtx) {
        let ramp = ramp_step(CLIP_RAMP_MS, ctx.sample_rate);
        let frames = ctx.frames;

        if self.poll_swap() {
            self.rebind(ramp);
        }
        self.retire();

        let transport = ctx.transport;
        let block_start = transport.sample_pos;

        // --- transport edges, before anything is started ---------------------
        //
        // Stop, seek and a mode switch all mean "the clips I am playing belong
        // to a position that is no longer under the playhead". Fade them; a cut
        // would be a click, and leaving them running would be a vocal that keeps
        // going after Stop.
        let started = transport.playing && !self.playing;
        let stopped = self.playing && !transport.playing;
        let seeked = transport.playing && block_start != self.next_sample;
        let switched = transport.mode != self.mode;
        if stopped || seeked || switched {
            self.release_all(ramp);
        }
        self.playing = transport.playing;
        self.mode = transport.mode;
        self.next_sample = block_start + frames as u64;

        // Only song mode arranges: pattern mode is the loop the grid is editing,
        // and a playlist clip is not part of it.
        let arranging = transport.playing && transport.mode == PlayMode::Song;
        let step_len = step_len_samples(ctx.sample_rate, transport.bpm);
        let usable = step_len.is_finite() && step_len >= 1.0;

        if arranging && usable && self.song.is_some() {
            let ramp_frames = ((1.0 / f64::from(ramp)).round() as u64).max(1);
            self.refresh_ends(block_start, step_len, ramp_frames);
            // A play edge, a seek or a switch *into* song mode is "put the
            // playhead here and play what is here", which includes starting
            // part-way into a clip. A block that merely follows the last one
            // starts only the clips whose first frame is inside it.
            let derive = started || seeked || switched;
            self.spawn(
                block_start,
                frames,
                step_len,
                derive,
                ramp,
                ramp_frames,
                ctx.sample_rate,
            );
        }

        self.render(ctx, ramp);
    }
}

impl AudioClipNode {
    /// Start whatever this block calls for.
    ///
    /// `derive` is the seek/play path: every clip *covering* `block_start` gets
    /// a voice at the source frame that position corresponds to. Otherwise only
    /// clips whose first frame falls inside the block start, at their own offset.
    #[allow(clippy::too_many_arguments)]
    fn spawn(
        &mut self,
        block_start: u64,
        frames: usize,
        step_len: f64,
        derive: bool,
        ramp: f32,
        ramp_frames: u64,
        sample_rate: f64,
    ) {
        let block_end = block_start + frames as u64;
        let count = self.song.as_deref().map_or(0, |song| song.clips().len());

        for index in 0..count {
            // Re-read through the borrow each time: `start` needs `&mut self`,
            // and holding a `&CompiledAudioClip` across it would not compile.
            // Every value taken here is a `Copy` scalar.
            let Some(song) = self.song.as_deref() else {
                return;
            };
            let clip = &song.clips()[index];
            let clip_start = tick_boundary(clip.start_ticks, step_len);
            let clip_end = tick_boundary(clip.end_ticks(), step_len);
            let source_rate = clip.source.sample_rate();
            let source_frames = clip.playable_end();
            let offset = clip.offset_frames;

            // Sorted by start tick, so once a clip begins after this block, so
            // does every clip after it.
            if !derive && clip_start >= block_end {
                return;
            }
            if clip_end <= clip_start {
                continue;
            }

            let step = playback_step(source_rate, sample_rate) * clip.pitch_ratio;
            if step <= 0.0 || offset >= source_frames {
                continue;
            }
            // Where the file (or the SMP region) runs out, in output frames
            // from the clip's start. `ceil` so the last partial frame is played
            // rather than dropped.
            let playable = ((source_frames - offset) as f64 / step).ceil() as u64;
            let end = clip_end.min(clip_start.saturating_add(playable));
            if end <= clip_start {
                continue;
            }
            // Silence is reached *at* the end, so the fade has to begin before
            // it. A clip shorter than one ramp fades from its own first frame.
            let release_at = end.saturating_sub(ramp_frames).max(clip_start);

            if derive {
                if block_start >= clip_start && block_start < end {
                    let into = (block_start - clip_start) as f64 * step;
                    let position = offset as f64 + into;
                    if position < source_frames as f64 {
                        // A derived voice is already under the playhead, so its
                        // first frame is this block's first frame.
                        self.start(index, position, block_start, release_at, ramp, sample_rate);
                    }
                }
                continue;
            }

            if clip_start >= block_start && clip_start < block_end {
                // `clip_start`, not `block_start`: a clip whose tick lands 22
                // frames into the block sounds on frame 22. This is the whole of
                // "sample-accurate at clip boundaries".
                self.start(
                    index,
                    offset as f64,
                    clip_start,
                    release_at,
                    ramp,
                    sample_rate,
                );
            }
        }
    }

    /// Sum every sounding voice into the bus.
    ///
    /// One pass per voice over the block. Adds; never clears -- see the type's
    /// docs on why this node is not a generator.
    fn render(&mut self, ctx: &mut ProcessCtx, ramp: f32) {
        let frames = ctx.frames;
        if frames == 0 {
            return;
        }
        let block_start = ctx.transport.sample_pos;
        let block_end = block_start + frames as u64;
        let current = self.song.as_deref();
        let retiring = self.retiring.as_deref();
        let [left, right] = ctx.buffers.as_mut_slices();

        for voice in &mut self.voices {
            if !voice.active {
                continue;
            }
            // A voice reads the arrangement it was bound to. That is the whole
            // reason the superseded one is kept for a ramp: a clip the user just
            // deleted still has two milliseconds of samples to fade out of.
            let song = if voice.retiring { retiring } else { current };
            let Some(clip) = song.and_then(|song| song.clips().get(voice.clip)) else {
                voice.active = false;
                continue;
            };
            let padded = clip.source.padded();
            let source_end = clip.playable_end() as f64;

            // Sample-accurate: a voice due part-way through the block is silent
            // until its own frame. Past this block entirely is a voice the
            // scheduler started early, which cannot happen -- but a clamp is
            // cheaper than trusting it.
            if voice.start_at >= block_end {
                continue;
            }
            let first = voice.start_at.saturating_sub(block_start) as usize;

            for frame in first..frames {
                if voice.position >= source_end || voice.position < 0.0 {
                    voice.active = false;
                    break;
                }
                // The clip's own end, recomputed from the absolute tick every
                // block, so a tempo change moves it.
                if !voice.releasing && block_start + frame as u64 >= voice.release_at {
                    voice.releasing = true;
                    voice.env_step = voice.env_step.max(ramp);
                }

                // `position` is finite and below `frames`, so this is in range.
                let index = voice.position as usize;
                let fraction = (voice.position - voice.position.floor()) as f32;
                let gain = voice.gain * voice.env;

                left[frame] += cubic(taps(padded, index, 0), fraction) * gain;
                if CHANNELS > 1 {
                    right[frame] += cubic(taps(padded, index, 1), fraction) * gain;
                }

                if voice.releasing {
                    voice.env -= voice.env_step;
                    if voice.env <= 0.0 {
                        // Exactly zero, then gone: "faded" has to mean digital
                        // silence, not "quiet enough".
                        voice.env = 0.0;
                        voice.active = false;
                        break;
                    }
                } else if voice.env < 1.0 {
                    voice.env = (voice.env + voice.env_step).min(1.0);
                }
                voice.position += voice.step;
            }
        }
    }
}

impl std::fmt::Debug for AudioClipNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioClipNode")
            .field("clips", &self.clip_count())
            .field("voices", &self.active_voices())
            .finish()
    }
}

/// Why an arrangement could not be handed to the audio thread.
#[derive(Debug, thiserror::Error)]
pub enum AudioClipInstallError {
    /// The audio thread has not consumed previous arrangements yet. Retryable;
    /// the rejected one is dropped here, on the non-RT thread.
    #[error("the audio clip queue is full; the audio thread has not picked up previous edits")]
    QueueFull,
    /// The audio thread is gone (the stream was torn down).
    #[error("the audio thread is not running")]
    Disconnected,
}

/// Non-RT end of the audio-clip channel, for one open stream.
pub struct AudioClipHandle {
    tx: Producer<Owned<CompiledAudioSong>>,
    gc: Handle,
    stats: Arc<AudioClipStats>,
}

impl AudioClipHandle {
    /// Publish an arrangement to the audio thread.
    ///
    /// # Errors
    /// [`AudioClipInstallError::QueueFull`] (retryable) or
    /// [`AudioClipInstallError::Disconnected`].
    pub fn install(&mut self, song: CompiledAudioSong) -> Result<(), AudioClipInstallError> {
        let owned = Owned::new(&self.gc, song);
        self.tx.push(owned).map_err(|_| {
            if self.tx.is_abandoned() {
                AudioClipInstallError::Disconnected
            } else {
                AudioClipInstallError::QueueFull
            }
        })
    }

    /// The counters the node maintains.
    pub fn stats(&self) -> &Arc<AudioClipStats> {
        &self.stats
    }
}

impl std::fmt::Debug for AudioClipHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioClipHandle").finish_non_exhaustive()
    }
}

/// Build the channel: a handle for the shell and a node for the graph.
///
/// The node is compiled into the graph as the **first master insert**, which is
/// what puts audio clips on the master bus before the master chain -- see the
/// module docs.
pub fn audio_clip_channel(capacity: usize, gc: Handle) -> (AudioClipHandle, AudioClipNode) {
    let (tx, rx) = rt_channel::<Owned<CompiledAudioSong>>(capacity);
    let stats = Arc::new(AudioClipStats::default());
    (
        AudioClipHandle {
            tx,
            gc,
            stats: Arc::clone(&stats),
        },
        AudioClipNode {
            rx,
            song: None,
            retiring: None,
            voices: [ClipVoice::idle(); AUDIO_CLIP_VOICES],
            next_age: 0,
            playing: false,
            mode: PlayMode::Pattern,
            next_sample: 0,
            stats,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern::TICKS_PER_BAR;
    use mydaw_rt_core::{AudioBuffers, GcThread, TransportSnapshot};

    const BAR: u64 = TICKS_PER_BAR as u64;

    /// A source of `frames` frames at `rate`, holding `value` on both channels.
    fn flat_source(id: u64, rate: f64, frames: usize, value: f32) -> Arc<AudioSource> {
        Arc::new(AudioSource::from_stereo(
            id,
            std::path::Path::new("test.wav"),
            rate,
            vec![value; frames * CHANNELS],
        ))
    }

    /// A source holding a `hz` sine at `rate`.
    fn sine_source(id: u64, rate: f64, frames: usize, hz: f64) -> Arc<AudioSource> {
        let mut data = Vec::with_capacity(frames * CHANNELS);
        for frame in 0..frames {
            let value = ((frame as f64 / rate) * hz * std::f64::consts::TAU).sin() as f32 * 0.5;
            data.push(value);
            data.push(value);
        }
        Arc::new(AudioSource::from_stereo(
            id,
            std::path::Path::new("sine.wav"),
            rate,
            data,
        ))
    }

    fn clip(clip_id: u64, source: Arc<AudioSource>, start: u64, length: u64) -> CompiledAudioClip {
        CompiledAudioClip {
            clip_id,
            source,
            start_ticks: start,
            length_ticks: length,
            offset_frames: 0,
            end_frames: 0,
            pitch_ratio: 1.0,
            gain: 1.0,
        }
    }

    /// A node with `song` already installed and picked up.
    struct Rig {
        node: AudioClipNode,
        handle: AudioClipHandle,
        buffers: AudioBuffers,
        _gc: GcThread,
    }

    impl Rig {
        fn new(song: CompiledAudioSong, max_block: usize) -> Self {
            let gc = GcThread::spawn();
            let (mut handle, node) = audio_clip_channel(AUDIO_CLIP_QUEUE_CAPACITY, gc.handle());
            handle.install(song).expect("installs");
            Self {
                node,
                handle,
                buffers: AudioBuffers::new(max_block),
                _gc: gc,
            }
        }

        /// Render one block and return the left channel.
        fn block(
            &mut self,
            sample_pos: u64,
            frames: usize,
            rate: f64,
            bpm: f64,
            playing: bool,
        ) -> Vec<f32> {
            self.block_in(sample_pos, frames, rate, bpm, playing, PlayMode::Song)
        }

        fn block_in(
            &mut self,
            sample_pos: u64,
            frames: usize,
            rate: f64,
            bpm: f64,
            playing: bool,
            mode: PlayMode,
        ) -> Vec<f32> {
            let mut ctx = ProcessCtx::new(
                &mut self.buffers,
                rate,
                frames,
                TransportSnapshot {
                    sample_pos,
                    playing,
                    bpm,
                    mode,
                },
            );
            ctx.buffers.silence();
            self.node.process(&mut ctx);
            self.buffers.as_slices()[0][..frames].to_vec()
        }

        /// Render `blocks` blocks of `frames` from sample 0, concatenated.
        fn run(&mut self, blocks: usize, frames: usize, rate: f64, bpm: f64) -> Vec<f32> {
            let mut out = Vec::with_capacity(blocks * frames);
            for block in 0..blocks {
                out.extend(self.block((block * frames) as u64, frames, rate, bpm, true));
            }
            out
        }
    }

    /// The first frame louder than `threshold`.
    fn first_onset(samples: &[f32], threshold: f32) -> Option<usize> {
        samples.iter().position(|s| s.abs() > threshold)
    }

    /// The last frame louder than `threshold`.
    fn last_onset(samples: &[f32], threshold: f32) -> Option<usize> {
        samples.iter().rposition(|s| s.abs() > threshold)
    }

    #[test]
    fn an_empty_arrangement_leaves_the_bus_exactly_as_it_found_it() {
        let mut rig = Rig::new(CompiledAudioSong::empty(), 128);
        let out = rig.block(0, 128, 48_000.0, 120.0, true);
        assert!(out.iter().all(|s| *s == 0.0));
        assert_eq!(rig.node.active_voices(), 0);
    }

    #[test]
    fn the_node_sums_into_the_bus_rather_than_replacing_it() {
        // The master bus already carries the track sum when this node runs; a
        // clip that overwrote it would silence every instrument in the project.
        let source = flat_source(1, 48_000.0, 48_000, 0.25);
        let song = CompiledAudioSong::new(vec![clip(1, source, 0, BAR)]);
        let mut rig = Rig::new(song, 128);

        let mut ctx = ProcessCtx::new(
            &mut rig.buffers,
            48_000.0,
            128,
            TransportSnapshot {
                sample_pos: 0,
                playing: true,
                bpm: 120.0,
                mode: PlayMode::Song,
            },
        );
        for channel in ctx.buffers.as_mut_slices() {
            channel.fill(0.5);
        }
        rig.node.process(&mut ctx);
        let out = rig.buffers.as_slices()[0];
        assert!(out[100] > 0.5, "the track sum must survive: {}", out[100]);
    }

    #[test]
    fn a_clip_sounds_at_the_sample_its_start_tick_names() {
        // The load-bearing claim: an audio clip on bar 3 and a kick on bar 3
        // land on the same sample, because both go through `tick_boundary`.
        for (rate, bpm, block) in [
            (44_100.0, 130.0, 512_usize),
            (48_000.0, 120.0, 64),
            (48_000.0, 174.3, 4_096),
            (44_100.0, 90.0, 128),
        ] {
            let step_len = step_len_samples(rate, bpm);
            let expected = tick_boundary(3 * BAR, step_len);
            let source = flat_source(1, rate, (rate * 4.0) as usize, 0.5);
            let song = CompiledAudioSong::new(vec![clip(1, source, 3 * BAR, BAR)]);
            let mut rig = Rig::new(song, block);

            let blocks = (expected as usize / block) + 3;
            let out = rig.run(blocks, block, rate, bpm);
            let onset = first_onset(&out, 1e-6).expect("the clip must sound");
            assert_eq!(
                onset as u64, expected,
                "{rate} Hz, {bpm} BPM, {block}-frame blocks"
            );
        }
    }

    #[test]
    fn a_clip_stops_at_its_own_end_and_reaches_silence_there() {
        let rate = 48_000.0;
        let bpm = 120.0;
        let step_len = step_len_samples(rate, bpm);
        let start = tick_boundary(BAR, step_len);
        let end = tick_boundary(2 * BAR, step_len);

        let source = flat_source(1, rate, (rate * 8.0) as usize, 0.5);
        let song = CompiledAudioSong::new(vec![clip(1, source, BAR, BAR)]);
        let mut rig = Rig::new(song, 256);
        let out = rig.run((end as usize / 256) + 4, 256, rate, bpm);

        assert_eq!(first_onset(&out, 1e-6).expect("sounds") as u64, start);
        let last = last_onset(&out, 1e-6).expect("sounds") as u64;
        assert!(
            last < end && end - last <= 2,
            "silence must land on the clip's end: last {last}, end {end}"
        );
        assert_eq!(rig.node.active_voices(), 0, "the voice must free itself");
    }

    #[test]
    fn a_clip_that_outlasts_its_file_ends_when_the_file_does() {
        let rate = 48_000.0;
        let bpm = 120.0;
        // Half a second of audio in a two-bar (four-second) clip.
        let source = flat_source(1, rate, 24_000, 0.5);
        let song = CompiledAudioSong::new(vec![clip(1, source, 0, 2 * BAR)]);
        let mut rig = Rig::new(song, 512);
        let out = rig.run(80, 512, rate, bpm);

        let last = last_onset(&out, 1e-6).expect("sounds");
        assert!(
            (23_500..=24_000).contains(&last),
            "the file runs out at 24000, got {last}"
        );
    }

    #[test]
    fn nothing_sounds_in_pattern_mode_or_while_the_transport_is_stopped() {
        let source = flat_source(1, 48_000.0, 48_000, 0.5);
        let song = CompiledAudioSong::new(vec![clip(1, source, 0, BAR)]);
        let mut rig = Rig::new(song, 256);

        let stopped = rig.block(0, 256, 48_000.0, 120.0, false);
        assert!(stopped.iter().all(|s| *s == 0.0), "stopped must be silent");

        let pattern = rig.block_in(0, 256, 48_000.0, 120.0, true, PlayMode::Pattern);
        assert!(
            pattern.iter().all(|s| *s == 0.0),
            "a playlist clip is not part of the pattern loop"
        );
    }

    #[test]
    fn stopping_fades_a_sounding_clip_out_instead_of_cutting_it() {
        let rate = 48_000.0;
        let source = flat_source(1, rate, 48_000, 0.5);
        let song = CompiledAudioSong::new(vec![clip(1, source, 0, 4 * BAR)]);
        let mut rig = Rig::new(song, 256);

        let playing = rig.block(0, 256, rate, 120.0, true);
        assert!(playing[200].abs() > 0.4, "{}", playing[200]);

        let stopping = rig.block(256, 256, rate, 120.0, false);
        // The first frame after Stop must be close to where playback was, and
        // the block must reach silence: that is a fade, not a cut.
        assert!(stopping[0].abs() > 0.3, "a cut would be zero here");
        assert_eq!(stopping[255], 0.0, "and it must reach exactly zero");
        assert_eq!(rig.node.active_voices(), 0);
    }

    #[test]
    fn no_sample_step_at_a_start_or_a_stop_is_bigger_than_the_ramp_allows() {
        // The no-click assertion, measured rather than asserted by eye: over a
        // whole start-play-stop cycle no two adjacent output samples differ by
        // more than one ramp step of the clip's own amplitude.
        let rate = 48_000.0;
        let amplitude = 0.5_f32;
        let source = flat_source(1, rate, 48_000, amplitude);
        let song = CompiledAudioSong::new(vec![clip(1, source, 0, 4 * BAR)]);
        let mut rig = Rig::new(song, 128);

        let mut out = Vec::new();
        for block in 0..8 {
            out.extend(rig.block(block * 128, 128, rate, 120.0, true));
        }
        for block in 8..12 {
            out.extend(rig.block(block * 128, 128, rate, 120.0, false));
        }

        let limit = amplitude * ramp_step(CLIP_RAMP_MS, rate) * 1.5;
        for (index, pair) in out.windows(2).enumerate() {
            let jump = (pair[1] - pair[0]).abs();
            assert!(
                jump <= limit,
                "frame {index}: {jump} > {limit} -- that is a click"
            );
        }
    }

    #[test]
    fn seeking_into_the_middle_of_a_clip_continues_it_from_the_right_frame() {
        // A ramp source, so the output value *is* the source position.
        let rate = 48_000.0;
        let bpm = 120.0;
        let frames = 96_000;
        let mut data = Vec::with_capacity(frames * CHANNELS);
        for frame in 0..frames {
            let value = frame as f32 / frames as f32;
            data.push(value);
            data.push(value);
        }
        let source = Arc::new(AudioSource::from_stereo(
            1,
            std::path::Path::new("ramp.wav"),
            rate,
            data,
        ));
        let song = CompiledAudioSong::new(vec![CompiledAudioClip {
            clip_id: 1,
            source,
            start_ticks: BAR,
            length_ticks: 4 * BAR,
            offset_frames: 1_000,
            end_frames: 0,
            pitch_ratio: 1.0,
            gain: 1.0,
        }]);
        let mut rig = Rig::new(song, 512);

        let step_len = step_len_samples(rate, bpm);
        let clip_start = tick_boundary(BAR, step_len);
        // Land 12 000 frames into the clip. Step is 1.0 (rates match), so the
        // source position must be offset + 12 000.
        let seek_to = clip_start + 12_000;
        rig.block(0, 512, rate, bpm, true);
        let out = rig.block(seek_to, 512, rate, bpm, true);

        // Frame 0 is at env 0 (the fade starts there), so read a frame past the
        // ramp and subtract the frames it took to get there.
        let probe = 300_usize;
        let expected = (1_000 + 12_000 + probe) as f32 / frames as f32;
        let got = out[probe];
        // Within one source frame.
        assert!(
            (got - expected).abs() < 1.5 / frames as f32,
            "seek landed at {} instead of {}",
            got * frames as f32,
            expected * frames as f32
        );
    }

    #[test]
    fn a_seek_that_lands_before_a_clip_starts_it_at_its_own_offset() {
        let rate = 48_000.0;
        let bpm = 120.0;
        let step_len = step_len_samples(rate, bpm);
        let start = tick_boundary(4 * BAR, step_len);
        let source = flat_source(1, rate, 480_000, 0.5);
        let song = CompiledAudioSong::new(vec![clip(1, source, 4 * BAR, 4 * BAR)]);
        let mut rig = Rig::new(song, 512);

        // Seek to two bars in -- before the clip. Nothing must sound yet.
        rig.block(0, 512, rate, bpm, true);
        let early = rig.block(tick_boundary(2 * BAR, step_len), 512, rate, bpm, true);
        assert!(early.iter().all(|s| *s == 0.0));
        assert_eq!(rig.node.active_voices(), 0);

        // And playing on from there still finds the clip at its own start.
        let mut out = Vec::new();
        let mut pos = tick_boundary(2 * BAR, step_len) + 512;
        while pos < start + 1_024 {
            out.extend(rig.block(pos, 512, rate, bpm, true));
            pos += 512;
        }
        let base = tick_boundary(2 * BAR, step_len) + 512;
        let onset = first_onset(&out, 1e-6).expect("sounds") as u64 + base;
        assert_eq!(onset, start);
    }

    #[test]
    fn a_source_at_another_rate_keeps_its_pitch() {
        // 441 Hz recorded at 44 100 Hz, played out at 48 000 Hz. The tone that
        // reaches the device must still be 441 Hz -- measured by counting zero
        // crossings, not by trusting the ratio.
        let device_rate = 48_000.0;
        let source = sine_source(1, 44_100.0, 44_100, 441.0);
        let song = CompiledAudioSong::new(vec![clip(1, source, 0, 8 * BAR)]);
        let mut rig = Rig::new(song, 1_024);
        let out = rig.run(40, 1_024, device_rate, 120.0);

        // Skip the fade-in, and stop before the file runs out.
        let window = &out[2_000..38_000];
        let crossings = window
            .windows(2)
            .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
            .count();
        let seconds = window.len() as f64 / device_rate;
        let hz = crossings as f64 / seconds;
        assert!(
            (hz - 441.0).abs() < 2.0,
            "441 Hz at 44.1 k on a 48 k device measured {hz} Hz"
        );
    }

    #[test]
    fn a_source_at_the_device_rate_is_played_back_frame_for_frame() {
        assert_eq!(playback_step(48_000.0, 48_000.0), 1.0);
        assert!((playback_step(44_100.0, 48_000.0) - 0.918_75).abs() < 1e-12);
        assert!((playback_step(48_000.0, 44_100.0) - 1.088_435_374).abs() < 1e-9);
        // Nonsense degrades to "hold", never to a divide by zero.
        assert_eq!(playback_step(0.0, 48_000.0), 0.0);
        assert_eq!(playback_step(48_000.0, 0.0), 0.0);
        assert_eq!(playback_step(f64::NAN, 48_000.0), 0.0);
    }

    #[test]
    fn the_clip_gain_is_applied() {
        let rate = 48_000.0;
        let source = flat_source(1, rate, 48_000, 0.5);
        let mut half = clip(1, Arc::clone(&source), 0, 4 * BAR);
        half.gain = db_to_gain(-6.0);
        let song = CompiledAudioSong::new(vec![half]);
        let mut rig = Rig::new(song, 512);
        let out = rig.run(2, 512, rate, 120.0);

        let peak = out.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()));
        assert!(
            (peak - 0.5 * db_to_gain(-6.0)).abs() < 1e-4,
            "peak {peak} is not -6 dB of 0.5"
        );
    }

    #[test]
    fn more_clips_than_voices_steals_the_oldest_and_counts_it() {
        let rate = 48_000.0;
        let source = flat_source(1, rate, 480_000, 0.1);
        // Every clip starts at tick 0, so they all want a voice in one block.
        let clips: Vec<_> = (0..AUDIO_CLIP_VOICES as u64 + 8)
            .map(|id| clip(id + 1, Arc::clone(&source), 0, 4 * BAR))
            .collect();
        let song = CompiledAudioSong::new(clips);
        let mut rig = Rig::new(song, 256);
        rig.block(0, 256, rate, 120.0, true);

        assert_eq!(rig.node.active_voices(), AUDIO_CLIP_VOICES);
        assert_eq!(rig.handle.stats().voices_stolen(), 8);
        assert_eq!(
            rig.handle.stats().voices_started(),
            AUDIO_CLIP_VOICES as u64 + 8
        );
    }

    #[test]
    fn an_edit_elsewhere_does_not_restart_a_sounding_clip() {
        let rate = 48_000.0;
        let ramp = flat_source(1, rate, 480_000, 0.5);
        let mut rig = Rig::new(
            CompiledAudioSong::new(vec![clip(1, Arc::clone(&ramp), 0, 8 * BAR)]),
            512,
        );
        rig.block(0, 512, rate, 120.0, true);
        rig.block(512, 512, rate, 120.0, true);
        let before = rig.handle.stats().voices_started();

        // A second clip appears somewhere else in the arrangement.
        rig.handle
            .install(CompiledAudioSong::new(vec![
                clip(1, Arc::clone(&ramp), 0, 8 * BAR),
                clip(2, Arc::clone(&ramp), 32 * BAR, BAR),
            ]))
            .expect("installs");
        let out = rig.block(1_024, 512, rate, 120.0, true);

        assert_eq!(
            rig.handle.stats().voices_started(),
            before,
            "the sounding clip must not be retriggered"
        );
        assert!(
            out[0].abs() > 0.4,
            "and it must not have dipped: {}",
            out[0]
        );
        assert_eq!(rig.node.active_voices(), 1);
    }

    #[test]
    fn a_clip_removed_by_an_edit_fades_out_rather_than_cutting() {
        let rate = 48_000.0;
        let source = flat_source(1, rate, 480_000, 0.5);
        let mut rig = Rig::new(
            CompiledAudioSong::new(vec![clip(1, Arc::clone(&source), 0, 8 * BAR)]),
            512,
        );
        rig.block(0, 512, rate, 120.0, true);
        rig.block(512, 512, rate, 120.0, true);

        rig.handle
            .install(CompiledAudioSong::empty())
            .expect("installs");
        let out = rig.block(1_024, 512, rate, 120.0, true);
        assert!(out[0].abs() > 0.3, "a cut would be zero here");
        assert_eq!(out[511], 0.0);
    }

    #[test]
    fn a_silent_clip_never_reaches_the_audio_thread() {
        let missing = Arc::new(AudioSource::missing(1, std::path::Path::new("gone.wav")));
        let real = flat_source(2, 48_000.0, 4_800, 0.5);
        let mut muted = clip(2, Arc::clone(&real), 0, BAR);
        muted.gain = 0.0;
        let mut past_the_end = clip(3, real, 0, BAR);
        past_the_end.offset_frames = 999_999;

        let song = CompiledAudioSong::new(vec![
            clip(1, missing, 0, BAR),
            muted,
            past_the_end,
            clip(4, flat_source(4, 48_000.0, 4_800, 0.5), 0, BAR),
        ]);
        assert_eq!(song.len(), 1, "only the one that can sound survives");
    }

    #[test]
    fn an_arrangement_sorts_its_clips_and_knows_where_it_ends() {
        let source = flat_source(1, 48_000.0, 48_000, 0.5);
        let song = CompiledAudioSong::new(vec![
            clip(3, Arc::clone(&source), 8 * BAR, BAR),
            clip(1, Arc::clone(&source), 0, BAR),
            clip(2, source, 4 * BAR, 2 * BAR),
        ]);
        assert_eq!(
            song.clips()
                .iter()
                .map(|clip| clip.start_ticks)
                .collect::<Vec<_>>(),
            vec![0, 4 * BAR, 8 * BAR]
        );
        assert_eq!(song.end_ticks(), 9 * BAR);
        assert!(!song.is_empty());
        assert!(CompiledAudioSong::empty().is_empty());
    }

    #[test]
    fn a_full_queue_is_reported_rather_than_dropped_silently() {
        let gc = GcThread::spawn();
        let (mut handle, _node) = audio_clip_channel(2, gc.handle());
        assert!(handle.install(CompiledAudioSong::empty()).is_ok());
        assert!(handle.install(CompiledAudioSong::empty()).is_ok());
        assert!(matches!(
            handle.install(CompiledAudioSong::empty()),
            Err(AudioClipInstallError::QueueFull)
        ));
    }

    #[test]
    fn an_abandoned_queue_reports_a_dead_audio_thread_not_a_full_one() {
        let gc = GcThread::spawn();
        let (mut handle, node) = audio_clip_channel(1, gc.handle());
        drop(node);
        // `rtrb` lets a push into an abandoned queue succeed while there is
        // room -- the arrangement simply has nowhere to go. It is the push that
        // *fails* which has to say which of the two things went wrong, because
        // "retry in a moment" and "the stream stopped" need different answers.
        handle.install(CompiledAudioSong::empty()).unwrap();
        assert!(matches!(
            handle.install(CompiledAudioSong::empty()),
            Err(AudioClipInstallError::Disconnected)
        ));
    }

    #[test]
    fn a_ramp_crosses_unity_in_the_time_it_advertises() {
        // 2 ms at 48 kHz is 96 frames.
        let step = ramp_step(2.0, 48_000.0);
        assert!((1.0 / step - 96.0).abs() < 0.5, "{step}");
        // And a nonsense rate is an instant fade rather than a stuck voice.
        assert_eq!(ramp_step(2.0, 0.0), 1.0);
        assert_eq!(ramp_step(0.0, 48_000.0), 1.0);
    }

    #[test]
    fn compiling_drops_clips_whose_source_is_not_loaded() {
        let mut playlist = Playlist::default();
        playlist.add_audio_clip(1, 0, 0, BAR, 0, 0.0).expect("ok");
        playlist.add_audio_clip(9, 0, BAR, BAR, 0, 0.0).expect("ok");
        let loaded = flat_source(1, 48_000.0, 48_000, 0.5);

        let song = compile_audio_clips(&playlist, |id| (id == 1).then(|| Arc::clone(&loaded)));
        assert_eq!(song.len(), 1);
        assert_eq!(song.clips()[0].clip_id, 1);
    }

    #[test]
    fn compiling_drops_a_muted_lanes_clips() {
        let mut playlist = Playlist::default();
        playlist.add_audio_clip(1, 0, 0, BAR, 0, 0.0).expect("ok");
        playlist.add_audio_clip(1, 1, 0, BAR, 0, 0.0).expect("ok");
        playlist.set_track_muted(0, true).expect("mutes");
        let loaded = flat_source(1, 48_000.0, 48_000, 0.5);

        let song = compile_audio_clips(&playlist, |_| Some(Arc::clone(&loaded)));
        assert_eq!(song.len(), 1);
        assert_eq!(song.clips()[0].clip_id, 2);
    }

    #[test]
    fn compiling_converts_the_clips_decibels_to_a_linear_gain() {
        let mut playlist = Playlist::default();
        playlist.add_audio_clip(1, 0, 0, BAR, 0, -6.0).expect("ok");
        let loaded = flat_source(1, 48_000.0, 48_000, 0.5);
        let song = compile_audio_clips(&playlist, |_| Some(Arc::clone(&loaded)));
        assert!((song.clips()[0].gain - db_to_gain(-6.0)).abs() < 1e-6);
    }

    #[test]
    fn compiling_bakes_channel_pitch_and_region_into_the_clip() {
        let mut playlist = Playlist::default();
        playlist.add_audio_clip(1, 0, 0, BAR, 100, 0.0).expect("ok");
        let loaded = flat_source(1, 48_000.0, 48_000, 0.5);
        let song = compile_audio_clips_with(
            &playlist,
            |_| Some(Arc::clone(&loaded)),
            |_| AudioClipTune {
                pitch_cents: 1_200.0,
                start_frames: 50,
                end_frames: 10_000,
            },
        );
        let clip = &song.clips()[0];
        assert_eq!(clip.offset_frames, 150);
        assert_eq!(clip.end_frames, 10_000);
        assert!(
            (clip.pitch_ratio - 2.0).abs() < 1e-9,
            "{}",
            clip.pitch_ratio
        );
    }

    #[test]
    fn for_source_keeps_only_that_file() {
        let a = flat_source(1, 48_000.0, 1_000, 0.5);
        let b = flat_source(2, 48_000.0, 1_000, 0.5);
        let song = CompiledAudioSong::new(vec![
            clip(1, Arc::clone(&a), 0, BAR),
            clip(2, Arc::clone(&b), BAR, BAR),
        ]);
        assert_eq!(song.for_source(1).len(), 1);
        assert_eq!(song.for_source(1).clips()[0].clip_id, 1);
        assert!(song.for_source(9).is_empty());
    }
}
