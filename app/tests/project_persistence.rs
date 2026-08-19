//! A project file, proved against a real plugin and a real graph.
//!
//! `app/src/project_file.rs` and `app/src/project_host.rs` carry the unit tests
//! for the format and for the phases of a load. Neither of them can answer the
//! question step 8 actually has to answer, which is not "did the bytes survive"
//! but **"did the session survive"** -- and a session is a sound.
//!
//! So this file borrows step 6's machinery: `TrackHost::build_spec` produces the
//! graph the shell itself runs, it is installed through a real `graph_channel`
//! and driven by this thread instead of by cpal, and the plugins are placed by
//! the real [`ClapHost`] through the real [`ProjectHost`]. What is substituted
//! is the *device* and the *collector thread*, both of which are played by hand
//! so the timing is deterministic rather than lucky.
//!
//! The four claims:
//!
//! 1. **A whole session round trips.** Two built-in tracks and one hosted CLAP
//!    instrument with a non-default state, a programmed beat, a tempo, faders,
//!    pans, mutes and a solo -- saved, loaded into a *fresh* session, and every
//!    field asserted. Then saved again, and the two files compared **byte for
//!    byte**.
//! 2. **A plugin's state comes back as a sound.** The fixture synth's state is a
//!    detune in semitones, so a state that survived the file is a *pitch* an FFT
//!    can measure: the reloaded session plays C5 where the saved one did,
//!    without anything having told it to.
//! 3. **A missing plugin does not cost the project.** With the bundle renamed
//!    away, the file still opens: that track is silent and marked, its reference
//!    is still in the project, and every other track plays.
//! 4. **The mixer comes back exactly**, solo included -- which is a *derived*
//!    value, recomputed from the whole project rather than stored.
//!
//! ```sh
//! cargo test -p mydaw-app --test project_persistence
//! ```
//! Runs everywhere. With no fixture bundle built (which is only possible if
//! `cargo test` was not what built it) the plugin tests report and return.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use mydaw_app::clap_bridge::{ClapBoard, ClapTarget};
use mydaw_app::clap_host::ClapHost;
use mydaw_app::project_file::{read_project_file, write_project_file, ProjectFile, SourceEntry};
use mydaw_app::project_host::{attach, capture, Hosts, ProjectHost, Session};
use mydaw_app::vst3_bridge::Vst3Board;
use mydaw_app::vst3_host::Vst3Host;
use mydaw_app::{ClapSourceRef, MissingPlugins, Project, Source, TrackHost};
use mydaw_fixture_clap_gain::synth::{self, PLUGIN_ID as SYNTH_ID, VOICE_GAIN};
use mydaw_plugin_audio_io::AudioIoService;
use mydaw_plugin_host_clap::test_util::{missing_fixture_message, stage_fixture_bundle};
use mydaw_plugin_instruments::InstrumentRegistry;
use mydaw_plugin_sequencer::{Pattern, PatternSet, PlayMode, Playlist, SequencerService, StepRow};
use mydaw_plugin_test_tone::TestToneFactory;
use mydaw_plugin_windows::PluginWindowService;
use mydaw_rt_core::basedrop::Collector;
use mydaw_rt_core::{
    graph_channel, AudioBuffers, GraphHandle, GraphLimits, NoteEvent, NoteKind, ProcessCtx,
    RtGraphSlot, TransportSnapshot,
};

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;
/// Blocks rendered before anything is measured: the fixture's 2 ms attack is 96
/// frames at 48 kHz, and the mixer's fader ramp is 480.
const SETTLE: usize = 8;
/// Blocks captured for a spectrum. 24 x 256 = 6144 frames, ~7.8 Hz per bin.
const MEASURE: usize = 24;

/// A4, and C4/C5 -- the pitches the state test measures.
const A4: f64 = 440.0;
const C4: f64 = 261.63;
const C5: f64 = 523.25;
/// Tolerance on a measured partial, in Hz. Bin spacing here is ~7.8 Hz and the
/// peaks are parabolically interpolated.
const HZ_TOLERANCE: f64 = 8.0;

/// The detune the saved project carries: one octave up.
const DETUNE_SEMITONES: f32 = 12.0;

fn limits() -> GraphLimits {
    GraphLimits {
        sample_rate: SR,
        max_block: BLOCK,
        max_events: 64,
    }
}

// ---------------------------------------------------------------------------
// Scratch space
// ---------------------------------------------------------------------------

/// A directory that deletes itself.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mydaw-project-persistence-{tag}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("scratch dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
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

/// Stage the workspace's own CLAP fixture, or say why not.
fn staged(tag: &str) -> Option<(TempDir, PathBuf)> {
    let dir = TempDir::new(tag);
    match stage_fixture_bundle(dir.path()) {
        Some(bundle) => Some((dir, bundle)),
        None => {
            eprintln!("{}", missing_fixture_message());
            None
        }
    }
}

// ---------------------------------------------------------------------------
// The rig: the shell's world, minus the device
// ---------------------------------------------------------------------------

struct Rig {
    tracks: Arc<TrackHost>,
    audio: Arc<AudioIoService>,
    sequencer: Arc<SequencerService>,
    clap: ClapHost,
    clap_board: ClapBoard,
    vst3: Vst3Host,
    vst3_board: Vst3Board,
    missing: MissingPlugins,
    project: ProjectHost,
    graph: RtGraphSlot,
    buffers: AudioBuffers,
    collector: Collector,
    /// Kept alive so the graph channel is not abandoned mid-test.
    handle: GraphHandle,
}

impl Rig {
    /// A session on `project`, with its graph already installed.
    fn new(project: Project) -> Self {
        let tracks = Arc::new(TrackHost::with_project(
            Arc::new(TestToneFactory::new()),
            Arc::new(InstrumentRegistry::new()),
            Arc::new(mydaw_plugin_sampler::SamplerRegistry::detached()),
        Arc::new(mydaw_plugin_effects::EffectRegistry::new()),
            project,
        ));
        let collector = Collector::new();
        let (mut handle, mut graph) = graph_channel(4, collector.handle());
        handle
            .install(tracks.build_spec(&limits(), collector.handle()))
            .expect("the shell's own graph must install");
        assert!(graph.poll_swap(), "the RT side must pick the graph up");

        let windows = Arc::new(PluginWindowService::new());
        Self {
            tracks,
            audio: Arc::new(AudioIoService::default()),
            sequencer: Arc::new(SequencerService::new()),
            clap: ClapHost::new(Arc::clone(&windows)),
            clap_board: ClapBoard::new(),
            vst3: Vst3Host::new(windows),
            vst3_board: Vst3Board::new(),
            missing: MissingPlugins::new(),
            project: ProjectHost::new(),
            graph,
            buffers: AudioBuffers::new(BLOCK),
            collector,
            handle,
        }
    }

    /// Install the graph the current project needs, and let the audio thread
    /// pick it up and retire the old one.
    ///
    /// This is the step the audio device performs in the application: a load
    /// installs a plugin-free graph between stripping the old session and
    /// attaching the new one's plugins. The two `collect()` calls stand in for
    /// the `basedrop` collector thread, which in the app sweeps every 250 ms and
    /// here sweeps only when this test says so.
    fn reinstall_graph(&mut self) {
        self.handle
            .install(self.tracks.build_spec(&limits(), self.collector.handle()))
            .expect("the rebuild installs");
        self.render(&[]);
        self.collector.collect();
        self.collector.collect();
    }

    /// The whole of a project load, with this test playing the audio device.
    fn load(&mut self, file: &ProjectFile) -> mydaw_app::LoadReport {
        let mut report = {
            let session = Session {
                tracks: &self.tracks,
                audio: &self.audio,
                sequencer: &self.sequencer,
            };
            let mut hosts = Hosts {
                clap: &mut self.clap,
                clap_board: &self.clap_board,
                vst3: &mut self.vst3,
                vst3_board: &self.vst3_board,
            };
            self.project
                .prepare(&session, &mut hosts, file, Some((SR, BLOCK)))
                .expect("a valid project")
        };
        self.reinstall_graph();
        {
            let session = Session {
                tracks: &self.tracks,
                audio: &self.audio,
                sequencer: &self.sequencer,
            };
            let mut hosts = Hosts {
                clap: &mut self.clap,
                clap_board: &self.clap_board,
                vst3: &mut self.vst3,
                vst3_board: &self.vst3_board,
            };
            attach(&session, &mut hosts, file, SR, BLOCK, &mut report);
        }
        report.publish(&self.missing);
        report
    }

    /// Everything a save would write.
    fn capture(&mut self) -> ProjectFile {
        let session = Session {
            tracks: &self.tracks,
            audio: &self.audio,
            sequencer: &self.sequencer,
        };
        let mut hosts = Hosts {
            clap: &mut self.clap,
            clap_board: &self.clap_board,
            vst3: &mut self.vst3,
            vst3_board: &self.vst3_board,
        };
        capture(&session, &mut hosts).0
    }

    /// Place a hosted instrument through the real command path.
    fn place(&mut self, track: usize, bundle: &Path) {
        self.clap
            .place_track_source(&self.tracks, track, bundle, SYNTH_ID, SR, BLOCK)
            .unwrap_or_else(|err| panic!("placing the synth on track {track}: {err}"));
    }

    /// Hand a placed instance a state blob, as a preset load would.
    fn load_state(&mut self, track: usize, state: &[u8]) {
        let instance = self
            .clap
            .instance_at(ClapTarget::Track { index: track })
            .expect("a plugin is placed there");
        self.clap
            .instances_mut()
            .load_state(instance, state)
            .expect("the fixture accepts its own blob");
    }

    /// Render one block carrying `events`, returning the master bus's left
    /// channel.
    fn render(&mut self, events: &[NoteEvent]) -> Vec<f32> {
        self.graph.poll_swap();
        let mut ctx = ProcessCtx::with_events(
            &mut self.buffers,
            SR,
            BLOCK,
            TransportSnapshot {
                playing: true,
                ..TransportSnapshot::default()
            },
            events,
        );
        self.graph.process(&mut ctx);
        self.buffers.as_slices()[0].to_vec()
    }

    /// Hold `notes` (channel, key) and capture the settled output.
    fn capture_audio(&mut self, notes: &[(u8, u8)]) -> Vec<f32> {
        let events: Vec<NoteEvent> = notes
            .iter()
            .map(|(channel, key)| NoteEvent {
                frame_offset: 0,
                channel: *channel,
                kind: NoteKind::NoteOn {
                    key: *key,
                    velocity: 1.0,
                },
            })
            .collect();
        self.render(&events);
        for _ in 0..SETTLE {
            self.render(&[]);
        }
        let mut captured = Vec::with_capacity(BLOCK * MEASURE);
        for _ in 0..MEASURE {
            captured.extend_from_slice(&self.render(&[]));
        }
        for (channel, key) in notes {
            let off = [NoteEvent {
                frame_offset: 0,
                channel: *channel,
                kind: NoteKind::NoteOff { key: *key },
            }];
            self.render(&off);
        }
        // Let the 50 ms release finish, so the next capture starts from silence.
        for _ in 0..24 {
            self.render(&[]);
        }
        captured
    }
}

fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()))
}

/// Whether the spectrum has a peak within [`HZ_TOLERANCE`] of `hz`.
fn has_partial(samples: &[f32], hz: f64) -> bool {
    mydaw_plugin_debug_tap::spectrum(samples, SR)
        .top_peaks(16)
        .iter()
        .any(|found| (found.frequency_hz - hz).abs() <= HZ_TOLERANCE)
}

/// The six loudest partials, for a failure message worth reading.
fn partials(samples: &[f32]) -> String {
    mydaw_plugin_debug_tap::spectrum(samples, SR)
        .top_peaks(6)
        .iter()
        .map(|peak| format!("{:.1} Hz", peak.frequency_hz))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The session the round-trip tests save: drums, a hosted synth, a test tone,
/// every mixer control moved off its default.
fn authored_project(bundle: &Path) -> Project {
    let mut project = Project::empty();
    project
        .add(Source::DrumKit, Some("Kit".to_owned()))
        .expect("room");
    project
        .add(
            Source::Clap(ClapSourceRef::new(bundle, SYNTH_ID).named("MYDAW Fixture Synth")),
            Some("Lead".to_owned()),
        )
        .expect("room");
    project
        .add(Source::TestTone, Some("Ref".to_owned()))
        .expect("room");
    project.tracks[0].gain_db = -4.5;
    project.tracks[0].pan = -0.5;
    project.tracks[2].muted = true;
    project.tracks[2].gain_db = -12.0;
    project.tracks[1].pan = 0.25;
    project.master_gain_db = -2.0;
    project
}

/// A beat with hits on both a kick row and a snare row.
fn authored_pattern() -> Pattern {
    let mut pattern = Pattern::with_channels(1, "Pattern 1", &[0, 1, 2, 3, 4], |channel| {
        mydaw_plugin_sequencer::DEFAULT_ROW_KEYS
            .get(channel as usize)
            .copied()
            .unwrap_or(mydaw_plugin_sequencer::DEFAULT_STEP_KEY)
    });
    pattern.channels[0].steps = Some(StepRow::empty(36).with_steps(&[0, 4, 8, 12]));
    pattern.channels[1].steps = Some(StepRow::empty(38).with_steps(&[4, 12]));
    pattern
}

// ---------------------------------------------------------------------------
// 1. The round trip
// ---------------------------------------------------------------------------

#[test]
fn a_whole_session_survives_a_save_and_a_load_into_a_fresh_engine() {
    let Some((_dir, bundle)) = staged("round-trip") else {
        return;
    };
    let files = TempDir::new("round-trip-files");
    let path = files.join("Session.mydaw");

    // --- author a session -------------------------------------------------
    let authored = authored_project(&bundle);
    let mut first = Rig::new(authored.clone());
    first.sequencer.set_bpm(147.0);
    first
        .sequencer
        .set_song(PatternSet::single(authored_pattern()), Playlist::default())
        .unwrap();
    first.tracks.set_selected(1).unwrap();
    first.tracks.set_soloed(1, true).unwrap();
    first.place(1, &bundle);
    first.load_state(1, &synth::state_blob(DETUNE_SEMITONES));
    assert_eq!(first.clap.instance_count(), 1);

    // --- save -------------------------------------------------------------
    let saved = first.capture();
    write_project_file(&path, &saved).expect("a fresh save");
    let on_disk = std::fs::read_to_string(&path).expect("the file is there");
    assert_eq!(saved.format_version, mydaw_app::FORMAT_VERSION);
    assert_eq!(saved.bpm, 147.0);
    assert_eq!(saved.tracks.len(), 3);
    assert_eq!(
        saved.track_state(1).unwrap(),
        synth::state_blob(DETUNE_SEMITONES),
        "the plugin's own bytes, not a summary of them"
    );
    assert!(
        saved.tracks[0].state_blob.is_empty() && saved.tracks[2].state_blob.is_empty(),
        "a built-in source has no state to save"
    );

    // --- load into a session that has never seen any of this --------------
    let read_back = read_project_file(&path).expect("what we just wrote");
    assert_eq!(read_back, saved);
    let mut second = Rig::new(Project::default());
    let report = second.load(&read_back);
    assert_eq!(report.missing_count(), 0, "{:?}", report.missing_tracks);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(second.clap.instance_count(), 1, "the plugin came back");

    // --- every field ------------------------------------------------------
    let restored = second.tracks.project();
    assert_eq!(restored.tracks.len(), 3);
    assert_eq!(
        restored.tracks.iter().map(|t| t.id).collect::<Vec<_>>(),
        vec![0, 1, 2],
        "ids are restored, not regenerated"
    );
    assert_eq!(
        restored
            .tracks
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Kit", "Lead", "Ref"]
    );
    assert_eq!(restored.tracks[0].source, Source::DrumKit);
    assert_eq!(restored.tracks[2].source, Source::TestTone);
    assert_eq!(
        restored.tracks[1].source.clap_ref().unwrap().plugin_id,
        SYNTH_ID
    );
    assert_eq!(
        restored.tracks[1].source.clap_ref().unwrap().bundle_path,
        bundle
    );
    assert_eq!(restored.tracks[0].gain_db, -4.5);
    assert_eq!(restored.tracks[0].pan, -0.5);
    assert_eq!(restored.tracks[1].pan, 0.25);
    assert_eq!(restored.tracks[2].gain_db, -12.0);
    assert!(restored.tracks[2].muted && !restored.tracks[0].muted);
    assert!(restored.tracks[1].soloed);
    assert_eq!(restored.master_gain_db, -2.0);
    assert_eq!(second.sequencer.bpm(), 147.0);
    assert_eq!(second.sequencer.active_pattern(), authored_pattern());
    assert_eq!(second.tracks.selected(), 1);

    // The solo is a *derived* value -- recomputed from the whole project, never
    // stored -- so its effect on the atomics is worth asserting separately.
    let params = second.tracks.params();
    assert!(params.track_muted(0), "silenced by track 1's solo");
    assert!(!params.track_muted(1));
    assert!(params.track_muted(2), "muted and not soloed");
    assert!((params.track_gain(0) - mydaw_rt_core::db_to_gain(-4.5)).abs() < 1e-6);
    assert_eq!(params.track_pan(1), 0.25);
    assert!((params.master_gain() - mydaw_rt_core::db_to_gain(-2.0)).abs() < 1e-6);

    // ...and the plugin's state came back with it.
    let instance = second
        .clap
        .instance_at(ClapTarget::Track { index: 1 })
        .expect("placed");
    assert_eq!(
        second.clap.instances_mut().save_state(instance).unwrap(),
        synth::state_blob(DETUNE_SEMITONES),
        "the reloaded plugin holds the state the file carried"
    );

    // --- re-save, byte for byte -------------------------------------------
    let resaved = second.capture();
    assert_eq!(resaved, read_back, "every field of the file");
    let second_path = files.join("Session-again.mydaw");
    write_project_file(&second_path, &resaved).unwrap();
    assert_eq!(
        std::fs::read_to_string(&second_path).unwrap(),
        on_disk,
        "save, load, save produces the same bytes -- the ordering is canonical"
    );
}

// ---------------------------------------------------------------------------
// 2. The state is a sound
// ---------------------------------------------------------------------------

#[test]
fn a_plugins_state_comes_back_through_the_file_as_the_pitch_it_encodes() {
    // The claim a byte-wise assertion cannot make. The fixture's state is a
    // detune in semitones, so a state that survived the file *moves the
    // sounding pitch*: a session saved with +12 plays C5 where a fresh one
    // plays C4, and nothing in the reloaded session was told to do that except
    // the file.
    let Some((_dir, bundle)) = staged("audible-state") else {
        return;
    };
    let files = TempDir::new("audible-state-files");
    let path = files.join("Detuned.mydaw");

    let mut project = Project::empty();
    project
        .add(
            Source::Clap(ClapSourceRef::new(&bundle, SYNTH_ID).named("MYDAW Fixture Synth")),
            Some("Lead".to_owned()),
        )
        .expect("room");

    // A session at the plugin's defaults sounds at the written pitch.
    let mut first = Rig::new(project);
    first.place(0, &bundle);
    let plain = first.capture_audio(&[(0, 60)]);
    assert!(
        has_partial(&plain, C4),
        "an undetuned fixture must sound C4; partials were {}",
        partials(&plain)
    );

    // Detune it, save, and hear the difference in the session that saved it.
    first.load_state(0, &synth::state_blob(DETUNE_SEMITONES));
    let detuned = first.capture_audio(&[(0, 60)]);
    assert!(
        has_partial(&detuned, C5) && !has_partial(&detuned, C4),
        "a +12 state must move C4 to C5; partials were {}",
        partials(&detuned)
    );
    write_project_file(&path, &first.capture()).expect("save");

    // A completely fresh engine, loading the file, must sound the same.
    let mut second = Rig::new(Project::default());
    let report = second.load(&read_project_file(&path).unwrap());
    assert_eq!(report.missing_count(), 0);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    let reloaded = second.capture_audio(&[(0, 60)]);
    assert!(
        has_partial(&reloaded, C5),
        "the state did not survive the file: expected C5, partials were {}",
        partials(&reloaded)
    );
    assert!(
        !has_partial(&reloaded, C4),
        "the plugin came back at its defaults; partials were {}",
        partials(&reloaded)
    );
    assert!(
        (peak(&reloaded) - peak(&detuned)).abs() < 0.02,
        "and at the same level: {} before, {} after",
        peak(&detuned),
        peak(&reloaded)
    );

    // The state the reloaded session would save is the state it was given.
    assert_eq!(
        second.capture().track_state(0).unwrap(),
        synth::state_blob(DETUNE_SEMITONES)
    );
}

// ---------------------------------------------------------------------------
// 3. A missing plugin
// ---------------------------------------------------------------------------

#[test]
fn a_project_whose_plugin_is_gone_still_opens_with_everything_else_intact() {
    // The policy that decides whether a user can open their own work on a
    // second machine. One absent plugin costs that track's sound and nothing
    // else -- not the beat, not the mixer, not the reference to the plugin.
    let Some((dir, shared)) = staged("missing") else {
        return;
    };
    // A second, identical copy of the fixture under a name **this process has
    // never opened**. Two Windows facts make that necessary: a mapped DLL cannot
    // be deleted, and the loader resolves an already-loaded module by its *base
    // name* -- so a bundle any test in this binary has opened would neither go
    // away nor fail to load from a path that no longer exists. Either would make
    // this test pass for entirely the wrong reason.
    let bundle = dir.join(&format!("mydaw-absent-{}.clap", std::process::id()));
    std::fs::copy(&shared, &bundle).expect("a copy of the fixture under our own name");

    let files = TempDir::new("missing-files");
    let path = files.join("NeedsAPlugin.mydaw");

    let mut project = Project::empty();
    project
        .add(Source::DrumKit, Some("Kit".to_owned()))
        .expect("room");
    project
        .add(
            Source::Clap(ClapSourceRef::new(&shared, SYNTH_ID).named("MYDAW Fixture Synth")),
            Some("Lead".to_owned()),
        )
        .expect("room");
    project.tracks[0].gain_db = -3.0;

    let mut first = Rig::new(project);
    first.sequencer.set_bpm(112.0);
    first
        .sequencer
        .set_song(PatternSet::single(authored_pattern()), Playlist::default())
        .unwrap();
    first.place(1, &shared);
    first.load_state(1, &synth::state_blob(-5.0));

    // A genuine save -- the state blob below is the bytes the plugin itself
    // wrote -- with the recorded bundle path pointed at the copy. This is
    // exactly the file a machine that *does* have the plugin produces, and the
    // only thing that will differ on the machine opening it is whether the
    // bundle is there.
    let mut file = first.capture();
    file.tracks[1].source = SourceEntry::Clap(mydaw_app::ClapEntry {
        bundle_path: bundle.display().to_string(),
        plugin_id: SYNTH_ID.to_owned(),
        name: "MYDAW Fixture Synth".to_owned(),
    });
    write_project_file(&path, &file).expect("save");
    assert_eq!(
        file.track_state(1).unwrap(),
        synth::state_blob(-5.0),
        "the blob is the plugin's own, not a placeholder"
    );
    first.clap.shutdown();
    drop(first);

    // The plugin leaves the machine.
    std::fs::remove_file(&bundle).expect("the fixture bundle goes away");
    assert!(!bundle.exists());

    let file = read_project_file(&path).expect("the project itself is fine");
    let mut second = Rig::new(Project::default());
    let report = second.load(&file);

    // The project opened.
    assert_eq!(second.tracks.track_count(), 2);
    assert_eq!(second.sequencer.bpm(), 112.0);
    assert_eq!(second.sequencer.active_pattern(), authored_pattern());
    assert_eq!(second.tracks.project().tracks[0].gain_db, -3.0);
    assert_eq!(second.tracks.project().tracks[0].source, Source::DrumKit);

    // The plugin did not, and says so, on the track that asked for it.
    assert_eq!(report.missing_count(), 1);
    assert!(second.missing.track(0).is_none(), "the kit is fine");
    let warning = second.missing.track(1).expect("track 1 asked for a plugin");
    assert_eq!(warning.name, "MYDAW Fixture Synth");
    assert_eq!(warning.format, "clap");
    assert_eq!(warning.path, bundle.display().to_string());
    assert!(!warning.reason.is_empty(), "the loader's own words");
    assert_eq!(second.clap.instance_count(), 0);

    // That track is silent -- not a fallback instrument, which would be a wrong
    // sound rather than an obvious absence.
    let silent = second.capture_audio(&[(1, 60)]);
    assert_eq!(
        peak(&silent),
        0.0,
        "a track whose plugin is missing must be silent"
    );
    // ...while the drum kit still plays.
    let kit = second.capture_audio(&[(0, 36)]);
    assert!(peak(&kit) > 0.01, "the rest of the project still sounds");

    // And the reference is still there, so saving does not throw the plugin
    // away and the project still opens on the machine that has it.
    let resaved = second.capture();
    assert_eq!(resaved.tracks[1].source, file.tracks[1].source);
    let SourceEntry::Clap(entry) = &resaved.tracks[1].source else {
        panic!("still a CLAP source");
    };
    assert_eq!(entry.plugin_id, SYNTH_ID);
    assert_eq!(entry.bundle_path, bundle.display().to_string());
    assert_eq!(
        resaved.tracks[1].state_blob, "",
        "there is no instance to ask, and an honest empty beats an invented blob"
    );

    // Put it back, reopen, and the plugin returns with its saved state -- which
    // is the whole point of having kept the reference.
    std::fs::copy(&shared, &bundle).expect("the fixture comes back");
    let mut third = Rig::new(Project::default());
    let report = third.load(&file);
    assert_eq!(report.missing_count(), 0);
    let instance = third
        .clap
        .instance_at(ClapTarget::Track { index: 1 })
        .expect("placed again");
    assert_eq!(
        third.clap.instances_mut().save_state(instance).unwrap(),
        synth::state_blob(-5.0),
        "and with the state the file has been carrying all along"
    );
    third.clap.shutdown();
}

// ---------------------------------------------------------------------------
// 4. Loading over a live session
// ---------------------------------------------------------------------------

#[test]
fn loading_over_a_live_session_replaces_every_plugin_and_leaves_nothing_behind() {
    // The failure this guards against is the expensive one: a load that leaves
    // the previous session's instances hosted, activated and unreachable.
    let Some((_dir, bundle)) = staged("teardown") else {
        return;
    };
    let files = TempDir::new("teardown-files");

    let mut project = Project::empty();
    for index in 0..2 {
        project
            .add(
                Source::Clap(ClapSourceRef::new(&bundle, SYNTH_ID).named("MYDAW Fixture Synth")),
                Some(format!("Synth {index}")),
            )
            .expect("room");
    }
    let mut rig = Rig::new(project);
    rig.place(0, &bundle);
    rig.place(1, &bundle);
    rig.load_state(0, &synth::state_blob(DETUNE_SEMITONES));
    assert_eq!(rig.clap.instance_count(), 2);

    // A saved copy of *this* session, to come back to.
    let both = rig.capture();
    let path = files.join("Two.mydaw");
    write_project_file(&path, &both).unwrap();

    // Load a one-track project with no plugins at all over the top of it.
    let mut plain = Project::empty();
    plain
        .add(Source::PolySynth, Some("Only".to_owned()))
        .expect("room");
    let report = rig.load(&ProjectFile::capture(
        &plain,
        0,
        130.0,
        &mydaw_app::Song {
            patterns: &PatternSet::single(authored_pattern()),
            playlist: &Playlist::default(),
            mode: PlayMode::Pattern,
            audio_sources: Vec::new(),
        },
        &[],
        None,
    ));
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(
        rig.clap.instance_count(),
        0,
        "both instances must be destroyed, not merely forgotten"
    );
    assert_eq!(rig.tracks.track_count(), 1);
    assert_eq!(rig.tracks.project().tracks[0].source, Source::PolySynth);

    // The built-in instrument the load installed actually sounds, which is what
    // proves the graph was rebuilt rather than left pointing at dead slots.
    let played = rig.capture_audio(&[(0, 69)]);
    assert!(peak(&played) > 0.01, "the new session plays");

    // ...and going back gets both plugins and the detune again.
    let report = rig.load(&both);
    assert_eq!(report.missing_count(), 0);
    assert_eq!(rig.clap.instance_count(), 2);
    let chord = rig.capture_audio(&[(0, 60), (1, 69)]);
    assert!(
        has_partial(&chord, C5),
        "track 0 is detuned an octave up; partials were {}",
        partials(&chord)
    );
    assert!(
        has_partial(&chord, A4),
        "track 1 is not; partials were {}",
        partials(&chord)
    );
    assert!(
        (peak(&chord) - VOICE_GAIN * 2.0).abs() < 0.05,
        "two voices at unity, centre-panned: expected ~{}, got {}",
        VOICE_GAIN * 2.0,
        peak(&chord)
    );

    // Teardown leaks nothing: every node comes home and the collector empties.
    rig.tracks.close();
    rig.clap.shutdown();
    assert_eq!(rig.clap.instance_count(), 0);
    drop(rig.graph);
    drop(rig.handle);
    rig.collector.collect();
    rig.collector.collect();
    assert_eq!(
        rig.collector.alloc_count(),
        0,
        "the collector is holding nothing after a project session ends"
    );
}
