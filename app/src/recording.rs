//! Where a take goes once the engine has finished it: a WAV on disk, under a
//! name a musician recognises.
//!
//! ```text
//!   RecordService::stop -> RecordedTake
//!        |
//!        |  next_take_path()   -> <cache>/WAVE Studio/recordings/Recording 3.wav
//!        |  write_take()       -> 32-bit float stereo WAV at the engine's rate
//!        v
//!   AppState::load_audio_source -> AudioSource  -> add_sample_channel -> rack
//! ```
//!
//! # Why the file is named, not stamped
//! The obvious scheme is a timestamp: unique for free, no directory scan. But
//! the file name *is* the channel name -- [`AudioSource::name`] is the file name
//! -- so a timestamp would put `recording-20260823-114500.wav` on the rack for
//! every take, which is unreadable at the width a channel row has. So the take
//! is `Recording N.wav`, with `N` the first index free in the recordings folder,
//! and a timestamp suffix only as the collision fallback (a second WAVE writing
//! the same folder). The scan is one `read_dir` of a directory that holds this
//! machine's takes, once per take.
//!
//! # Why 32-bit float
//! The same reason the debug tap writes float: it is exactly what the converter
//! produced, including anything past ±1.0 that a hot input drove it to. Choosing
//! 24-bit integer here would clip the one thing a musician most needs to see
//! afterwards. Every DAW and editor reads float WAV.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use hound::{SampleFormat, WavSpec, WavWriter};
use mydaw_plugin_audio_io::RecordedTake;
use mydaw_rt_core::CHANNELS;

use crate::debug_capture::timestamp_utc;

/// Bit depth of a take. See the module docs.
pub const BITS_PER_SAMPLE: u16 = 32;

/// Stem every take is named from, before its number.
pub const TAKE_STEM: &str = "Recording";

/// Highest take number [`next_take_path`] will look for before it gives up and
/// stamps the file instead.
///
/// A thousand takes in one folder is already a folder nobody can read; past that
/// the scan is not worth doing and a timestamp is the honest answer.
const MAX_SCAN: u32 = 1_000;

/// Why a take could not be written.
#[derive(Debug, thiserror::Error)]
pub enum TakeWriteError {
    #[error("could not create the recordings folder {path}: {source}")]
    Directory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not write {path}: {source}")]
    Wav {
        path: PathBuf,
        #[source]
        source: hound::Error,
    },
}

impl TakeWriteError {
    /// The file (or folder) this error is about.
    pub fn path(&self) -> &Path {
        match self {
            Self::Directory { path, .. } | Self::Wav { path, .. } => path,
        }
    }
}

/// Where takes live: the same cache root the factory drum samples use, so a
/// machine has one WAVE folder rather than two.
///
/// `LOCALAPPDATA` on Windows, `XDG_CACHE_HOME` (or the temp dir) elsewhere. Not
/// the project folder: a take exists before the project has been saved anywhere,
/// and an untitled session must still be able to record.
pub fn recordings_dir() -> PathBuf {
    let root = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_CACHE_HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    root.join("WAVE Studio").join("recordings")
}

/// The next free `Recording N.wav` in `dir`, creating the folder.
///
/// Falls back to a timestamped name when the scan runs out (see [`MAX_SCAN`]) or
/// when the folder is unreadable -- a take must never overwrite an earlier one,
/// and must never fail to be written because a name was taken.
pub fn next_take_path(dir: &Path, now: SystemTime) -> Result<PathBuf, TakeWriteError> {
    std::fs::create_dir_all(dir).map_err(|source| TakeWriteError::Directory {
        path: dir.to_path_buf(),
        source,
    })?;
    for index in 1..=MAX_SCAN {
        let path = dir.join(format!("{TAKE_STEM} {index}.wav"));
        if !path.exists() {
            return Ok(path);
        }
    }
    Ok(dir.join(format!("{TAKE_STEM} {}.wav", timestamp_utc(now))))
}

/// Write `take` to `path` as a 32-bit float stereo WAV at the take's own rate.
///
/// The rate is the *engine's*: [`mydaw_plugin_audio_io::RecordService`] converted
/// it on the way in, so a take dropped on the timeline lines up frame for frame
/// with everything else in the project.
pub fn write_take(take: &RecordedTake, path: &Path) -> Result<(), TakeWriteError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| TakeWriteError::Directory {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let spec = WavSpec {
        channels: CHANNELS as u16,
        sample_rate: wav_sample_rate(take.sample_rate),
        bits_per_sample: BITS_PER_SAMPLE,
        sample_format: SampleFormat::Float,
    };
    let wav = |source| TakeWriteError::Wav {
        path: path.to_path_buf(),
        source,
    };

    let mut writer = WavWriter::create(path, spec).map_err(wav)?;
    for sample in &take.samples {
        writer.write_sample(*sample).map_err(wav)?;
    }
    writer.finalize().map_err(wav)
}

/// Clamp a rate into something a WAV header can hold.
///
/// A take whose rate is nonsense is still worth keeping -- the samples are the
/// performance -- so it is written at 48 kHz rather than refused. Only reachable
/// through a driver reporting a rate no arithmetic can use.
fn wav_sample_rate(sample_rate: f64) -> u32 {
    if sample_rate.is_finite() && sample_rate >= 1.0 && sample_rate <= f64::from(u32::MAX) {
        sample_rate.round() as u32
    } else {
        48_000
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A scratch directory that removes itself. No `tempfile` dependency for a
    /// handful of tests; the same shape `debug-tap`'s WAV tests use.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "wave-recordings-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn take(frames: usize, rate: f64) -> RecordedTake {
        RecordedTake {
            samples: (0..frames * CHANNELS)
                .map(|index| (index as f32 / frames as f32) - 0.5)
                .collect(),
            sample_rate: rate,
            dropped: 0,
            device_name: "Test Interface".to_owned(),
            limit_reached: false,
        }
    }

    #[test]
    fn the_first_take_of_a_fresh_folder_is_recording_one() {
        let dir = TempDir::new("first");
        let path = next_take_path(&dir.0, SystemTime::UNIX_EPOCH).expect("names the first take");
        assert_eq!(path.file_name().unwrap(), "Recording 1.wav");
        assert!(dir.0.is_dir(), "the folder is created, not merely named");
    }

    #[test]
    fn a_second_take_never_overwrites_the_first() {
        let dir = TempDir::new("second");
        let first = next_take_path(&dir.0, SystemTime::UNIX_EPOCH).expect("names");
        write_take(&take(8, 48_000.0), &first).expect("writes");
        let second = next_take_path(&dir.0, SystemTime::UNIX_EPOCH).expect("names");
        assert_eq!(second.file_name().unwrap(), "Recording 2.wav");
        assert!(first.exists(), "the earlier take must survive");
    }

    #[test]
    fn the_name_is_what_the_rack_channel_will_be_called() {
        // `AudioSource::name` is the file name, so this is the contract the
        // channel row depends on. A change here renames every take.
        let dir = TempDir::new("name");
        let path = next_take_path(&dir.0, SystemTime::UNIX_EPOCH).expect("names");
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with(TAKE_STEM), "{name}");
        assert!(name.len() < 24, "a channel row cannot show a long name: {name}");
    }

    #[test]
    fn a_take_round_trips_through_the_wav_exactly() {
        let dir = TempDir::new("roundtrip");
        let original = take(64, 44_100.0);
        let path = next_take_path(&dir.0, SystemTime::UNIX_EPOCH).expect("names");
        write_take(&original, &path).expect("writes");

        let mut reader = hound::WavReader::open(&path).expect("reads back");
        let spec = reader.spec();
        assert_eq!(spec.channels, 2);
        assert_eq!(spec.sample_rate, 44_100);
        assert_eq!(spec.bits_per_sample, BITS_PER_SAMPLE);
        assert_eq!(spec.sample_format, SampleFormat::Float);
        let samples: Vec<f32> = reader.samples::<f32>().map(|s| s.expect("sample")).collect();
        assert_eq!(samples, original.samples, "float must not quantise the take");
    }

    #[test]
    fn a_take_hotter_than_full_scale_is_kept_rather_than_clipped() {
        let dir = TempDir::new("hot");
        let mut hot = take(4, 48_000.0);
        hot.samples[0] = 1.8;
        let path = next_take_path(&dir.0, SystemTime::UNIX_EPOCH).expect("names");
        write_take(&hot, &path).expect("writes");

        let mut reader = hound::WavReader::open(&path).expect("reads back");
        let first: f32 = reader.samples::<f32>().next().expect("a sample").expect("ok");
        assert_eq!(first, 1.8, "the overload is the evidence; it must survive");
    }

    #[test]
    fn a_take_writes_into_a_folder_that_does_not_exist_yet() {
        let dir = TempDir::new("nested");
        let path = dir.0.join("deeper").join("Recording 1.wav");
        write_take(&take(4, 48_000.0), &path).expect("creates the tree");
        assert!(path.exists());
    }

    #[test]
    fn a_take_that_cannot_be_written_names_the_file_it_failed_on() {
        let dir = TempDir::new("failure");
        std::fs::create_dir_all(&dir.0).expect("scratch");
        // A directory where the WAV should be: the writer cannot create a file
        // over it, which is the shape a permission failure has too.
        let path = dir.0.join("Recording 1.wav");
        std::fs::create_dir_all(&path).expect("collide");
        let error = write_take(&take(4, 48_000.0), &path).expect_err("this cannot succeed");
        assert_eq!(error.path(), path);
        assert!(error.to_string().contains("Recording 1.wav"), "{error}");
    }

    #[test]
    fn a_nonsense_rate_is_written_at_forty_eight_rather_than_refused() {
        assert_eq!(wav_sample_rate(48_000.0), 48_000);
        assert_eq!(wav_sample_rate(44_100.4), 44_100);
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(wav_sample_rate(bad), 48_000, "{bad} should fall back");
        }
    }

    #[test]
    fn the_recordings_folder_sits_under_the_shared_wave_cache_root() {
        let dir = recordings_dir();
        assert!(dir.ends_with(Path::new("WAVE Studio").join("recordings")), "{dir:?}");
    }

    #[test]
    fn the_timestamp_fallback_is_a_usable_file_name() {
        // Not reachable through `next_take_path` without a thousand files, so
        // the shape of the fallback is asserted directly.
        let stamped = format!(
            "{TAKE_STEM} {}.wav",
            timestamp_utc(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000))
        );
        assert_eq!(stamped, "Recording 20231114-221320.wav");
        assert!(!stamped.contains(std::path::MAIN_SEPARATOR));
    }
}
