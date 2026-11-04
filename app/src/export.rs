//! File › Export › WAV: the finished song as a file to send to someone.
//!
//! The render itself is the snapshot worker's ([`crate::jobs`]): an isolated
//! process that rebuilds the unsaved session -- every channel, built-in insert,
//! hosted CLAP/VST3 plugin, send, bus and the master chain -- and runs the real
//! graph offline. This module decides how long that render is, streams it to a
//! float scratch file in the job directory while tracking the peak, and then
//! encodes the destination WAV at the chosen bit depth, optionally normalized.
//!
//! Two passes because normalizing needs the peak of the whole song before the
//! first sample is written; the scratch file keeps memory flat for a long song.
//! The WAV is written next to its destination as `<name>.part` and renamed over
//! it only once complete, so a failed or cancelled export never leaves a
//! truncated file -- or destroys the previous export -- under the real name.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::jobs::{render_into, RenderLength, RenderOptions};
use crate::ProjectFile;

/// The file in the job directory the worker writes its 0..1 progress to.
pub const PROGRESS_FILE: &str = "progress";
/// The float render in the job directory, removed once encoded -- or by the
/// job monitor when the worker is cancelled or dies.
pub const SCRATCH_FILE: &str = "render.f32";
/// Longest export: an hour of audio.
pub const MAX_EXPORT_SECONDS: f64 = 60.0 * 60.0;
/// Where Normalize puts the loudest sample, in dBFS. Just under full scale, so
/// the 16-bit dither and a lossy re-encode downstream have room.
pub const NORMALIZE_CEILING_DBFS: f32 = -0.1;
/// Share of the progress bar the render takes; the encode is the rest.
const RENDER_SHARE: f64 = 0.95;
/// Minimum interval between progress file writes.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// Samples per read while encoding.
const ENCODE_CHUNK: usize = 64 * 1024;

/// What the export dialog asks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportOptions {
    /// song: the whole arrangement to its last clip; pattern: one loop of the active pattern.
    pub mode: String,
    /// 44100, 48000 or 96000 Hz.
    pub sample_rate: u32,
    /// 16 (PCM, TPDF dither), 24 (PCM) or 32 (IEEE float).
    pub bit_depth: u16,
    /// Seconds rendered past the end for reverb and delay tails, 0..30.
    pub tail_seconds: f64,
    /// Scale the whole file so its loudest sample peaks at -0.1 dBFS.
    pub normalize: bool,
}

/// One export, as handed to the worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportTarget {
    pub options: ExportOptions,
    pub path: PathBuf,
}

/// The sample formats the dialog offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitDepth {
    Pcm16,
    Pcm24,
    Float32,
}

impl BitDepth {
    /// # Errors
    /// Anything but 16, 24 or 32.
    pub fn from_bits(bits: u16) -> Result<Self, String> {
        match bits {
            16 => Ok(Self::Pcm16),
            24 => Ok(Self::Pcm24),
            32 => Ok(Self::Float32),
            _ => Err("bitDepth must be 16, 24 or 32".into()),
        }
    }

    const fn bits(self) -> u16 {
        match self {
            Self::Pcm16 => 16,
            Self::Pcm24 => 24,
            Self::Float32 => 32,
        }
    }

    fn spec(self, sample_rate: u32) -> hound::WavSpec {
        hound::WavSpec {
            channels: 2,
            sample_rate,
            bits_per_sample: self.bits(),
            sample_format: match self {
                Self::Float32 => hound::SampleFormat::Float,
                Self::Pcm16 | Self::Pcm24 => hound::SampleFormat::Int,
            },
        }
    }
}

/// # Errors
/// The first option out of range, worded for the status bar.
pub fn validate(options: &ExportOptions) -> Result<(), String> {
    if !matches!(options.mode.as_str(), "song" | "pattern") {
        return Err("mode must be song or pattern".into());
    }
    if !matches!(options.sample_rate, 44100 | 48000 | 96000) {
        return Err("sampleRate must be 44100, 48000 or 96000".into());
    }
    BitDepth::from_bits(options.bit_depth)?;
    if !options.tail_seconds.is_finite() || !(0.0..=30.0).contains(&options.tail_seconds) {
        return Err("tailSeconds must be 0..30".into());
    }
    Ok(())
}

/// Refuse an export that would render nothing, before a worker starts.
///
/// # Errors
/// Song mode over an empty playlist.
pub fn precheck(options: &ExportOptions, project: &ProjectFile) -> Result<(), String> {
    if options.mode == "song" && project.playlist.clips.is_empty() {
        return Err(EMPTY_PLAYLIST.into());
    }
    Ok(())
}

/// The empty-playlist refusal, shared with the renderer's own check.
pub const EMPTY_PLAYLIST: &str =
    "the playlist is empty; place clips in the playlist or render the pattern";

fn render_options(options: &ExportOptions) -> RenderOptions {
    RenderOptions {
        mode: options.mode.clone(),
        pattern_id: None,
        start_bar: None,
        bars: None,
        tail_seconds: Some(options.tail_seconds),
        sample_rate: Some(options.sample_rate),
        track_id: None,
    }
}

/// Where the WAV is written until it is complete: `<name>.part`, beside it.
pub fn part_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    path.with_file_name(name)
}

/// `path` with a `.wav` extension, appended rather than substituted so a name
/// with a dot in it ("mix v1.2") keeps all of itself.
pub fn ensure_wav_extension(path: PathBuf) -> PathBuf {
    if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
    {
        return path;
    }
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".wav");
    path.with_file_name(name)
}

/// `<project name>.wav`, or `Untitled.wav` for a session never saved.
pub fn default_file_name(project: Option<&Path>) -> String {
    let stem = project
        .and_then(Path::file_stem)
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| crate::project_file::UNTITLED.to_owned());
    format!("{stem}.wav")
}

/// Refuse a destination that cannot be written, before anything is rendered.
///
/// # Errors
/// A folder, a missing parent, or a folder the user may not write to.
pub fn check_writable(path: &Path) -> Result<(), String> {
    if path.is_dir() {
        return Err(format!("{} is a folder", path.display()));
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| format!("{} has no folder", path.display()))?;
    if !parent.is_dir() {
        return Err(format!("the folder {} does not exist", parent.display()));
    }
    let probe = part_path(path);
    File::create(&probe).map_err(|e| format!("cannot write to {}: {e}", parent.display()))?;
    let _ = std::fs::remove_file(probe);
    Ok(())
}

/// The progress a worker last wrote, if it is readable yet.
pub fn read_progress(path: &Path) -> Option<f64> {
    std::fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(0.0, 1.0))
}

/// Throttled writer of [`PROGRESS_FILE`].
struct Progress {
    path: PathBuf,
    last: Option<Instant>,
}

impl Progress {
    fn report(&mut self, value: f64) {
        if self
            .last
            .is_some_and(|last| last.elapsed() < PROGRESS_INTERVAL)
        {
            return;
        }
        self.last = Some(Instant::now());
        // Progress is a courtesy; a failed write must not fail the export.
        let _ = std::fs::write(&self.path, format!("{value:.4}"));
    }
}

/// Linear gain Normalize applies for a whole-file `peak`; `1.0` when off or silent.
pub fn normalize_gain(peak: f32, normalize: bool) -> f32 {
    if !normalize || !peak.is_finite() || peak <= 0.0 {
        return 1.0;
    }
    10.0_f32.powf(NORMALIZE_CEILING_DBFS / 20.0) / peak
}

/// Triangular-PDF dither, ±1 LSB, from a fixed-seed xorshift: deterministic, so
/// the same session exports to the same bytes.
struct Tpdf(u64);

impl Tpdf {
    const fn new() -> Self {
        Self(0x9E37_79B9_7F4A_7C15)
    }

    fn uniform(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1_u64 << 24) as f32
    }

    /// In LSBs, in (-1, 1).
    fn next(&mut self) -> f32 {
        self.uniform() - self.uniform()
    }
}

/// What an encode measured.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EncodeStats {
    pub frames: u64,
    /// Loudest sample after the gain, linear.
    pub peak: f32,
    /// Samples beyond full scale after the gain: clipped in PCM, kept in float.
    pub over: u64,
}

/// Encode `frames` interleaved stereo little-endian `f32` frames from `source`
/// into a WAV at `path`, through `<path>.part`.
///
/// # Errors
/// A short or unreadable source, or a write/rename failure. The partial file
/// is removed on every failure.
pub fn encode_wav(
    source: &mut dyn Read,
    frames: u64,
    path: &Path,
    sample_rate: u32,
    depth: BitDepth,
    gain: f32,
    progress: &mut dyn FnMut(f64),
) -> Result<EncodeStats, String> {
    let part = part_path(path);
    let result =
        encode_part(source, frames, &part, sample_rate, depth, gain, progress).and_then(|stats| {
            std::fs::rename(&part, path).map_err(|e| {
                format!(
                    "could not write {}: {e} (is it open in another program?)",
                    path.display()
                )
            })?;
            Ok(stats)
        });
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result
}

fn encode_part(
    source: &mut dyn Read,
    frames: u64,
    part: &Path,
    sample_rate: u32,
    depth: BitDepth,
    gain: f32,
    progress: &mut dyn FnMut(f64),
) -> Result<EncodeStats, String> {
    let io = |e: &dyn std::fmt::Display| format!("could not write {}: {e}", part.display());
    let mut writer = hound::WavWriter::create(part, depth.spec(sample_rate)).map_err(|e| io(&e))?;
    let total = frames * 2;
    let mut bytes = vec![0_u8; ENCODE_CHUNK * 4];
    let mut dither = Tpdf::new();
    let mut stats = EncodeStats {
        frames,
        peak: 0.0,
        over: 0,
    };
    let mut done = 0_u64;
    while done < total {
        let count = (total - done).min(ENCODE_CHUNK as u64) as usize;
        let chunk = &mut bytes[..count * 4];
        source
            .read_exact(chunk)
            .map_err(|e| format!("could not read the rendered audio: {e}"))?;
        for raw in chunk.as_chunks::<4>().0 {
            let sample = f32::from_le_bytes(*raw) * gain;
            let magnitude = sample.abs();
            stats.peak = stats.peak.max(magnitude);
            if magnitude > 1.0 {
                stats.over += 1;
            }
            match depth {
                BitDepth::Float32 => writer.write_sample(sample),
                BitDepth::Pcm24 => writer.write_sample(quantize(sample, 8_388_607.0, 0.0)),
                BitDepth::Pcm16 => {
                    writer.write_sample(quantize(sample, 32_767.0, dither.next()) as i16)
                }
            }
            .map_err(|e| io(&e))?;
        }
        done += count as u64;
        progress(done as f64 / total.max(1) as f64);
    }
    writer.finalize().map_err(|e| io(&e))?;
    Ok(stats)
}

/// Scale to a signed integer of full scale `full`, add `dither` LSBs, round,
/// and clip to the format's range.
fn quantize(sample: f32, full: f32, dither: f32) -> i32 {
    let value = (sample * full + dither).round();
    value.clamp(-full - 1.0, full) as i32
}

/// The worker's half: render `target`'s session and write its WAV.
///
/// # Errors
/// Any render failure (missing plugin, empty playlist, too long, non-finite
/// audio) or file error, worded for the export dialog.
pub fn run(project: &ProjectFile, target: &ExportTarget, dir: &Path) -> Result<Value, String> {
    let options = &target.options;
    validate(options)?;
    let depth = BitDepth::from_bits(options.bit_depth)?;
    let scratch = dir.join(SCRATCH_FILE);
    let mut progress = Progress {
        path: dir.join(PROGRESS_FILE),
        last: None,
    };
    let result = render_and_encode(project, target, depth, &scratch, &mut progress);
    let _ = std::fs::remove_file(&scratch);
    let (rate, peak_before, stats) = result?;
    let _ = std::fs::write(&progress.path, "1");
    let gain = normalize_gain(peak_before, options.normalize);
    Ok(json!({
        "status": "completed",
        "progress": 1.0,
        "path": target.path,
        "frames": stats.frames,
        "seconds": stats.frames as f64 / rate,
        "sampleRate": options.sample_rate,
        "bitDepth": options.bit_depth,
        "peakDbfs": (stats.peak > 0.0).then(|| 20.0 * f64::from(stats.peak).log10()),
        "clippedSamples": stats.over,
        "normalizeGainDb": options.normalize.then(|| 20.0 * f64::from(gain).log10()),
        "silent": peak_before == 0.0,
    }))
}

fn render_and_encode(
    project: &ProjectFile,
    target: &ExportTarget,
    depth: BitDepth,
    scratch: &Path,
    progress: &mut Progress,
) -> Result<(f64, f32, EncodeStats), String> {
    let scratch_error = |e: std::io::Error| format!("could not write the render scratch file: {e}");
    let mut writer = BufWriter::new(File::create(scratch).map_err(scratch_error)?);
    let mut peak = 0.0_f32;
    let mut frames = 0_usize;
    let mut bytes = Vec::with_capacity(256 * 2 * 4);
    let rate = render_into(
        project,
        &render_options(&target.options),
        None,
        RenderLength::Exact {
            max_seconds: MAX_EXPORT_SECONDS,
        },
        &mut |block, delivered, total| {
            bytes.clear();
            for sample in block {
                peak = peak.max(sample.abs());
                bytes.extend_from_slice(&sample.to_le_bytes());
            }
            writer.write_all(&bytes).map_err(scratch_error)?;
            frames = delivered;
            progress.report(RENDER_SHARE * delivered as f64 / total.max(1) as f64);
            Ok(())
        },
    )?;
    writer.flush().map_err(scratch_error)?;
    drop(writer);
    let gain = normalize_gain(peak, target.options.normalize);
    let mut reader = BufReader::new(File::open(scratch).map_err(scratch_error)?);
    let stats = encode_wav(
        &mut reader,
        frames as u64,
        &target.path,
        target.options.sample_rate,
        depth,
        gain,
        &mut |share| progress.report(RENDER_SHARE + (1.0 - RENDER_SHARE) * share),
    )?;
    Ok((rate, peak, stats))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_file::{AudioSourceEntry, ClipEntry, PlaylistTrackEntry};
    use crate::{AppState, ChannelClipDto, MelodicNoteDto, PatternDto, SourceEntry};

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(label: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("mydaw-export-{label}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn raw(samples: &[f32]) -> Vec<u8> {
        samples.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn encode(samples: &[f32], depth: BitDepth, gain: f32, path: &Path) -> EncodeStats {
        let bytes = raw(samples);
        encode_wav(
            &mut bytes.as_slice(),
            (samples.len() / 2) as u64,
            path,
            48_000,
            depth,
            gain,
            &mut |_| {},
        )
        .unwrap()
    }

    fn options(mode: &str, bit_depth: u16, tail_seconds: f64) -> ExportOptions {
        ExportOptions {
            mode: mode.into(),
            sample_rate: 48_000,
            bit_depth,
            tail_seconds,
            normalize: false,
        }
    }

    #[test]
    fn each_bit_depth_writes_its_own_format_and_every_frame() {
        let dir = TempDir::new("depth");
        let samples = [0.5_f32, -0.5, 0.25, -0.25, 0.0, 1.0];
        for (depth, bits, format) in [
            (BitDepth::Pcm16, 16, hound::SampleFormat::Int),
            (BitDepth::Pcm24, 24, hound::SampleFormat::Int),
            (BitDepth::Float32, 32, hound::SampleFormat::Float),
        ] {
            let path = dir.0.join(format!("{bits}.wav"));
            let stats = encode(&samples, depth, 1.0, &path);
            assert_eq!(stats.frames, 3);
            let reader = hound::WavReader::open(&path).unwrap();
            let spec = reader.spec();
            assert_eq!(
                (
                    spec.channels,
                    spec.sample_rate,
                    spec.bits_per_sample,
                    spec.sample_format
                ),
                (2, 48_000, bits, format)
            );
            assert_eq!(reader.duration(), 3, "{bits}-bit frames");
            assert!(!part_path(&path).exists(), "the partial file was renamed");
        }
        let mut float = hound::WavReader::open(dir.0.join("32.wav")).unwrap();
        let values: Vec<f32> = float.samples::<f32>().map(Result::unwrap).collect();
        assert_eq!(values, samples);
        let mut pcm24 = hound::WavReader::open(dir.0.join("24.wav")).unwrap();
        let values: Vec<i32> = pcm24.samples::<i32>().map(Result::unwrap).collect();
        assert_eq!(
            values,
            [4_194_304, -4_194_304, 2_097_152, -2_097_152, 0, 8_388_607]
        );
    }

    #[test]
    fn sixteen_bit_is_dithered_within_one_lsb_and_deterministic() {
        let dir = TempDir::new("dither");
        let samples: Vec<f32> = (0..2_000)
            .map(|i| ((i as f32) * 0.01).sin() * 0.3)
            .collect();
        let first = dir.0.join("a.wav");
        let second = dir.0.join("b.wav");
        encode(&samples, BitDepth::Pcm16, 1.0, &first);
        encode(&samples, BitDepth::Pcm16, 1.0, &second);
        let read = |path: &Path| -> Vec<i16> {
            hound::WavReader::open(path)
                .unwrap()
                .samples::<i16>()
                .map(Result::unwrap)
                .collect()
        };
        let written = read(&first);
        assert_eq!(written, read(&second), "same session, same bytes");
        let mut differs = false;
        for (value, sample) in written.iter().zip(&samples) {
            let exact = sample * 32_767.0;
            assert!(
                (f32::from(*value) - exact).abs() <= 1.5,
                "{value} vs {exact}"
            );
            differs |= f32::from(*value) != exact.round();
        }
        assert!(differs, "dither must actually move some samples");
    }

    #[test]
    fn overs_are_counted_clipped_in_pcm_and_kept_in_float() {
        let dir = TempDir::new("over");
        let samples = [1.5_f32, -2.0, 0.5, 0.5];
        let pcm = dir.0.join("pcm.wav");
        let stats = encode(&samples, BitDepth::Pcm24, 1.0, &pcm);
        assert_eq!(stats.over, 2);
        assert_eq!(stats.peak, 2.0);
        let values: Vec<i32> = hound::WavReader::open(&pcm)
            .unwrap()
            .samples::<i32>()
            .map(Result::unwrap)
            .collect();
        assert_eq!(&values[..2], &[8_388_607, -8_388_608]);
        let float = dir.0.join("float.wav");
        assert_eq!(encode(&samples, BitDepth::Float32, 1.0, &float).over, 2);
        let values: Vec<f32> = hound::WavReader::open(&float)
            .unwrap()
            .samples::<f32>()
            .map(Result::unwrap)
            .collect();
        assert_eq!(&values[..2], &[1.5, -2.0]);
    }

    #[test]
    fn normalize_puts_the_peak_at_the_ceiling_and_is_off_by_default() {
        assert_eq!(normalize_gain(0.5, false), 1.0);
        assert_eq!(normalize_gain(0.0, true), 1.0, "silence stays silence");
        for peak in [0.25_f32, 1.0, 3.0] {
            let db = 20.0 * (peak * normalize_gain(peak, true)).log10();
            assert!((db - NORMALIZE_CEILING_DBFS).abs() < 1e-4, "{db}");
        }
        let dir = TempDir::new("normalize");
        let path = dir.0.join("n.wav");
        let stats = encode(
            &[2.0, -1.0],
            BitDepth::Float32,
            normalize_gain(2.0, true),
            &path,
        );
        assert_eq!(stats.over, 0, "normalizing an over brings it back under");
    }

    #[test]
    fn a_short_source_fails_and_leaves_neither_the_part_nor_the_destination() {
        let dir = TempDir::new("short");
        let path = dir.0.join("song.wav");
        std::fs::write(&path, b"previous export").unwrap();
        let bytes = raw(&[0.1, 0.1]);
        let error = encode_wav(
            &mut bytes.as_slice(),
            10,
            &path,
            48_000,
            BitDepth::Pcm24,
            1.0,
            &mut |_| {},
        )
        .unwrap_err();
        assert!(error.contains("rendered audio"), "{error}");
        assert!(!part_path(&path).exists());
        assert_eq!(std::fs::read(&path).unwrap(), b"previous export");
    }

    #[test]
    fn options_are_validated_and_names_are_wav() {
        assert!(validate(&options("song", 24, 2.0)).is_ok());
        assert!(validate(&options("pattern", 16, 0.0)).is_ok());
        assert!(validate(&options("loop", 24, 2.0))
            .unwrap_err()
            .contains("mode"));
        assert!(validate(&options("song", 8, 2.0))
            .unwrap_err()
            .contains("bitDepth"));
        assert!(validate(&options("song", 24, 31.0))
            .unwrap_err()
            .contains("tail"));
        assert!(validate(&options("song", 24, f64::NAN)).is_err());
        let mut rate = options("song", 24, 2.0);
        rate.sample_rate = 22_050;
        assert!(validate(&rate).unwrap_err().contains("sampleRate"));

        assert_eq!(
            ensure_wav_extension(PathBuf::from("C:/m/Song.WAV")),
            PathBuf::from("C:/m/Song.WAV")
        );
        assert_eq!(
            ensure_wav_extension(PathBuf::from("C:/m/mix v1.2")),
            PathBuf::from("C:/m/mix v1.2.wav")
        );
        assert_eq!(
            part_path(Path::new("C:/m/a.wav")),
            PathBuf::from("C:/m/a.wav.part")
        );
        assert_eq!(
            default_file_name(Some(Path::new("C:/p/Night Drive.mydaw"))),
            "Night Drive.wav"
        );
        assert_eq!(default_file_name(None), "Untitled.wav");
    }

    #[test]
    fn an_unwritable_destination_is_refused_before_rendering() {
        let dir = TempDir::new("writable");
        assert!(check_writable(&dir.0.join("ok.wav")).is_ok());
        assert!(
            !part_path(&dir.0.join("ok.wav")).exists(),
            "the probe is removed"
        );
        assert!(check_writable(&dir.0).unwrap_err().contains("folder"));
        assert!(check_writable(&dir.0.join("missing").join("a.wav"))
            .unwrap_err()
            .contains("does not exist"));
    }

    #[test]
    fn progress_is_read_back_clamped_and_garbage_is_ignored() {
        let dir = TempDir::new("progress");
        let path = dir.0.join(PROGRESS_FILE);
        assert_eq!(read_progress(&path), None);
        std::fs::write(&path, "0.4200").unwrap();
        assert_eq!(read_progress(&path), Some(0.42));
        std::fs::write(&path, "7").unwrap();
        assert_eq!(read_progress(&path), Some(1.0));
        std::fs::write(&path, "0.4").unwrap();
        std::fs::write(&path, "").unwrap();
        assert_eq!(read_progress(&path), None);
    }

    /// A one-track session, poly-synth, 120 BPM (one bar = 2 s), no clips.
    fn session() -> ProjectFile {
        let mut kernel = crate::boot_kernel().unwrap();
        let state = AppState::from_kernel(&kernel).unwrap();
        let mut file = crate::default_project_file(state.sampler());
        kernel.shutdown();
        file.tracks.truncate(1);
        file.tracks[0].source = SourceEntry::Internal {
            id: "poly-synth".into(),
        };
        file.tracks[0].source_params.clear();
        file.tracks[0].inserts.clear();
        file.bpm = 120.0;
        file.patterns = vec![PatternDto {
            id: 1,
            name: "Melody".into(),
            color: None,
            steps: 32,
            channels: vec![ChannelClipDto {
                channel: 0,
                mode: "roll".into(),
                steps: None,
                notes: vec![MelodicNoteDto {
                    key: 69,
                    start: 0,
                    length: 1920,
                    velocity: 0.8,
                }],
            }],
        }];
        file.active_pattern = 1;
        file.playlist.clips.clear();
        file.playlist.tracks = vec![PlaylistTrackEntry {
            id: 1,
            name: "Vocals".into(),
            muted: false,
            soloed: false,
        }];
        file
    }

    fn read_frames(path: &Path) -> (hound::WavSpec, Vec<i32>) {
        let mut reader = hound::WavReader::open(path).unwrap();
        let spec = reader.spec();
        (spec, reader.samples::<i32>().map(Result::unwrap).collect())
    }

    #[test]
    fn a_song_with_only_an_audio_clip_exports_to_the_clip_end_plus_the_tail() {
        let dir = TempDir::new("song");
        // One second of a 0.5 sine: a recorded vocal take or a dropped beat.
        let take = dir.0.join("take.wav");
        let mut writer = hound::WavWriter::create(
            &take,
            hound::WavSpec {
                channels: 1,
                sample_rate: 48_000,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )
        .unwrap();
        for i in 0..48_000 {
            let phase = std::f32::consts::TAU * 220.0 * i as f32 / 48_000.0;
            writer.write_sample(phase.sin() * 0.5).unwrap();
        }
        writer.finalize().unwrap();
        let mut file = session();
        file.audio_sources = vec![AudioSourceEntry {
            id: 7,
            path: take.to_string_lossy().into_owned(),
        }];
        // Starts at bar 2 (2 s), lasts half a bar (1 s): the arrangement ends
        // at 3 s, mid-bar, which a bar-rounded render would pad to 4 s.
        file.playlist.clips = vec![ClipEntry {
            id: 1,
            kind: "audio".into(),
            track: 0,
            start: 3_840,
            length: 1_920,
            pattern_id: None,
            source_id: Some(7),
            offset_frames: Some(0),
            gain_db: Some(0.0),
            muted: false,
        }];
        let target = ExportTarget {
            options: options("song", 24, 0.5),
            path: dir.0.join("song.wav"),
        };
        let work = TempDir::new("song-job");
        let result = run(&file, &target, &work.0).unwrap();
        assert_eq!(result["status"], "completed");
        assert!(!work.0.join(SCRATCH_FILE).exists(), "scratch is removed");
        let (spec, samples) = read_frames(&target.path);
        assert_eq!((spec.bits_per_sample, spec.sample_rate), (24, 48_000));
        assert_eq!(samples.len() / 2, 168_000, "3 s arrangement + 0.5 s tail");
        assert_eq!(result["frames"], 168_000);
        let peak = |from: usize, to: usize| {
            samples[from * 2..to * 2]
                .iter()
                .map(|v| v.unsigned_abs())
                .max()
                .unwrap_or(0)
        };
        assert_eq!(peak(0, 95_000), 0, "silent before the clip");
        assert!(peak(97_000, 143_000) > 1_000_000, "the clip is audible");
        assert!(result["peakDbfs"].as_f64().unwrap() < 0.0);
        assert_eq!(result["clippedSamples"], 0);
        assert_eq!(result["silent"], false);
    }

    #[test]
    fn an_empty_playlist_is_refused_and_pattern_mode_exports_one_loop() {
        let dir = TempDir::new("empty");
        let file = session();
        let target = ExportTarget {
            options: options("song", 16, 0.0),
            path: dir.0.join("empty.wav"),
        };
        assert_eq!(
            precheck(&target.options, &file).unwrap_err(),
            EMPTY_PLAYLIST
        );
        assert!(precheck(&options("pattern", 16, 0.0), &file).is_ok());
        let error = run(&file, &target, &dir.0).unwrap_err();
        assert!(error.contains("playlist is empty"), "{error}");
        assert!(!target.path.exists());
        assert!(!part_path(&target.path).exists());

        let pattern = ExportTarget {
            options: options("pattern", 32, 0.25),
            path: dir.0.join("pattern.wav"),
        };
        let result = run(&file, &pattern, &dir.0).unwrap();
        // 32 sixteenths at 120 BPM = 4 s, plus the 0.25 s tail; not rounded
        // to anything else.
        let expected = 32 * 6_000 + 12_000;
        assert_eq!(result["frames"], expected);
        let reader = hound::WavReader::open(&pattern.path).unwrap();
        assert_eq!(u64::from(reader.duration()), expected);
        assert_eq!(reader.spec().sample_format, hound::SampleFormat::Float);
    }
}
