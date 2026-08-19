//! Does a session survive being closed and reopened? Measured, through the
//! sound card, across two processes.
//!
//! Every other test in this repository proves something about MYDAW *while it is
//! running*. This one is the only one that can answer step 8's actual question,
//! because the thing being tested is what happens **after the application
//! exits** -- and a test that never exits cannot observe that. So it is two
//! tests, run as two `cargo test` invocations, and therefore two operating
//! system processes:
//!
//! ```sh
//! cargo test -p mydaw-app --test project_hardware -- --ignored --nocapture --exact \
//!     session_a_authors_a_project_measures_it_and_saves
//! cargo test -p mydaw-app --test project_hardware -- --ignored --nocapture --exact \
//!     session_b_reopens_the_project_in_a_fresh_process_and_measures_the_same_thing
//! ```
//!
//! **A** boots the shell, programs a beat, places the fixture synth on a track,
//! detunes it by loading a state blob, plays the result through the real device,
//! captures it, and saves the project. It writes what it measured to
//! [`STATS_FILE`].
//!
//! **B** boots a completely fresh process -- new kernel, new device, no plugin
//! anywhere -- opens the saved project through the same command a user's Open
//! menu sends, captures the same thing, and asserts it matches A: the same onset
//! grid, the same partials to within a bin, the same levels to within half a
//! decibel. Nothing tells B what to expect except the file.
//!
//! Both leave a WAV, an annotated PNG and a statistics JSON under
//! `.debug/captures/<timestamp>/`, and print the paths, so a disagreement can be
//! looked at rather than guessed at.
//!
//! `#[ignore]`d, because they open the audio device, are audible, and depend on
//! each other's order.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use mydaw_app::clap_bridge::{ClapCommand, ClapHostBridge, ClapTarget};
use mydaw_app::clap_host::ClapHost;
use mydaw_app::project_bridge::{ProjectBridge, ProjectCommand};
use mydaw_app::project_host::{Hosts, ProjectHost, Session};
use mydaw_app::vst3_host::Vst3Host;
use mydaw_app::{boot_kernel, capture_root, capture_to_disk, AppState, CaptureOutcome, TrackHost};
use mydaw_fixture_clap_gain::synth::{self, PLUGIN_ID as SYNTH_ID};
use mydaw_plugin_audio_io::AudioIoService;
use mydaw_plugin_debug_tap::{align_grid, detect_onsets, spectrum, OnsetConfig, Spectrum};
use mydaw_plugin_host_clap::test_util::{missing_fixture_message, stage_fixture_bundle};
use mydaw_plugin_sequencer::{AudioSourceRegistry, Pattern, SequencerService, StepRow};
use mydaw_plugin_windows::PluginWindowService;
use serde::{Deserialize, Serialize};

/// Tempo the session runs at. A dance tempo, and the engine's default, so the
/// beat grid below is a round number of samples at 48 kHz.
const BPM: f64 = 130.0;

/// The note held throughout both captures: C4, which the +12 semitone state in
/// the project turns into C5.
const HELD_KEY: u8 = 60;

/// The state the project carries: one octave up.
const DETUNE_SEMITONES: f32 = 12.0;

/// What [`HELD_KEY`] sounds at with the fixture at its defaults, and what it
/// sounds at with the project's state applied. The difference between them is
/// how this test can tell "the plugin loaded" from "the plugin loaded *and kept
/// its settings*".
const C4: f64 = 261.63;
const C5: f64 = 523.25;

/// Seconds each measured capture runs for.
const CAPTURE_SECONDS: f32 = 2.0;

/// Settling before a capture, so it starts inside steady-state playback rather
/// than on the device's first block after a change.
const SETTLE: Duration = Duration::from_millis(700);

/// The kick's band. The kit's kick sweeps 150 -> 50 Hz.
const KICK_BAND: (f64, f64) = (30.0, 140.0);

/// Partials quieter than this below the loudest one are not compared: they are
/// the tail of a drum's spectrum, and which of them makes the top of the list is
/// not a property of the *session*.
const PARTIAL_FLOOR_DB: f32 = 24.0;

/// How many of the loudest partials the two sessions must agree on.
const PARTIALS_COMPARED: usize = 4;

/// Tolerance on a matched partial, in FFT bins. One bin is the resolution the
/// measurement has; asking for better would be asking the FFT for something it
/// cannot say.
const PARTIAL_TOLERANCE_BINS: f64 = 1.0;

/// Tolerance on the peak level, in dB. The loudest sample in both captures is
/// the same kick attack of the same synthesised kit, so this is tight on
/// purpose.
const PEAK_TOLERANCE_DB: f32 = 0.5;

/// Tolerance on the RMS level, in dB. Looser than the peak, and for a reason
/// that is measurement rather than engine: a capture does not begin on a bar
/// line, so the two windows contain slightly different fractions of a kick's
/// decay.
const RMS_TOLERANCE_DB: f32 = 1.5;

/// Where the two processes meet.
///
/// Next to the captures, under `.debug/` -- which is diagnostic output rather
/// than source, and is already ignored by git. A project file written by a test
/// is exactly that.
fn workspace() -> PathBuf {
    capture_root().join(".debug").join("project-restart")
}

/// The project session A saves and session B opens.
fn project_path() -> PathBuf {
    workspace().join("restart.mydaw")
}

/// What session A measured, for session B to compare against.
const STATS_FILE: &str = "session-a.json";

// ---------------------------------------------------------------------------
// What a session sounds like
// ---------------------------------------------------------------------------

/// One capture, reduced to the numbers that describe a *session* rather than a
/// recording: where the beat lands, what pitches are sounding, how loud.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Measured {
    sample_rate: f64,
    frames: usize,
    fft_size: usize,
    bin_hz: f64,
    peak_dbfs: f32,
    rms_dbfs: f32,
    /// Energy in the kick's band, dBFS.
    kick_dbfs: f32,
    /// Kick attacks detected in the capture.
    onsets: usize,
    /// Mean interval between them, in milliseconds. At 130 bpm a beat is
    /// 461.5 ms, and the pattern puts a kick on every one.
    onset_interval_ms: f64,
    /// Worst deviation from an evenly spaced beat grid, in milliseconds.
    grid_max_error_ms: f64,
    /// The loudest partials, in descending magnitude, with their levels.
    partials_hz: Vec<f64>,
    partials_dbfs: Vec<f32>,
    wav: String,
    png: String,
}

impl Measured {
    /// Capture, analyse, and leave the artefacts on disk.
    fn take(state: &AppState, label: &str) -> Self {
        let outcome: CaptureOutcome =
            capture_to_disk(state.debug_tap(), CAPTURE_SECONDS, &capture_root())
                .expect("the engine is running, so a capture must arrive");
        let stats = &outcome.stats;
        let left = outcome.capture.channel(0);
        let sample_rate = stats.sample_rate;
        let spectrum = spectrum(&left, sample_rate);

        // The kick, on every beat. A low-band onset detector, because a hi-hat
        // is broadband and would smear a measurement of *when* the beat is.
        let onsets = detect_onsets(&left, sample_rate, &OnsetConfig::low(KICK_BAND.1 as f32));
        let beat_samples = 60.0 / BPM * sample_rate;
        let grid = align_grid(&onsets, beat_samples, sample_rate);
        let onset_interval_ms = if onsets.len() < 2 {
            0.0
        } else {
            let first = onsets.first().expect("checked").frame as f64;
            let last = onsets.last().expect("checked").frame as f64;
            (last - first) / (onsets.len() - 1) as f64 / sample_rate * 1_000.0
        };

        let peaks = spectrum.top_peaks(12);
        let loudest = peaks.first().map_or(-200.0, |peak| peak.magnitude_dbfs);
        let kept: Vec<_> = peaks
            .iter()
            .filter(|peak| peak.magnitude_dbfs > loudest - PARTIAL_FLOOR_DB)
            .take(PARTIALS_COMPARED)
            .collect();

        let channel = &stats.per_channel[0];
        let measured = Self {
            sample_rate,
            frames: stats.frames,
            fft_size: stats.fft_size,
            bin_hz: stats.frequency_resolution_hz,
            peak_dbfs: channel.peak_dbfs,
            rms_dbfs: channel.rms_dbfs,
            kick_dbfs: band_dbfs(&spectrum, KICK_BAND),
            onsets: onsets.len(),
            onset_interval_ms,
            grid_max_error_ms: grid.max_abs_error_ms,
            partials_hz: kept.iter().map(|peak| peak.frequency_hz).collect(),
            partials_dbfs: kept.iter().map(|peak| peak.magnitude_dbfs).collect(),
            wav: outcome.artifacts.wav.display().to_string(),
            png: outcome.artifacts.png.display().to_string(),
        };
        measured.report(label);
        measured
    }

    /// The lines an agent reads.
    fn report(&self, label: &str) {
        eprintln!(
            "[{label}] {} frames @ {} Hz ({:.3} s) | fft {} ({:.2} Hz/bin)",
            self.frames,
            self.sample_rate,
            self.frames as f64 / self.sample_rate,
            self.fft_size,
            self.bin_hz
        );
        eprintln!(
            "[{label}]   peak {:.2} dBFS  rms {:.2} dBFS  kick band {:.2} dB",
            self.peak_dbfs, self.rms_dbfs, self.kick_dbfs
        );
        eprintln!(
            "[{label}]   onsets {} @ {:.1} ms apart (grid error <= {:.2} ms)",
            self.onsets, self.onset_interval_ms, self.grid_max_error_ms
        );
        let partials: Vec<String> = self
            .partials_hz
            .iter()
            .zip(&self.partials_dbfs)
            .map(|(hz, db)| format!("{hz:.1} Hz ({db:.1} dB)"))
            .collect();
        eprintln!("[{label}]   partials: {}", partials.join(", "));
        eprintln!("[{label}]   wav {}", self.wav);
        eprintln!("[{label}]   png {}", self.png);
    }
}

/// Energy in a frequency band, as dBFS.
fn band_dbfs(spectrum: &Spectrum, band: (f64, f64)) -> f32 {
    if spectrum.is_empty() || spectrum.bin_hz <= 0.0 {
        return -200.0;
    }
    let low = (band.0 / spectrum.bin_hz).floor().max(1.0) as usize;
    let high = ((band.1 / spectrum.bin_hz).ceil() as usize).min(spectrum.magnitudes.len() - 1);
    if low > high {
        return -200.0;
    }
    let energy: f64 = spectrum.magnitudes[low..=high]
        .iter()
        .map(|m| f64::from(*m) * f64::from(*m))
        .sum();
    if energy <= 0.0 {
        return -200.0;
    }
    20.0 * energy.sqrt().log10() as f32
}

// ---------------------------------------------------------------------------
// The shell, played by this test
// ---------------------------------------------------------------------------

/// Everything the run loop owns, on this thread.
///
/// `ClapHost`, `Vst3Host` and `ProjectHost` are main-thread values; commands are
/// posted from worker threads exactly as Tauri commands post them, and served
/// from the loop below exactly as `RunEvent::MainEventsCleared` serves them.
struct Rig {
    state: AppState,
    clap: ClapHost,
    vst3: Vst3Host,
    project: ProjectHost,
    clap_bridge: Arc<ClapHostBridge>,
    project_bridge: Arc<ProjectBridge>,
    tracks: Arc<TrackHost>,
    audio: Arc<AudioIoService>,
    sequencer: Arc<SequencerService>,
    sources: Arc<AudioSourceRegistry>,
}

impl Rig {
    /// Boot the shell and open the device, or say why this machine cannot.
    fn boot(kernel: &mydaw_kernel::Kernel) -> Option<Self> {
        let state = AppState::from_kernel(kernel).expect("services present");
        let windows = kernel
            .ctx()
            .services
            .get::<PluginWindowService>()
            .expect("plugin-windows provides its service");

        match state.start_audio_with_project() {
            Ok(info) => eprintln!(
                "device: {} @ {} Hz, {} ch (max block {})",
                info.device_name, info.sample_rate, info.channels, info.max_block
            ),
            Err(err) => {
                eprintln!("no audio device ({err}); nothing to verify on this machine");
                return None;
            }
        }

        let clap_bridge = Arc::clone(state.clap_bridge());
        let project_bridge = Arc::clone(state.project_bridge());
        let tracks = Arc::clone(state.tracks());
        let audio = Arc::clone(state.audio());
        let sequencer = Arc::clone(state.sequencer());
        let sources = Arc::clone(state.audio_sources());
        let mut clap = ClapHost::new(Arc::clone(&windows));
        let mut vst3 = Vst3Host::new(windows);
        clap.publish(&tracks, clap_bridge.board());
        vst3.publish(&tracks, state.vst3_bridge().board());
        clap_bridge.set_served(true);
        state.vst3_bridge().set_served(true);
        project_bridge.set_served(true);

        Some(Self {
            state,
            clap,
            vst3,
            project: ProjectHost::new(),
            clap_bridge,
            project_bridge,
            tracks,
            audio,
            sequencer,
            sources,
        })
    }

    /// One pass of the run loop, in the shell's own order.
    fn tick(&mut self) {
        self.clap.tick(&self.tracks, &self.audio, &self.clap_bridge);
        self.vst3.tick(
            &self.tracks,
            &self.audio,
            self.state.vst3_bridge(),
            &mut self.clap,
        );
        let session = Session {
            tracks: &self.tracks,
            audio: &self.audio,
            sequencer: &self.sequencer,
            sources: &self.sources,
        };
        let mut hosts = Hosts {
            clap: &mut self.clap,
            clap_board: self.clap_bridge.board(),
            vst3: &mut self.vst3,
            vst3_board: self.state.vst3_bridge().board(),
        };
        self.project
            .tick(&session, &self.project_bridge, &mut hosts);
    }

    /// Pump the loop for a while, as the window's own timer does.
    fn pump(&mut self, how_long: Duration) {
        let deadline = Instant::now() + how_long;
        while Instant::now() < deadline {
            self.tick();
            sleep(Duration::from_millis(3));
        }
    }

    /// Post a CLAP request from a worker thread and serve it here.
    fn clap_command(&mut self, command: ClapCommand) {
        let bridge = Arc::clone(&self.clap_bridge);
        let worker = std::thread::spawn(move || bridge.request(command));
        self.serve_until(&worker, Duration::from_secs(10));
        worker
            .join()
            .expect("the requesting thread must not panic")
            .expect("the CLAP host answered with an error");
    }

    /// Post a project request from a worker thread and serve it here. Returns
    /// the project information the main thread published.
    fn project_command(&mut self, command: ProjectCommand) -> mydaw_app::ProjectInfoDto {
        let bridge = Arc::clone(&self.project_bridge);
        let worker = std::thread::spawn(move || bridge.request(command));
        self.serve_until(&worker, Duration::from_secs(90));
        worker
            .join()
            .expect("the requesting thread must not panic")
            .expect("the project host answered with an error")
            .into_info()
            .expect("project information")
    }

    fn serve_until<T>(&mut self, worker: &std::thread::JoinHandle<T>, patience: Duration) {
        let deadline = Instant::now() + patience;
        while !worker.is_finished() && Instant::now() < deadline {
            self.tick();
            sleep(Duration::from_millis(2));
        }
    }

    /// Hold `key` on `track`, live, the way the keyboard piano does.
    fn hold(&mut self, track: usize, key: u8) {
        self.state
            .set_selected_track(track)
            .expect("the track exists");
        self.state
            .note_on(key, 1.0)
            .expect("the note queue accepts");
    }

    fn release(&mut self, track: usize, key: u8) {
        self.state
            .set_selected_track(track)
            .expect("the track exists");
        self.state.note_off(key).expect("the note queue accepts");
    }

    /// Settle, then capture.
    fn measure(&mut self, label: &str) -> Measured {
        self.pump(SETTLE);
        Measured::take(&self.state, label)
    }

    /// Tear down in the shell's own order: stop pushing frames, close the
    /// device (which retires the graph), then destroy the plugins.
    fn shutdown(&mut self) {
        self.state.set_playing(false).ok();
        self.state.meters().shutdown();
        self.state.audio().stop();
        self.vst3.shutdown();
        self.clap.shutdown();
    }
}

/// Kick on every beat, and nothing else: a hi-hat is broadband and would smear
/// both the onsets and the partials this test measures.
fn kick_only() -> Pattern {
    let mut pattern = Pattern::with_channels(1, "Pattern 1", &[0, 1, 2, 3, 4], |channel| {
        mydaw_plugin_sequencer::DEFAULT_ROW_KEYS
            .get(channel as usize)
            .copied()
            .unwrap_or(mydaw_plugin_sequencer::DEFAULT_STEP_KEY)
    });
    let kick = pattern.channels[0].steps.as_ref().unwrap().key;
    pattern.channels[0].steps = Some(StepRow::empty(kick).with_steps(&[0, 4, 8, 12]));
    pattern
}

fn write_stats(path: &Path, measured: &Measured) {
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("workspace");
    std::fs::write(
        path,
        serde_json::to_string_pretty(measured).expect("the measurements serialise"),
    )
    .expect("the measurements are written");
}

fn read_stats(path: &Path) -> Measured {
    let text = std::fs::read_to_string(path).unwrap_or_else(|err| {
        panic!(
            "{} is not there ({err}). Run session A first:\n  \
             cargo test -p mydaw-app --test project_hardware -- --ignored --nocapture \
             --exact session_a_authors_a_project_measures_it_and_saves",
            path.display()
        )
    });
    serde_json::from_str(&text).expect("session A's measurements")
}

// ---------------------------------------------------------------------------
// A
// ---------------------------------------------------------------------------

#[test]
#[ignore = "opens the audio device, plays an audible beat, and writes a project for session B"]
fn session_a_authors_a_project_measures_it_and_saves() {
    let staging = workspace().join("plugin");
    std::fs::create_dir_all(&staging).expect("staging dir");
    let Some(bundle) = stage_fixture_bundle(&staging) else {
        eprintln!("{}", missing_fixture_message());
        return;
    };

    let kernel = boot_kernel().expect("kernel boots");
    let Some(mut rig) = Rig::boot(&kernel) else {
        return;
    };

    // --- author the session ------------------------------------------------
    assert_eq!(rig.state.set_bpm(BPM).expect("the tempo is legal"), BPM);
    rig.state
        .set_pattern(kick_only())
        .expect("the beat is valid");
    rig.state.set_playing(true).expect("the transport starts");

    rig.clap_command(ClapCommand::SetTrackSource {
        track: 1,
        bundle_path: bundle.clone(),
        plugin_id: SYNTH_ID.to_owned(),
    });
    rig.clap_command(ClapCommand::LoadState {
        target: ClapTarget::Track { index: 1 },
        state: synth::state_blob(DETUNE_SEMITONES),
    });
    // A fader move, so the saved mixer is not all defaults and B has something
    // to get wrong.
    rig.state
        .set_track_gain_db(0, -3.0)
        .expect("track 0 exists");
    rig.state.set_track_pan(1, 0.0).expect("track 1 exists");
    assert_eq!(rig.clap.instance_count(), 1);

    // --- measure it --------------------------------------------------------
    rig.hold(1, HELD_KEY);
    let a = rig.measure("A/authored");
    rig.release(1, HELD_KEY);

    assert!(
        a.onsets >= 3,
        "the beat must be audible: {} onsets found",
        a.onsets
    );
    assert!(
        !a.partials_hz.is_empty(),
        "the hosted instrument must be sounding"
    );

    // --- save it -----------------------------------------------------------
    let path = project_path();
    let info = rig.project_command(ProjectCommand::Save { path: path.clone() });
    assert_eq!(info.name, "restart");
    assert_eq!(info.missing_plugins, 0);
    assert!(info.warnings.is_empty(), "{:?}", info.warnings);
    assert!(path.is_file(), "the project file is on disk");

    // The file has to carry the plugin's state, or B has nothing to restore.
    let saved = mydaw_app::read_project_file(&path).expect("what we just wrote");
    assert_eq!(
        saved.track_state(1).unwrap(),
        synth::state_blob(DETUNE_SEMITONES),
        "the detune the capture above heard is in the file"
    );
    assert_eq!(saved.bpm, BPM);

    write_stats(&workspace().join(STATS_FILE), &a);
    eprintln!(
        "[A] saved {} and {}",
        path.display(),
        workspace().join(STATS_FILE).display()
    );

    rig.shutdown();
    assert_eq!(rig.clap.instance_count(), 0, "every instance destroyed");
}

// ---------------------------------------------------------------------------
// B
// ---------------------------------------------------------------------------

#[test]
#[ignore = "opens the audio device and reopens the project session A saved"]
fn session_b_reopens_the_project_in_a_fresh_process_and_measures_the_same_thing() {
    let path = project_path();
    let a = read_stats(&workspace().join(STATS_FILE));
    assert!(
        path.is_file(),
        "session A must have saved {}",
        path.display()
    );

    let kernel = boot_kernel().expect("kernel boots");
    let Some(mut rig) = Rig::boot(&kernel) else {
        return;
    };

    // This process has never seen the project: the default session, at the
    // default tempo, with no plugin anywhere.
    assert_eq!(rig.clap.instance_count(), 0);
    assert_eq!(rig.state.bpm(), mydaw_rt_core::DEFAULT_BPM);
    assert!(rig.state.sequencer().active_pattern().is_empty());

    // --- open it -----------------------------------------------------------
    let info = rig.project_command(ProjectCommand::Open { path: path.clone() });
    assert_eq!(info.name, "restart");
    assert_eq!(
        info.missing_plugins, 0,
        "the fixture is where the project says it is"
    );
    assert!(info.warnings.is_empty(), "{:?}", info.warnings);

    // Everything the file said, before a single sample is measured.
    assert_eq!(rig.state.bpm(), BPM);
    assert_eq!(rig.state.sequencer().active_pattern(), kick_only());
    assert_eq!(rig.clap.instance_count(), 1, "the plugin came back");
    let mixer = rig.state.mixer_state();
    assert_eq!(mixer.tracks[0].gain_db, -3.0);
    assert_eq!(mixer.tracks[1].source_id, format!("clap:{SYNTH_ID}"));
    assert!(
        mixer.tracks.iter().all(|track| track.missing.is_none()),
        "nothing is missing"
    );

    rig.state.set_playing(true).expect("the transport starts");
    rig.hold(1, HELD_KEY);
    let b = rig.measure("B/reopened");
    rig.release(1, HELD_KEY);

    // --- A next to B -------------------------------------------------------
    eprintln!("\n--- session A vs session B -------------------------------------");
    eprintln!("  {:<22} {:>14} {:>14}", "", "A (authored)", "B (reopened)");
    eprintln!(
        "  {:<22} {:>14.2} {:>14.2}",
        "peak dBFS", a.peak_dbfs, b.peak_dbfs
    );
    eprintln!(
        "  {:<22} {:>14.2} {:>14.2}",
        "rms dBFS", a.rms_dbfs, b.rms_dbfs
    );
    eprintln!(
        "  {:<22} {:>14.2} {:>14.2}",
        "kick band dB", a.kick_dbfs, b.kick_dbfs
    );
    eprintln!("  {:<22} {:>14} {:>14}", "onsets", a.onsets, b.onsets);
    eprintln!(
        "  {:<22} {:>14.1} {:>14.1}",
        "onset interval ms", a.onset_interval_ms, b.onset_interval_ms
    );
    eprintln!(
        "  {:<22} {:>14.2} {:>14.2}",
        "grid error ms (max)", a.grid_max_error_ms, b.grid_max_error_ms
    );
    for index in 0..a.partials_hz.len().max(b.partials_hz.len()) {
        let show = |list: &Vec<f64>, db: &Vec<f32>| match (list.get(index), db.get(index)) {
            (Some(hz), Some(level)) => format!("{hz:.1} ({level:.1})"),
            _ => "-".to_owned(),
        };
        eprintln!(
            "  {:<22} {:>14} {:>14}",
            format!("partial {index} Hz (dB)"),
            show(&a.partials_hz, &a.partials_dbfs),
            show(&b.partials_hz, &b.partials_dbfs)
        );
    }
    eprintln!("----------------------------------------------------------------\n");

    // --- the assertions ----------------------------------------------------
    assert_eq!(
        a.sample_rate, b.sample_rate,
        "the same device, so the same rate"
    );
    // One either way: the tap is armed some blocks after Play, so the two
    // windows do not begin at the same point in the bar and one of them can
    // contain an extra attack at its edge. *Where the beats are* is the claim
    // this test makes, and that is the interval and the grid below.
    assert!(
        a.onsets.abs_diff(b.onsets) <= 1,
        "the beat did not survive the restart: {} onsets became {}",
        a.onsets,
        b.onsets
    );
    assert!(
        (a.onset_interval_ms - b.onset_interval_ms).abs() < 5.0,
        "the beat came back at a different tempo: {:.1} ms vs {:.1} ms",
        a.onset_interval_ms,
        b.onset_interval_ms
    );
    let beat_ms = 60.0 / BPM * 1_000.0;
    for (label, measured) in [("A", &a), ("B", &b)] {
        assert!(
            (measured.onset_interval_ms - beat_ms).abs() < 8.0,
            "{label}: kicks are {:.1} ms apart, and a beat at {BPM} bpm is {beat_ms:.1} ms",
            measured.onset_interval_ms
        );
        assert!(
            measured.grid_max_error_ms < 15.0,
            "{label}: the beat is not on a grid ({:.2} ms worst error)",
            measured.grid_max_error_ms
        );
    }

    // The partials are the whole point: they are what the *plugin's restored
    // state* sounds like, and nothing in this process chose them.
    let tolerance_hz = a.bin_hz.max(b.bin_hz) * PARTIAL_TOLERANCE_BINS;
    assert!(
        !a.partials_hz.is_empty(),
        "session A measured no partials at all"
    );
    for (hz, level) in a.partials_hz.iter().zip(&a.partials_dbfs) {
        let matched = b
            .partials_hz
            .iter()
            .zip(&b.partials_dbfs)
            .find(|(other, _)| (*other - hz).abs() <= tolerance_hz);
        let (found, found_db) = matched.unwrap_or_else(|| {
            panic!(
                "{hz:.1} Hz is not in the reopened session (within {tolerance_hz:.2} Hz); \
                 B has {:?}",
                b.partials_hz
            )
        });
        assert!(
            (level - found_db).abs() <= 3.0,
            "{hz:.1} Hz came back at {found:.1} Hz but {found_db:.1} dB instead of {level:.1} dB"
        );
    }

    // And the claim that makes the partial comparison mean something. C4 is
    // what the fixture sounds at its defaults and C5 is what the project's
    // saved +12 state turns it into, so a reopened session that is playing C4
    // has loaded the plugin and thrown its settings away -- which would pass
    // every other assertion in this file.
    assert!(
        b.partials_hz.iter().any(|hz| (hz - C5).abs() < 4.0),
        "the reopened session is not playing C5 ({C5} Hz): {:?}",
        b.partials_hz
    );
    assert!(
        !b.partials_hz.iter().any(|hz| (hz - C4).abs() < 4.0),
        "the plugin came back at its defaults -- C4 ({C4} Hz) is sounding, \
         so the state did not survive the restart: {:?}",
        b.partials_hz
    );

    assert!(
        (a.peak_dbfs - b.peak_dbfs).abs() <= PEAK_TOLERANCE_DB,
        "peak level moved: {:.2} -> {:.2} dBFS",
        a.peak_dbfs,
        b.peak_dbfs
    );
    assert!(
        (a.rms_dbfs - b.rms_dbfs).abs() <= RMS_TOLERANCE_DB,
        "rms level moved: {:.2} -> {:.2} dBFS",
        a.rms_dbfs,
        b.rms_dbfs
    );
    // The kick's band is **reported and not compared**, and the reason is worth
    // writing down rather than hiding behind a wide tolerance. It is summed
    // energy over one FFT, and an FFT covers the first 32768 frames of the
    // capture -- 0.74 s, which at this tempo is one beat and a half. So the
    // number says "one kick fell in the window" or "two did", a difference of
    // about 3 dB that is a property of *when the tap armed* and not of the
    // session. The beat is asserted where it is actually knowable: the onset
    // grid above, which spans the whole capture. All that is claimed here is
    // that both sessions have a kick at all.
    for (label, measured) in [("A", &a), ("B", &b)] {
        assert!(
            measured.kick_dbfs > -60.0,
            "{label}: the beat is inaudible ({:.2} dB in the kick's band)",
            measured.kick_dbfs
        );
    }

    rig.shutdown();
    assert_eq!(rig.clap.instance_count(), 0, "every instance destroyed");
    eprintln!("the session survived a restart: A and B agree.");
}
