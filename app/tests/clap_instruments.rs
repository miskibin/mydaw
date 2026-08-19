//! Hosted CLAP instruments as track sources, through the shell's own graph.
//!
//! No device and no webview: [`TrackHost::build_spec`] produces the graph, it is
//! installed through a real `graph_channel` and driven by this thread instead of
//! by cpal, and the plugins are placed by the real [`ClapHost`] -- the same code
//! the run loop calls when the webview sends `set_track_source_clap`. What is
//! substituted is the *device* and the *collector thread*, both of which this
//! file plays by hand so that the timing is deterministic rather than lucky.
//!
//! The four claims step 6 has to make about instruments on tracks:
//!
//! 1. **Two hosted synths on two tracks sound at once**, each playing only the
//!    notes addressed to it -- which is what "a plugin per track" means and what
//!    step 5's single-instance model could not do.
//! 2. **A plugin's state changes what it sounds like, and survives a round
//!    trip** through the host's save/load.
//! 3. **A structural edit no longer destroys the arrangement.** Adding a track
//!    rebuilds the graph, and every hosted plugin comes back into it -- with its
//!    state, because the instance is kept rather than recreated.
//! 4. **Teardown under load leaks nothing.** A destroyed instance's node comes
//!    home from the collector, and the collector ends up holding nothing.
//!
//! The plugin is the workspace's own synth fixture, whose state is a detune in
//! semitones -- so "the state took effect" is a measurable *pitch*, not a
//! promise.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use mydaw_app::clap_bridge::ClapTarget;
use mydaw_app::clap_host::ClapHost;
use mydaw_app::{ClapSourceRef, Project, Source, TrackHost};
use mydaw_fixture_clap_gain::synth::{self, PLUGIN_ID as SYNTH_ID, VOICE_GAIN};
use mydaw_fixture_clap_gain::{GAIN, PLUGIN_ID as GAIN_ID};
use mydaw_plugin_host_clap::test_util::{missing_fixture_message, stage_fixture_bundle};
use mydaw_plugin_instruments::InstrumentRegistry;
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

fn limits() -> GraphLimits {
    GraphLimits {
        sample_rate: SR,
        max_block: BLOCK,
        max_events: 64,
    }
}

/// Scratch directory that deletes itself.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mydaw-clap-instruments-{tag}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("scratch dir");
        Self(path)
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

/// The shell's world, minus the device: a mixer, a CLAP host, a graph, and the
/// "audio thread" and "collector thread" this file plays by hand.
struct Rig {
    tracks: Arc<TrackHost>,
    clap: ClapHost,
    graph: RtGraphSlot,
    buffers: AudioBuffers,
    collector: Collector,
    /// Kept alive so the graph channel is not abandoned mid-test.
    handle: GraphHandle,
}

impl Rig {
    fn new(project: Project) -> Self {
        let tracks = Arc::new(TrackHost::with_project(
            Arc::new(TestToneFactory::new()),
            Arc::new(InstrumentRegistry::new()),
            Arc::new(mydaw_plugin_sampler::SamplerRegistry::detached()),
            project,
        ));
        let collector = Collector::new();
        let (mut handle, mut graph) = graph_channel(4, collector.handle());
        handle
            .install(tracks.build_spec(&limits(), collector.handle()))
            .expect("the shell's own graph must install");
        assert!(graph.poll_swap(), "the RT side must pick the graph up");

        Self {
            tracks,
            clap: ClapHost::new(Arc::new(PluginWindowService::new())),
            graph,
            buffers: AudioBuffers::new(BLOCK),
            collector,
            handle,
        }
    }

    /// Place a hosted instrument on `track`, through the real command path.
    fn place(&mut self, track: usize, bundle: &Path, plugin_id: &str) -> Result<(), String> {
        self.clap
            .place_track_source(&self.tracks, track, bundle, plugin_id, SR, BLOCK)
    }

    /// Rebuild and reinstall the graph, then re-attach every hosted plugin --
    /// the whole of what `add_track` / `remove_track` do with plugins loaded.
    ///
    /// The two `collect()` calls are this file standing in for the `basedrop`
    /// collector thread. In the application a GC thread sweeps every 250 ms and
    /// [`ClapHost::reattach_all`] simply waits for it; here nothing sweeps
    /// unless the test says so, and a re-activation before the old node is freed
    /// is exactly the failure this ordering exists to avoid.
    fn rebuild(&mut self) -> Result<(), String> {
        self.handle
            .install(self.tracks.build_spec(&limits(), self.collector.handle()))
            .expect("rebuild installs");
        // The audio thread swaps the graph and drops the old one...
        self.render(&[]);
        // ...and the collector runs its destructors, handing every processor
        // back to its instance.
        self.collector.collect();
        self.clap.reattach_all(&self.tracks, SR, BLOCK)
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

    /// Hold `notes` and capture the settled output, returning the left channel.
    fn capture(&mut self, notes: &[(u8, u8)]) -> Vec<f32> {
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

/// Whether the spectrum has a peak within `tolerance` Hz of `hz`.
fn has_partial(samples: &[f32], hz: f64, tolerance: f64) -> bool {
    mydaw_plugin_debug_tap::spectrum(samples, SR)
        .top_peaks(16)
        .iter()
        .any(|found| (found.frequency_hz - hz).abs() <= tolerance)
}

/// A project of `count` tracks, each headed by the hosted synth at `bundle`.
///
/// The source is recorded before any instance exists, which is exactly what a
/// project file will produce: the track renders silence until the main thread
/// attaches a plugin to it.
fn synth_project(count: usize, bundle: &Path) -> Project {
    let mut project = Project::empty();
    for index in 0..count {
        project
            .add(
                Source::Clap(ClapSourceRef::new(bundle, SYNTH_ID).named("MYDAW Fixture Synth")),
                Some(format!("Synth {index}")),
            )
            .expect("room");
    }
    project
}

#[test]
fn two_hosted_synths_on_two_tracks_each_play_their_own_notes() {
    // Step 5 could host exactly one CLAP instrument, on track 0, named by an
    // environment variable. This is the claim that replaces it.
    let Some((_dir, bundle)) = staged("two-tracks") else {
        return;
    };
    let mut rig = Rig::new(synth_project(2, &bundle));

    // Before either plugin is attached, the tracks are silent -- not a fallback
    // tone, which is the difference between a gap and a wrong sound.
    let silent = rig.capture(&[(0, 69), (1, 81)]);
    assert_eq!(
        peak(&silent),
        0.0,
        "an unattached CLAP track must be silent"
    );

    rig.place(0, &bundle, SYNTH_ID).expect("track 0");
    rig.place(1, &bundle, SYNTH_ID).expect("track 1");
    assert_eq!(rig.clap.instance_count(), 2, "two instances, one graph");

    // A4 on track 0, A5 on track 1: the channel *is* the track, so each plugin
    // may only hear its own.
    let both = rig.capture(&[(0, 69), (1, 81)]);
    assert!(has_partial(&both, 440.0, 8.0), "track 0's A4 is missing");
    assert!(has_partial(&both, 880.0, 8.0), "track 1's A5 is missing");
    assert!(
        (peak(&both) - VOICE_GAIN * 2.0).abs() < 0.05,
        "two voices at unity, centre-panned: expected ~{}, got {}",
        VOICE_GAIN * 2.0,
        peak(&both)
    );

    // One note at a time proves the routing rather than the summing: a note on
    // channel 0 must not reach track 1's plugin.
    let only_track_zero = rig.capture(&[(0, 69)]);
    assert!(has_partial(&only_track_zero, 440.0, 8.0));
    assert!(
        !has_partial(&only_track_zero, 880.0, 8.0),
        "a note addressed to track 0 reached track 1's plugin"
    );
    assert!(
        (peak(&only_track_zero) - VOICE_GAIN).abs() < 0.03,
        "one voice, got {}",
        peak(&only_track_zero)
    );

    // Muting one track leaves the other exactly where it was.
    rig.tracks.set_muted(1, true).expect("track 1 exists");
    let muted = rig.capture(&[(0, 69), (1, 81)]);
    assert!(has_partial(&muted, 440.0, 8.0));
    assert!(
        !has_partial(&muted, 880.0, 8.0),
        "a muted track's plugin still reached the master bus"
    );

    rig.clap.shutdown();
}

#[test]
fn a_state_blob_changes_what_a_placed_plugin_sounds_like_and_restores_it() {
    // The audible half of the state round trip, measured through the whole
    // graph rather than against a bare node.
    let Some((_dir, bundle)) = staged("state") else {
        return;
    };
    let mut rig = Rig::new(synth_project(1, &bundle));
    rig.place(0, &bundle, SYNTH_ID).expect("track 0");
    let target = ClapTarget::Track { index: 0 };

    let plain = rig.capture(&[(0, 69)]);
    assert!(has_partial(&plain, 440.0, 8.0), "the default state is A4");

    // A +12 semitone detune, through the command path a project load will use.
    let instance = rig.clap.instance_at(target).expect("placed");
    let saved_default = rig
        .clap
        .instances_mut()
        .save_state(instance)
        .expect("the synth has state");
    rig.clap
        .instances_mut()
        .load_state(instance, &synth::state_blob(12.0))
        .expect("loads");

    let detuned = rig.capture(&[(0, 69)]);
    assert!(
        has_partial(&detuned, 880.0, 12.0),
        "a +12 semitone state must move A4 to A5"
    );
    assert!(
        !has_partial(&detuned, 440.0, 6.0),
        "and it must move it, not add to it"
    );

    // Byte-wise: what the host saves is what it was given.
    let saved = rig
        .clap
        .instances_mut()
        .save_state(instance)
        .expect("saves");
    assert_eq!(saved, synth::state_blob(12.0));

    // ...and restoring the default restores the pitch, with the plugin still
    // activated and still in the running graph.
    rig.clap
        .instances_mut()
        .load_state(instance, &saved_default)
        .expect("restores");
    let restored = rig.capture(&[(0, 69)]);
    assert!(has_partial(&restored, 440.0, 8.0), "back to A4");

    rig.clap.shutdown();
}

#[test]
fn adding_a_track_rebuilds_the_graph_and_every_hosted_plugin_comes_back_with_its_state() {
    // The step-5 limitation, deleted and measured. `TrackError::ClapInGraph`
    // used to refuse this outright.
    let Some((_dir, bundle)) = staged("rebuild") else {
        return;
    };
    let mut rig = Rig::new(synth_project(2, &bundle));
    rig.place(0, &bundle, SYNTH_ID).expect("track 0");
    rig.place(1, &bundle, SYNTH_ID).expect("track 1");

    // Give the two instances *different* states, so a rebuild that mixed them up
    // -- or reset them -- would be audible rather than merely wrong.
    for (index, semitones) in [(0_usize, 12.0_f32), (1, -12.0)] {
        let instance = rig
            .clap
            .instance_at(ClapTarget::Track { index })
            .expect("placed");
        rig.clap
            .instances_mut()
            .load_state(instance, &synth::state_blob(semitones))
            .expect("loads");
    }
    let before = rig.capture(&[(0, 69), (1, 69)]);
    assert!(has_partial(&before, 880.0, 12.0), "track 0 is an octave up");
    assert!(
        has_partial(&before, 220.0, 6.0),
        "track 1 is an octave down"
    );
    let instances_before = rig.clap.instance_count();

    // The layout edit, and the re-attach that follows it.
    {
        let mut project = rig.tracks.project();
        project
            .add(Source::TestTone, Some("Added".to_owned()))
            .expect("room");
        rig.tracks.set_project(project);
    }
    rig.tracks.set_muted(2, true).expect("the new track exists");
    rig.rebuild().expect("every plugin must be re-attached");

    assert_eq!(
        rig.clap.instance_count(),
        instances_before,
        "the instances are re-activated, not recreated -- see the ClapHost docs"
    );
    assert_eq!(rig.tracks.track_count(), 3);

    let after = rig.capture(&[(0, 69), (1, 69)]);
    assert!(
        has_partial(&after, 880.0, 12.0),
        "track 0's plugin lost its state or its node across the rebuild"
    );
    assert!(
        has_partial(&after, 220.0, 6.0),
        "track 1's plugin lost its state or its node across the rebuild"
    );
    assert!(
        (peak(&after) - peak(&before)).abs() < 0.03,
        "the level must be what it was: {} -> {}",
        peak(&before),
        peak(&after)
    );

    // And the states really are still the ones that were set, byte for byte.
    for (index, semitones) in [(0_usize, 12.0_f32), (1, -12.0)] {
        let instance = rig
            .clap
            .instance_at(ClapTarget::Track { index })
            .expect("still placed");
        assert_eq!(
            rig.clap.instances_mut().save_state(instance).unwrap(),
            synth::state_blob(semitones),
            "track {index}'s state changed across the rebuild"
        );
    }

    rig.clap.shutdown();
}

#[test]
fn a_hosted_effect_on_the_master_bus_processes_the_sum_and_can_be_removed() {
    // The master insert slot: inserted, audible, replaced by nothing, all with
    // no graph rebuild at all.
    let Some((_dir, bundle)) = staged("master") else {
        return;
    };
    let mut rig = Rig::new(synth_project(1, &bundle));
    rig.place(0, &bundle, SYNTH_ID).expect("track 0");

    let dry = peak(&rig.capture(&[(0, 69)]));
    assert!((dry - VOICE_GAIN).abs() < 0.03, "one voice, got {dry}");

    rig.clap
        .place_master_effect(&rig.tracks, &bundle, GAIN_ID, SR, BLOCK)
        .expect("the gain fixture is a stereo effect");
    assert!(rig.tracks.master_insert().is_occupied());
    let wet = peak(&rig.capture(&[(0, 69)]));
    assert!(
        (wet - dry * GAIN).abs() < 0.01,
        "the master effect's {GAIN}x must be applied: {dry} -> {wet}"
    );

    // Removing it is a slot swap, not a rebuild.
    rig.tracks.master_insert().clear().expect("clears");
    let removed = peak(&rig.capture(&[(0, 69)]));
    assert!(
        (removed - dry).abs() < 0.03,
        "clearing the insert must restore the dry sum: {removed}"
    );

    rig.clap.shutdown();
}

#[test]
fn a_plugin_whose_ports_do_not_fit_the_slot_is_refused_and_not_left_hosted() {
    // The descriptor's features are what the picker filters on; the port layout
    // is what the placement is checked against. A plugin that reached the wrong
    // slot anyway (a lying descriptor, a stale UI) must be refused *and* not
    // leave an instance nobody can see or destroy.
    let Some((_dir, bundle)) = staged("wrong-slot") else {
        return;
    };
    let mut rig = Rig::new(synth_project(1, &bundle));

    let err = rig
        .place(0, &bundle, GAIN_ID)
        .expect_err("an effect cannot head a track");
    assert!(err.contains("not an instrument"), "{err}");
    assert_eq!(rig.clap.instance_count(), 0, "nothing may be left hosted");

    let err = rig
        .clap
        .place_master_effect(&rig.tracks, &bundle, SYNTH_ID, SR, BLOCK)
        .expect_err("an instrument cannot process the master bus");
    assert!(err.contains("not a stereo audio effect"), "{err}");
    assert_eq!(rig.clap.instance_count(), 0);
    assert!(!rig.tracks.master_insert().is_occupied());

    // A bundle that is not a bundle, and an id the bundle does not export.
    assert!(rig.place(0, Path::new("no-such.clap"), SYNTH_ID).is_err());
    assert!(rig.place(0, &bundle, "com.acme.not-here").is_err());
    assert_eq!(rig.clap.instance_count(), 0);
}

#[test]
fn replacing_and_destroying_placed_plugins_under_load_leaks_nothing() {
    // `alloc_count` counts live `basedrop::Owned` allocations. Zero at the end
    // means every superseded source node, every retired graph and every plugin
    // node was freed -- by the collector, never by the audio thread.
    let Some((_dir, bundle)) = staged("teardown") else {
        return;
    };
    let mut rig = Rig::new(synth_project(1, &bundle));

    for round in 0..3 {
        rig.place(0, &bundle, SYNTH_ID)
            .unwrap_or_else(|err| panic!("round {round}: {err}"));
        assert_eq!(
            rig.clap.instance_count(),
            1,
            "round {round}: replacing a track's plugin must destroy the old one"
        );
        // Play through it, then let the collector run: replacing the source
        // pushed the old node onto the collector's queue.
        let sound = rig.capture(&[(0, 69)]);
        assert!(has_partial(&sound, 440.0, 8.0), "round {round}");
        rig.collector.collect();
    }

    // Switching the track back to a built-in must destroy the plugin it
    // displaces -- an instance nobody points at is a plugin nobody can free.
    rig.clap
        .replace_with_built_in(&rig.tracks, 0, "poly-synth", SR, BLOCK)
        .expect("the built-in swap must succeed");
    assert_eq!(
        rig.clap.instance_count(),
        0,
        "the displaced instance must be destroyed, not orphaned"
    );
    assert!(rig
        .clap
        .instance_at(ClapTarget::Track { index: 0 })
        .is_none());
    rig.collector.collect();

    // A master effect too, then everything down.
    rig.clap
        .place_master_effect(&rig.tracks, &bundle, GAIN_ID, SR, BLOCK)
        .expect("placed");
    rig.capture(&[(0, 69)]);
    rig.tracks.master_insert().clear().expect("clears");
    rig.render(&[]);
    rig.collector.collect();

    rig.clap.shutdown();
    assert_eq!(rig.clap.instance_count(), 0);

    // Retire the graph itself and sweep: nothing may be left.
    rig.tracks.close();
    drop(rig.graph);
    drop(rig.handle);
    rig.collector.collect();
    assert_eq!(
        rig.collector.alloc_count(),
        0,
        "something was stranded on the collector"
    );
}
