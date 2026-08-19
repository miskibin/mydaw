//! Decoding one audio file to interleaved stereo `f32`. **Never on the audio
//! thread.**
//!
//! A thin wrapper over `symphonia`, and thin on purpose: the only things this
//! adds to "decode the file" are the two policies the memory budget needs --
//! stop at a frame count the caller chooses, and say whether stopping cut
//! anything off, so [`Decoded::from_stereo`](crate::bank::Decoded::from_stereo)
//! knows whether a fade is owed.
//!
//! # Channel policy
//! Mono is duplicated, stereo is taken as-is, and anything wider keeps its
//! first two channels. A sampler that tried to fold 5.1 into stereo would be
//! guessing at a downmix matrix nobody asked it for; taking the front pair is
//! the choice that is obviously a simplification rather than subtly wrong.

use std::path::{Path, PathBuf};

use symphonia::core::audio::GenericAudioBufferRef;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use crate::bank::{Decoded, CHANNELS};

/// Why one sample could not be decoded.
///
/// Each of these is survivable: the loader logs it and skips the entry. That is
/// why they name the file -- the warning is the only place a user will see
/// which of ninety samples went missing.
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

/// Decode `path` to interleaved stereo `f32`.
///
/// `max_frames_for` is handed the file's own sample rate and returns the frame
/// cap, or `None` for "keep all of it" -- a closure rather than a number
/// because the cap is stated in the manifest as *seconds*, and seconds only
/// become frames once the file has been probed.
///
/// `fade_seconds` of linear fade is applied to the tail **only if** the cap
/// actually cut the file short.
///
/// # Errors
/// A [`DecodeError`] naming `path`. Every variant is survivable: callers skip
/// the entry and carry on.
pub fn decode_file(
    path: &Path,
    max_frames_for: impl Fn(f64) -> Option<usize>,
    fade_seconds: f32,
) -> Result<Decoded, DecodeError> {
    let file = std::fs::File::open(path).map_err(|source| DecodeError::Open {
        path: path.to_path_buf(),
        source,
    })?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(extension);
    }

    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|err| DecodeError::Format {
            path: path.to_path_buf(),
            reason: err.to_string(),
        })?;

    let track = format
        .first_track_known_codec(TrackType::Audio)
        .ok_or_else(|| DecodeError::NoTrack {
            path: path.to_path_buf(),
        })?;
    let track_id = track.id;
    let Some(CodecParameters::Audio(params)) = track.codec_params.as_ref() else {
        return Err(DecodeError::NoTrack {
            path: path.to_path_buf(),
        });
    };

    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|err| DecodeError::Format {
            path: path.to_path_buf(),
            reason: err.to_string(),
        })?;

    let mut content: Vec<f32> = Vec::new();
    let mut scratch: Vec<f32> = Vec::new();
    let mut sample_rate = 0_f64;
    let mut max_frames = None;
    let mut truncated = false;

    'packets: loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(err) => {
                return Err(DecodeError::Decode {
                    path: path.to_path_buf(),
                    reason: err.to_string(),
                })
            }
        };
        if packet.track_id != track_id {
            continue;
        }
        let buffer = decoder.decode(&packet).map_err(|err| DecodeError::Decode {
            path: path.to_path_buf(),
            reason: err.to_string(),
        })?;

        let (rate, channels) = spec_of(&buffer);
        if sample_rate == 0.0 {
            if rate == 0 {
                return Err(DecodeError::BadRate {
                    path: path.to_path_buf(),
                    rate,
                });
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
        return Err(DecodeError::Empty {
            path: path.to_path_buf(),
        });
    }

    Ok(Decoded::from_stereo(
        content,
        sample_rate,
        fade_seconds,
        truncated,
    ))
}

/// Sample rate and channel count of a decoded buffer.
fn spec_of(buffer: &GenericAudioBufferRef<'_>) -> (u32, usize) {
    let spec = buffer.spec();
    (spec.rate(), spec.channels().count())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{write_wav, TempDir};

    #[test]
    fn a_mono_wav_is_decoded_and_duplicated_to_both_channels() {
        let dir = TempDir::new("decode-mono");
        let path = dir.path().join("mono.wav");
        write_wav(&path, 8_000, 1, &[0.25, -0.5, 0.75, -1.0]);

        let decoded = decode_file(&path, |_| None, 0.0).expect("decodes");
        assert_eq!(decoded.frames, 4);
        assert_eq!(decoded.sample_rate, 8_000.0);
        // Padding aside, left and right must be identical.
        let content = &decoded.data[CHANNELS..decoded.data.len() - 2 * CHANNELS];
        for frame in content.chunks_exact(CHANNELS) {
            assert_eq!(frame[0], frame[1], "mono must land on both channels");
        }
        assert!((content[0] - 0.25).abs() < 1e-4, "{}", content[0]);
    }

    #[test]
    fn a_stereo_wav_keeps_its_channels_apart() {
        let dir = TempDir::new("decode-stereo");
        let path = dir.path().join("stereo.wav");
        write_wav(&path, 8_000, 2, &[1.0, -1.0, 0.5, -0.5]);

        let decoded = decode_file(&path, |_| None, 0.0).expect("decodes");
        assert_eq!(decoded.frames, 2);
        let content = &decoded.data[CHANNELS..decoded.data.len() - 2 * CHANNELS];
        assert!((content[0] - 1.0).abs() < 1e-4);
        assert!((content[1] + 1.0).abs() < 1e-4);
    }

    #[test]
    fn the_frame_cap_truncates_and_the_fade_lands_on_the_tail() {
        let dir = TempDir::new("decode-cap");
        let path = dir.path().join("long.wav");
        let samples: Vec<f32> = std::iter::repeat_n(1.0_f32, 100).collect();
        write_wav(&path, 100, 1, &samples);

        // 0.5 s at 100 Hz = 50 frames, with a 0.1 s (10 frame) fade.
        let decoded = decode_file(&path, |rate| Some((rate * 0.5) as usize), 0.1).expect("decodes");
        assert_eq!(decoded.frames, 50, "the cap must be honoured exactly");
        let content = &decoded.data[CHANNELS..decoded.data.len() - 2 * CHANNELS];
        let last = content[(decoded.frames - 1) * CHANNELS];
        assert_eq!(last, 0.0, "a truncated tail must be faded to silence");
        let head = content[0];
        assert!((head - 1.0).abs() < 1e-4, "the head is untouched: {head}");
    }

    /// A file shorter than the cap ends on its own and must not be faded.
    #[test]
    fn a_file_shorter_than_the_cap_is_not_faded() {
        let dir = TempDir::new("decode-short");
        let path = dir.path().join("short.wav");
        write_wav(
            &path,
            100,
            1,
            &std::iter::repeat_n(1.0_f32, 10).collect::<Vec<_>>(),
        );

        let decoded = decode_file(&path, |rate| Some(rate as usize), 0.5).expect("decodes");
        assert_eq!(decoded.frames, 10);
        let content = &decoded.data[CHANNELS..decoded.data.len() - 2 * CHANNELS];
        let last = content[9 * CHANNELS];
        assert!(
            (last - 1.0).abs() < 1e-4,
            "an untruncated sample must keep its tail, got {last}"
        );
    }

    #[test]
    fn a_missing_file_is_an_error_that_names_it() {
        let err = decode_file(Path::new("no-such-sample-9f2a.wav"), |_| None, 0.0).unwrap_err();
        assert!(matches!(err, DecodeError::Open { .. }));
        assert!(err.to_string().contains("no-such-sample-9f2a.wav"), "{err}");
    }

    #[test]
    fn a_file_that_is_not_audio_is_refused_rather_than_decoded_as_noise() {
        let dir = TempDir::new("decode-garbage");
        let path = dir.path().join("garbage.wav");
        std::fs::write(&path, b"this is definitely not a wave file").unwrap();
        let err = decode_file(&path, |_| None, 0.0).unwrap_err();
        assert!(
            matches!(err, DecodeError::Format { .. } | DecodeError::Decode { .. }),
            "{err}"
        );
    }
}
