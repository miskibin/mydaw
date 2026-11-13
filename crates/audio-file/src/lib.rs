//! `wave-audio-file` -- one audio file, in memory, as interleaved stereo `f32`.
//!
//! ```text
//!   *.wav / *.flac / *.ogg / *.mp3  --decode_stereo-->  DecodedAudio
//!      (non-RT: symphonia)                       { data, frames, sample_rate }
//!                                                        |
//!                                       into_padded() ----+
//!                                                         |
//!                                                         v
//!                                        [..pad..][ content ][..pad..]
//!                                                     ^
//!                                          cubic(taps(..), x) reads it
//! ```
//!
//! Two crates need the same three things and were about to grow two copies of
//! them: the **sampler** (an instrument's recordings) and the **sequencer** (an
//! audio clip on the playlist). What they share is exactly what is here --
//! "turn a path into stereo samples", "pad it so an interpolator needs no bounds
//! check", and "the interpolation kernel itself". What they do *not* share stays
//! where it belongs: the sampler's per-sample truncation fade and velocity
//! layers, the sequencer's clip registry and waveform mipmaps.
//!
//! Nothing here is real-time safe except [`cubic`], which is arithmetic on four
//! floats and is marked as such.
//!
//! # Channel policy
//! Mono is duplicated, stereo is taken as-is, and anything wider keeps its first
//! two channels. Folding 5.1 into stereo would mean guessing at a downmix matrix
//! nobody asked for; taking the front pair is obviously a simplification rather
//! than subtly wrong.
//!
//! # Format policy
//! `symphonia` with the formats a musician actually drops on a timeline: **WAV,
//! FLAC and Ogg Vorbis**, plus **MP3** through `minimp3` because
//! `symphonia-bundle-mp3` has no released 0.6. Default features stay off on
//! symphonia -- every extra decoder is attack surface for a file this project
//! did not author.

use std::path::{Path, PathBuf};

use symphonia::core::audio::GenericAudioBufferRef;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

/// Channels everything here is stored in. See the module docs.
pub const CHANNELS: usize = 2;

/// The extensions the formats in this build are usually written with.
///
/// **Not a gate.** [`decode_stereo`] *probes* the stream and never switches on
/// **Not a gate.** [`decode_stereo`] *probes* the stream (and, for `.mp3`,
/// falls back to minimp3). This exists so a file dialog's type filter and the
/// decoders cannot drift apart.
pub const AUDIO_EXTENSIONS: &[&str] = &["wav", "flac", "ogg", "oga", "mp3"];

/// Interpolation taps needed before the current frame.
pub const PAD_LEAD: usize = 1;

/// Interpolation taps needed after the current frame.
pub const PAD_TRAIL: usize = 2;

/// Why one file could not be decoded.
///
/// Every variant names the path: these are shown to a user who has just dropped
/// a file on a timeline, or logged as a warning while a project loads, and "could
/// not decode" without a filename is not a message, it is a shrug.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("could not open {path}: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is not an audio file this build can read: {reason}")]
    Format { path: PathBuf, reason: String },
    #[error("{path} has no audio track")]
    NoTrack { path: PathBuf },
    #[error("{path} could not be decoded: {reason}")]
    Decode { path: PathBuf, reason: String },
    #[error("{path} declares a sample rate of {rate}, which is not usable")]
    BadRate { path: PathBuf, rate: u32 },
    #[error("{path} decoded to no audio at all")]
    Empty { path: PathBuf },
}

impl DecodeError {
    /// The file this error is about.
    pub fn path(&self) -> &Path {
        match self {
            Self::Open { path, .. }
            | Self::Format { path, .. }
            | Self::NoTrack { path }
            | Self::Decode { path, .. }
            | Self::BadRate { path, .. }
            | Self::Empty { path } => path,
        }
    }
}

/// One decoded file: interleaved stereo `f32`, and the rate it was recorded at.
///
/// **Not** resampled to any device rate. A file keeps its own rate and the voice
/// playing it folds the ratio into the step it was going to compute anyway --
/// one multiply, at zero quality cost, versus resampling everything at load
/// (slow) or on every device change (a rebuild of the whole library).
pub struct DecodedAudio {
    /// Interleaved stereo, `frames * CHANNELS` long. No padding.
    pub data: Vec<f32>,
    /// Frames in `data`.
    pub frames: usize,
    /// The rate the file was recorded at, which need not be the device's.
    pub sample_rate: f64,
    /// Whether the frame cap stopped the decode before the file ended.
    pub truncated: bool,
}

/// Counts rather than a megabyte of samples.
impl std::fmt::Debug for DecodedAudio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecodedAudio")
            .field("frames", &self.frames)
            .field("sample_rate", &self.sample_rate)
            .field("truncated", &self.truncated)
            .finish()
    }
}

impl DecodedAudio {
    /// Seconds of audio.
    pub fn seconds(&self) -> f64 {
        if self.sample_rate > 0.0 {
            self.frames as f64 / self.sample_rate
        } else {
            0.0
        }
    }

    /// The content wrapped in [`PAD_LEAD`] silent frames before and
    /// [`PAD_TRAIL`] after, so [`taps`] never needs a bounds check.
    ///
    /// Consumes the decode: the padded buffer is the one a player keeps, and
    /// handing back both would be handing back two copies of a file that can be
    /// hundreds of megabytes.
    pub fn into_padded(self) -> Vec<f32> {
        let mut data = Vec::with_capacity((self.frames + PAD_LEAD + PAD_TRAIL) * CHANNELS);
        data.extend(std::iter::repeat_n(0.0, PAD_LEAD * CHANNELS));
        data.extend_from_slice(&self.data[..self.frames * CHANNELS]);
        data.extend(std::iter::repeat_n(0.0, PAD_TRAIL * CHANNELS));
        data
    }
}

/// The four interpolation taps around content frame `index` of `padded`.
///
/// `padded` must have come from [`DecodedAudio::into_padded`] and `index` must
/// be a *content* frame in `0..frames`; the padding is what makes `index == 0`
/// and `index == frames - 1` legal without a branch.
///
/// # RT safety
/// Four slice reads and no allocation. Safe in a callback.
#[inline]
pub fn taps(padded: &[f32], index: usize, channel: usize) -> [f32; 4] {
    // Content frame `index` sits at padded frame `index + PAD_LEAD`, so the tap
    // at `index - 1` is padded frame `index`, which always exists.
    let base = index * CHANNELS + channel;
    [padded[base], padded[base + CHANNELS], padded[base + 2 * CHANNELS], padded[base + 3 * CHANNELS]]
}

/// Four-point Catmull-Rom at `x` in `0.0..1.0` between `taps[1]` and `taps[2]`.
///
/// Exact at the sample points: `x == 0.0` returns `taps[1]` and `x == 1.0`
/// returns `taps[2]`, which is what makes unshifted playback bit-exact rather
/// than an approximation of it. Linear interpolation on resampled material is
/// audible as a dull, slightly grainy top end -- it is a lowpass whose corner
/// moves with the rate ratio. Catmull-Rom costs three more multiplies per
/// channel per frame and is C¹ continuous, so it introduces no discontinuity of
/// its own.
///
/// # RT safety
/// Seven multiplies and no allocation. Safe in a callback.
#[inline]
pub fn cubic(taps: [f32; 4], x: f32) -> f32 {
    let [y0, y1, y2, y3] = taps;
    let c0 = y1;
    let c1 = 0.5 * (y2 - y0);
    let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
    let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
    ((c3 * x + c2) * x + c1) * x + c0
}

/// Decode `path` to interleaved stereo `f32`. **Never on the audio thread.**
///
/// `max_frames_for` is handed the file's own sample rate and returns the frame
/// cap, or `None` for "keep all of it" -- a closure rather than a number because
/// a cap is usually stated in *seconds*, and seconds only become frames once the
/// file has been probed. [`DecodedAudio::truncated`] says whether it bit.
///
/// # Errors
/// A [`DecodeError`] naming `path`. Every variant is survivable: a caller
/// loading ninety files skips the entry and carries on, and a caller loading one
/// shows the message.
pub fn decode_stereo(path: &Path, max_frames_for: impl Fn(f64) -> Option<usize>) -> Result<DecodedAudio, DecodeError> {
    match decode_symphonia(path, &max_frames_for) {
        Ok(audio) => Ok(audio),
        Err(first) if looks_like_mp3(path) => match decode_mp3(path, max_frames_for) {
            Ok(audio) => Ok(audio),
            Err(_) => Err(first),
        },
        Err(err) => Err(err),
    }
}

fn looks_like_mp3(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("mp3"))
}

/// MPEG-1/2 Layer III through minimp3. i16 interleaved → stereo f32.
fn decode_mp3(path: &Path, max_frames_for: impl Fn(f64) -> Option<usize>) -> Result<DecodedAudio, DecodeError> {
    let file = std::fs::File::open(path).map_err(|source| DecodeError::Open { path: path.to_path_buf(), source })?;
    let mut decoder = minimp3::Decoder::new(file);
    let mut content: Vec<f32> = Vec::new();
    let mut sample_rate = 0_f64;
    let mut max_frames = None;
    let mut truncated = false;

    loop {
        match decoder.next_frame() {
            Ok(frame) => {
                if sample_rate == 0.0 {
                    if frame.sample_rate == 0 {
                        return Err(DecodeError::BadRate { path: path.to_path_buf(), rate: 0 });
                    }
                    sample_rate = f64::from(frame.sample_rate);
                    max_frames = max_frames_for(sample_rate);
                }
                let channels = frame.channels.max(1);
                for pcm in frame.data.chunks(channels) {
                    if max_frames.is_some_and(|cap| content.len() / CHANNELS >= cap) {
                        truncated = true;
                        break;
                    }
                    let left = pcm[0] as f32 / 32768.0;
                    let right = if channels == 1 { left } else { pcm[1] as f32 / 32768.0 };
                    content.push(left);
                    content.push(right);
                }
                if truncated {
                    break;
                }
            }
            Err(minimp3::Error::Eof) => break,
            Err(err) => return Err(DecodeError::Decode { path: path.to_path_buf(), reason: err.to_string() }),
        }
    }

    if content.is_empty() || sample_rate <= 0.0 {
        return Err(DecodeError::Empty { path: path.to_path_buf() });
    }
    let frames = content.len() / CHANNELS;
    Ok(DecodedAudio { data: content, frames, sample_rate, truncated })
}

fn decode_symphonia(path: &Path, max_frames_for: &impl Fn(f64) -> Option<usize>) -> Result<DecodedAudio, DecodeError> {
    let file = std::fs::File::open(path).map_err(|source| DecodeError::Open { path: path.to_path_buf(), source })?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    decode_symphonia_stream(stream, path, max_frames_for)
}

/// Audio that is already in memory (a file embedded in the program). `name` only supplies the
/// extension hint and the path named in errors; nothing is read from disk.
///
/// # Errors
/// A [`DecodeError`] naming `name`.
pub fn decode_stereo_bytes(
    bytes: &'static [u8],
    name: &Path,
    max_frames_for: impl Fn(f64) -> Option<usize>,
) -> Result<DecodedAudio, DecodeError> {
    let stream = MediaSourceStream::new(Box::new(std::io::Cursor::new(bytes)), Default::default());
    decode_symphonia_stream(stream, name, &max_frames_for)
}

fn decode_symphonia_stream(
    stream: MediaSourceStream<'_>,
    path: &Path,
    max_frames_for: &impl Fn(f64) -> Option<usize>,
) -> Result<DecodedAudio, DecodeError> {
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(extension);
    }

    let mut format = symphonia::default::get_probe()
        .probe(&hint, stream, FormatOptions::default(), MetadataOptions::default())
        .map_err(|err| DecodeError::Format { path: path.to_path_buf(), reason: err.to_string() })?;

    let track = format.first_track_known_codec(TrackType::Audio).ok_or_else(|| DecodeError::NoTrack { path: path.to_path_buf() })?;
    let track_id = track.id;
    let Some(CodecParameters::Audio(params)) = track.codec_params.as_ref() else {
        return Err(DecodeError::NoTrack { path: path.to_path_buf() });
    };

    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|err| DecodeError::Format { path: path.to_path_buf(), reason: err.to_string() })?;

    let mut content: Vec<f32> = Vec::new();
    let mut scratch: Vec<f32> = Vec::new();
    let mut sample_rate = 0_f64;
    let mut max_frames = None;
    let mut truncated = false;

    'packets: loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(err) => return Err(DecodeError::Decode { path: path.to_path_buf(), reason: err.to_string() }),
        };
        if packet.track_id != track_id {
            continue;
        }
        let buffer = decoder.decode(&packet).map_err(|err| DecodeError::Decode { path: path.to_path_buf(), reason: err.to_string() })?;

        let (rate, channels) = spec_of(&buffer);
        if sample_rate == 0.0 {
            if rate == 0 {
                return Err(DecodeError::BadRate { path: path.to_path_buf(), rate });
            }
            sample_rate = f64::from(rate);
            max_frames = max_frames_for(sample_rate);
        }
        if channels == 0 {
            continue;
        }

        scratch.clear();
        buffer.copy_to_vec_interleaved(&mut scratch);

        for frame in scratch.chunks_exact(channels) {
            if max_frames.is_some_and(|cap| content.len() / CHANNELS >= cap) {
                truncated = true;
                break 'packets;
            }
            let left = frame[0];
            // Mono duplicates; anything wider keeps the front pair.
            let right = if channels == 1 { left } else { frame[1] };
            content.push(left);
            content.push(right);
        }
    }

    if content.is_empty() || sample_rate <= 0.0 {
        return Err(DecodeError::Empty { path: path.to_path_buf() });
    }

    let frames = content.len() / CHANNELS;
    Ok(DecodedAudio { data: content, frames, sample_rate, truncated })
}

/// Sample rate and channel count of a decoded buffer.
fn spec_of(buffer: &GenericAudioBufferRef<'_>) -> (u32, usize) {
    let spec = buffer.spec();
    (spec.rate(), spec.channels().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory that removes itself.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let base = std::env::temp_dir().join(format!("wave-audio-file-{tag}-{}-{:?}", std::process::id(), std::thread::current().id()));
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

    fn write_wav(path: &Path, rate: u32, channels: u16, samples: &[f32]) {
        let spec = hound::WavSpec { channels, sample_rate: rate, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
        let mut writer = hound::WavWriter::create(path, spec).expect("wav writer");
        for sample in samples {
            writer.write_sample(*sample).expect("wav sample");
        }
        writer.finalize().expect("wav finalize");
    }

    #[test]
    fn a_mono_wav_is_decoded_and_duplicated_to_both_channels() {
        let dir = TempDir::new("mono");
        let path = dir.path().join("mono.wav");
        write_wav(&path, 8_000, 1, &[0.25, -0.5, 0.75, -1.0]);

        let decoded = decode_stereo(&path, |_| None).expect("decodes");
        assert_eq!(decoded.frames, 4);
        assert_eq!(decoded.sample_rate, 8_000.0);
        assert!(!decoded.truncated);
        for frame in decoded.data.as_chunks::<CHANNELS>().0 {
            assert_eq!(frame[0], frame[1], "mono must land on both channels");
        }
        assert!((decoded.data[0] - 0.25).abs() < 1e-6);
    }

    #[test]
    fn a_stereo_wav_keeps_its_channels_apart() {
        let dir = TempDir::new("stereo");
        let path = dir.path().join("stereo.wav");
        write_wav(&path, 8_000, 2, &[1.0, -1.0, 0.5, -0.5]);

        let decoded = decode_stereo(&path, |_| None).expect("decodes");
        assert_eq!(decoded.frames, 2);
        assert!((decoded.data[0] - 1.0).abs() < 1e-6);
        assert!((decoded.data[1] + 1.0).abs() < 1e-6);
    }

    /// Every non-WAV format this build claims, against real files -- **skipped**
    /// when there are none.
    ///
    /// WAV is generated above, because `hound` writes one. FLAC and Ogg Vorbis
    /// have no encoder in this workspace, and hand-rolling a valid FLAC frame to
    /// prove that symphonia can read it would be testing the fixture rather than
    /// the decoder. So this test decodes whatever real content is on the
    /// machine: point `WAVE_AUDIO_FIXTURES` at a directory of audio files, or
    /// let it find the sampler's downloaded instruments. With neither, it prints
    /// a skip and passes -- CI has no sample content and must not fail for it.
    #[test]
    fn every_format_this_build_claims_decodes_when_a_real_file_is_available() {
        let mut roots: Vec<PathBuf> = Vec::new();
        if let Ok(dir) = std::env::var("WAVE_AUDIO_FIXTURES") {
            roots.push(PathBuf::from(dir));
        }
        roots.push(PathBuf::from("../../assets/instruments"));

        let mut checked = 0_usize;
        for root in &roots {
            for entry in walk(root) {
                let extension = entry.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
                // Driven by the exported list, so a format added to the filter
                // without a decoder behind it fails here rather than in a file
                // dialog that offers something this build cannot open.
                if !AUDIO_EXTENSIONS.contains(&extension.as_str()) {
                    continue;
                }
                let decoded = decode_stereo(&entry, |rate| Some(rate as usize)).unwrap_or_else(|err| panic!("{}: {err}", entry.display()));
                assert!(decoded.frames > 0, "{} decoded to nothing", entry.display());
                assert!(decoded.sample_rate > 0.0);
                checked += 1;
                if checked >= 8 {
                    return;
                }
            }
        }
        if checked == 0 {
            eprintln!(
                "skipped: no FLAC/OGG content on this machine \
                 (set WAVE_AUDIO_FIXTURES or install the sampler assets)"
            );
        }
    }

    /// Every file under `root`, one level of recursion at a time. Missing
    /// directories are empty rather than an error -- see the test above.
    fn walk(root: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    found.push(path);
                }
            }
        }
        found
    }

    #[test]
    fn the_frame_cap_is_honoured_exactly_and_reported() {
        let dir = TempDir::new("cap");
        let path = dir.path().join("long.wav");
        write_wav(&path, 100, 1, &[1.0_f32; 100]);

        let decoded = decode_stereo(&path, |rate| Some((rate * 0.5) as usize)).expect("decodes");
        assert_eq!(decoded.frames, 50, "the cap must be honoured exactly");
        assert!(decoded.truncated);
    }

    #[test]
    fn a_file_shorter_than_the_cap_is_not_reported_as_truncated() {
        let dir = TempDir::new("short");
        let path = dir.path().join("short.wav");
        write_wav(&path, 100, 1, &[1.0_f32; 10]);

        let decoded = decode_stereo(&path, |rate| Some(rate as usize)).expect("decodes");
        assert_eq!(decoded.frames, 10);
        assert!(!decoded.truncated);
    }

    #[test]
    fn a_missing_file_is_an_error_that_names_it() {
        let err = decode_stereo(Path::new("no-such-file-9f2a.wav"), |_| None).unwrap_err();
        assert!(matches!(err, DecodeError::Open { .. }));
        assert!(err.to_string().contains("no-such-file-9f2a.wav"), "{err}");
        assert!(err.path().ends_with("no-such-file-9f2a.wav"));
    }

    #[test]
    fn a_file_that_is_not_audio_is_refused_rather_than_decoded_as_noise() {
        let dir = TempDir::new("garbage");
        let path = dir.path().join("garbage.wav");
        std::fs::write(&path, b"this is definitely not a wave file").unwrap();
        let err = decode_stereo(&path, |_| None).unwrap_err();
        assert!(matches!(err, DecodeError::Format { .. } | DecodeError::Decode { .. }), "{err}");
    }

    #[test]
    fn padding_puts_the_content_where_taps_expects_it() {
        let dir = TempDir::new("pad");
        let path = dir.path().join("pad.wav");
        write_wav(&path, 8_000, 1, &[0.1, 0.2, 0.3, 0.4]);
        let decoded = decode_stereo(&path, |_| None).expect("decodes");
        let frames = decoded.frames;
        let padded = decoded.into_padded();

        assert_eq!(padded.len(), (frames + PAD_LEAD + PAD_TRAIL) * CHANNELS);
        // Frame 0's four taps are [silence, 0.1, 0.2, 0.3] -- no bounds check.
        let first = taps(&padded, 0, 0);
        assert_eq!(first[0], 0.0);
        assert!((first[1] - 0.1).abs() < 1e-6);
        assert!((first[3] - 0.3).abs() < 1e-6);
        // And the last content frame's taps run into silence rather than off the
        // end.
        let last = taps(&padded, frames - 1, 0);
        assert!((last[1] - 0.4).abs() < 1e-6);
        assert_eq!(last[2], 0.0);
    }

    #[test]
    fn cubic_interpolation_passes_exactly_through_the_sample_points() {
        let taps = [0.2_f32, -0.4, 0.9, 0.1];
        assert_eq!(cubic(taps, 0.0), -0.4);
        assert!((cubic(taps, 1.0) - 0.9).abs() < 1e-6);
    }

    #[test]
    fn cubic_interpolation_reproduces_a_ramp_exactly() {
        // A straight line is the case where any interpolator must be exact.
        for step in 0..=10 {
            let x = step as f32 / 10.0;
            let value = cubic([0.0, 1.0, 2.0, 3.0], x);
            assert!((value - (1.0 + x)).abs() < 1e-5, "x={x} gave {value}");
        }
    }

    #[test]
    fn mp3_is_in_the_filter_this_build_claims() {
        assert!(AUDIO_EXTENSIONS.contains(&"mp3"), "the dialog and the library listing share this list");
    }

    #[test]
    fn an_mp3_decodes_through_the_minimp3_path() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("sine.mp3");
        let decoded = decode_stereo(&path, |_| None).expect("decodes mp3");
        assert!(decoded.frames > 1_000, "220 ms of 22.05 kHz should be thousands of frames, got {}", decoded.frames);
        assert!((decoded.sample_rate - 22_050.0).abs() < 1.0);
        assert_eq!(decoded.data.len(), decoded.frames * CHANNELS);
        // The fixture is a 220 Hz sine: both channels must move, not sit at zero.
        assert!(decoded.data.iter().any(|sample| sample.abs() > 0.1));
    }
}
