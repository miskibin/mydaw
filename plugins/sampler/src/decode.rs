//! Decoding one audio file to interleaved stereo `f32`. **Never on the audio
//! thread.**
//!
//! The decode itself lives in [`mydaw_audio_file`], because the sequencer's
//! audio clips need exactly the same thing and two copies of a symphonia probe
//! loop is two places for the channel policy to drift. What stays here is the
//! part that is the *sampler's* policy rather than the file's: the memory budget
//! (stop at a frame count the manifest chose) and the fade that hides the cut.
//!
//! # Channel policy
//! Mono is duplicated, stereo is taken as-is, anything wider keeps its first two
//! channels -- see [`mydaw_audio_file`] for why.

use std::path::Path;

use crate::bank::Decoded;

/// Why one sample could not be decoded.
///
/// The shared decoder's error, re-exported under the name the sampler has always
/// used. Each variant is survivable: the loader logs it and skips the entry,
/// which is why they name the file -- the warning is the only place a user will
/// see which of ninety samples went missing.
pub use mydaw_audio_file::DecodeError;

/// Decode `path` to interleaved stereo `f32`.
///
/// `max_frames_for` is handed the file's own sample rate and returns the frame
/// cap, or `None` for "keep all of it" -- a closure rather than a number because
/// the cap is stated in the manifest as *seconds*, and seconds only become
/// frames once the file has been probed.
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
    let decoded = mydaw_audio_file::decode_stereo(path, max_frames_for)?;
    Ok(Decoded::from_stereo(
        decoded.data,
        decoded.sample_rate,
        fade_seconds,
        decoded.truncated,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bank::CHANNELS;
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
