//! The agent-facing capture path: run a capture, write the artefacts, report.
//!
//! Two entry points on top of `mydaw-plugin-debug-tap`:
//!
//! - [`capture_to_disk`], behind the `debug_capture` Tauri command, and
//! - [`run_headless_capture`], behind the [`DEBUG_CAPTURE_ENV`] environment
//!   variable, which is how an automated agent uses MYDAW: start the binary,
//!   read stdout, look at the PNG.
//!
//! Both write the same five files into
//! `<root>/.debug/captures/<YYYYMMDD-HHMMSS>/`:
//!
//! | File | What |
//! | --- | --- |
//! | `capture.wav` | 32-bit float stereo, exactly what the graph produced |
//! | `capture.png` | waveform + zoom + log spectrum, with baked-in axis labels |
//! | `capture.axes.json` | tick values and their pixel positions in that PNG |
//! | `stats.json` | [`CaptureStats`], camelCase |
//! | `summary.txt` | the same report that goes to stdout |
//!
//! The directory is timestamped rather than overwritten, because comparing a
//! capture before a change with one after it is the whole point.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mydaw_plugin_debug_tap::{
    Capture, CaptureError, CaptureIoError, CaptureStats, DebugTapService, MAX_CAPTURE_SECONDS,
};
use serde::Serialize;

use crate::state::AppState;

/// Set this to a number of seconds and MYDAW captures that much audio on
/// startup, writes the artefacts, prints a report and exits -- no window, no
/// clicking. This is the interface an agent drives.
///
/// ```text
/// MYDAW_DEBUG_CAPTURE=2 cargo run -p mydaw-app --features custom-protocol
/// ```
pub const DEBUG_CAPTURE_ENV: &str = "MYDAW_DEBUG_CAPTURE";

/// Overrides the directory captures are written under. Defaults to the current
/// working directory, which for `cargo run` is the workspace root.
pub const CAPTURE_ROOT_ENV: &str = "MYDAW_DEBUG_DIR";

/// Directory, relative to the root, that captures land in. Git-ignored.
pub const CAPTURE_SUBDIR: [&str; 2] = [".debug", "captures"];

/// Shortest capture worth taking. Below one block of audio there is nothing to
/// analyse.
pub const MIN_CAPTURE_SECONDS: f32 = 0.01;

/// Slack added to the requested duration before a capture is declared dead.
///
/// Generous on purpose: the audio device may take a moment to come up, and a
/// timeout that fires early would report "no audio" for a system that was merely
/// slow. Deliberately bounded so a wedged audio thread cannot hang the IPC
/// command forever.
pub const CAPTURE_TIMEOUT_SLACK: Duration = Duration::from_secs(3);

/// Markers around the headless report, so a caller can find it in a log full of
/// `env_logger` output.
pub const REPORT_BEGIN: &str = "===== MYDAW DEBUG CAPTURE BEGIN =====";
/// Closing marker; see [`REPORT_BEGIN`].
pub const REPORT_END: &str = "===== MYDAW DEBUG CAPTURE END =====";

/// Why a capture-to-disk did not produce a report.
#[derive(Debug, thiserror::Error)]
pub enum DebugCaptureError {
    /// A duration that is not a usable number of seconds.
    #[error("capture duration must be between {MIN_CAPTURE_SECONDS} and {MAX_CAPTURE_SECONDS} seconds, got {0}")]
    InvalidDuration(f32),

    /// Not a single sample arrived. Almost always "the audio device never
    /// opened" or "the transport is not playing".
    #[error(
        "no audio was captured; the output device may not be open (check the startup log) or \
         the transport may be stopped"
    )]
    NoAudio,

    /// The tap itself refused.
    #[error(transparent)]
    Tap(#[from] CaptureError),

    /// An artefact could not be written.
    #[error(transparent)]
    Io(#[from] CaptureIoError),

    /// The timestamped directory could not be created.
    #[error("could not create the capture directory {path}: {source}")]
    Directory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Where the five artefacts of one capture live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureArtifacts {
    pub dir: PathBuf,
    pub wav: PathBuf,
    pub png: PathBuf,
    pub axes_json: PathBuf,
    pub stats_json: PathBuf,
    pub summary_txt: PathBuf,
}

/// A completed capture, measured and written out.
#[derive(Debug)]
pub struct CaptureOutcome {
    pub capture: Capture,
    pub stats: CaptureStats,
    pub summary: String,
    pub artifacts: CaptureArtifacts,
    /// Whether the tap timed out and this is a partial capture.
    pub timed_out: bool,
    /// Frames asked for (versus `stats.frames`, which is what arrived).
    pub requested_frames: usize,
}

/// [`CaptureOutcome`] in the shape the webview reads (camelCase).
///
/// Paths are `String`s, not `PathBuf`s: serde renders a `PathBuf` as whatever
/// the OS encoding happens to be, and one non-UTF-8 byte in a Windows path would
/// fail the whole command. Lossy conversion means a mangled character in a
/// tooltip instead of an error.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureReportDto {
    /// Every measurement. `null` where a level is digital silence (-inf dBFS).
    pub stats: CaptureStats,
    /// The human/agent-readable report, identical to `summary.txt`.
    pub summary: String,
    pub dir: String,
    pub wav_path: String,
    pub png_path: String,
    pub axes_json_path: String,
    pub stats_json_path: String,
    pub summary_path: String,
    /// Samples the audio thread dropped. Non-zero means the capture has holes.
    pub dropped_samples: u64,
    /// Whether this is a partial capture that timed out.
    pub timed_out: bool,
    pub requested_frames: usize,
    pub captured_frames: usize,
}

impl From<&CaptureOutcome> for CaptureReportDto {
    fn from(outcome: &CaptureOutcome) -> Self {
        let path = |p: &Path| p.to_string_lossy().into_owned();
        Self {
            stats: outcome.stats.clone(),
            summary: outcome.summary.clone(),
            dir: path(&outcome.artifacts.dir),
            wav_path: path(&outcome.artifacts.wav),
            png_path: path(&outcome.artifacts.png),
            axes_json_path: path(&outcome.artifacts.axes_json),
            stats_json_path: path(&outcome.artifacts.stats_json),
            summary_path: path(&outcome.artifacts.summary_txt),
            dropped_samples: outcome.capture.dropped,
            timed_out: outcome.timed_out,
            requested_frames: outcome.requested_frames,
            captured_frames: outcome.stats.frames,
        }
    }
}

/// Root directory captures are written under: [`CAPTURE_ROOT_ENV`] if set,
/// otherwise the current working directory (the workspace root under
/// `cargo run`).
pub fn capture_root() -> PathBuf {
    std::env::var_os(CAPTURE_ROOT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

/// `<root>/.debug/captures`.
pub fn captures_dir(root: &Path) -> PathBuf {
    let mut path = root.to_path_buf();
    for part in CAPTURE_SUBDIR {
        path.push(part);
    }
    path
}

/// UTC timestamp in `YYYYMMDD-HHMMSS` form.
///
/// Hand-rolled rather than pulling in `chrono` for one format string. The
/// calendar conversion is Howard Hinnant's `civil_from_days`, which is exact for
/// every date the proleptic Gregorian calendar covers.
pub fn timestamp_utc(time: SystemTime) -> String {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        rest / 3_600,
        (rest % 3_600) / 60,
        rest % 60,
    )
}

/// Days since 1970-01-01 to `(year, month, day)`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Shift the epoch to 0000-03-01 so leap days land at the end of the cycle.
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100); // [0, 365]
    let month_prime = (5 * day_of_year + 2) / 153; // [0, 11], March = 0
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Create `<captures>/<timestamp>`, suffixing if that name is taken.
///
/// Two captures within the same second are entirely possible from the UI, and
/// silently overwriting the first one would be the worst possible behaviour for
/// a tool whose job is comparison.
fn create_capture_dir(root: &Path, now: SystemTime) -> Result<PathBuf, DebugCaptureError> {
    let base = captures_dir(root);
    let stamp = timestamp_utc(now);
    for attempt in 0..100 {
        let name = if attempt == 0 {
            stamp.clone()
        } else {
            format!("{stamp}-{attempt}")
        };
        let path = base.join(name);
        match std::fs::create_dir_all(&path) {
            Ok(()) if is_empty_dir(&path) || attempt == 99 => return Ok(path),
            // Directory already exists and has content: try the next suffix.
            Ok(()) => continue,
            Err(source) => return Err(DebugCaptureError::Directory { path, source }),
        }
    }
    unreachable!("the loop returns on its last iteration")
}

fn is_empty_dir(path: &Path) -> bool {
    std::fs::read_dir(path)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false)
}

/// Run a capture and write every artefact.
///
/// A capture that times out with *some* audio is a success with
/// [`CaptureOutcome::timed_out`] set: partial audio is still evidence, and the
/// caller can see how much arrived. A capture that times out with *no* audio is
/// [`DebugCaptureError::NoAudio`], because there is nothing to look at.
pub fn capture_to_disk(
    service: &DebugTapService,
    seconds: f32,
    root: &Path,
) -> Result<CaptureOutcome, DebugCaptureError> {
    if !seconds.is_finite()
        || seconds < MIN_CAPTURE_SECONDS
        || f64::from(seconds) > MAX_CAPTURE_SECONDS
    {
        return Err(DebugCaptureError::InvalidDuration(seconds));
    }

    let duration = Duration::from_secs_f64(f64::from(seconds));
    let timeout = duration.saturating_mul(2) + CAPTURE_TIMEOUT_SLACK;

    let (capture, timed_out, requested_frames) = match service.capture(duration, timeout) {
        Ok(capture) => {
            let frames = capture.frames();
            (capture, false, frames)
        }
        Err(CaptureError::TimedOutWithSamples { got, want, capture }) if got > 0 => {
            log::warn!("debug capture timed out with {got} of {want} frames; writing what arrived");
            (*capture, true, want)
        }
        Err(CaptureError::TimedOutWithSamples { .. }) => return Err(DebugCaptureError::NoAudio),
        Err(other) => return Err(DebugCaptureError::Tap(other)),
    };

    let dir = create_capture_dir(root, SystemTime::now())?;
    let wav = dir.join("capture.wav");
    let png = dir.join("capture.png");
    let stats_json = dir.join("stats.json");
    let summary_txt = dir.join("summary.txt");

    capture.write_wav(&wav)?;
    let rendered = capture.render_png(&png)?;

    let stats = capture.stats();
    let summary = capture.summary_text();

    // `to_string_pretty` on a struct with no map keys cannot fail, but the
    // harness is a debugging tool: it does not get to `unwrap`.
    let stats_text =
        serde_json::to_string_pretty(&stats).map_err(|source| CaptureIoError::Json {
            path: stats_json.clone(),
            source,
        })?;
    std::fs::write(&stats_json, stats_text).map_err(|source| CaptureIoError::Io {
        path: stats_json.clone(),
        source,
    })?;
    std::fs::write(&summary_txt, &summary).map_err(|source| CaptureIoError::Io {
        path: summary_txt.clone(),
        source,
    })?;

    Ok(CaptureOutcome {
        capture,
        stats,
        summary,
        artifacts: CaptureArtifacts {
            dir,
            wav,
            png,
            axes_json: rendered.axes_json,
            stats_json,
            summary_txt,
        },
        timed_out,
        requested_frames,
    })
}

/// Read [`DEBUG_CAPTURE_ENV`]. `None` when unset or empty (the normal case).
///
/// A malformed value is `Some(Err(..))` rather than `None`: silently booting the
/// GUI because somebody typed `MYDAW_DEBUG_CAPTURE=2s` would be a very confusing
/// twenty minutes.
pub fn debug_capture_seconds_from_env() -> Option<Result<f32, String>> {
    let raw = std::env::var(DEBUG_CAPTURE_ENV).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(parse_capture_seconds(trimmed))
}

/// Parse the value of [`DEBUG_CAPTURE_ENV`].
pub fn parse_capture_seconds(value: &str) -> Result<f32, String> {
    let seconds: f32 = value
        .trim()
        .parse()
        .map_err(|_| format!("{DEBUG_CAPTURE_ENV} must be a number of seconds, got '{value}'"))?;
    if !seconds.is_finite()
        || seconds < MIN_CAPTURE_SECONDS
        || f64::from(seconds) > MAX_CAPTURE_SECONDS
    {
        return Err(format!(
            "{DEBUG_CAPTURE_ENV} must be between {MIN_CAPTURE_SECONDS} and {MAX_CAPTURE_SECONDS} seconds, got {seconds}"
        ));
    }
    Ok(seconds)
}

/// Render the stdout report for a finished capture, between the markers.
pub fn report_text(outcome: &CaptureOutcome) -> String {
    let mut out = String::new();
    out.push_str(REPORT_BEGIN);
    out.push('\n');
    out.push_str(&outcome.summary);
    if outcome.timed_out {
        out.push_str(&format!(
            "  WARNING: partial capture, {} of {} frames arrived before the timeout\n",
            outcome.stats.frames, outcome.requested_frames,
        ));
    }
    out.push_str("----- STATS JSON -----\n");
    match serde_json::to_string_pretty(&outcome.stats) {
        Ok(json) => out.push_str(&json),
        Err(err) => out.push_str(&format!("<stats could not be serialised: {err}>")),
    }
    out.push_str("\n----- FILES -----\n");
    out.push_str(&format!("dir  {}\n", outcome.artifacts.dir.display()));
    out.push_str(&format!("wav  {}\n", outcome.artifacts.wav.display()));
    out.push_str(&format!("png  {}\n", outcome.artifacts.png.display()));
    out.push_str(&format!("axes {}\n", outcome.artifacts.axes_json.display()));
    out.push_str(&format!(
        "json {}\n",
        outcome.artifacts.stats_json.display()
    ));
    out.push_str(&format!(
        "text {}\n",
        outcome.artifacts.summary_txt.display()
    ));
    out.push_str(REPORT_END);
    out.push('\n');
    out
}

/// Render the stdout report for a capture that never happened.
pub fn failure_text(error: &DebugCaptureError) -> String {
    format!("{REPORT_BEGIN}\nCAPTURE FAILED: {error}\n{REPORT_END}\n")
}

/// The headless path: play, capture, write, print, stop.
///
/// Returns whether the capture succeeded, which the caller turns into the
/// process exit code. Never panics -- an agent parsing stdout should always find
/// the markers, whatever went wrong.
pub fn run_headless_capture(state: &AppState, seconds: f32) -> bool {
    log::info!("headless capture requested: {seconds} s");

    // Start the transport. Failure here is not fatal on its own -- the capture
    // will report the real problem (no device) with a better message.
    if let Err(err) = state.set_playing(true) {
        log::warn!("could not start the transport for the capture: {err}");
    }

    let result = capture_to_disk(state.debug_tap(), seconds, &capture_root());

    if let Err(err) = state.set_playing(false) {
        log::debug!("could not stop the transport after the capture: {err}");
    }

    match result {
        Ok(outcome) => {
            print!("{}", report_text(&outcome));
            true
        }
        Err(err) => {
            print!("{}", failure_text(&err));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_plugin_debug_tap::{debug_tap_channel, TapRtSide};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    const SR: f64 = 48_000.0;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "mydaw-app-debug-capture-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("scratch directory");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A tap fed by a background thread, standing in for the audio callback.
    struct Feeder {
        stop: Arc<AtomicBool>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl Feeder {
        fn start(mut rt: TapRtSide, service: Arc<DebugTapService>, amplitude: f32) -> Self {
            let stop = Arc::new(AtomicBool::new(false));
            let thread_stop = Arc::clone(&stop);
            let handle = std::thread::spawn(move || {
                let mut phase = 0.0_f64;
                let increment = 440.0 / SR;
                let mut left = vec![0.0_f32; 128];
                while !thread_stop.load(Ordering::Relaxed) {
                    if service.is_armed() {
                        for sample in left.iter_mut() {
                            *sample = (phase * std::f64::consts::TAU).sin() as f32 * amplitude;
                            phase += increment;
                            if phase >= 1.0 {
                                phase -= 1.0;
                            }
                        }
                        let right = left.clone();
                        rt.push_block(&left, &right);
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            });
            Self {
                stop,
                handle: Some(handle),
            }
        }
    }

    impl Drop for Feeder {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn fed_service(amplitude: f32) -> (Arc<DebugTapService>, Feeder) {
        let (rt, consumer) = debug_tap_channel(1 << 17);
        rt.set_sample_rate(SR);
        let service = Arc::new(DebugTapService::new(consumer));
        let feeder = Feeder::start(rt, Arc::clone(&service), amplitude);
        (service, feeder)
    }

    #[test]
    fn the_timestamp_is_utc_and_sorts_lexicographically() {
        assert_eq!(
            timestamp_utc(UNIX_EPOCH),
            "19700101-000000",
            "the epoch is the anchor of the whole conversion"
        );
        // 2026-08-18 00:00:00 UTC.
        let midnight = UNIX_EPOCH + Duration::from_secs(1_787_011_200);
        assert_eq!(timestamp_utc(midnight), "20260818-000000");
        assert_eq!(
            timestamp_utc(midnight + Duration::from_secs(13 * 3_600 + 45 * 60 + 1)),
            "20260818-134501"
        );
        // A leap day, and the day after it.
        let leap = UNIX_EPOCH + Duration::from_secs(1_709_164_800);
        assert_eq!(timestamp_utc(leap), "20240229-000000");
        assert_eq!(
            timestamp_utc(leap + Duration::from_secs(86_400)),
            "20240301-000000"
        );
        // A time before the epoch cannot be represented; it must not panic.
        assert_eq!(
            timestamp_utc(UNIX_EPOCH - Duration::from_secs(10)),
            "19700101-000000"
        );

        // The point of the format: lexicographic order is chronological order,
        // so a directory listing is a timeline.
        assert!(timestamp_utc(leap) < timestamp_utc(midnight));
        assert!(timestamp_utc(UNIX_EPOCH) < timestamp_utc(leap));
    }

    #[test]
    fn the_capture_directory_is_under_a_git_ignored_debug_folder() {
        let root = Path::new("D:/MYDAW");
        let dir = captures_dir(root);
        assert!(
            dir.ends_with(Path::new(".debug").join("captures")),
            "{dir:?}"
        );
        assert!(dir.starts_with(root));
    }

    #[test]
    fn the_capture_root_honours_the_environment_override() {
        // Not asserted against a set variable: the test process shares its
        // environment with every other test in this binary. The contract under
        // test is that the default is a real, absolute directory.
        let root = capture_root();
        assert!(!root.as_os_str().is_empty());
    }

    #[test]
    fn a_second_capture_in_the_same_second_gets_its_own_directory() {
        let temp = TempDir::new("collide");
        let now = UNIX_EPOCH + Duration::from_secs(1_787_011_200);

        let first = create_capture_dir(&temp.0, now).expect("first directory");
        std::fs::write(first.join("capture.wav"), b"x").unwrap();
        let second = create_capture_dir(&temp.0, now).expect("second directory");

        assert_ne!(
            first, second,
            "a capture must never overwrite an earlier one"
        );
        assert!(first.ends_with("20260818-000000"));
        assert!(second.ends_with("20260818-000000-1"));
    }

    #[test]
    fn a_capture_writes_all_five_artefacts_and_measures_the_signal() {
        let temp = TempDir::new("artefacts");
        let (service, _feeder) = fed_service(0.5);

        let outcome = capture_to_disk(&service, 0.1, &temp.0).expect("the feeder supplies audio");

        assert!(!outcome.timed_out);
        assert_eq!(outcome.stats.frames, 4_800);
        assert_eq!(outcome.requested_frames, 4_800);
        assert!((outcome.stats.per_channel[0].peak_dbfs - (-6.02)).abs() < 0.2);
        assert!(
            (outcome.stats.per_channel[0].dominant_hz.unwrap() - 440.0).abs() < 2.0,
            "dominant was {:?}",
            outcome.stats.per_channel[0].dominant_hz
        );

        for path in [
            &outcome.artifacts.wav,
            &outcome.artifacts.png,
            &outcome.artifacts.axes_json,
            &outcome.artifacts.stats_json,
            &outcome.artifacts.summary_txt,
        ] {
            assert!(path.is_file(), "{path:?} was not written");
            assert!(
                std::fs::metadata(path).unwrap().len() > 0,
                "{path:?} is empty"
            );
        }
        assert!(outcome.artifacts.dir.starts_with(captures_dir(&temp.0)));

        // stats.json is what an agent reads; it must parse and be camelCase.
        let text = std::fs::read_to_string(&outcome.artifacts.stats_json).unwrap();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["sampleRate"], 48_000.0);
        assert_eq!(json["frames"], 4_800);
        assert_eq!(json["window"], "hann");
        assert!(json["perChannel"][0]["peakDbfs"].is_number());

        // summary.txt matches what the report prints.
        let summary = std::fs::read_to_string(&outcome.artifacts.summary_txt).unwrap();
        assert_eq!(summary, outcome.summary);
    }

    #[test]
    fn a_capture_with_nothing_playing_is_a_typed_no_audio_error() {
        let temp = TempDir::new("noaudio");
        let (rt, consumer) = debug_tap_channel(1 << 12);
        rt.set_sample_rate(SR);
        let service = DebugTapService::new(consumer);

        // 0.01 s wanted, 3.02 s of patience, nothing feeding it.
        let error = capture_to_disk(&service, MIN_CAPTURE_SECONDS, &temp.0)
            .expect_err("nothing is producing audio");
        assert!(matches!(error, DebugCaptureError::NoAudio), "{error}");
        assert!(
            error.to_string().contains("transport"),
            "the message should point at the likely cause: {error}"
        );
    }

    #[test]
    fn a_capture_before_the_engine_starts_reports_that_and_writes_nothing() {
        let temp = TempDir::new("nostream");
        let (_rt, consumer) = debug_tap_channel(1 << 12);
        let service = DebugTapService::new(consumer);

        let error = capture_to_disk(&service, 0.1, &temp.0).expect_err("no stream has run");
        assert!(
            matches!(
                error,
                DebugCaptureError::Tap(CaptureError::EngineNotRunning)
            ),
            "{error}"
        );
        assert!(!captures_dir(&temp.0).exists(), "a failure writes nothing");
    }

    #[test]
    fn absurd_durations_are_rejected_before_the_tap_is_armed() {
        let temp = TempDir::new("duration");
        let (service, _feeder) = fed_service(0.5);

        for seconds in [0.0_f32, -1.0, 0.001, 1e9, f32::NAN, f32::INFINITY] {
            let error = capture_to_disk(&service, seconds, &temp.0)
                .expect_err("{seconds} should have been rejected");
            assert!(
                matches!(error, DebugCaptureError::InvalidDuration(_)),
                "{seconds} gave {error}"
            );
        }
        assert!(!service.is_armed(), "a rejected request never arms the tap");
        assert!(
            !captures_dir(&temp.0).exists(),
            "a rejected request writes nothing"
        );
    }

    #[test]
    fn the_report_is_delimited_and_carries_the_stats_json() {
        let temp = TempDir::new("report");
        let (service, _feeder) = fed_service(0.25);
        let outcome = capture_to_disk(&service, 0.05, &temp.0).expect("capture");

        let text = report_text(&outcome);
        assert!(text.starts_with(REPORT_BEGIN), "{text}");
        assert!(text.trim_end().ends_with(REPORT_END), "{text}");
        assert!(text.contains("----- STATS JSON -----"));
        assert!(text.contains("\"sampleRate\": 48000.0"), "{text}");
        assert!(text.contains("MYDAW capture:"));
        assert!(text.contains(&outcome.artifacts.png.display().to_string()));
        assert!(
            text.is_ascii(),
            "the report must survive any console code page"
        );
    }

    #[test]
    fn a_failure_report_is_still_delimited() {
        let text = failure_text(&DebugCaptureError::NoAudio);
        assert!(text.starts_with(REPORT_BEGIN));
        assert!(text.trim_end().ends_with(REPORT_END));
        assert!(text.contains("CAPTURE FAILED"));
    }

    #[test]
    fn the_dto_uses_the_key_names_the_ui_reads() {
        let temp = TempDir::new("dto");
        let (service, _feeder) = fed_service(0.5);
        let outcome = capture_to_disk(&service, 0.05, &temp.0).expect("capture");

        let dto = CaptureReportDto::from(&outcome);
        let json: serde_json::Value = serde_json::to_value(&dto).unwrap();
        let object = json.as_object().unwrap();

        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "axesJsonPath",
                "capturedFrames",
                "dir",
                "droppedSamples",
                "pngPath",
                "requestedFrames",
                "stats",
                "statsJsonPath",
                "summary",
                "summaryPath",
                "timedOut",
                "wavPath",
            ]
        );
        assert_eq!(object["capturedFrames"], 2_400);
        assert_eq!(object["timedOut"], false);
        assert!(object["pngPath"].as_str().unwrap().ends_with("capture.png"));
        assert_eq!(object["stats"]["window"], "hann");
    }

    #[test]
    fn the_environment_variable_is_parsed_strictly() {
        assert_eq!(parse_capture_seconds("2"), Ok(2.0));
        assert_eq!(parse_capture_seconds(" 0.5 "), Ok(0.5));
        for bad in ["", "2s", "abc", "0", "-1", "1e9", "nan", "inf"] {
            assert!(
                parse_capture_seconds(bad).is_err(),
                "'{bad}' should be rejected"
            );
        }
        assert!(parse_capture_seconds("abc")
            .unwrap_err()
            .contains(DEBUG_CAPTURE_ENV));
    }
}
