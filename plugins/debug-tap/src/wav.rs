//! WAV output for a [`Capture`], and the reader the round-trip test uses.
//!
//! 32-bit float, stereo, at the engine's sample rate. Float rather than 16- or
//! 24-bit integer on purpose: the point of the harness is to show what the graph
//! *actually produced*, and that includes samples beyond +/-1.0 and any NaN a
//! broken plugin emitted. Quantising to an integer format would silently clamp
//! exactly the pathologies the tool exists to find.

use std::path::Path;

use hound::{SampleFormat, WavSpec, WavWriter};

use crate::capture::Capture;
use crate::error::{ensure_parent_dir, CaptureIoError};

/// Bit depth of a capture WAV.
pub const BITS_PER_SAMPLE: u16 = 32;

/// Write `capture` to `path` as 32-bit float stereo, creating parent
/// directories.
///
/// A capture with an unusable sample rate (0, negative, NaN) is written at
/// 48 000 Hz rather than refused: the samples are still worth having, and the
/// real rate is in the stats JSON next to it.
pub fn write_wav(capture: &Capture, path: &Path) -> Result<(), CaptureIoError> {
    ensure_parent_dir(path)?;

    let spec = WavSpec {
        channels: capture.channels as u16,
        sample_rate: wav_sample_rate(capture.sample_rate),
        bits_per_sample: BITS_PER_SAMPLE,
        sample_format: SampleFormat::Float,
    };

    let mut writer = WavWriter::create(path, spec).map_err(|source| CaptureIoError::Wav {
        path: path.to_path_buf(),
        source,
    })?;
    for sample in &capture.samples {
        writer
            .write_sample(*sample)
            .map_err(|source| CaptureIoError::Wav {
                path: path.to_path_buf(),
                source,
            })?;
    }
    writer.finalize().map_err(|source| CaptureIoError::Wav {
        path: path.to_path_buf(),
        source,
    })
}

/// Read a float WAV back. Public because the round-trip test is the only proof
/// that the writer is correct, and because an agent may want to re-load a
/// capture it wrote earlier.
pub fn read_wav(path: &Path) -> Result<Capture, CaptureIoError> {
    let mut reader = hound::WavReader::open(path).map_err(|source| CaptureIoError::Wav {
        path: path.to_path_buf(),
        source,
    })?;
    let spec = reader.spec();
    let samples: Result<Vec<f32>, hound::Error> = reader.samples::<f32>().collect();
    let samples = samples.map_err(|source| CaptureIoError::Wav {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(Capture::new(samples, f64::from(spec.sample_rate), 0))
}

/// Clamp a capture's sample rate into something a WAV header can hold.
fn wav_sample_rate(sample_rate: f64) -> u32 {
    if sample_rate.is_finite() && sample_rate >= 1.0 && sample_rate <= u32::MAX as f64 {
        sample_rate.round() as u32
    } else {
        48_000
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A scratch directory that removes itself. No `tempfile` dependency for a
    /// handful of tests.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "mydaw-debug-tap-wav-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("scratch directory");
            Self(path)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn capture(frames: usize) -> Capture {
        let samples: Vec<f32> = (0..frames * 2)
            .map(|i| (i as f32 / frames as f32) - 0.5)
            .collect();
        Capture::new(samples, 44_100.0, 0)
    }

    #[test]
    fn a_capture_survives_a_write_read_round_trip_exactly() {
        let dir = TempDir::new("roundtrip");
        let path = dir.join("capture.wav");
        let original = capture(1_000);

        write_wav(&original, &path).expect("write");
        let read_back = read_wav(&path).expect("read");

        assert_eq!(read_back.sample_rate, 44_100.0);
        assert_eq!(read_back.channels, 2);
        assert_eq!(read_back.frames(), original.frames());
        assert_eq!(
            read_back.samples, original.samples,
            "32-bit float is lossless; the bytes must match exactly"
        );
    }

    #[test]
    fn parent_directories_are_created_on_demand() {
        let dir = TempDir::new("nested");
        let path = dir.join("a").join("b").join("capture.wav");
        write_wav(&capture(16), &path).expect("write into a fresh tree");
        assert!(path.is_file());
    }

    #[test]
    fn out_of_range_and_non_finite_samples_are_preserved_not_clamped() {
        let dir = TempDir::new("hostile");
        let path = dir.join("capture.wav");
        let original = Capture::new(
            vec![2.5, -3.0, f32::NAN, f32::INFINITY, 0.0, 0.0],
            48_000.0,
            0,
        );

        write_wav(&original, &path).expect("write");
        let read_back = read_wav(&path).expect("read");

        assert_eq!(read_back.samples[0], 2.5, "float WAV must not clamp");
        assert_eq!(read_back.samples[1], -3.0);
        assert!(read_back.samples[2].is_nan(), "a NaN must survive the trip");
        assert_eq!(read_back.samples[3], f32::INFINITY);
    }

    #[test]
    fn an_empty_capture_writes_a_valid_but_empty_wav() {
        let dir = TempDir::new("empty");
        let path = dir.join("capture.wav");
        write_wav(&Capture::new(Vec::new(), 48_000.0, 0), &path).expect("write");
        let read_back = read_wav(&path).expect("read");
        assert_eq!(read_back.frames(), 0);
    }

    #[test]
    fn a_nonsense_sample_rate_falls_back_instead_of_failing_the_write() {
        assert_eq!(wav_sample_rate(48_000.0), 48_000);
        assert_eq!(wav_sample_rate(44_100.4), 44_100);
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, 1e30] {
            assert_eq!(wav_sample_rate(bad), 48_000, "{bad} should fall back");
        }

        let dir = TempDir::new("badrate");
        let path = dir.join("capture.wav");
        write_wav(&Capture::new(vec![0.1, 0.2], f64::NAN, 0), &path).expect("write");
        assert_eq!(read_wav(&path).unwrap().sample_rate, 48_000.0);
    }

    #[test]
    fn writing_into_a_path_that_cannot_exist_is_an_error_not_a_panic() {
        // A file used as a directory component: guaranteed to fail on Windows
        // and on POSIX alike.
        let dir = TempDir::new("blocked");
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, b"not a directory").unwrap();
        let path = blocker.join("nested").join("capture.wav");

        let error = write_wav(&capture(4), &path).expect_err("this cannot succeed");
        assert!(
            error.to_string().contains("could not write"),
            "unhelpful message: {error}"
        );
    }
}
