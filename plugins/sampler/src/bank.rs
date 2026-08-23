//! Turning a manifest and a directory of audio files into something the audio
//! thread can play without thinking.
//!
//! ```text
//!   manifest.json + *.flac  --load-->  SampleBank  --Arc-->  SamplerNode
//!        (non-RT: decode,                (immutable)          (RT: index,
//!         truncate, fade,                                      interpolate)
//!         build lookup)
//! ```
//!
//! Everything expensive happens here, once, off the audio thread: decoding,
//! truncation, the fade that hides it, and -- the part that matters most for
//! [`process`](crate::SamplerNode::process) -- a **flat lookup table** from
//! (key, velocity) to sample index. A voice starting on the audio thread does
//! one array index, not a scan over ninety regions with four comparisons each.
//!
//! # Three decisions worth stating
//!
//! **`f32` in RAM, not `i16`.** Halving the footprint would mean eight integer
//! conversions per output frame (four interpolation taps, two channels) inside
//! the voice loop, on the audio thread, to save memory the budget does not need:
//! the shipped piano is 242 MB against a 400 MB ceiling. Memory is cheap here
//! and the audio callback is not.
//!
//! **No resampling at load.** A sample keeps its own rate and the voice folds
//! the device-rate ratio into the pitch ratio it was going to compute anyway --
//! one multiply, at zero quality cost, versus resampling every sample at load
//! (slow start-up) or on a device change (a rebuild of the whole bank).
//!
//! **Everything is stereo.** Mono content is duplicated into both channels at
//! load. It costs a mono instrument twice the memory it needs; it buys the voice
//! a single code path instead of a channel-count branch, and every instrument
//! that actually ships here is stereo.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::decode::{decode_file, DecodeError};
use crate::manifest::{InstrumentManifest, ManifestError, MANIFEST_FILE_NAME, MIN_VELOCITY};

/// Interpolation taps needed before the current frame.
const PAD_LEAD: usize = mydaw_audio_file::PAD_LEAD;
/// Interpolation taps needed after the current frame.
const PAD_TRAIL: usize = mydaw_audio_file::PAD_TRAIL;
/// Channels every sample is stored in. See the module docs.
pub const CHANNELS: usize = mydaw_audio_file::CHANNELS;

/// Lookup entry meaning "no sample covers this note".
const NO_SAMPLE: u16 = u16::MAX;

/// Most samples one instrument may hold. Bounded by the lookup table's `u16`;
/// a piano uses 90 and a manifest with 65 535 entries is a mistake, not an
/// instrument.
pub const MAX_SAMPLES: usize = NO_SAMPLE as usize;

/// Keys and velocities in the lookup table.
const KEYS: usize = 128;
const VELOCITIES: usize = 128;

/// One-shots longer than this stay on the playlist, not the rack.
pub const ONESHOT_MAX_SECONDS: f32 = 15.0;
/// Short fade so a dropped file does not click when the note ends.
const ONESHOT_RELEASE_MS: f32 = 40.0;

/// One decoded sample, padded so the interpolator never needs a bounds check.
#[derive(Debug)]
pub struct LoadedSample {
    /// Interleaved stereo, with [`PAD_LEAD`] silent frames before the content
    /// and [`PAD_TRAIL`] after it.
    data: Vec<f32>,
    /// Playable frames, excluding the padding.
    frames: usize,
    /// The rate this sample was recorded at, which need not be the device's.
    sample_rate: f64,
    /// Key at which this sample plays back at its recorded pitch.
    root_key: u8,
    /// Fine detune, folded into the playback ratio at note-on.
    tune_cents: f32,
    /// The velocity span this sample was chosen for, kept so that a voice can
    /// trim *within* the layer -- see [`layer_gain`](crate::node::layer_gain).
    vel_lo: u8,
    vel_hi: u8,
}

impl LoadedSample {
    /// Playable frames.
    pub const fn frames(&self) -> usize {
        self.frames
    }

    /// The rate this sample was recorded at.
    pub const fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    pub const fn root_key(&self) -> u8 {
        self.root_key
    }

    pub const fn tune_cents(&self) -> f32 {
        self.tune_cents
    }

    /// The velocity span this sample covers.
    pub const fn velocity_span(&self) -> (u8, u8) {
        (self.vel_lo, self.vel_hi)
    }

    /// Bytes of audio this sample holds.
    pub fn bytes(&self) -> usize {
        std::mem::size_of_val(self.data.as_slice())
    }

    /// The four interpolation taps around `index`, per channel.
    ///
    /// `index` is a *content* frame in `0..frames`; the padding is what makes
    /// `index == 0` and `index == frames - 1` legal without a branch. RT-safe:
    /// two slice reads and no allocation.
    #[inline]
    pub fn taps(&self, index: usize, channel: usize) -> [f32; 4] {
        // Content frame `index` sits at padded frame `index + PAD_LEAD`, so the
        // tap at `index - 1` is padded frame `index`, which always exists.
        mydaw_audio_file::taps(&self.data, index, channel)
    }
}

/// Every sample of one instrument, plus the table that finds them.
///
/// Immutable once built and shared through an [`Arc`], which is what lets a
/// node be created (and a source swapped) without copying 240 MB of piano.
#[derive(Debug)]
pub struct SampleBank {
    id: String,
    name: String,
    description: String,
    attribution: String,
    release_ms: f32,
    /// Note-off is ignored: the recording is the note. See
    /// [`InstrumentManifest::one_shot`](crate::manifest::InstrumentManifest::one_shot).
    one_shot: bool,
    /// Where this instrument wants the sampler's knobs, by parameter id.
    ///
    /// Read by the shell when a track picks the instrument up, never by the
    /// audio thread -- the knobs it reads are the track's own block.
    default_params: Vec<(String, f32)>,
    samples: Vec<LoadedSample>,
    /// `key * VELOCITIES + velocity` -> index into `samples`, [`NO_SAMPLE`] for
    /// a note this instrument does not cover. 32 KB, built once, shared by
    /// every node.
    lookup: Box<[u16]>,
    /// Files named by the manifest that were missing or unreadable.
    skipped: Vec<String>,
    bytes: usize,
    /// Play every key at the recorded speed. A dropped kick is a drum, not a
    /// pitched instrument: the rack's default row keys are GM drum notes, and
    /// stretching a one-shot two octaves is the wrong sound.
    unpitched: bool,
}

impl SampleBank {
    /// Stable id; the directory name, and part of the source id a project file
    /// records.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Name for the source picker.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// One line for a tooltip.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// The attribution the licence requires be shown.
    pub fn attribution(&self) -> &str {
        &self.attribution
    }

    /// Note-off fade, in milliseconds.
    pub const fn release_ms(&self) -> f32 {
        self.release_ms
    }

    /// Whether a note plays to the end of its recording whatever the key does.
    pub const fn one_shot(&self) -> bool {
        self.one_shot
    }

    /// Where this instrument wants the sampler's knobs, by parameter id.
    ///
    /// Empty for an instrument that is happy with the sampler's own defaults
    /// except for its release, which every instrument states and which is
    /// always the first pair here.
    pub fn default_params(&self) -> &[(String, f32)] {
        &self.default_params
    }

    /// Samples actually loaded. Fewer than the manifest named if files were
    /// missing -- see [`skipped`](Self::skipped).
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Files the manifest named that could not be loaded.
    pub fn skipped(&self) -> &[String] {
        &self.skipped
    }

    /// Bytes of decoded audio this bank holds. What the memory budget is
    /// measured against.
    pub const fn bytes(&self) -> usize {
        self.bytes
    }

    /// Whether keys play at the recorded speed rather than in equal temperament.
    pub const fn unpitched(&self) -> bool {
        self.unpitched
    }

    /// The sample for `key` at `velocity`, or `None` where the instrument has
    /// nothing.
    ///
    /// **RT-safe**: one bounds-checked index into a flat table. This is the
    /// whole reason the table exists.
    #[inline]
    pub fn sample_for(&self, key: u8, velocity: u8) -> Option<&LoadedSample> {
        let key = key as usize;
        let velocity = velocity as usize;
        if key >= KEYS || velocity >= VELOCITIES {
            return None;
        }
        let index = self.lookup[key * VELOCITIES + velocity];
        if index == NO_SAMPLE {
            return None;
        }
        self.samples.get(index as usize)
    }

    /// Index of the sample for `key` at `velocity`, for a voice that wants to
    /// hold a reference across blocks.
    #[inline]
    pub fn index_for(&self, key: u8, velocity: u8) -> Option<u16> {
        let key = key as usize;
        let velocity = velocity as usize;
        if key >= KEYS || velocity >= VELOCITIES {
            return None;
        }
        let index = self.lookup[key * VELOCITIES + velocity];
        (index != NO_SAMPLE).then_some(index)
    }

    /// The sample at `index`. **RT-safe.**
    #[inline]
    pub fn sample(&self, index: u16) -> Option<&LoadedSample> {
        self.samples.get(index as usize)
    }

    /// The lowest and highest key this bank answers for, if any.
    pub fn key_range(&self) -> Option<(u8, u8)> {
        let covered: Vec<usize> = (0..KEYS)
            .filter(|key| {
                self.lookup[key * VELOCITIES..(key + 1) * VELOCITIES]
                    .iter()
                    .any(|index| *index != NO_SAMPLE)
            })
            .collect();
        match (covered.first(), covered.last()) {
            (Some(lo), Some(hi)) => Some((*lo as u8, *hi as u8)),
            _ => None,
        }
    }

    /// One audio file on every key, played at recorded speed.
    ///
    /// Channel-rack drops land on GM drum keys (36, 38, 42, 46). Pitching a
    /// kick two octaves would make it unusable; `unpitched` keeps the file
    /// sounding like itself on every pad.
    pub fn oneshot(decoded: Decoded, id: String, name: String) -> Result<Arc<Self>, LoadError> {
        if decoded.frames == 0 {
            return Err(LoadError::NoSamples {
                id: id.clone(),
                dir: PathBuf::new(),
                named: 1,
            });
        }
        let sample = LoadedSample {
            data: decoded.data,
            frames: decoded.frames,
            sample_rate: decoded.sample_rate,
            root_key: 60,
            tune_cents: 0.0,
            vel_lo: MIN_VELOCITY,
            vel_hi: 127,
        };
        let bytes = sample.bytes();
        Ok(Arc::new(Self {
            id,
            name,
            description: "Dropped audio file, played at recorded speed".to_owned(),
            attribution: String::new(),
            release_ms: ONESHOT_RELEASE_MS,
            // Not a declared one-shot: a dropped file is gated by whatever put
            // it on the rack, and its 40 ms release is short enough that the
            // gate is what the ear hears rather than the ramp -- which is also
            // where its Release knob starts.
            one_shot: false,
            default_params: vec![(
                crate::node::param::RELEASE.to_owned(),
                ONESHOT_RELEASE_MS,
            )],
            samples: vec![sample],
            lookup: vec![0u16; KEYS * VELOCITIES].into_boxed_slice(),
            skipped: Vec::new(),
            bytes,
            unpitched: true,
        }))
    }

    /// Decode `path` into an unpitched oneshot bank. Id is `file:` + path.
    pub fn oneshot_from_path(path: &Path) -> Result<Arc<Self>, LoadError> {
        let max_frames_for =
            |rate: f64| Some((f64::from(ONESHOT_MAX_SECONDS) * rate).round().max(1.0) as usize);
        let stereo = mydaw_audio_file::decode_stereo(path, max_frames_for).map_err(|source| {
            LoadError::File {
                path: path.to_path_buf(),
                source,
            }
        })?;
        if stereo.truncated {
            return Err(LoadError::TooLong {
                path: path.to_path_buf(),
                max: ONESHOT_MAX_SECONDS,
            });
        }
        let decoded = Decoded::from_stereo(stereo.data, stereo.sample_rate, 0.0, false);
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("Sample")
            .to_owned();
        Self::oneshot(decoded, file_instrument_id(path), name)
    }
}

/// Wire id for a user-dropped file sampler (`file:C:\...`).
pub fn file_instrument_id(path: &Path) -> String {
    format!("file:{}", path.display())
}

/// The filesystem path inside a `file:` instrument id, if it is one.
pub fn file_path_of(instrument_id: &str) -> Option<&Path> {
    instrument_id.strip_prefix("file:").map(Path::new)
}

/// Why an instrument could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    /// Every file the manifest named was missing or unreadable. A *partly*
    /// loaded instrument is a success with warnings; a wholly empty one is not
    /// an instrument.
    #[error(
        "the instrument '{id}' at {dir} has no playable samples: \
         all {named} file(s) named by its manifest were missing or unreadable"
    )]
    NoSamples {
        id: String,
        dir: PathBuf,
        named: usize,
    },
    #[error(
        "the instrument '{id}' names {named} samples, more than the {MAX_SAMPLES} a bank can hold"
    )]
    TooManySamples { id: String, named: usize },
    #[error("could not decode {}: {source}", path.display())]
    File {
        path: PathBuf,
        #[source]
        source: DecodeError,
    },
    #[error(
        "'{}' is longer than {max} seconds; drop it on a playlist lane instead",
        path.display()
    )]
    TooLong { path: PathBuf, max: f32 },
}

/// Load the instrument in `dir`, reading `dir/manifest.json`.
///
/// A file the manifest names but that is missing or unreadable is **skipped
/// with a warning**, not an error: a partial download leaves exactly that
/// state, and 89 of 90 piano samples is worth far more than nothing. The load
/// fails only when *no* sample survived.
///
/// # Errors
/// [`LoadError::Manifest`] for a manifest that cannot be read or does not mean
/// anything, [`LoadError::NoSamples`] when nothing loaded, and
/// [`LoadError::TooManySamples`] for a manifest past [`MAX_SAMPLES`].
pub fn load_instrument(dir: &Path) -> Result<Arc<SampleBank>, LoadError> {
    let manifest = InstrumentManifest::read(&dir.join(MANIFEST_FILE_NAME))?;
    load_from_manifest(dir, &manifest)
}

/// Load the samples an already-parsed manifest names out of `dir`.
///
/// # Errors
/// See [`load_instrument`].
pub fn load_from_manifest(
    dir: &Path,
    manifest: &InstrumentManifest,
) -> Result<Arc<SampleBank>, LoadError> {
    let named = manifest.samples.len();
    if named > MAX_SAMPLES {
        return Err(LoadError::TooManySamples {
            id: manifest.id.clone(),
            named,
        });
    }

    let max_frames_for = |rate: f64| {
        manifest
            .max_seconds
            .map(|seconds| (f64::from(seconds) * rate).round().max(1.0) as usize)
    };
    let fade_seconds = manifest.fade_out_seconds.unwrap_or(0.0);

    let mut samples = Vec::with_capacity(named);
    let mut skipped = Vec::new();
    let mut bytes = 0_usize;
    // Manifest index -> bank index, so the lookup table can be built from the
    // manifest's own ordering even though skipped entries shift everything.
    let mut bank_index = vec![NO_SAMPLE; named];

    for (index, entry) in manifest.samples.iter().enumerate() {
        let path = dir.join(&entry.file);
        match decode_file(&path, max_frames_for, fade_seconds) {
            Ok(decoded) => {
                let sample = LoadedSample {
                    data: decoded.data,
                    frames: decoded.frames,
                    sample_rate: decoded.sample_rate,
                    root_key: entry.root_key,
                    tune_cents: entry.tune_cents,
                    vel_lo: entry.vel_lo,
                    vel_hi: entry.vel_hi,
                };
                bytes += sample.bytes();
                bank_index[index] = samples.len() as u16;
                samples.push(sample);
            }
            Err(err) => {
                // Deliberately a warning: see the function docs.
                log::warn!(
                    "sampler: skipping '{}' in instrument '{}': {err}",
                    entry.file,
                    manifest.id
                );
                skipped.push(entry.file.clone());
            }
        }
    }

    if samples.is_empty() {
        return Err(LoadError::NoSamples {
            id: manifest.id.clone(),
            dir: dir.to_path_buf(),
            named,
        });
    }

    // The manifest's release is the default position of the release *knob*: a
    // marimba rings and a piano damps, and the number that says which is which
    // has to survive the arrival of a knob that can override it. Anything the
    // kit states explicitly is applied after, and so wins.
    let mut default_params = vec![(
        crate::node::param::RELEASE.to_owned(),
        manifest.release_ms,
    )];
    default_params.extend(
        manifest
            .default_params
            .iter()
            .map(|(id, value)| (id.clone(), *value)),
    );

    let lookup = build_lookup(manifest, &bank_index);
    let description = if manifest.description.trim().is_empty() {
        format!(
            "Sampled instrument, {} sample(s) at {} MB",
            samples.len(),
            bytes / 1_000_000
        )
    } else {
        manifest.description.clone()
    };

    Ok(Arc::new(SampleBank {
        id: manifest.id.clone(),
        name: manifest.name.clone(),
        description,
        attribution: manifest.attribution.clone(),
        release_ms: manifest.release_ms,
        one_shot: manifest.one_shot,
        default_params,
        samples,
        lookup,
        skipped,
        bytes,
        unpitched: false,
    }))
}

/// Flatten the manifest's regions into a (key, velocity) table.
///
/// Entries whose file did not load are passed over, so a missing sample falls
/// through to the next layer that covers the note rather than leaving a hole --
/// which is what makes a partial download sound thin rather than gapped.
fn build_lookup(manifest: &InstrumentManifest, bank_index: &[u16]) -> Box<[u16]> {
    let mut lookup = vec![NO_SAMPLE; KEYS * VELOCITIES].into_boxed_slice();
    for key in 0..KEYS {
        for velocity in 0..VELOCITIES {
            // Velocity 0 is a legal CLAP note-on. Manifests describe MIDI's
            // 1..=127, so a zero-velocity note takes the softest layer rather
            // than silence.
            let lookup_velocity = (velocity as u8).max(MIN_VELOCITY);
            let found = manifest
                .samples
                .iter()
                .enumerate()
                .filter(|(index, _)| bank_index[*index] != NO_SAMPLE)
                .find(|(_, entry)| entry.covers(key as u8, lookup_velocity));
            if let Some((index, _)) = found {
                lookup[key * VELOCITIES + velocity] = bank_index[index];
            }
        }
    }
    lookup
}

/// A decoded file, before it becomes a [`LoadedSample`].
pub struct Decoded {
    /// Interleaved stereo, padded by [`PAD_LEAD`] and [`PAD_TRAIL`].
    pub data: Vec<f32>,
    pub frames: usize,
    pub sample_rate: f64,
}

/// Elides the audio itself: a `Decoded` holds megabytes, and a test failure
/// that printed all of it would be unreadable.
impl std::fmt::Debug for Decoded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decoded")
            .field("frames", &self.frames)
            .field("sample_rate", &self.sample_rate)
            .finish_non_exhaustive()
    }
}

impl Decoded {
    /// Wrap interleaved stereo content in the padding the interpolator needs,
    /// applying `fade_seconds` of linear fade-out when the content was cut
    /// short.
    ///
    /// `truncated` says whether the tail was removed; a sample that ended on
    /// its own already decays to silence and must not be faded again.
    pub fn from_stereo(
        mut content: Vec<f32>,
        sample_rate: f64,
        fade_seconds: f32,
        truncated: bool,
    ) -> Self {
        let frames = content.len() / CHANNELS;
        if truncated && fade_seconds > 0.0 && frames > 0 {
            let fade_frames = ((f64::from(fade_seconds) * sample_rate).round() as usize)
                .min(frames)
                .max(1);
            let first = frames - fade_frames;
            for frame in first..frames {
                // Linear to exactly zero: the last frame of a truncated sample
                // must be silence, or the cut is still a step.
                let gain = (frames - 1 - frame) as f32 / fade_frames as f32;
                for channel in 0..CHANNELS {
                    content[frame * CHANNELS + channel] *= gain;
                }
            }
        }

        let mut data = Vec::with_capacity((frames + PAD_LEAD + PAD_TRAIL) * CHANNELS);
        data.extend(std::iter::repeat_n(0.0, PAD_LEAD * CHANNELS));
        data.append(&mut content);
        data.extend(std::iter::repeat_n(0.0, PAD_TRAIL * CHANNELS));

        Self {
            data,
            frames,
            sample_rate,
        }
    }
}

impl From<DecodeError> for LoadError {
    fn from(err: DecodeError) -> Self {
        // Decode failures never fail a whole load; they are logged and skipped.
        // This exists only so `?` reads naturally in tests.
        Self::NoSamples {
            id: err.to_string(),
            dir: PathBuf::new(),
            named: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::SampleEntry;
    use crate::test_support::{write_wav, TempDir};

    fn entry(file: &str, root: u8, lo: u8, hi: u8, vlo: u8, vhi: u8) -> SampleEntry {
        SampleEntry {
            file: file.to_owned(),
            root_key: root,
            key_lo: lo,
            key_hi: hi,
            vel_lo: vlo,
            vel_hi: vhi,
            tune_cents: 0.0,
        }
    }

    fn manifest(samples: Vec<SampleEntry>) -> InstrumentManifest {
        InstrumentManifest {
            id: "t".to_owned(),
            name: "T".to_owned(),
            description: String::new(),
            release_ms: 300.0,
            one_shot: false,
            default_params: Default::default(),
            max_seconds: None,
            fade_out_seconds: None,
            attribution: String::new(),
            samples,
        }
    }

    /// All entries loaded: the table is the manifest, flattened.
    #[test]
    fn the_lookup_table_flattens_key_and_velocity_regions() {
        let m = manifest(vec![
            entry("soft.wav", 60, 59, 61, 1, 63),
            entry("loud.wav", 60, 59, 61, 64, 127),
            entry("high.wav", 72, 71, 73, 1, 127),
        ]);
        let table = build_lookup(&m, &[0, 1, 2]);
        let at = |key: usize, vel: usize| table[key * VELOCITIES + vel];

        assert_eq!(at(60, 1), 0);
        assert_eq!(at(60, 63), 0);
        assert_eq!(
            at(60, 64),
            1,
            "the layer boundary is where the manifest says"
        );
        assert_eq!(at(60, 127), 1);
        assert_eq!(at(72, 100), 2);
        assert_eq!(at(64, 100), NO_SAMPLE, "a key nothing covers stays empty");
        assert_eq!(at(0, 0), NO_SAMPLE);
    }

    /// CLAP allows a note-on at velocity 0, and MIDI regions start at 1.
    #[test]
    fn a_zero_velocity_note_on_takes_the_softest_layer_rather_than_silence() {
        let m = manifest(vec![
            entry("soft.wav", 60, 0, 127, 1, 63),
            entry("loud.wav", 60, 0, 127, 64, 127),
        ]);
        let table = build_lookup(&m, &[0, 1]);
        assert_eq!(table[60 * VELOCITIES], 0, "velocity 0 plays the soft layer");
    }

    /// The behaviour that makes a partial download sound thin instead of gapped.
    #[test]
    fn a_skipped_file_falls_through_to_the_next_layer_that_covers_the_note() {
        let m = manifest(vec![
            entry("missing.wav", 60, 0, 127, 1, 63),
            entry("present.wav", 60, 0, 127, 1, 127),
        ]);
        // Entry 0 did not load; entry 1 became bank index 0.
        let table = build_lookup(&m, &[NO_SAMPLE, 0]);
        assert_eq!(
            table[60 * VELOCITIES + 30],
            0,
            "a note the missing layer owned must fall through, not go silent"
        );
        assert_eq!(table[60 * VELOCITIES + 100], 0);
    }

    #[test]
    fn padding_makes_the_first_and_last_frame_safe_to_interpolate() {
        // Four frames of stereo ramp: L = 1,2,3,4 and R = -1,-2,-3,-4.
        let content = vec![1.0, -1.0, 2.0, -2.0, 3.0, -3.0, 4.0, -4.0];
        let decoded = Decoded::from_stereo(content, 48_000.0, 0.0, false);
        assert_eq!(decoded.frames, 4);
        assert_eq!(decoded.data.len(), (4 + PAD_LEAD + PAD_TRAIL) * CHANNELS);

        let sample = LoadedSample {
            data: decoded.data,
            frames: decoded.frames,
            sample_rate: 48_000.0,
            root_key: 60,
            tune_cents: 0.0,
            vel_lo: 1,
            vel_hi: 127,
        };
        // At frame 0 the "previous" tap is the lead pad.
        assert_eq!(sample.taps(0, 0), [0.0, 1.0, 2.0, 3.0]);
        assert_eq!(sample.taps(0, 1), [0.0, -1.0, -2.0, -3.0]);
        // At the last frame both trailing taps are pad.
        assert_eq!(sample.taps(3, 0), [3.0, 4.0, 0.0, 0.0]);
    }

    #[test]
    fn a_truncated_sample_is_faded_to_exactly_zero_and_a_whole_one_is_not() {
        let content: Vec<f32> = (0..20).flat_map(|_| [1.0_f32, 1.0]).collect();
        // 10 frames of fade at 10 Hz = 1.0 s over 20 frames.
        let faded = Decoded::from_stereo(content.clone(), 10.0, 1.0, true);
        let last = faded.frames - 1;
        let sample = LoadedSample {
            data: faded.data,
            frames: faded.frames,
            sample_rate: 10.0,
            root_key: 60,
            tune_cents: 0.0,
            vel_lo: 1,
            vel_hi: 127,
        };
        assert_eq!(
            sample.taps(last, 0)[1],
            0.0,
            "a truncated sample must end in digital silence"
        );
        assert_eq!(sample.taps(0, 0)[1], 1.0, "the head is untouched");
        // Monotone decline over the fade.
        let values: Vec<f32> = (10..20).map(|f| sample.taps(f, 0)[1]).collect();
        assert!(
            values.windows(2).all(|w| w[0] >= w[1]),
            "the fade must not rise: {values:?}"
        );

        // A sample that ended on its own already decayed; fading it again would
        // be wrong.
        let whole = Decoded::from_stereo(content, 10.0, 1.0, false);
        let sample = LoadedSample {
            data: whole.data,
            frames: whole.frames,
            sample_rate: 10.0,
            root_key: 60,
            tune_cents: 0.0,
            vel_lo: 1,
            vel_hi: 127,
        };
        assert_eq!(sample.taps(19, 0)[1], 1.0);
    }

    #[test]
    fn a_oneshot_covers_every_key_and_plays_unpitched() {
        let dir = TempDir::new("oneshot");
        let path = dir.path().join("kick.wav");
        write_wav(&path, 8_000, 1, &[0.5_f32; 80]);

        let bank = SampleBank::oneshot_from_path(&path).expect("loads");
        assert!(bank.unpitched());
        assert_eq!(bank.id(), file_instrument_id(&path));
        assert_eq!(bank.name(), "kick");
        assert_eq!(bank.key_range(), Some((0, 127)));
        assert!(bank.sample_for(36, 100).is_some(), "GM kick pad");
        assert!(bank.sample_for(60, 1).is_some());
        assert!(file_path_of(bank.id()).is_some());
    }

    #[test]
    fn a_oneshot_longer_than_the_cap_is_refused() {
        let dir = TempDir::new("oneshot-long");
        let path = dir.path().join("loop.wav");
        // 16 s at 100 Hz, past ONESHOT_MAX_SECONDS.
        write_wav(&path, 100, 1, &vec![0.2_f32; 1_600]);

        let err = SampleBank::oneshot_from_path(&path).unwrap_err();
        assert!(
            matches!(err, LoadError::TooLong { max, .. } if (max - ONESHOT_MAX_SECONDS).abs() < f32::EPSILON),
            "{err}"
        );
    }
}
