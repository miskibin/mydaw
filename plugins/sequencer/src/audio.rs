//! The audio source registry: files a clip can point at, decoded into RAM.
//!
//! ```text
//!   non-RT (a command thread)                        audio thread
//!   -------------------------                        ------------
//!   load_audio_source(path)
//!        |  mydaw_audio_file::decode_stereo
//!        v
//!   Arc<AudioSource> { padded stereo f32, rate, mipmaps }
//!        |                       |
//!        |  registry (id -> Arc) |  waveform(bins) -> the UI draws it
//!        v
//!   CompiledAudioSong  -- rtrb + basedrop -->  AudioClipNode voices
//! ```
//!
//! # Why a table, and why ids
//! [`AudioClip`](crate::AudioClip) names a **source id**, not a path. Two clips
//! of the same take are the common case, and a path repeated per clip is a path
//! that can disagree with itself -- so the project file carries an
//! `audio_sources` table (it has since format version 3) and this is the live
//! side of it. Ids are stable across a session and are what a saved project
//! stores, so re-opening a file rebuilds exactly the references it had.
//!
//! # Why the whole file is in RAM
//! Same trade the sampler makes, one level up: streaming from disk in the
//! callback is a syscall in the callback, and the alternative -- a prefetch
//! thread with a ring per voice -- is a lot of machinery for a step whose clips
//! are loops and takes rather than hour-long stems. Sixty seconds of 48 kHz
//! stereo `f32` is 23 MB; [`MAX_SOURCE_SECONDS`] caps one file at ten minutes,
//! which is 230 MB and an honest ceiling rather than an unbounded one.
//!
//! # Why nothing is resampled at load
//! A source keeps its own rate, and the voice folds the device ratio into the
//! step it computes anyway (see [`crate::clips`]). Resampling at load would be
//! slow to open and would have to be redone on every device change.
//!
//! # A missing file is a warning, not a refusal
//! [`AudioSourceRegistry::register_missing`] puts a **silent** source of zero
//! frames under the id the project asked for, so every clip that referenced it
//! stays in the arrangement, keeps its position, and can be re-pointed at the
//! file once it is found. Deleting the clips instead would make "I moved my
//! samples folder" a destructive edit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mydaw_audio_file::{decode_stereo, DecodeError, CHANNELS, PAD_LEAD};
use parking_lot::Mutex;

use crate::playlist::Playlist;

/// Longest single file the registry will hold, in seconds.
///
/// Ten minutes: past any loop, take or stem this step is for, and a hard number
/// rather than "as much as fits", so a user who drags in a two-hour podcast gets
/// a message instead of a swapping machine. At 48 kHz stereo `f32` this is
/// 230 MB.
pub const MAX_SOURCE_SECONDS: f64 = 600.0;

/// Files the registry will hold at once.
///
/// Sixty-four distinct takes in one project is generous for a step whose
/// playlist holds [`MAX_CLIPS`](crate::MAX_CLIPS) clips; the point of the bound
/// is that "how much audio can a project load" has an answer.
pub const MAX_SOURCES: usize = 64;

/// Frames one waveform bin covers at mip level 0.
///
/// 256 frames is 5.8 ms at 44.1 kHz -- finer than a pixel at any zoom a clip is
/// drawn at, and 1/256th of the file in memory. See [`AudioSource::waveform`].
pub const MIP_BASE_FRAMES: usize = 256;

/// Mip levels built at load. Each is 8x coarser than the one before, so the
/// three cover 256, 2048 and 16 384 frames per bin -- roughly "one bar of a
/// loop", "one clip on screen" and "the whole arrangement".
pub const MIP_LEVELS: usize = 3;

/// Ratio between one mip level and the next.
pub const MIP_RATIO: usize = 8;

/// Bins [`AudioSource::waveform`] will return at most.
///
/// The IPC spike measured a 64 kB message as free; 4096 bins is 8192 floats,
/// which is roughly 40 kB of JSON and sits inside that. A UI that wants more
/// than one pair per pixel at 4K is asking for detail no display can show.
pub const MAX_WAVEFORM_BINS: usize = 4_096;

/// Why a file could not become a source.
#[derive(Debug, thiserror::Error)]
pub enum AudioSourceError {
    /// The file could not be read or decoded. Carries the decoder's own
    /// sentence, which names the path.
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error("{path} is {seconds:.1} s long; a source may be at most {MAX_SOURCE_SECONDS:.0} s")]
    TooLong { path: PathBuf, seconds: f64 },
    #[error("a project may hold at most {MAX_SOURCES} audio sources")]
    TooManySources,
    #[error("there is no audio source {0}")]
    NoSuchSource(u64),
    /// Unloading a file that clips still play would silence them without saying
    /// so. The count is in the message because "it is in use" without "by how
    /// many" is not actionable.
    #[error("audio source {id} is still played by {clips} clip(s)")]
    StillReferenced { id: u64, clips: usize },
}

/// One waveform mip level: `min`/`max` per bin, over both channels.
///
/// One pair per bin rather than one per channel: a clip on a timeline is drawn
/// as a single envelope at the sizes this is for, and doubling the payload to
/// draw two envelopes nobody can tell apart at 40 pixels tall is not a trade.
/// The *outer* bound of the pair is what a waveform is -- so `min` is the
/// minimum across left and right and `max` the maximum, which never hides a
/// transient that is on one channel only.
#[derive(Debug, Clone, PartialEq)]
pub struct WaveformMip {
    /// Frames one bin covers.
    pub bin_frames: usize,
    pub min: Vec<f32>,
    pub max: Vec<f32>,
}

impl WaveformMip {
    /// Bins in this level.
    pub fn len(&self) -> usize {
        self.min.len()
    }

    /// Whether this level has no bins (a source of zero frames).
    pub fn is_empty(&self) -> bool {
        self.min.is_empty()
    }
}

/// One decoded audio file, immutable once built and shared by `Arc`.
///
/// The `Arc` is what makes a source cheap to hand to the audio thread: a
/// compiled arrangement of forty clips of the same vocal carries forty pointers,
/// not forty copies. It is also what makes unloading safe -- the last reference
/// goes away on the GC thread when the superseded arrangement is collected, so
/// no `free()` of a 200 MB buffer ever happens in a callback.
pub struct AudioSource {
    id: u64,
    path: PathBuf,
    /// Interleaved stereo, padded by
    /// [`PAD_LEAD`](mydaw_audio_file::PAD_LEAD) /
    /// [`PAD_TRAIL`](mydaw_audio_file::PAD_TRAIL) so the interpolator needs no
    /// bounds check.
    data: Vec<f32>,
    frames: usize,
    sample_rate: f64,
    /// Coarsest last. Empty for a missing file.
    mips: Vec<WaveformMip>,
    /// Whether this is a placeholder for a file that was not there.
    missing: bool,
}

/// Counts rather than a hundred megabytes of samples.
impl std::fmt::Debug for AudioSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioSource")
            .field("id", &self.id)
            .field("path", &self.path)
            .field("frames", &self.frames)
            .field("sample_rate", &self.sample_rate)
            .field("missing", &self.missing)
            .finish()
    }
}

impl AudioSource {
    /// Decode `path` under `id`. **Non-RT**: this reads a file and allocates.
    ///
    /// # Errors
    /// [`AudioSourceError::Decode`] for anything the decoder refused (the
    /// message names the file), or [`AudioSourceError::TooLong`] past
    /// [`MAX_SOURCE_SECONDS`].
    pub fn load(id: u64, path: &Path) -> Result<Self, AudioSourceError> {
        let cap = (MAX_SOURCE_SECONDS * 2.0) as usize;
        // The cap handed to the decoder is *twice* the limit, so that a file
        // just past the line is detected and reported rather than silently
        // truncated to exactly ten minutes. Anything wildly longer stops early
        // instead of being decoded in full only to be rejected.
        let decoded = decode_stereo(path, |rate| Some((rate as usize).saturating_mul(cap)))?;
        let seconds = decoded.seconds();
        if seconds > MAX_SOURCE_SECONDS {
            return Err(AudioSourceError::TooLong {
                path: path.to_path_buf(),
                seconds,
            });
        }
        let frames = decoded.frames;
        let sample_rate = decoded.sample_rate;
        let data = decoded.into_padded();
        let mips = build_mips(&data, frames);
        Ok(Self {
            id,
            path: path.to_path_buf(),
            data,
            frames,
            sample_rate,
            mips,
            missing: false,
        })
    }

    /// Build a source from interleaved stereo samples already in memory.
    ///
    /// The seam between "where the audio came from" and "what a source is". A
    /// file goes through [`load`](Self::load); this is what a caller with the
    /// samples in hand uses -- the tests, and (later) a recorded take, which has
    /// never been a file at the moment it becomes a clip.
    ///
    /// `data` must be interleaved stereo and **unpadded**; the padding and the
    /// mipmaps are built here.
    pub fn from_stereo(id: u64, path: &Path, sample_rate: f64, data: Vec<f32>) -> Self {
        let frames = data.len() / CHANNELS;
        let decoded = mydaw_audio_file::DecodedAudio {
            data,
            frames,
            sample_rate,
            truncated: false,
        };
        let data = decoded.into_padded();
        let mips = build_mips(&data, frames);
        Self {
            id,
            path: path.to_path_buf(),
            data,
            frames,
            sample_rate,
            mips,
            missing: false,
        }
    }

    /// A silent placeholder for a file that could not be found or read.
    ///
    /// Zero frames, so every voice that starts on it ends immediately and the
    /// clip is simply inaudible. See the module docs on why this is better than
    /// dropping the clips.
    pub fn missing(id: u64, path: &Path) -> Self {
        Self {
            id,
            path: path.to_path_buf(),
            // Padding only: `taps` is never reached, because `frames == 0`
            // stops a voice before its first sample, but a `data` that is empty
            // would make an index into it a panic rather than a dead branch.
            data: vec![0.0; (PAD_LEAD + mydaw_audio_file::PAD_TRAIL) * CHANNELS],
            frames: 0,
            sample_rate: 0.0,
            mips: Vec::new(),
            missing: true,
        }
    }

    /// The id clips reference.
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// Where the file was when it was loaded.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The file's own name, for a UI that has no room for a full path.
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.to_string_lossy().into_owned())
    }

    /// Playable frames, excluding the padding. `0` for a missing file.
    pub const fn frames(&self) -> usize {
        self.frames
    }

    /// The rate the file was recorded at, which need not be the device's.
    pub const fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// Seconds of audio.
    pub fn seconds(&self) -> f64 {
        if self.sample_rate > 0.0 {
            self.frames as f64 / self.sample_rate
        } else {
            0.0
        }
    }

    /// Whether this is a placeholder for a file that was not there.
    pub const fn is_missing(&self) -> bool {
        self.missing
    }

    /// Bytes of audio this source holds.
    pub fn bytes(&self) -> usize {
        std::mem::size_of_val(self.data.as_slice())
            + self
                .mips
                .iter()
                .map(|mip| std::mem::size_of_val(mip.min.as_slice()) * 2)
                .sum::<usize>()
    }

    /// The padded sample buffer, for the audio thread's interpolator.
    ///
    /// Index it with [`mydaw_audio_file::taps`], which is what the padding is
    /// for.
    #[inline]
    pub fn padded(&self) -> &[f32] {
        &self.data
    }

    /// Interleaved stereo content, excluding interpolator padding. **Non-RT.**
    pub fn stereo(&self) -> &[f32] {
        let start = PAD_LEAD * CHANNELS;
        let end = start.saturating_add(self.frames.saturating_mul(CHANNELS));
        let end = end.min(self.data.len());
        if start >= end {
            &[]
        } else {
            &self.data[start..end]
        }
    }

    /// The mip levels built at load, finest first.
    pub fn mips(&self) -> &[WaveformMip] {
        &self.mips
    }

    /// `target_bins` min/max pairs spanning the whole file. **Non-RT.**
    ///
    /// Served from the coarsest mip level that still has *at least* `target_bins`
    /// bins, then folded down by whole bins -- so the answer is always the
    /// true min and max of the frames it covers, never a resample of a resample.
    /// A request for more bins than the finest mip is answered from the samples
    /// themselves: mip 0 is a 256-frame overview for clips, and a short kick in
    /// the sample editor needs the actual envelope, not that overview stretched.
    /// Past one bin per frame there is nothing to add.
    ///
    /// Clamped to [`MAX_WAVEFORM_BINS`]. Empty for a missing file.
    pub fn waveform(&self, target_bins: usize) -> Vec<(f32, f32)> {
        let target = target_bins.clamp(1, MAX_WAVEFORM_BINS);
        let Some(finest) = self.mips.first() else {
            return Vec::new();
        };
        if finest.is_empty() || self.frames == 0 {
            return Vec::new();
        }
        if target > finest.len() {
            return fold_padded_peaks(&self.data, self.frames, target.min(self.frames));
        }
        // Coarsest level that still has enough bins.
        let level = self
            .mips
            .iter()
            .rev()
            .find(|mip| mip.len() >= target)
            .unwrap_or(finest);

        let bins = level.len();
        let out_bins = target.min(bins);
        let mut out = Vec::with_capacity(out_bins);
        for bin in 0..out_bins {
            // Whole-bin spans, so every source bin lands in exactly one output
            // bin and none is skipped. The partition uses `out_bins`, not the
            // (possibly larger) request: dividing by `target` while looping
            // `bins` times only scanned the start of a short file.
            let start = bin * bins / out_bins;
            let end = ((bin + 1) * bins / out_bins).max(start + 1).min(bins);
            let mut min = f32::INFINITY;
            let mut max = f32::NEG_INFINITY;
            for index in start..end {
                min = min.min(level.min[index]);
                max = max.max(level.max[index]);
            }
            out.push((min, max));
        }
        out
    }
}

/// Min/max pairs over padded stereo `data`, one pair per output bin.
fn fold_padded_peaks(data: &[f32], frames: usize, bins: usize) -> Vec<(f32, f32)> {
    let bins = bins.max(1).min(frames);
    let mut out = Vec::with_capacity(bins);
    for bin in 0..bins {
        let start = bin * frames / bins;
        let end = ((bin + 1) * frames / bins).max(start + 1).min(frames);
        let mut lo = f32::INFINITY;
        let mut hi = f32::NEG_INFINITY;
        for frame in start..end {
            let base = (frame + PAD_LEAD) * CHANNELS;
            for channel in 0..CHANNELS {
                let sample = data[base + channel];
                lo = lo.min(sample);
                hi = hi.max(sample);
            }
        }
        out.push((lo, hi));
    }
    out
}

/// Build [`MIP_LEVELS`] levels over padded stereo `data`.
///
/// Level 0 is computed from the samples; every level after it folds
/// [`MIP_RATIO`] bins of the one before, which is what makes the whole set cost
/// one pass over the audio plus a geometric tail rather than three passes.
fn build_mips(data: &[f32], frames: usize) -> Vec<WaveformMip> {
    if frames == 0 {
        return Vec::new();
    }
    let mut levels = Vec::with_capacity(MIP_LEVELS);

    let bins = frames.div_ceil(MIP_BASE_FRAMES);
    let mut min = Vec::with_capacity(bins);
    let mut max = Vec::with_capacity(bins);
    for bin in 0..bins {
        let first = bin * MIP_BASE_FRAMES;
        let last = ((bin + 1) * MIP_BASE_FRAMES).min(frames);
        let mut lo = f32::INFINITY;
        let mut hi = f32::NEG_INFINITY;
        for frame in first..last {
            // Content frame `frame` sits at padded frame `frame + PAD_LEAD`.
            let base = (frame + PAD_LEAD) * CHANNELS;
            for channel in 0..CHANNELS {
                let sample = data[base + channel];
                lo = lo.min(sample);
                hi = hi.max(sample);
            }
        }
        min.push(lo);
        max.push(hi);
    }
    levels.push(WaveformMip {
        bin_frames: MIP_BASE_FRAMES,
        min,
        max,
    });

    while levels.len() < MIP_LEVELS {
        let previous = levels.last().expect("level 0 was just pushed");
        if previous.len() <= 1 {
            break;
        }
        let bins = previous.len().div_ceil(MIP_RATIO);
        let mut min = Vec::with_capacity(bins);
        let mut max = Vec::with_capacity(bins);
        for bin in 0..bins {
            let first = bin * MIP_RATIO;
            let last = ((bin + 1) * MIP_RATIO).min(previous.len());
            let mut lo = f32::INFINITY;
            let mut hi = f32::NEG_INFINITY;
            for index in first..last {
                lo = lo.min(previous.min[index]);
                hi = hi.max(previous.max[index]);
            }
            min.push(lo);
            max.push(hi);
        }
        let bin_frames = previous.bin_frames * MIP_RATIO;
        levels.push(WaveformMip {
            bin_frames,
            min,
            max,
        });
    }

    levels
}

/// Every audio file the session has loaded, by id.
///
/// `Send + Sync` and never locked from the audio thread: the callback holds
/// `Arc<AudioSource>`s handed to it inside a compiled arrangement, and asks this
/// nothing.
///
/// # Ref counting
/// The `Arc` count *is* the reference count, and it spans the RT wall: a source
/// dropped from this table while a compiled arrangement still names it stays
/// alive until that arrangement is collected. What the registry additionally
/// refuses is unloading a source the **playlist** still references, which is a
/// question about the project rather than about memory -- see
/// [`unload`](Self::unload).
pub struct AudioSourceRegistry {
    sources: Mutex<Table>,
}

#[derive(Default)]
struct Table {
    by_id: BTreeMap<u64, Arc<AudioSource>>,
    next_id: u64,
}

impl Default for AudioSourceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioSourceRegistry {
    /// An empty registry. Ids start at 1, so `0` is never a source.
    pub fn new() -> Self {
        Self {
            sources: Mutex::new(Table {
                by_id: BTreeMap::new(),
                next_id: 1,
            }),
        }
    }

    /// Load `path`, or return the source already loaded from it.
    ///
    /// Loading the same file twice returns the **same** source rather than a
    /// second copy of it: a user dragging one loop onto four lanes has one file,
    /// and four decodes of it would be four times the memory for no difference
    /// anyone can hear.
    ///
    /// # Errors
    /// [`AudioSourceError::Decode`], [`AudioSourceError::TooLong`], or
    /// [`AudioSourceError::TooManySources`].
    pub fn load(&self, path: &Path) -> Result<Arc<AudioSource>, AudioSourceError> {
        if let Some(existing) = self.find_by_path(path) {
            if !existing.is_missing() {
                return Ok(existing);
            }
            return self.load_with_id(existing.id(), path);
        }
        // The id is reserved first and the decode runs *outside* the lock:
        // reading a hundred megabytes off a disk must not stop a command thread
        // from listing what is already loaded. A decode that then fails leaves a
        // gap in the sequence, which costs nothing -- an id is opaque.
        let id = {
            let mut table = self.sources.lock();
            if let Some(existing) = table
                .by_id
                .values()
                .find(|candidate| candidate.path() == path)
            {
                if !existing.is_missing() {
                    return Ok(Arc::clone(existing));
                }
                let id = existing.id();
                drop(table);
                return self.load_with_id(id, path);
            }
            if table.by_id.len() >= MAX_SOURCES {
                return Err(AudioSourceError::TooManySources);
            }
            let id = table.next_id;
            table.next_id += 1;
            id
        };
        let source = Arc::new(AudioSource::load(id, path)?);

        let mut table = self.sources.lock();
        // Another thread may have finished the same path while this decode ran.
        // Whoever landed first wins; ours is dropped here, on this thread.
        if let Some(existing) = table
            .by_id
            .values()
            .find(|candidate| candidate.path() == path)
        {
            if !existing.is_missing() {
                return Ok(Arc::clone(existing));
            }
        }
        table.by_id.insert(id, Arc::clone(&source));
        Ok(source)
    }

    /// Load `path` **under an id the caller chose** -- the project-load path.
    ///
    /// A saved project's clips name ids, so restoring one has to put the file
    /// back under the id it had. Replaces whatever was at `id`.
    ///
    /// # Errors
    /// As [`load`](Self::load), minus the id allocation.
    pub fn load_with_id(&self, id: u64, path: &Path) -> Result<Arc<AudioSource>, AudioSourceError> {
        let source = Arc::new(AudioSource::load(id, path)?);
        let mut table = self.sources.lock();
        table.by_id.insert(id, Arc::clone(&source));
        table.next_id = table.next_id.max(id + 1);
        Ok(source)
    }

    /// Put a **silent placeholder** at `id` for a file that was not there.
    ///
    /// The project-load path's other half. See the module docs.
    pub fn register_missing(&self, id: u64, path: &Path) -> Arc<AudioSource> {
        let source = Arc::new(AudioSource::missing(id, path));
        let mut table = self.sources.lock();
        table.by_id.insert(id, Arc::clone(&source));
        table.next_id = table.next_id.max(id + 1);
        source
    }

    /// The source at `id`, if there is one.
    pub fn get(&self, id: u64) -> Option<Arc<AudioSource>> {
        self.sources.lock().by_id.get(&id).cloned()
    }

    /// The source loaded from `path`, if there is one.
    pub fn find_by_path(&self, path: &Path) -> Option<Arc<AudioSource>> {
        self.sources
            .lock()
            .by_id
            .values()
            .find(|source| source.path() == path)
            .cloned()
    }

    /// Every source, by ascending id.
    pub fn list(&self) -> Vec<Arc<AudioSource>> {
        self.sources.lock().by_id.values().cloned().collect()
    }

    /// How many sources are loaded.
    pub fn len(&self) -> usize {
        self.sources.lock().by_id.len()
    }

    /// Whether nothing is loaded.
    pub fn is_empty(&self) -> bool {
        self.sources.lock().by_id.is_empty()
    }

    /// Bytes of audio the registry holds.
    pub fn bytes(&self) -> usize {
        self.sources
            .lock()
            .by_id
            .values()
            .map(|source| source.bytes())
            .sum()
    }

    /// Drop `id`, refusing while `playlist` still has clips that play it.
    ///
    /// Explicit rather than automatic: a user who deletes a clip and then draws
    /// it again a second later should not pay for a re-decode, so removing the
    /// last clip does **not** unload the file. What does unload it is this call,
    /// [`retain`](Self::retain) at project-load time, and closing the project.
    ///
    /// # Errors
    /// [`AudioSourceError::NoSuchSource`] or
    /// [`AudioSourceError::StillReferenced`].
    pub fn unload(&self, id: u64, playlist: &Playlist) -> Result<(), AudioSourceError> {
        let clips = reference_count(playlist, id);
        if clips > 0 {
            return Err(AudioSourceError::StillReferenced { id, clips });
        }
        let mut table = self.sources.lock();
        table
            .by_id
            .remove(&id)
            .map(|_| ())
            .ok_or(AudioSourceError::NoSuchSource(id))
    }

    /// Keep only the ids in `keep`, dropping the rest. Returns how many went.
    ///
    /// The project-load path: a freshly opened file's sources are exactly the
    /// ones its table names, and whatever the previous project loaded is not
    /// part of this one.
    pub fn retain(&self, keep: &[u64]) -> usize {
        let mut table = self.sources.lock();
        let before = table.by_id.len();
        table.by_id.retain(|id, _| keep.contains(id));
        before - table.by_id.len()
    }

    /// Drop everything. Returns how many sources went.
    pub fn clear(&self) -> usize {
        let mut table = self.sources.lock();
        let count = table.by_id.len();
        table.by_id.clear();
        count
    }
}

impl std::fmt::Debug for AudioSourceRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let table = self.sources.lock();
        f.debug_struct("AudioSourceRegistry")
            .field("sources", &table.by_id.len())
            .field("next_id", &table.next_id)
            .finish()
    }
}

/// How many clips in `playlist` play source `id`.
///
/// Muted lanes count: a mute is not a deletion, and unloading a file because
/// every clip that plays it happens to be silenced would be a surprise on unmute.
pub fn reference_count(playlist: &Playlist, id: u64) -> usize {
    playlist
        .clips()
        .iter()
        .filter(|clip| matches!(clip, crate::Clip::Audio(audio) if audio.source_id == id))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory that removes itself.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let base = std::env::temp_dir().join(format!(
                "mydaw-audio-source-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&base);
            std::fs::create_dir_all(&base).expect("temp dir");
            Self(base)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Write `samples` (interleaved, `channels` wide) as a 32-bit float WAV.
    fn write_wav(path: &Path, rate: u32, channels: u16, samples: &[f32]) {
        let spec = hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(path, spec).expect("wav writer");
        for sample in samples {
            writer.write_sample(*sample).expect("wav sample");
        }
        writer.finalize().expect("wav finalize");
    }

    /// A sine of `hz` at `rate`, `frames` long, mono.
    fn sine(hz: f64, rate: u32, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|n| ((n as f64 / f64::from(rate)) * hz * std::f64::consts::TAU).sin() as f32 * 0.5)
            .collect()
    }

    #[test]
    fn a_wav_becomes_a_source_that_knows_its_length_and_rate() {
        let dir = TempDir::new("load");
        let path = dir.path().join("take.wav");
        write_wav(&path, 44_100, 1, &sine(441.0, 44_100, 4_410));

        let source = AudioSource::load(7, &path).expect("loads");
        assert_eq!(source.id(), 7);
        assert_eq!(source.frames(), 4_410);
        assert_eq!(source.sample_rate(), 44_100.0);
        assert!((source.seconds() - 0.1).abs() < 1e-9);
        assert!(!source.is_missing());
        assert_eq!(source.name(), "take.wav");
    }

    #[test]
    fn the_padded_buffer_is_indexable_at_both_ends_without_a_bounds_check() {
        let dir = TempDir::new("pad");
        let path = dir.path().join("pad.wav");
        write_wav(&path, 8_000, 2, &[1.0, -1.0, 0.5, -0.5, 0.25, -0.25]);
        let source = AudioSource::load(1, &path).expect("loads");

        // The first and last content frames both resolve four taps.
        let first = mydaw_audio_file::taps(source.padded(), 0, 0);
        assert_eq!(first[0], 0.0, "the lead pad");
        assert!((first[1] - 1.0).abs() < 1e-6);
        let last = mydaw_audio_file::taps(source.padded(), source.frames() - 1, 1);
        assert!((last[1] + 0.25).abs() < 1e-6);
        assert_eq!(last[2], 0.0, "the trail pad");
    }

    #[test]
    fn a_missing_file_is_a_silent_source_rather_than_a_refusal() {
        let source = AudioSource::missing(4, Path::new("C:/gone/vox.wav"));
        assert_eq!(source.id(), 4);
        assert_eq!(source.frames(), 0);
        assert!(source.is_missing());
        assert!(source.waveform(64).is_empty());
        assert_eq!(source.name(), "vox.wav");
    }

    #[test]
    fn a_file_that_is_not_audio_names_itself_in_the_error() {
        let dir = TempDir::new("garbage");
        let path = dir.path().join("notes.wav");
        std::fs::write(&path, b"these are not samples").unwrap();
        let err = AudioSource::load(1, &path).unwrap_err();
        assert!(matches!(err, AudioSourceError::Decode(_)), "{err}");
        assert!(err.to_string().contains("notes.wav"), "{err}");
    }

    // ------------------------------------------------------------- mipmaps

    /// The brute-force answer: min and max over exactly the frames a bin covers.
    fn brute_force(samples: &[f32], bins: usize) -> Vec<(f32, f32)> {
        let frames = samples.len();
        (0..bins)
            .map(|bin| {
                let first = bin * frames / bins;
                let last = ((bin + 1) * frames / bins).max(first + 1).min(frames);
                let mut lo = f32::INFINITY;
                let mut hi = f32::NEG_INFINITY;
                for sample in &samples[first..last] {
                    lo = lo.min(*sample);
                    hi = hi.max(*sample);
                }
                (lo, hi)
            })
            .collect()
    }

    #[test]
    fn the_finest_mip_is_the_true_min_and_max_of_every_bin() {
        let dir = TempDir::new("mip-exact");
        let path = dir.path().join("mip.wav");
        // Not a multiple of the bin size, so the ragged last bin is covered too.
        let samples = sine(37.0, 8_000, MIP_BASE_FRAMES * 5 + 91);
        write_wav(&path, 8_000, 1, &samples);
        let source = AudioSource::load(1, &path).expect("loads");

        let level = &source.mips()[0];
        assert_eq!(level.bin_frames, MIP_BASE_FRAMES);
        assert_eq!(level.len(), samples.len().div_ceil(MIP_BASE_FRAMES));
        for bin in 0..level.len() {
            let first = bin * MIP_BASE_FRAMES;
            let last = ((bin + 1) * MIP_BASE_FRAMES).min(samples.len());
            let lo = samples[first..last]
                .iter()
                .copied()
                .fold(f32::MAX, f32::min);
            let hi = samples[first..last]
                .iter()
                .copied()
                .fold(f32::MIN, f32::max);
            // The WAV round trip is exact for 32-bit float, so this is equality.
            assert!(
                (level.min[bin] - lo).abs() < 1e-6 && (level.max[bin] - hi).abs() < 1e-6,
                "bin {bin}: mip ({}, {}) vs samples ({lo}, {hi})",
                level.min[bin],
                level.max[bin]
            );
        }
    }

    #[test]
    fn every_coarser_level_bounds_the_one_below_it() {
        let dir = TempDir::new("mip-levels");
        let path = dir.path().join("long.wav");
        write_wav(&path, 8_000, 1, &sine(11.0, 8_000, MIP_BASE_FRAMES * 200));
        let source = AudioSource::load(1, &path).expect("loads");

        assert_eq!(source.mips().len(), MIP_LEVELS);
        for pair in source.mips().windows(2) {
            let (fine, coarse) = (&pair[0], &pair[1]);
            assert_eq!(coarse.bin_frames, fine.bin_frames * MIP_RATIO);
            assert_eq!(coarse.len(), fine.len().div_ceil(MIP_RATIO));
            for bin in 0..fine.len() {
                let parent = bin / MIP_RATIO;
                assert!(coarse.min[parent] <= fine.min[bin] + 1e-7);
                assert!(coarse.max[parent] >= fine.max[bin] - 1e-7);
            }
        }
    }

    #[test]
    fn a_waveform_query_matches_a_brute_force_scan_of_the_samples() {
        let dir = TempDir::new("waveform");
        let path = dir.path().join("wave.wav");
        // A whole number of base bins, and bin counts that divide it, so a mip
        // fold and a direct scan cover exactly the same frames. Counts above
        // the finest mip (64 here) take the sample-scan path.
        let frames = MIP_BASE_FRAMES * 64;
        let samples = sine(23.0, 8_000, frames);
        write_wav(&path, 8_000, 1, &samples);
        let source = AudioSource::load(1, &path).expect("loads");

        for bins in [8_usize, 16, 64, 256, 1_024] {
            let mipped = source.waveform(bins);
            let direct = brute_force(&samples, bins);
            assert_eq!(mipped.len(), bins);
            for (index, (got, want)) in mipped.iter().zip(direct.iter()).enumerate() {
                assert!(
                    (got.0 - want.0).abs() < 1e-6 && (got.1 - want.1).abs() < 1e-6,
                    "{bins} bins, bin {index}: {got:?} vs {want:?}"
                );
            }
        }
    }

    #[test]
    fn a_waveform_never_hides_a_transient_that_is_on_one_channel_only() {
        let dir = TempDir::new("waveform-stereo");
        let path = dir.path().join("stereo.wav");
        // Left is silent; right has one spike. A per-channel average would lose
        // it; the outer bound must not.
        let mut samples = vec![0.0_f32; MIP_BASE_FRAMES * 2 * CHANNELS];
        samples[10 * CHANNELS + 1] = 0.9;
        samples[11 * CHANNELS + 1] = -0.8;
        write_wav(&path, 8_000, 2, &samples);
        let source = AudioSource::load(1, &path).expect("loads");

        let wave = source.waveform(2);
        assert!((wave[0].1 - 0.9).abs() < 1e-6, "{:?}", wave[0]);
        assert!((wave[0].0 + 0.8).abs() < 1e-6, "{:?}", wave[0]);
    }

    #[test]
    fn asking_for_more_bins_than_frames_gives_one_bin_per_frame() {
        let frames = 50;
        let source = AudioSource::from_stereo(
            1,
            Path::new("tiny.wav"),
            8_000.0,
            sine(50.0, 8_000, frames)
                .into_iter()
                .flat_map(|sample| [sample, sample])
                .collect(),
        );

        let wave = source.waveform(10_000);
        assert_eq!(
            wave.len(),
            frames,
            "finer than one bin per frame would invent samples"
        );
    }

    #[test]
    fn a_short_decaying_sample_keeps_its_shape_when_the_ui_asks_for_dense_bins() {
        // Kick-length: a few thousand frames, so mip 0 has a handful of 256-frame
        // bins while the sample editor asks for 1024. Folding with `target` as the
        // denominator and `mip.len()` as the loop bound only scanned the attack,
        // so the picture was a solid bar.
        let frames = MIP_BASE_FRAMES * 8;
        let samples: Vec<f32> = (0..frames)
            .map(|n| 0.9 * (-6.0 * n as f32 / frames as f32).exp())
            .collect();
        let source = AudioSource::from_stereo(
            1,
            Path::new("kick.wav"),
            48_000.0,
            samples
                .iter()
                .copied()
                .flat_map(|sample| [sample, sample])
                .collect(),
        );

        let wave = source.waveform(1_024);
        assert!(
            wave.len() > source.mips()[0].len(),
            "the sample editor needs the samples, not the overview mip; got {} bins",
            wave.len()
        );
        assert_eq!(wave.len(), 1_024.min(frames));

        let quarter = wave.len() / 4;
        let head = wave[..quarter]
            .iter()
            .fold(0.0_f32, |acc, pair| acc.max(pair.1));
        let tail = wave[wave.len() - quarter..]
            .iter()
            .fold(0.0_f32, |acc, pair| acc.max(pair.1));
        assert!(
            head > tail * 2.0,
            "a decaying hit must stay decaying; head {head} vs tail {tail}"
        );
        let last = wave.last().expect("non-empty").1;
        assert!(
            last < 0.2,
            "the last bin must be the quiet end, not another copy of the attack ({last})"
        );
    }

    #[test]
    fn a_waveform_is_clamped_to_what_a_message_can_carry() {
        let dir = TempDir::new("waveform-cap");
        let path = dir.path().join("big.wav");
        write_wav(
            &path,
            8_000,
            1,
            &sine(3.0, 8_000, MIP_BASE_FRAMES * (MAX_WAVEFORM_BINS + 500)),
        );
        let source = AudioSource::load(1, &path).expect("loads");
        assert_eq!(source.waveform(usize::MAX).len(), MAX_WAVEFORM_BINS);
        assert_eq!(source.waveform(0).len(), 1, "zero is not a bin count");
    }

    // ------------------------------------------------------------ registry

    #[test]
    fn the_registry_hands_out_ascending_ids_starting_at_one() {
        let dir = TempDir::new("registry-ids");
        let registry = AudioSourceRegistry::new();
        let mut ids = Vec::new();
        for index in 0..3 {
            let path = dir.path().join(format!("take{index}.wav"));
            write_wav(&path, 8_000, 1, &sine(100.0, 8_000, 800));
            ids.push(registry.load(&path).expect("loads").id());
        }
        assert_eq!(ids, vec![1, 2, 3]);
        assert_eq!(registry.len(), 3);
        assert_eq!(
            registry.list().iter().map(|s| s.id()).collect::<Vec<_>>(),
            ids
        );
    }

    #[test]
    fn loading_the_same_file_twice_gives_the_same_source_not_a_second_copy() {
        let dir = TempDir::new("registry-dedupe");
        let path = dir.path().join("loop.wav");
        write_wav(&path, 8_000, 1, &sine(100.0, 8_000, 800));
        let registry = AudioSourceRegistry::new();

        let first = registry.load(&path).expect("loads");
        let second = registry.load(&path).expect("loads");
        assert!(Arc::ptr_eq(&first, &second), "one file, one decode");
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn a_project_load_puts_a_file_back_under_the_id_its_clips_name() {
        let dir = TempDir::new("registry-with-id");
        let path = dir.path().join("vox.wav");
        write_wav(&path, 8_000, 1, &sine(100.0, 8_000, 800));
        let registry = AudioSourceRegistry::new();

        let restored = registry.load_with_id(42, &path).expect("loads");
        assert_eq!(restored.id(), 42);
        assert_eq!(registry.get(42).expect("is there").id(), 42);
        // And the next fresh id is past the restored one, so nothing collides.
        let other = dir.path().join("other.wav");
        write_wav(&other, 8_000, 1, &sine(100.0, 8_000, 800));
        assert_eq!(registry.load(&other).expect("loads").id(), 43);
    }

    #[test]
    fn a_missing_file_keeps_its_id_and_is_silent() {
        let registry = AudioSourceRegistry::new();
        let placeholder = registry.register_missing(9, Path::new("D:/moved/vox.wav"));
        assert!(placeholder.is_missing());
        assert_eq!(registry.get(9).expect("is there").frames(), 0);
    }

    #[test]
    fn loading_a_path_that_was_missing_replaces_the_placeholder() {
        let dir = TempDir::new("registry-reload-missing");
        let path = dir.path().join("found.wav");
        let registry = AudioSourceRegistry::new();
        let placeholder = registry.register_missing(9, &path);
        assert!(placeholder.is_missing());

        write_wav(&path, 8_000, 1, &sine(100.0, 8_000, 800));
        let restored = registry
            .load(&path)
            .expect("decodes the file that appeared");
        assert_eq!(restored.id(), 9);
        assert!(!restored.is_missing());
        assert!(restored.frames() > 0);
        assert!(Arc::ptr_eq(&registry.get(9).expect("same id"), &restored));
    }

    #[test]
    fn unloading_a_source_a_clip_still_plays_is_refused_and_says_how_many() {
        let dir = TempDir::new("registry-unload");
        let path = dir.path().join("used.wav");
        write_wav(&path, 8_000, 1, &sine(100.0, 8_000, 800));
        let registry = AudioSourceRegistry::new();
        let source = registry.load(&path).expect("loads");

        let mut playlist = Playlist::default();
        playlist
            .add_audio_clip(source.id(), 0, 0, 3_840, 0, 0.0)
            .expect("places");
        playlist
            .add_audio_clip(source.id(), 1, 0, 3_840, 0, 0.0)
            .expect("places");

        let err = registry.unload(source.id(), &playlist).unwrap_err();
        assert!(
            matches!(err, AudioSourceError::StillReferenced { clips: 2, .. }),
            "{err}"
        );
        assert!(err.to_string().contains("2 clip"), "{err}");

        // With the clips gone it unloads, and unloading twice is an error that
        // names the id rather than a silent no-op.
        let empty = Playlist::default();
        registry.unload(source.id(), &empty).expect("unloads");
        assert!(registry.is_empty());
        assert!(matches!(
            registry.unload(source.id(), &empty).unwrap_err(),
            AudioSourceError::NoSuchSource(_)
        ));
    }

    #[test]
    fn a_muted_lane_still_counts_as_a_reference() {
        let mut playlist = Playlist::default();
        playlist.add_audio_clip(3, 0, 0, 3_840, 0, 0.0).expect("ok");
        playlist.set_track_muted(0, true).expect("mutes");
        assert_eq!(reference_count(&playlist, 3), 1);
        assert_eq!(reference_count(&playlist, 4), 0);
    }

    #[test]
    fn a_project_load_keeps_only_the_sources_the_new_file_names() {
        let dir = TempDir::new("registry-retain");
        let registry = AudioSourceRegistry::new();
        for index in 0..3 {
            let path = dir.path().join(format!("t{index}.wav"));
            write_wav(&path, 8_000, 1, &sine(100.0, 8_000, 400));
            registry.load(&path).expect("loads");
        }
        assert_eq!(registry.retain(&[2]), 2);
        assert_eq!(registry.len(), 1);
        assert!(registry.get(2).is_some());
        assert_eq!(registry.clear(), 1);
        assert!(registry.is_empty());
    }

    #[test]
    fn a_source_reports_the_memory_it_is_holding() {
        let dir = TempDir::new("registry-bytes");
        let path = dir.path().join("big.wav");
        write_wav(&path, 8_000, 2, &vec![0.1_f32; 8_000 * CHANNELS]);
        let registry = AudioSourceRegistry::new();
        let source = registry.load(&path).expect("loads");
        // 8000 frames of padded stereo f32 is at least 64 kB.
        assert!(source.bytes() >= 8_000 * CHANNELS * 4, "{}", source.bytes());
        assert_eq!(registry.bytes(), source.bytes());
    }
}
