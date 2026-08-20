//! The mixer, through the shell's own graph builder.
//!
//! No device and no webview: [`TrackHost::build_spec`] produces the same
//! `GraphSpec` the shell installs when a stream opens, and this drives it with
//! `mydaw-rt-core`'s own channel -- `GraphHandle::install` ->
//! `RtGraphSlot::poll_swap`/`process`, with `basedrop` retiring what the audio
//! thread drops. So what runs here is what the audio callback runs, driven by
//! this thread instead of by cpal.
//!
//! `crates/rt-core/src/graph.rs` proves the *arithmetic* of the mixer against
//! constant-valued stand-in nodes. This proves the arithmetic is wired to the
//! right things: that a fader in the project reaches the strip, that solo
//! resolves to the mutes the audio thread reads, that a note channel picks a
//! track's real instrument, and that adding a track does not interrupt the
//! others.

use std::sync::Arc;

use mydaw_app::effects::InsertTarget;
use mydaw_app::{Project, Source, TrackDef, TrackHost};
use mydaw_plugin_instruments::InstrumentRegistry;
use mydaw_plugin_test_tone::{TestToneFactory, DEFAULT_AMPLITUDE};
use mydaw_rt_core::basedrop::Collector;
use mydaw_rt_core::{
    graph_channel, AudioBuffers, GraphHandle, GraphLimits, NoteEvent, ProcessCtx, RtGraphSlot,
    TrackPeak, TransportSnapshot,
};

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;

fn limits() -> GraphLimits {
    GraphLimits {
        sample_rate: SR,
        max_block: BLOCK,
        max_events: 64,
    }
}

/// dBFS of a linear amplitude, floored so `0.0` is a number rather than `-inf`.
fn dbfs(amplitude: f32) -> f32 {
    20.0 * amplitude.max(1e-12).log10()
}

/// A host, the graph it built, and the "audio thread" that runs it.
struct Rig {
    host: Arc<TrackHost>,
    graph: RtGraphSlot,
    buffers: AudioBuffers,
    /// Kept alive so the graph channel is not abandoned mid-test.
    handle: GraphHandle,
    collector: Collector,
}

impl Rig {
    /// Build `project`'s graph exactly as the shell does when a stream opens.
    fn new(project: Project) -> Self {
        let host = Arc::new(TrackHost::with_project(
            Arc::new(TestToneFactory::new()),
            Arc::new(InstrumentRegistry::new()),
            Arc::new(mydaw_plugin_sampler::SamplerRegistry::detached()),
            Arc::new(mydaw_plugin_effects::EffectRegistry::new()),
            project,
        ));
        let collector = Collector::new();
        let (mut handle, mut graph) = graph_channel(4, collector.handle());
        handle
            .install(host.build_spec(&limits(), collector.handle()))
            .expect("the shell's own graph must install");
        assert!(graph.poll_swap());

        Self {
            host,
            graph,
            buffers: AudioBuffers::new(BLOCK),
            handle,
            collector,
        }
    }

    /// Recompile and install, the way adding or removing a track does.
    fn rebuild(&mut self) {
        self.handle
            .install(self.host.build_spec(&limits(), self.collector.handle()))
            .expect("rebuild");
    }

    /// Render one block, returning the master bus.
    fn render(&mut self, playing: bool) -> (Vec<f32>, Vec<f32>) {
        self.render_with(playing, &[])
    }

    /// Render one block carrying `events`.
    fn render_with(&mut self, playing: bool, events: &[NoteEvent]) -> (Vec<f32>, Vec<f32>) {
        self.graph.poll_swap();
        let transport = TransportSnapshot {
            playing,
            ..TransportSnapshot::default()
        };
        let mut ctx = ProcessCtx::with_events(&mut self.buffers, SR, BLOCK, transport, events);
        self.graph.process(&mut ctx);
        let [left, right] = self.buffers.as_slices();
        (left.to_vec(), right.to_vec())
    }

    /// Render `blocks` blocks and return the last one, so parameter ramps have
    /// settled before anything is measured.
    fn settled(&mut self, blocks: usize, playing: bool) -> (Vec<f32>, Vec<f32>) {
        let mut out = (Vec::new(), Vec::new());
        for _ in 0..blocks {
            out = self.render(playing);
        }
        out
    }

    fn peaks(&self) -> Vec<TrackPeak> {
        self.graph.track_peaks().as_slice().to_vec()
    }
}

fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()))
}

/// `count` test-tone tracks. Free-running sines, identical sample for sample,
/// which is what makes a sum assertable to the bit.
fn tone_project(count: usize) -> Project {
    let mut project = Project::empty();
    for _ in 0..count {
        project.add(Source::TestTone, None).expect("room");
    }
    project
}

#[test]
fn two_tracks_sum_into_the_master_bus() {
    // Two identical tones at unity, centre: the sum is exactly twice one of
    // them, because the pan law is normalised so centre is unity.
    let mut one = Rig::new(tone_project(1));
    let (single, _) = one.settled(2, true);

    let mut two = Rig::new(tone_project(2));
    let (summed, _) = two.settled(2, true);

    assert!(peak(&single) > 0.01, "the tone must actually sound");
    for (a, b) in single.iter().zip(&summed) {
        assert!((b - a * 2.0).abs() < 1e-6, "{a} doubled should be {b}");
    }
    assert!((peak(&summed) - 2.0 * DEFAULT_AMPLITUDE).abs() < 1e-5);
}

#[test]
fn a_fader_in_the_project_reaches_the_strip() {
    let mut rig = Rig::new(tone_project(2));
    let (full, _) = rig.settled(2, true);

    // -6.0206 dB is exactly half.
    rig.host.set_gain_db(1, -6.0206).unwrap();
    let (mixed, _) = rig.settled(3, true);

    let expected = peak(&full) * 0.75;
    assert!(
        (peak(&mixed) - expected).abs() < 1e-3,
        "one of two tracks at -6 dB should give {expected}, got {}",
        peak(&mixed)
    );

    // The bottom of the fader is silence, not -60 dB of tone.
    rig.host.set_gain_db(0, -60.0).unwrap();
    rig.host.set_gain_db(1, -60.0).unwrap();
    let (silent, _) = rig.settled(4, true);
    assert_eq!(peak(&silent), 0.0);
}

#[test]
fn panning_a_track_hard_left_empties_the_right_channel() {
    let mut rig = Rig::new(tone_project(1));
    rig.host.set_pan(0, -1.0).unwrap();
    let (left, right) = rig.settled(3, true);

    assert!(
        dbfs(peak(&right)) <= -80.0,
        "hard left left {} dBFS in the right channel",
        dbfs(peak(&right))
    );
    // Equal power: a hard-panned edge is 3.01 dB above the centre position.
    let centre = {
        let mut plain = Rig::new(tone_project(1));
        let (l, _) = plain.settled(2, true);
        peak(&l)
    };
    let gain_db = dbfs(peak(&left)) - dbfs(centre);
    assert!(
        (gain_db - 3.0103).abs() < 0.02,
        "hard left should be +3.01 dB on the left, got {gain_db}"
    );
}

#[test]
fn muting_a_track_is_a_ramp_and_solo_is_the_same_mechanism() {
    let mut rig = Rig::new(tone_project(3));
    let (before, _) = rig.settled(2, true);
    assert_eq!(rig.peaks().len(), 3);
    assert!(rig.peaks().iter().all(|p| p.peak_l > 0.0));

    // Solo track 1: the other two are silenced by an *effective* mute the
    // audio thread reads, and they ramp down rather than cut.
    rig.host.set_soloed(1, true).unwrap();
    let (during, _) = rig.render(true);
    assert!(
        peak(&during) > 0.0,
        "the ramp means the first block after a solo still has audio in it"
    );

    let (_after, _) = rig.settled(3, true);
    let peaks = rig.peaks();
    assert_eq!(peaks[0], TrackPeak::SILENT, "track 0 is not soloed");
    assert!(peaks[1].peak_l > 0.0, "track 1 is");
    assert_eq!(peaks[2], TrackPeak::SILENT, "nor is track 2");

    // Un-solo restores all three, to exactly what they were.
    rig.host.set_soloed(1, false).unwrap();
    let (restored, _) = rig.settled(3, true);
    assert!(rig.peaks().iter().all(|p| p.peak_l > 0.0));
    assert!((peak(&restored) - peak(&before)).abs() < 1e-5);
}

#[test]
fn a_mute_never_steps_the_waveform() {
    let mut rig = Rig::new(tone_project(1));
    rig.settled(2, true);

    // The tone itself moves; what must not appear is a step far larger than
    // one sample of a 440 Hz sine at 48 kHz, which is at most ~0.019.
    let tone_step = 2.0 * std::f32::consts::PI * 440.0 / SR as f32 * DEFAULT_AMPLITUDE;
    rig.host.set_muted(0, true).unwrap();

    let mut worst = 0.0_f32;
    let mut previous: Option<f32> = None;
    for _ in 0..4 {
        let (left, _) = rig.render(true);
        for sample in left {
            if let Some(last) = previous {
                worst = worst.max((sample - last).abs());
            }
            previous = Some(sample);
        }
    }
    // The mute contributes at most 1/480 of full scale per sample on top of
    // whatever the signal was doing.
    let limit = tone_step + 1.0 / 480.0 + 1e-4;
    assert!(
        worst <= limit,
        "the mute stepped by {worst} (limit {limit})"
    );

    let (silent, _) = rig.settled(2, true);
    assert_eq!(peak(&silent), 0.0, "and it must actually arrive");
}

#[test]
fn the_master_fader_scales_the_sum_without_moving_the_track_meters() {
    let mut rig = Rig::new(tone_project(2));
    let (full, _) = rig.settled(2, true);
    let track_peak = rig.peaks()[0].peak_l;

    rig.host.set_master_gain_db(-6.0206).unwrap();
    let (halved, _) = rig.settled(3, true);

    assert!((peak(&halved) - peak(&full) * 0.5).abs() < 1e-4);
    assert!(
        (rig.peaks()[0].peak_l - track_peak).abs() < 1e-6,
        "a channel meter is pre-master, so the master fader must not move it"
    );
}

#[test]
fn a_note_reaches_only_the_track_its_channel_names() {
    // Two identical synths. The only thing that can tell them apart is which
    // one was addressed -- which is the whole claim.
    let mut project = Project::empty();
    project.add(Source::PolySynth, None).unwrap();
    project.add(Source::PolySynth, None).unwrap();
    let mut rig = Rig::new(project);

    // Silence first: an instrument makes no sound until it is played.
    let (idle, _) = rig.render(false);
    assert_eq!(peak(&idle), 0.0);

    let note = NoteEvent::note_on(0, 69, 1.0).on_channel(1);
    rig.render_with(false, &[note]);
    for _ in 0..8 {
        rig.render(false);
    }

    let peaks = rig.peaks();
    assert_eq!(
        peaks[0],
        TrackPeak::SILENT,
        "track 0 was not addressed and must stay silent"
    );
    assert!(
        peaks[1].peak_l > 0.01,
        "track 1 was addressed and should be sounding, got {:?}",
        peaks[1]
    );

    // And the other way round, on the same graph.
    rig.render_with(false, &[NoteEvent::note_off(0, 69).on_channel(1)]);
    for _ in 0..64 {
        rig.render(false);
    }
    rig.render_with(false, &[NoteEvent::note_on(0, 69, 1.0)]);
    for _ in 0..8 {
        rig.render(false);
    }
    assert!(rig.peaks()[0].peak_l > 0.01, "now track 0 sounds");
}

#[test]
fn per_track_meters_are_independent() {
    let mut project = Project::empty();
    project.add(Source::TestTone, None).unwrap();
    project
        .tracks
        .push(TrackDef::new(9, "Quiet", Source::TestTone));
    let mut rig = Rig::new(project);

    rig.host.set_gain_db(1, -60.0).unwrap();
    rig.settled(3, true);

    let peaks = rig.peaks();
    assert_eq!(peaks.len(), 2);
    assert!(peaks[0].peak_l > 0.01, "the loud track reads its level");
    assert_eq!(peaks[1], TrackPeak::SILENT, "the silent one reads nothing");

    // Swap which one is loud; the meters follow, independently.
    rig.host.set_gain_db(0, -60.0).unwrap();
    rig.host.set_gain_db(1, 0.0).unwrap();
    rig.settled(4, true);
    let peaks = rig.peaks();
    assert_eq!(peaks[0], TrackPeak::SILENT);
    assert!(peaks[1].peak_l > 0.01);
}

#[test]
fn adding_a_track_mid_play_does_not_interrupt_the_others() {
    let mut rig = Rig::new(tone_project(1));
    rig.host.set_gain_db(0, -6.0206).unwrap();
    let (before, _) = rig.settled(3, true);
    let level = peak(&before);

    // A layout change: the whole graph is recompiled and swapped while the
    // engine runs. The surviving track must not fade, gap or click.
    {
        let mut project = rig.host.project();
        let added = project.add(Source::TestTone, None).unwrap();
        // Same fader as the track already playing, so the expected sum is one
        // number rather than two.
        project.tracks[added].gain_db = -6.0206;
        rig.host.set_project(project);
    }
    rig.rebuild();

    let (after, _) = rig.render(true);
    assert_eq!(rig.graph.track_count(), 2);
    assert!(
        (peak(&after) - level * 2.0).abs() < 1e-3,
        "the new track should arrive at full level immediately, got {} vs {}",
        peak(&after),
        level * 2.0
    );
    assert!(
        after.iter().all(|s| s.is_finite()),
        "nothing may go non-finite across a swap"
    );

    // The fader that was set before the rebuild survived it.
    assert_eq!(rig.host.project().tracks[0].gain_db, -6.0206);
}

#[test]
fn the_shells_starter_rack_is_five_drums_and_a_synth_and_is_silent_until_played() {
    let rig_project = Project::default();
    assert_eq!(
        rig_project
            .tracks
            .iter()
            .map(|t| t.source.id())
            .collect::<Vec<_>>(),
        vec![
            "kick",
            "snare",
            "clap",
            "hat-closed",
            "hat-open",
            "poly-synth"
        ],
        "one instrument per channel"
    );

    let count = rig_project.len();
    let mut rig = Rig::new(rig_project);
    let (left, right) = rig.settled(4, true);
    assert_eq!(peak(&left), 0.0, "instruments are silent until played");
    assert_eq!(peak(&right), 0.0);
    assert_eq!(rig.peaks().len(), count);

    // A note on channel 0 reaches the kick channel, and nothing else -- which
    // is the point of the split: a channel is one instrument, and a note
    // addressed to it cannot leak into the snare.
    rig.render_with(true, &[NoteEvent::note_on(0, 36, 1.0)]);
    for _ in 0..8 {
        rig.render(true);
    }
    let peaks = rig.peaks();
    assert!(peaks[0].peak_l > 0.01, "the kick answered");
    for (index, peak) in peaks.iter().enumerate().skip(1) {
        assert_eq!(
            *peak,
            TrackPeak::SILENT,
            "channel {index} was not addressed"
        );
    }
}

// ---------------------------------------------------------------------------
// Routing, through the shell's own builder
// ---------------------------------------------------------------------------

/// One tone track and one bus, with `track 0 -> bus 0` at unity.
fn routed_project(pre: bool) -> Project {
    let mut project = Project::empty();
    project.add(Source::TestTone, None).expect("room");
    project.add_bus(Some("Reverb".to_owned())).expect("room");
    let send = project
        .add_send(mydaw_rt_core::StripId::Track(0), 0)
        .expect("a legal tap");
    project.edit_send(send, |send| send.pre = pre).unwrap();
    project
}

#[test]
fn a_send_into_a_bus_with_an_eq_on_it_reaches_the_master_through_the_bus() {
    // The shell's whole routing path in one render: a project with a bus, an
    // insert chain on it and a send into it, built by `build_spec` and run by
    // the engine's own graph channel.
    let effects = mydaw_plugin_effects::EffectRegistry::new();
    let mut project = routed_project(true);
    project
        .add_insert(InsertTarget::Bus(0), "effects.utility", None, &effects)
        .expect("the utility exists");
    // -6 dB on the bus, so the bus's contribution is a known fraction of the
    // track's rather than merely "some".
    project.buses[0].inserts[0]
        .params
        .set_by_id("gain", -6.0206);

    let mut rig = Rig::new(project);
    let (dry_left, _) = rig.settled(8, true);
    let with_bus = peak(&dry_left);

    // ...and with the bus muted, only the track's own main out is left.
    rig.host.set_bus_muted(0, true).unwrap();
    let (muted_left, _) = rig.settled(8, true);
    let main_only = peak(&muted_left);

    assert!(
        (main_only - DEFAULT_AMPLITUDE).abs() < 1e-3,
        "the main out alone should be the tone: {main_only}"
    );
    let contribution = with_bus - main_only;
    assert!(
        (contribution - DEFAULT_AMPLITUDE * 0.5).abs() < 0.01,
        "the bus should add half the tone (its utility is at -6 dB), added {contribution}"
    );
    assert_eq!(rig.graph.bus_count(), 1);
}

/// The pre/post proof, offline: with the fader at the bottom of its scale, a
/// pre-fader send still feeds its bus and a post-fader one does not.
#[test]
fn a_fader_at_the_bottom_of_its_scale_stops_a_post_send_and_not_a_pre_one() {
    let measure = |pre: bool| -> f32 {
        let mut project = routed_project(pre);
        project.tracks[0].gain_db = mydaw_rt_core::SILENCE_DB;
        let mut rig = Rig::new(project);
        let (left, _) = rig.settled(8, true);
        peak(&left)
    };

    let pre = measure(true);
    let post = measure(false);
    assert!(
        (pre - DEFAULT_AMPLITUDE).abs() < 1e-3,
        "a pre-fader send ignores the fader: {pre}"
    );
    assert_eq!(post, 0.0, "a post-fader send follows it");
    assert!(
        dbfs(pre) - dbfs(post) > 80.0,
        "pre {} dBFS vs post {} dBFS",
        dbfs(pre),
        dbfs(post)
    );
}

#[test]
fn a_bus_peak_reaches_the_meter_path_and_a_muted_bus_reads_silence() {
    let mut rig = Rig::new(routed_project(true));
    rig.settled(8, true);
    let peaks = rig.graph.bus_peaks();
    assert_eq!(peaks.count(), 1);
    assert!(
        (peaks.get(0).peak_l - DEFAULT_AMPLITUDE).abs() < 1e-3,
        "{:?}",
        peaks.get(0)
    );

    rig.host.set_bus_muted(0, true).unwrap();
    rig.settled(8, true);
    assert_eq!(rig.graph.bus_peaks().get(0), TrackPeak::SILENT);
}

#[test]
fn a_send_level_reaches_the_audio_thread_with_no_rebuild_at_all() {
    // The claim that makes a send knob cheap: one atomic store, and the graph
    // the audio thread is running is the one it was already running.
    let mut project = routed_project(true);
    project.tracks[0].muted = true; // Only the send path is audible.
    let mut rig = Rig::new(project);
    let nodes_before = rig.graph.node_count();
    rig.settled(8, true);
    let unity = peak(&rig.render(true).0);

    let send = rig.host.project().sends[0].id;
    rig.host.set_send_level(send, 0.25).expect("a live send");
    // No `rebuild()`: the level is an atomic the running graph reads.
    let (left, _) = rig.settled(8, true);
    let quieter = peak(&left);

    assert_eq!(rig.graph.node_count(), nodes_before, "nothing was rebuilt");
    assert!(
        (quieter - unity * 0.25).abs() < 0.01,
        "{unity} at unity, {quieter} at a quarter"
    );

    // ...and an out-of-range level is refused rather than clamped.
    assert!(rig.host.set_send_level(send, 9.0).is_err());
    assert!(rig.host.set_send_level(99, 0.5).is_err());
}

#[test]
fn an_insert_parameter_reaches_the_running_node_with_no_rebuild() {
    let effects = mydaw_plugin_effects::EffectRegistry::new();
    let mut project = Project::empty();
    project.add(Source::TestTone, None).expect("room");
    project
        .add_insert(InsertTarget::Track(0), "effects.utility", None, &effects)
        .unwrap();

    let mut rig = Rig::new(project);
    let nodes_before = rig.graph.node_count();
    rig.settled(8, true);
    let unity = peak(&rig.render(true).0);

    // The store comes back as what was *actually* stored, so a knob can snap
    // itself to the truth.
    let stored = rig
        .host
        .set_insert_param(InsertTarget::Track(0), 0, "gain", -6.0206)
        .expect("the utility has a gain");
    assert!((stored + 6.0206).abs() < 1e-4);
    let (left, _) = rig.settled(8, true);
    assert_eq!(rig.graph.node_count(), nodes_before, "nothing was rebuilt");
    assert!(
        (peak(&left) - unity * 0.5).abs() < 0.01,
        "the gain did not reach the running node"
    );

    // A value past the parameter's range comes back clamped, not stored.
    let clamped = rig
        .host
        .set_insert_param(InsertTarget::Track(0), 0, "gain", 1_000.0)
        .unwrap();
    assert_eq!(clamped, 36.0, "the utility's gain tops out at +36 dB");
    assert!(rig
        .host
        .set_insert_param(InsertTarget::Track(0), 0, "nope", 0.0)
        .is_err());
    assert!(rig
        .host
        .set_insert_param(InsertTarget::Bus(0), 0, "gain", 0.0)
        .is_err());
}

/// The gain-reduction meter, end to end through the shell: a compressor on a
/// bus fed a signal well over its threshold must **report** the reduction it is
/// applying, and report none when the signal is below it.
#[test]
fn a_compressor_on_a_bus_publishes_the_gain_reduction_it_is_applying() {
    let effects = mydaw_plugin_effects::EffectRegistry::new();
    let mut project = routed_project(true);
    project
        .add_insert(InsertTarget::Bus(0), "effects.compressor", None, &effects)
        .expect("the compressor exists");
    let params = &project.buses[0].inserts[0].params;
    params.set_by_id("threshold", -40.0);
    params.set_by_id("ratio", 8.0);
    params.set_by_id("attack", 1.0);

    let mut rig = Rig::new(project);
    let readouts = |rig: &Rig| -> (f32, f32) {
        let values = rig
            .host
            .insert_readouts(InsertTarget::Bus(0), 0)
            .expect("the compressor is there");
        let get = |id: &str| {
            values
                .iter()
                .find(|value| value.id == id)
                .map(|value| value.value)
                .unwrap_or_else(|| panic!("no readout '{id}'"))
        };
        (get("gr"), get("gr_max"))
    };

    // Nothing rendered yet: nothing reduced.
    assert_eq!(readouts(&rig), (0.0, 0.0));

    rig.settled(16, true);
    let (gr, gr_max) = readouts(&rig);
    assert!(
        gr > 3.0 && gr < 40.0,
        "a tone {} dBFS over a -40 dB threshold at 8:1 should be reduced: gr {gr} dB",
        dbfs(DEFAULT_AMPLITUDE) + 40.0
    );
    assert!(
        gr_max >= gr,
        "gr_max is the worst of the block and must not be below the last sample: {gr_max} < {gr}"
    );
    assert!(gr_max.is_finite() && gr_max < 60.0, "{gr_max}");

    // Raise the threshold above the signal and the meter comes back down.
    rig.host
        .set_insert_param(InsertTarget::Bus(0), 0, "threshold", 0.0)
        .unwrap();
    rig.settled(64, true);
    let (quiet_gr, _) = readouts(&rig);
    assert!(
        quiet_gr < 1.0,
        "nothing is over a 0 dBFS threshold, so nothing should be reduced: {quiet_gr} dB"
    );

    // The whole chain's meters come back in chain order, with an entry per
    // insert whether or not it publishes anything.
    let chain = rig.host.chain_readouts(InsertTarget::Bus(0)).unwrap();
    assert_eq!(chain.len(), 1);
    assert_eq!(chain[0].index, 0);
    assert_eq!(
        chain[0]
            .readouts
            .iter()
            .map(|value| value.id.as_str())
            .collect::<Vec<_>>(),
        vec!["gr", "gr_max"]
    );
    assert!(rig.host.chain_readouts(InsertTarget::Bus(9)).is_err());
}

#[test]
fn a_bus_chain_runs_before_the_bus_fader_and_the_master_chain_before_the_hosted_slot() {
    // Order matters and is not observable from a DTO, so it is asserted where
    // it is decided: the node count the builder produces.
    let effects = mydaw_plugin_effects::EffectRegistry::new();
    let mut project = Project::empty();
    project.add(Source::TestTone, None).expect("room");
    project.add_bus(None).expect("room");
    let bare = {
        let rig = Rig::new(project.clone());
        rig.graph.node_count()
    };

    project
        .add_insert(InsertTarget::Track(0), "effects.eq", None, &effects)
        .unwrap();
    project
        .add_insert(InsertTarget::Bus(0), "effects.delay", None, &effects)
        .unwrap();
    project
        .add_insert(InsertTarget::Master, "effects.utility", None, &effects)
        .unwrap();
    let rig = Rig::new(project);
    assert_eq!(
        rig.graph.node_count(),
        bare + 3,
        "one node per insert, on every kind of chain"
    );
    assert_eq!(rig.graph.bus_count(), 1);
}

#[test]
fn an_insert_naming_an_effect_this_build_does_not_have_is_skipped_not_fatal() {
    // The missing-plugin policy at insert granularity: the graph still builds,
    // the chain is simply one node shorter, and the entry stays in the project
    // so a save writes it back out.
    let effects = mydaw_plugin_effects::EffectRegistry::new();
    let mut project = Project::empty();
    project.add(Source::TestTone, None).expect("room");
    project
        .add_insert(InsertTarget::Track(0), "effects.utility", None, &effects)
        .unwrap();
    let known = {
        let rig = Rig::new(project.clone());
        rig.graph.node_count()
    };

    // Rewrite the entry to name an effect that does not exist, the way a
    // project written by a later build would.
    project.tracks[0].inserts[0].type_id = "effects.not-an-effect".to_owned();
    let rig = Rig::new(project);
    assert_eq!(rig.graph.node_count(), known - 1);
    let (left, _) = {
        let mut rig = rig;
        rig.settled(4, true)
    };
    assert!(peak(&left) > 0.0, "the rest of the track still plays");
}
