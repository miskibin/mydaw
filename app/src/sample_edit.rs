//! Destructive edits to the file behind a sample channel: reverse, normalize,
//! trim and gain.
//!
//! ```text
//!   channel ──▶ AudioSource 7  "Kick.wav"          (untouched, still shared)
//!      │
//!      │  edit_track_sample(track, Reverse)
//!      ▼
//!   channel ──▶ AudioSource 12 "Kick (reversed).wav"
//! ```
//!
//! # Copy on write, always
//!
//! An [`AudioSource`] is immutable and shared by `Arc` across the RT wall (see
//! `mydaw_plugin_sequencer::audio`), and two channels — or forty playlist clips
//! — routinely point at the same one. Editing the buffer in place would mean
//! reaching through an `Arc` the audio thread is reading, and it would change a
//! sound the user did not select. So every edit here **writes a new 32-bit
//! float WAV**, loads it as a **new source id**, and repoints only the channel
//! that was edited plus that channel's own playlist clips.
//!
//! That has three consequences worth stating plainly:
//!
//! 1. Another channel on the same file keeps playing the original.
//! 2. The original stays loaded, so undo is a **pointer swap** rather than a
//!    re-decode — see [`AppState::set_track_audio_source`].
//! 3. The written file is a real file on disk. A project saved after an edit
//!    references it, which is the same contract a dropped sample has.
//!
//! # The undo story
//!
//! The engine has no undo stack (see `ui/src/lib/undo.ts`): every command is a
//! forward edit and the shell keeps a stack of inverses. A destructive sample
//! edit fits that model exactly, because the inverse of "point this channel at
//! source 12, region 0..0" is "point it at source 7, region 4410..28000" — one
//! command, one Ctrl+Z, no re-encode. [`SampleEditDto`] carries both halves so
//! the shell can push the pair the moment the edit lands.
//!
//! # Where the files go
//!
//! Beside the factory drum renders, under the same cache root
//! (`crate::user_dirs`): `WAVE Studio/sample-edits/`. Names follow the file
//! that was edited — `Kick (reversed).wav`, `Kick (+3.0 dB).wav` — and collide
//! into ` 2`, ` 3` rather than overwriting, because the registry keys sources
//! by path and reusing a path with different audio behind it would hand the
//! wrong buffer to whoever still holds the old id.

use std::path::{Path, PathBuf};

use mydaw_audio_file::CHANNELS;
use mydaw_plugin_sequencer::AudioSourceError;
use serde::{Deserialize, Serialize};

use crate::dto::AudioSourceDto;
use crate::project::{MixerStateDto, TrackError};
use crate::source::SourceError;
use crate::state::AppState;
use crate::tracks::MixerError;

/// Peak a Normalize lands on, in dBFS.
///
/// Not 0.0: a file that peaks at exactly full scale clips the moment anything
/// downstream — a resampler's overshoot, a pitch change, an intersample peak in
/// a converter — adds a fraction of a dB. Three tenths of a dB is inaudible as
/// level and is the headroom every mastering guide asks for.
pub const NORMALIZE_TARGET_DBFS: f32 = -0.3;

/// Largest gain a single Gain edit will apply, in dB, either way.
///
/// Bounded because this is destructive: +60 dB on a quiet take is not a gain
/// stage, it is a mistake that has already been written to disk.
pub const MAX_EDIT_GAIN_DB: f32 = 24.0;

/// Below this peak a buffer is silence, and normalizing it would only raise the
/// noise floor by however many dB the dither happens to sit at.
const SILENCE_PEAK: f32 = 1.0e-6;

/// One destructive operation, as the webview names it.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum SampleOp {
    /// Play the file backwards. The played region is mirrored with it, so what
    /// you were hearing is what you hear reversed.
    Reverse,
    /// Scale the **whole file** so its loudest sample lands on
    /// [`NORMALIZE_TARGET_DBFS`].
    ///
    /// The whole file rather than the region: normalizing to a region's peak
    /// would leave the audio outside it above full scale, and a destructive
    /// edit must not create a file that is hotter than it claims to be. Trim
    /// first and the two compose into exactly the region behaviour.
    Normalize,
    /// Crop the file to the played region and reset the region to the whole of
    /// what is left.
    Trim,
    /// Scale the whole file by `db`, clamped to ±[`MAX_EDIT_GAIN_DB`].
    Gain { db: f32 },
}

impl SampleOp {
    /// What the written file is called, in `Kick (<suffix>).wav`.
    fn suffix(self) -> String {
        match self {
            Self::Reverse => "reversed".to_owned(),
            Self::Normalize => "normalized".to_owned(),
            Self::Trim => "trimmed".to_owned(),
            Self::Gain { db } => format!("{:+.1} dB", clamp_gain_db(db)),
        }
    }

    /// What the status bar says.
    pub fn label(self) -> &'static str {
        match self {
            Self::Reverse => "Reverse",
            Self::Normalize => "Normalize",
            Self::Trim => "Trim",
            Self::Gain { .. } => "Gain",
        }
    }
}

/// Why a destructive edit was refused.
#[derive(Debug, thiserror::Error)]
pub enum SampleEditError {
    #[error(transparent)]
    Mixer(#[from] MixerError),
    #[error("track {0} plays no audio file")]
    NotASampleChannel(usize),
    #[error("audio source {0} is not loaded")]
    MissingSource(u64),
    #[error("{0} has no audio to edit")]
    Empty(String),
    #[error("{0} is silent; there is nothing to normalize")]
    Silent(String),
    #[error("Trim needs a region shorter than the whole file — drag one on the waveform first")]
    NothingToTrim,
    #[error(transparent)]
    Audio(#[from] AudioSourceError),
    #[error("could not write {path}: {message}")]
    Io { path: String, message: String },
}

impl From<TrackError> for SampleEditError {
    fn from(err: TrackError) -> Self {
        Self::Mixer(err.into())
    }
}

impl From<SourceError> for SampleEditError {
    fn from(err: SourceError) -> Self {
        Self::Mixer(MixerError::Source(err))
    }
}

/// What an edit did, in the shape the shell needs to record an undo entry.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SampleEditDto {
    /// The channel that was edited.
    pub track: usize,
    /// The label the status bar shows: `Reverse`, `Gain`, …
    pub op: String,
    /// The file the channel now plays.
    pub source: AudioSourceDto,
    /// The file it played before, still loaded, so an undo is a pointer swap.
    pub previous_source_id: u64,
    pub start_frames: u64,
    pub end_frames: u64,
    pub previous_start_frames: u64,
    pub previous_end_frames: u64,
    /// How far this lane's playlist clips moved inside the file. Non-zero only
    /// for Trim; undo passes its negation back.
    pub clip_offset_shift_frames: i64,
    /// The mixer after the edit, so the caller needs no second round trip.
    pub mixer: MixerStateDto,
}

// ---------------------------------------------------------------------------
// The buffer operations
//
// Pure functions over **interleaved stereo** `f32`, which is what
// `AudioSource::stereo` hands out and what `AudioSource::from_stereo` takes
// back. Kept free of any engine type so they can be tested for what they claim
// arithmetically -- reverse twice is identity, normalize lands on the target,
// a trim is exactly as long as the region, a gain in dB is the ratio it says.
// ---------------------------------------------------------------------------

/// `db` clamped to the range a single destructive gain may apply.
pub fn clamp_gain_db(db: f32) -> f32 {
    if db.is_finite() {
        db.clamp(-MAX_EDIT_GAIN_DB, MAX_EDIT_GAIN_DB)
    } else {
        0.0
    }
}

/// A decibel ratio as a linear multiplier.
pub fn db_to_linear(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

/// A linear multiplier as decibels. `-inf` for zero.
pub fn linear_to_db(linear: f32) -> f32 {
    if linear <= 0.0 {
        f32::NEG_INFINITY
    } else {
        20.0 * linear.log10()
    }
}

/// The largest absolute sample in an interleaved buffer.
pub fn peak(data: &[f32]) -> f32 {
    data.iter()
        .copied()
        .filter(|sample| sample.is_finite())
        .fold(0.0_f32, |worst, sample| worst.max(sample.abs()))
}

/// The buffer played backwards, **frame** by frame — the channels stay put.
///
/// Reversing the interleaved slice sample by sample would also swap left and
/// right, which is a stereo flip nobody asked for and which the second reverse
/// would not undo audibly.
pub fn reverse(data: &[f32]) -> Vec<f32> {
    let frames = data.len() / CHANNELS;
    let mut out = Vec::with_capacity(frames * CHANNELS);
    for frame in (0..frames).rev() {
        for channel in 0..CHANNELS {
            out.push(data[frame * CHANNELS + channel]);
        }
    }
    out
}

/// The buffer scaled by `linear`.
pub fn scale(data: &[f32], linear: f32) -> Vec<f32> {
    data.iter().map(|sample| sample * linear).collect()
}

/// The buffer scaled so its peak lands on `target_dbfs`.
///
/// `None` when there is nothing to normalize: an empty buffer, or one whose
/// peak is below [`SILENCE_PEAK`].
pub fn normalize(data: &[f32], target_dbfs: f32) -> Option<Vec<f32>> {
    let current = peak(data);
    if data.is_empty() || current < SILENCE_PEAK {
        return None;
    }
    Some(scale(data, db_to_linear(target_dbfs) / current))
}

/// Frames `start..end` of the buffer, clamped to what is there.
///
/// `end == 0` means "to the end", which is how the engine stores an untrimmed
/// region everywhere else (see `Project::sample_end_frames`).
pub fn crop(data: &[f32], start_frames: u64, end_frames: u64) -> Vec<f32> {
    let frames = data.len() / CHANNELS;
    let start = (start_frames as usize).min(frames);
    let end = if end_frames == 0 {
        frames
    } else {
        (end_frames as usize).clamp(start, frames)
    };
    data[start * CHANNELS..end * CHANNELS].to_vec()
}

/// The region `start..end` mirrored inside a file of `frames` frames.
///
/// What Reverse does to the played region: the last second of a take is the
/// first second of the take backwards.
pub fn mirror_region(start: u64, end: u64, frames: u64) -> (u64, u64) {
    let start = start.min(frames);
    let end = if end == 0 || end > frames {
        frames
    } else {
        end
    };
    if start >= end {
        return (0, 0);
    }
    let mirrored_start = frames - end;
    let mirrored_end = frames - start;
    if mirrored_start == 0 && mirrored_end == frames {
        (0, 0)
    } else {
        (mirrored_start, mirrored_end)
    }
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

/// Where edited copies are written. Beside the factory drum renders.
pub fn edits_dir() -> PathBuf {
    crate::user_dirs::sample_cache_dir().join("sample-edits")
}

/// `Kick.wav` + `reversed` -> `<dir>/Kick (reversed).wav`, never overwriting.
fn edit_path(dir: &Path, original: &Path, suffix: &str) -> PathBuf {
    let stem = original
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Sample".to_owned());
    let stem = sanitize(&stem);
    let suffix = sanitize(suffix);
    let first = dir.join(format!("{stem} ({suffix}).wav"));
    if !first.exists() {
        return first;
    }
    // A second reverse of the same file is a different file: the registry keys
    // sources by path, so reusing this one would hand the *old* audio to the
    // new id and silently undo the edit.
    for attempt in 2..10_000 {
        let candidate = dir.join(format!("{stem} ({suffix}) {attempt}.wav"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{stem} ({suffix}) {}.wav", std::process::id()))
}

/// Everything a file name may not contain, replaced with a space.
fn sanitize(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            other if other.is_control() => ' ',
            other => other,
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "Sample".to_owned()
    } else {
        trimmed.to_owned()
    }
}

// ---------------------------------------------------------------------------
// The command
// ---------------------------------------------------------------------------

impl AppState {
    /// Apply one destructive edit to the file behind `track`.
    ///
    /// Writes a new WAV, loads it as a new source, and repoints this channel
    /// (and this channel's playlist clips) at it. Nothing else that plays the
    /// original is touched, and the original stays loaded so the returned
    /// [`SampleEditDto`] describes a one-command undo.
    ///
    /// # Errors
    /// [`SampleEditError`] — a channel with no file, a file that is not loaded
    /// or has no frames, Normalize on silence, Trim with no region, or the
    /// write failing.
    pub fn edit_track_sample(
        &self,
        track: usize,
        op: SampleOp,
    ) -> Result<SampleEditDto, SampleEditError> {
        let (source_id, start, end) = {
            let project = self.tracks().project();
            let def = project
                .tracks
                .get(track)
                .ok_or(TrackError::NoSuchTrack(track))?;
            let id = def
                .source
                .sample_file_id()
                .ok_or(SampleEditError::NotASampleChannel(track))?;
            (id, def.sample_start_frames, def.sample_end_frames)
        };
        let source = self
            .audio_source(source_id)
            .ok_or(SampleEditError::MissingSource(source_id))?;
        let name = source.name();
        let frames = source.frames() as u64;
        if frames == 0 {
            return Err(SampleEditError::Empty(name));
        }
        let stereo = source.stereo();

        // `clip_shift` is how far into its file every clip of this channel has
        // to move. Only a crop can produce one: the other three keep the file's
        // length, so an offset still addresses the same moment.
        let (data, next_start, next_end, clip_shift) = match op {
            SampleOp::Reverse => {
                let (start, end) = mirror_region(start, end, frames);
                (reverse(stereo), start, end, 0)
            }
            SampleOp::Normalize => {
                let data = normalize(stereo, NORMALIZE_TARGET_DBFS)
                    .ok_or(SampleEditError::Silent(name))?;
                (data, start, end, 0)
            }
            SampleOp::Trim => {
                let end_frame = if end == 0 || end > frames {
                    frames
                } else {
                    end
                };
                if start >= end_frame || (start == 0 && end_frame == frames) {
                    return Err(SampleEditError::NothingToTrim);
                }
                (crop(stereo, start, end_frame), 0, 0, -(start as i64))
            }
            SampleOp::Gain { db } => (
                scale(stereo, db_to_linear(clamp_gain_db(db))),
                start,
                end,
                0,
            ),
        };

        let dir = edits_dir();
        std::fs::create_dir_all(&dir).map_err(|err| SampleEditError::Io {
            path: dir.display().to_string(),
            message: err.to_string(),
        })?;
        let path = edit_path(&dir, source.path(), &op.suffix());
        write_stereo_wav(&path, source.sample_rate(), &data)?;

        let written = self.audio_sources().load(&path)?;
        let new_id = written.id();
        self.set_track_audio_source(track, new_id, next_start, next_end, clip_shift)?;

        Ok(SampleEditDto {
            track,
            op: op.label().to_owned(),
            source: AudioSourceDto::of(&written, self.bpm()),
            previous_source_id: source_id,
            start_frames: next_start,
            end_frames: next_end,
            previous_start_frames: start,
            previous_end_frames: end,
            clip_offset_shift_frames: clip_shift,
            mixer: self.mixer_state(),
        })
    }

    /// Point a sample channel at a different loaded file, with a region.
    ///
    /// The forward half of every destructive edit **and** its own inverse, which
    /// is what makes one Ctrl+Z restore the take: both directions are this call
    /// with a different source id. Playlist clips **on this channel** follow the
    /// pointer, because a clip that kept the old id would go on playing audio
    /// the channel no longer has.
    ///
    /// `clip_offset_shift_frames` moves this lane's clips inside the file, which
    /// only a crop needs. Undo passes the negation and lands exactly where the
    /// arrangement started.
    ///
    /// # Errors
    /// [`SampleEditError`] for a channel that plays no file, or a source id that
    /// is not loaded.
    pub fn set_track_audio_source(
        &self,
        track: usize,
        source_id: u64,
        start_frames: u64,
        end_frames: u64,
        clip_offset_shift_frames: i64,
    ) -> Result<(), SampleEditError> {
        let previous = {
            let project = self.tracks().project();
            let def = project
                .tracks
                .get(track)
                .ok_or(TrackError::NoSuchTrack(track))?;
            def.source.clone()
        };
        let old_id = previous
            .sample_file_id()
            .ok_or(SampleEditError::NotASampleChannel(track))?;
        if self.audio_source(source_id).is_none() {
            return Err(SampleEditError::MissingSource(source_id));
        }
        if old_id != source_id {
            // `repoint_sample_source` rather than `set_track_source`: swapping
            // one decoded file for another needs no device, and a machine with
            // no sound card must not end up with the edit written to disk and
            // not applied. See that method for the whole argument.
            if let Err(err) = self
                .tracks()
                .repoint_sample_source(track, source_id, self.audio())
            {
                if !err.is_only_no_device() {
                    return Err(err.into());
                }
            }
            // Only this lane's clips: another channel may legitimately still
            // play the original file, and a global retarget would edit it too.
            let _ = self.edit_playlist(|playlist| {
                playlist.retarget_audio_source(old_id, source_id, Some(track));
                playlist.shift_audio_offsets(source_id, track, clip_offset_shift_frames);
                Ok(())
            });
        }
        self.set_track_sample_region(track, start_frames, end_frames)?;
        Ok(())
    }
}

/// Write interleaved stereo `f32` as a 32-bit float WAV.
pub(crate) fn write_stereo_wav(
    path: &Path,
    sample_rate: f64,
    interleaved: &[f32],
) -> Result<(), SampleEditError> {
    let io = |err: hound::Error| SampleEditError::Io {
        path: path.display().to_string(),
        message: err.to_string(),
    };
    let rate = if sample_rate.is_finite() && sample_rate >= 1.0 {
        sample_rate.round() as u32
    } else {
        48_000
    };
    let spec = hound::WavSpec {
        channels: CHANNELS as u16,
        sample_rate: rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec).map_err(io)?;
    for sample in interleaved {
        writer.write_sample(*sample).map_err(io)?;
    }
    writer.finalize().map_err(io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two frames of interleaved stereo, distinct in every slot.
    fn buffer() -> Vec<f32> {
        vec![0.10, -0.20, 0.30, -0.40, 0.50, -0.60]
    }

    #[test]
    fn reversing_twice_is_the_identity() {
        let original = buffer();
        assert_eq!(reverse(&reverse(&original)), original);
    }

    #[test]
    fn reverse_turns_the_frames_around_without_swapping_the_channels() {
        // The bug this guards: reversing the *interleaved* slice would give
        // [-0.60, 0.50, ...] -- left and right exchanged on every frame.
        assert_eq!(
            reverse(&buffer()),
            vec![0.50, -0.60, 0.30, -0.40, 0.10, -0.20]
        );
    }

    #[test]
    fn normalize_lands_the_peak_on_the_target() {
        let normalized = normalize(&buffer(), NORMALIZE_TARGET_DBFS).expect("not silent");
        let landed = linear_to_db(peak(&normalized));
        assert!(
            (landed - NORMALIZE_TARGET_DBFS).abs() < 1.0e-4,
            "peak landed at {landed} dBFS, wanted {NORMALIZE_TARGET_DBFS}"
        );
    }

    #[test]
    fn normalize_lifts_a_quiet_file_and_lowers_a_hot_one() {
        for peak_in in [0.001_f32, 0.5, 0.999] {
            let data = vec![peak_in, -peak_in * 0.5];
            let out = normalize(&data, NORMALIZE_TARGET_DBFS).expect("not silent");
            assert!((linear_to_db(peak(&out)) - NORMALIZE_TARGET_DBFS).abs() < 1.0e-4);
            // The shape survives: the ratio between the two samples is kept.
            assert!((out[0] / out[1] - data[0] / data[1]).abs() < 1.0e-4);
        }
    }

    #[test]
    fn normalize_refuses_silence_rather_than_amplifying_it() {
        assert!(normalize(&[0.0; 8], NORMALIZE_TARGET_DBFS).is_none());
        assert!(normalize(&[], NORMALIZE_TARGET_DBFS).is_none());
        // A dither-floor buffer is silence too: normalizing it would raise a
        // noise floor by 120 dB and call it a sample.
        assert!(normalize(&[1.0e-9, -1.0e-9], NORMALIZE_TARGET_DBFS).is_none());
    }

    #[test]
    fn a_trim_is_exactly_as_long_as_the_region() {
        let data: Vec<f32> = (0..40).map(|n| n as f32).collect(); // 20 frames
        let cropped = crop(&data, 4, 11);
        assert_eq!(cropped.len(), 7 * CHANNELS);
        assert_eq!(cropped[0], 8.0, "frame 4 is samples 8 and 9");
        assert_eq!(*cropped.last().unwrap(), 21.0, "frame 10 ends at sample 21");
    }

    #[test]
    fn a_trim_past_the_end_stops_at_the_end() {
        let data: Vec<f32> = (0..40).map(|n| n as f32).collect();
        assert_eq!(crop(&data, 15, 999).len(), 5 * CHANNELS);
        assert_eq!(crop(&data, 0, 0), data, "end 0 means the whole file");
        assert!(crop(&data, 30, 40).is_empty());
    }

    #[test]
    fn gain_in_db_is_the_ratio_it_says() {
        let doubled = scale(&buffer(), db_to_linear(6.020_6));
        for (out, original) in doubled.iter().zip(buffer()) {
            assert!(
                (out - original * 2.0).abs() < 1.0e-5,
                "+6.0206 dB must be exactly twice: {out} vs {}",
                original * 2.0
            );
        }
        let halved = scale(&buffer(), db_to_linear(-6.020_6));
        for (out, original) in halved.iter().zip(buffer()) {
            assert!((out - original * 0.5).abs() < 1.0e-5);
        }
        assert_eq!(scale(&buffer(), db_to_linear(0.0)), buffer());
    }

    #[test]
    fn gain_is_bounded_in_both_directions() {
        assert_eq!(clamp_gain_db(90.0), MAX_EDIT_GAIN_DB);
        assert_eq!(clamp_gain_db(-90.0), -MAX_EDIT_GAIN_DB);
        assert_eq!(clamp_gain_db(f32::NAN), 0.0);
        assert_eq!(clamp_gain_db(3.5), 3.5);
    }

    #[test]
    fn reverse_mirrors_the_played_region() {
        // The last 200 frames of a 1000-frame file become the first 200.
        assert_eq!(mirror_region(800, 1_000, 1_000), (0, 200));
        assert_eq!(mirror_region(0, 200, 1_000), (800, 1_000));
        // A whole-file region stays a whole-file region, in the engine's own
        // "0 means the end" spelling.
        assert_eq!(mirror_region(0, 0, 1_000), (0, 0));
        assert_eq!(mirror_region(0, 1_000, 1_000), (0, 0));
    }

    #[test]
    fn an_edit_name_never_collides_and_never_escapes_its_directory() {
        let dir = std::env::temp_dir().join(format!("mydaw-edit-name-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let first = edit_path(&dir, Path::new("/samples/Kick.wav"), "reversed");
        assert_eq!(first.file_name().unwrap(), "Kick (reversed).wav");
        std::fs::write(&first, b"").expect("touch");
        let second = edit_path(&dir, Path::new("/samples/Kick.wav"), "reversed");
        assert_eq!(second.file_name().unwrap(), "Kick (reversed) 2.wav");

        let hostile = edit_path(&dir, Path::new("/samples/a:b*c.wav"), "+3.0 dB");
        assert_eq!(hostile.parent(), Some(dir.as_path()));
        assert_eq!(hostile.file_name().unwrap(), "a b c (+3.0 dB).wav");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // The command, against a booted session
    //
    // Channel 0 is a real one-shot channel over a real WAV, so these run the
    // whole path: decode, edit, encode, register, repoint.
    // -----------------------------------------------------------------------

    /// A short WAV to edit: a quiet sine that is not silent anywhere.
    ///
    /// Written rather than taken from the factory content, because `assets/` is
    /// gitignored and these tests have to run on a fresh clone.
    fn write_take() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "mydaw-edit-take-{}-{:?}.wav",
            std::process::id(),
            std::thread::current().id()
        ));
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(&path, spec).expect("wav");
        for frame in 0..24_000 {
            let t = frame as f64 / 48_000.0;
            // Fading, so reverse and trim are visible in the samples.
            let sample = ((t * 220.0 * std::f64::consts::TAU).sin() * (1.0 - t)) as f32 * 0.5;
            writer.write_sample(sample).unwrap();
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();
        path
    }

    /// A booted shell whose channel 0 is a one-shot over [`write_take`].
    ///
    /// The starter rack is whatever the machine has installed -- on a fresh
    /// clone, the poly synth alone -- so the sample channel is added and the
    /// synth channel dropped, leaving the one-shot at index 0 where every test
    /// below addresses it.
    fn session() -> (mydaw_kernel::Kernel, AppState) {
        let kernel = crate::boot_kernel().expect("kernel boots");
        let state = AppState::from_kernel(&kernel).expect("services present");
        let path = write_take();
        match state.add_sample_channel(&path) {
            Ok(_) => {}
            // No device in a test process: the track is added, the graph is not.
            Err(crate::tracks::MixerError::Audio(
                mydaw_plugin_audio_io::AudioIoError::NotRunning,
            )) => {}
            Err(err) => panic!("sample channel: {err:?}"),
        }
        match state.remove_track(0) {
            Ok(()) => {}
            Err(crate::tracks::MixerError::Audio(
                mydaw_plugin_audio_io::AudioIoError::NotRunning,
            )) => {}
            Err(err) => panic!("dropping the synth channel: {err:?}"),
        }
        assert_eq!(state.mixer_state().tracks[0].engine, "sample");
        (kernel, state)
    }

    /// Every file an edit wrote, so a test leaves no rubbish in the cache.
    fn clean_up(state: &AppState, from: u64) {
        for source in state.audio_sources().list() {
            if source.id() > from && source.path().starts_with(edits_dir()) {
                let _ = std::fs::remove_file(source.path());
            }
        }
    }

    #[test]
    fn an_edit_makes_a_new_source_and_leaves_the_original_loaded() {
        let (_kernel, state) = session();
        let before = state.mixer_state().tracks[0]
            .audio_source_id
            .expect("the kick plays a file");
        let edit = state
            .edit_track_sample(0, SampleOp::Reverse)
            .expect("the kick reverses");

        assert_eq!(edit.previous_source_id, before);
        assert_ne!(edit.source.id, before, "an edit is copy on write");
        assert!(
            state.audio_source(before).is_some(),
            "the original take must stay loaded, or undo would be a re-decode"
        );
        assert_eq!(
            state.mixer_state().tracks[0].audio_source_id,
            Some(edit.source.id)
        );
        assert!(
            edit.source.name.contains("(reversed)"),
            "{}",
            edit.source.name
        );
        clean_up(&state, before);
    }

    #[test]
    fn one_repoint_undoes_a_destructive_edit() {
        let (_kernel, state) = session();
        let before = state.mixer_state().tracks[0].audio_source_id.unwrap();
        let edit = state
            .edit_track_sample(0, SampleOp::Gain { db: -6.0 })
            .expect("the kick takes a gain edit");

        state
            .set_track_audio_source(
                0,
                edit.previous_source_id,
                edit.previous_start_frames,
                edit.previous_end_frames,
                -edit.clip_offset_shift_frames,
            )
            .expect("undo is one command");

        let track = &state.mixer_state().tracks[0];
        assert_eq!(track.audio_source_id, Some(before));
        assert_eq!(track.sample_start_frames, edit.previous_start_frames);
        assert_eq!(track.sample_end_frames, edit.previous_end_frames);
        clean_up(&state, before);
    }

    #[test]
    fn gain_writes_a_file_that_is_quieter_by_exactly_that_much() {
        let (_kernel, state) = session();
        let before = state.mixer_state().tracks[0].audio_source_id.unwrap();
        let original = peak(state.audio_source(before).unwrap().stereo());
        let edit = state
            .edit_track_sample(0, SampleOp::Gain { db: -6.0 })
            .expect("the kick takes a gain edit");
        let edited = peak(state.audio_source(edit.source.id).unwrap().stereo());
        let moved = linear_to_db(edited) - linear_to_db(original);
        assert!(
            (moved + 6.0).abs() < 0.05,
            "a -6 dB edit must land -6 dB down, not {moved:.3}"
        );
        clean_up(&state, before);
    }

    #[test]
    fn normalize_writes_a_file_that_peaks_on_the_target() {
        let (_kernel, state) = session();
        let before = state.mixer_state().tracks[0].audio_source_id.unwrap();
        let edit = state
            .edit_track_sample(0, SampleOp::Normalize)
            .expect("the kick normalizes");
        let landed = linear_to_db(peak(state.audio_source(edit.source.id).unwrap().stereo()));
        assert!(
            (landed - NORMALIZE_TARGET_DBFS).abs() < 0.01,
            "the written file peaks at {landed} dBFS"
        );
        clean_up(&state, before);
    }

    #[test]
    fn trim_shortens_the_file_to_the_region_and_clears_it() {
        let (_kernel, state) = session();
        let before = state.mixer_state().tracks[0].audio_source_id.unwrap();
        let frames = state.audio_source(before).unwrap().frames() as u64;
        let (start, end) = (frames / 4, frames / 2);
        state
            .set_track_sample_region(0, start, end)
            .expect("the region is set");

        let edit = state
            .edit_track_sample(0, SampleOp::Trim)
            .expect("it trims");
        assert_eq!(
            state.audio_source(edit.source.id).unwrap().frames() as u64,
            end - start,
            "a trim is exactly as long as the region it kept"
        );
        let track = &state.mixer_state().tracks[0];
        assert_eq!(
            (track.sample_start_frames, track.sample_end_frames),
            (0, 0),
            "after a trim the whole of what is left plays"
        );
        assert_eq!(
            edit.clip_offset_shift_frames,
            -(start as i64),
            "this lane's clips move with the crop"
        );
        clean_up(&state, before);
    }

    #[test]
    fn trim_is_refused_when_the_whole_file_already_plays() {
        let (_kernel, state) = session();
        assert!(matches!(
            state.edit_track_sample(0, SampleOp::Trim),
            Err(SampleEditError::NothingToTrim)
        ));
    }

    #[test]
    fn a_channel_with_no_file_is_refused_rather_than_silently_ignored() {
        let (_kernel, state) = session();
        // The session is one-shot channels only, so put a synthesiser on the end
        // to be refused.
        match state.add_track(crate::source::Source::PolySynth, None) {
            Ok(_) => {}
            Err(crate::tracks::MixerError::Audio(
                mydaw_plugin_audio_io::AudioIoError::NotRunning,
            )) => {}
            Err(err) => panic!("adding a synth channel: {err:?}"),
        }
        let synth = state
            .mixer_state()
            .tracks
            .iter()
            .position(|track| track.engine == "synth")
            .expect("the channel just added is a synthesiser");
        assert!(matches!(
            state.edit_track_sample(synth, SampleOp::Reverse),
            Err(SampleEditError::NotASampleChannel(_))
        ));
        assert!(matches!(
            state.edit_track_sample(999, SampleOp::Reverse),
            Err(SampleEditError::Mixer(_))
        ));
    }

    #[test]
    fn editing_one_channel_leaves_another_on_the_same_file_alone() {
        let (_kernel, state) = session();
        let kick = state.mixer_state().tracks[0].audio_source_id.unwrap();
        // A second channel on the very same file, which is what a duplicated
        // one-shot is.
        state
            .add_track(
                crate::Source::OneShot { source_id: kick },
                Some("Kick copy".to_owned()),
            )
            .ok();
        let twin = state
            .mixer_state()
            .tracks
            .iter()
            .position(|track| track.name == "Kick copy")
            .expect("the twin was added");
        assert_eq!(state.mixer_state().tracks[twin].audio_source_id, Some(kick));

        state
            .edit_track_sample(0, SampleOp::Reverse)
            .expect("the kick reverses");

        assert_eq!(
            state.mixer_state().tracks[twin].audio_source_id,
            Some(kick),
            "copy on write means the other channel keeps the take it had"
        );
        clean_up(&state, kick);
    }

    #[test]
    fn the_gain_suffix_reads_as_a_signed_level() {
        assert_eq!(SampleOp::Gain { db: 3.0 }.suffix(), "+3.0 dB");
        assert_eq!(SampleOp::Gain { db: -2.5 }.suffix(), "-2.5 dB");
        assert_eq!(SampleOp::Reverse.suffix(), "reversed");
    }
}
