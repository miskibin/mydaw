//! Many instances, their state, and their teardown -- against real plugins.
//!
//! Step 6's host-side claims, each measured rather than asserted by eye:
//!
//! 1. **Two instances of the same plugin coexist** and are independently
//!    activatable. That is what "the same synth on two tracks" means, and it is
//!    the thing spike 2's one-plugin-per-`LoadedClapPlugin` model could not do.
//! 2. **`clap.state` round-trips**, byte for byte, through the host's own
//!    `save_state`/`load_state` -- and a plugin with no `state` extension
//!    answers with an empty blob rather than an error.
//! 3. **Port introspection is the authority** on what a plugin may be used for,
//!    and it agrees with the descriptor's `features` that the picker filters on.
//! 4. **Teardown frees everything.** A destroyed instance's node comes back from
//!    the `basedrop` collector and the collector ends up holding nothing --
//!    which is the leak this whole ownership design exists to prevent.
//!
//! The plugins are the workspace's own fixtures (`test-fixtures/clap-gain`),
//! loaded from disk through the OS loader. If they have not been built, each
//! test prints a skip message and returns; the unit test
//! `test_util::the_workspace_fixture_is_findable_from_the_test_binary` is what
//! makes a permanently-skipping suite impossible to miss.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use mydaw_fixture_clap_gain::synth::{
    self, PLUGIN_ID as SYNTH_ID, STATE_MAGIC, VOICES, VOICE_GAIN,
};
use mydaw_fixture_clap_gain::PLUGIN_ID as GAIN_ID;
use mydaw_plugin_host_clap::test_util::{missing_fixture_message, stage_fixture_bundle};
use mydaw_plugin_host_clap::{
    ClapInstanceError, ClapInstanceHost, ClapPluginKind, ClapScanError, ClapStateError,
};
use mydaw_plugin_windows::PluginWindowService;
use mydaw_rt_core::basedrop::Collector;
use mydaw_rt_core::{
    AudioBuffers, NoteEvent, NoteKind, ProcessCtx, RtNode, TransportSnapshot, CHANNELS,
};

const SAMPLE_RATE: f64 = 48_000.0;
const BLOCK: usize = 256;
/// Enough blocks for the synth's 2 ms attack to finish and settle.
const BLOCKS: usize = 24;

/// Scratch directory that deletes itself.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mydaw-clap-instances-{tag}-{}-{unique}",
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

/// Stage the fixture bundle, or `None` (having said why) if it was not built.
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

fn host() -> ClapInstanceHost {
    ClapInstanceHost::new(Arc::new(PluginWindowService::new()))
}

/// Render `BLOCKS` blocks through `node`, sending `key` as a note-on in the
/// first block, and return the peak absolute sample on the left channel.
///
/// The whole block is measured after the attack has finished, so the peak is the
/// voice's steady-state amplitude and can be predicted exactly.
fn render_note(node: &mut dyn RtNode, key: u8, channel: u8) -> f32 {
    let mut buffers = AudioBuffers::new(BLOCK);
    let mut peak = 0.0_f32;
    for block in 0..BLOCKS {
        let events: Vec<NoteEvent> = if block == 0 {
            vec![NoteEvent {
                frame_offset: 0,
                channel,
                kind: NoteKind::NoteOn { key, velocity: 1.0 },
            }]
        } else {
            Vec::new()
        };
        buffers.silence();
        {
            let mut ctx = ProcessCtx::with_events(
                &mut buffers,
                SAMPLE_RATE,
                BLOCK,
                TransportSnapshot::default(),
                &events,
            );
            node.process(&mut ctx);
        }
        // Only the tail blocks count: the first few are the attack ramp.
        if block >= BLOCKS / 2 {
            for sample in buffers.as_slices()[0] {
                assert!(sample.is_finite(), "a non-finite sample left the plugin");
                peak = peak.max(sample.abs());
            }
        }
    }
    peak
}

/// The dominant partial of `BLOCKS` blocks of `key`, in Hz.
fn render_pitch(node: &mut dyn RtNode, key: u8, channel: u8) -> f64 {
    let mut buffers = AudioBuffers::new(BLOCK);
    let mut captured: Vec<f32> = Vec::with_capacity(BLOCK * BLOCKS);
    for block in 0..BLOCKS {
        let events: Vec<NoteEvent> = if block == 0 {
            vec![NoteEvent {
                frame_offset: 0,
                channel,
                kind: NoteKind::NoteOn { key, velocity: 1.0 },
            }]
        } else {
            Vec::new()
        };
        buffers.silence();
        {
            let mut ctx = ProcessCtx::with_events(
                &mut buffers,
                SAMPLE_RATE,
                BLOCK,
                TransportSnapshot::default(),
                &events,
            );
            node.process(&mut ctx);
        }
        if block >= BLOCKS / 4 {
            captured.extend_from_slice(buffers.as_slices()[0]);
        }
    }
    mydaw_plugin_debug_tap::spectrum(&captured, SAMPLE_RATE)
        .dominant()
        .expect("a held note has a dominant partial")
        .frequency_hz
}

#[test]
fn two_instances_of_the_same_plugin_are_hosted_and_activated_independently() {
    // The claim the whole step rests on: "the same synth on two tracks" is two
    // instances with two sets of voices, not one shared plugin.
    let Some((_dir, bundle)) = staged("two-instances") else {
        return;
    };
    let mut host = host();

    let first = host.create(&bundle, SYNTH_ID).expect("instance one");
    let second = host.create(&bundle, SYNTH_ID).expect("instance two");
    assert_ne!(first, second);
    assert_eq!(host.len(), 2);
    assert_eq!(host.ids(), vec![first, second], "creation order is kept");
    assert_eq!(
        host.info(first).map(|info| info.id.as_str()),
        Some(SYNTH_ID)
    );

    // Both activate *at the same time*. Spike 2's model could not: one
    // `LoadedClapPlugin` refuses a second `create_node` while one is outstanding.
    let mut node_one = host
        .create_node(first, SAMPLE_RATE, BLOCK)
        .expect("instance one activates");
    let mut node_two = host
        .create_node(second, SAMPLE_RATE, BLOCK)
        .expect("instance two activates while the first is live");

    // Distinct notes, on distinct channels, and each instance sounds its own.
    let pitch_one = render_pitch(&mut node_one, 69, 0);
    let pitch_two = render_pitch(&mut node_two, 81, 1);
    assert!(
        (pitch_one - 440.0).abs() < 3.0,
        "instance one should sound A4, got {pitch_one:.1} Hz"
    );
    assert!(
        (pitch_two - 880.0).abs() < 6.0,
        "instance two should sound A5, got {pitch_two:.1} Hz"
    );

    // A third, activated while the other two are still live and still holding
    // notes. Its voices are its own: one note at full velocity is exactly one
    // voice's amplitude, not the two the first instance is now sounding.
    let third = host.create(&bundle, SYNTH_ID).expect("instance three");
    let mut node_three = host
        .create_node(third, SAMPLE_RATE, BLOCK)
        .expect("a third activation while two are live");
    assert_eq!(host.len(), 3);
    let peak = render_note(&mut node_three, 60, 0);
    assert!(
        (peak - VOICE_GAIN).abs() < 0.02,
        "a fresh instance sounds one voice ({VOICE_GAIN}), got {peak}"
    );

    drop(node_one);
    drop(node_two);
    drop(node_three);
    host.shutdown();
    assert_eq!(host.len(), 0);
}

#[test]
fn a_state_blob_round_trips_byte_for_byte_and_changes_what_the_plugin_sounds_like() {
    // Both halves matter. Byte equality proves the host's stream plumbing is
    // faithful; the pitch proves the blob is a *state* and not a decoration the
    // plugin ignores.
    let Some((_dir, bundle)) = staged("state") else {
        return;
    };
    let mut host = host();
    let instance = host.create(&bundle, SYNTH_ID).expect("the synth loads");

    // A fresh instance is at its default, and saves it.
    let default = host.save_state(instance).expect("the synth has state");
    assert_eq!(default.len(), 8, "the fixture's blob is magic + one f32");
    assert_eq!(&default[..4], &STATE_MAGIC, "{default:?}");
    assert_eq!(synth::parse_state(&default), Some(0.0));

    // An octave up, restored through the host's own load path.
    let octave = synth::state_blob(12.0);
    host.load_state(instance, &octave).expect("the blob loads");
    let saved = host.save_state(instance).expect("save after load");
    assert_eq!(saved, octave, "what came back must be what went in");

    // ...and it is audible: key 69 now sounds A5 rather than A4.
    let mut node = host
        .create_node(instance, SAMPLE_RATE, BLOCK)
        .expect("activates");
    let detuned = render_pitch(&mut node, 69, 0);
    assert!(
        (detuned - 880.0).abs() < 6.0,
        "a +12 semitone state must double the pitch, got {detuned:.1} Hz"
    );

    // Back to the default, on a plugin that is *already activated and playing* --
    // the state extension is main-thread work and must not need a deactivation.
    host.load_state(instance, &default).expect("restores");
    let restored = render_pitch(&mut node, 69, 0);
    assert!(
        (restored - 440.0).abs() < 3.0,
        "restoring the default state must restore the pitch, got {restored:.1} Hz"
    );

    drop(node);
    host.shutdown();
}

#[test]
fn state_survives_being_moved_into_a_brand_new_instance() {
    // The project-load path: save, destroy, create afresh, load. Step 6's graph
    // rebuild deliberately does *not* do this (it keeps the instance), but step
    // 8's project loader will, so it is proven here rather than assumed.
    let Some((_dir, bundle)) = staged("state-move") else {
        return;
    };
    let mut host = host();

    let original = host.create(&bundle, SYNTH_ID).expect("loads");
    host.load_state(original, &synth::state_blob(-12.0))
        .expect("loads");
    let saved = host.save_state(original).expect("saves");
    assert!(host.destroy(original));
    assert_eq!(host.len(), 0);

    let replacement = host.create(&bundle, SYNTH_ID).expect("loads again");
    assert_ne!(original, replacement, "a destroyed id is never reused");
    assert_eq!(
        host.save_state(replacement).unwrap(),
        synth::state_blob(0.0),
        "a fresh instance starts at its default, not at the old one's state"
    );
    host.load_state(replacement, &saved).expect("restores");

    let mut node = host
        .create_node(replacement, SAMPLE_RATE, BLOCK)
        .expect("activates");
    let pitch = render_pitch(&mut node, 69, 0);
    assert!(
        (pitch - 220.0).abs() < 3.0,
        "the restored -12 semitone state must halve the pitch, got {pitch:.1} Hz"
    );

    drop(node);
    host.shutdown();
}

#[test]
fn a_plugin_with_no_state_extension_saves_nothing_and_loading_is_a_no_op() {
    // The gain fixture deliberately implements no `clap.state`. Per the CLAP
    // specification that means it has no state to persist, which is an answer
    // and not a failure -- so a project file records an empty blob for it and a
    // reload hands that back without complaint.
    let Some((_dir, bundle)) = staged("no-state") else {
        return;
    };
    let mut host = host();
    let instance = host
        .create(&bundle, GAIN_ID)
        .expect("the gain fixture loads");

    assert!(
        host.save_state(instance).expect("not an error").is_empty(),
        "no state extension means no state"
    );
    host.load_state(instance, &[])
        .expect("an empty blob is a no-op");
    host.load_state(instance, b"whatever a project file happened to hold")
        .expect("a blob for a plugin with no state extension is ignored, not refused");
    assert!(host.save_state(instance).unwrap().is_empty());

    host.shutdown();
}

#[test]
fn a_state_blob_larger_than_the_hosts_limit_is_refused_before_it_reaches_the_plugin() {
    // Reachable from a corrupted or hostile project file.
    let Some((_dir, bundle)) = staged("state-huge") else {
        return;
    };
    let mut host = host();
    let instance = host.create(&bundle, SYNTH_ID).expect("loads");

    let huge = vec![0_u8; mydaw_plugin_host_clap::MAX_STATE_BYTES + 1];
    let err = host.load_state(instance, &huge).unwrap_err();
    assert!(
        matches!(
            err,
            ClapInstanceError::State(ClapStateError::BlobTooLarge { .. })
        ),
        "got {err:?}"
    );

    // A malformed blob of a sane size is the plugin's own refusal, reported.
    let err = host.load_state(instance, b"NOPE1234").unwrap_err();
    assert!(
        matches!(err, ClapInstanceError::State(ClapStateError::Load { .. })),
        "got {err:?}"
    );
    assert_eq!(
        host.save_state(instance).unwrap(),
        synth::state_blob(0.0),
        "a refused load must leave the plugin as it was"
    );

    host.shutdown();
}

#[test]
fn what_a_plugin_may_be_used_for_is_read_from_its_ports_and_matches_its_features() {
    // Two questions with two answers, deliberately. The picker filters on the
    // descriptor's `features`, which is free but is only the plugin's claim; the
    // port layout is what it actually declared to the host, and it is what a
    // placement is checked against.
    let Some((_dir, bundle)) = staged("ports") else {
        return;
    };

    // The free answer: the scan, with no instance created at all.
    let scanned = mydaw_plugin_host_clap::ClapHostService::scan_bundle(&bundle)
        .expect("the fixture bundle scans");
    let synth_info = scanned
        .iter()
        .find(|info| info.id == SYNTH_ID)
        .expect("the bundle exports the synth");
    let gain_info = scanned
        .iter()
        .find(|info| info.id == GAIN_ID)
        .expect("the bundle exports the gain");
    assert_eq!(synth_info.kind(), ClapPluginKind::Instrument);
    assert_eq!(gain_info.kind(), ClapPluginKind::AudioEffect);
    assert!(synth_info.is_instrument() && !synth_info.is_audio_effect());
    assert!(gain_info.is_audio_effect() && !gain_info.is_instrument());

    // The authoritative answer: the ports, which need an instance.
    let mut host = host();
    let synth = host.create(&bundle, SYNTH_ID).unwrap();
    let gain = host.create(&bundle, GAIN_ID).unwrap();

    let synth_layout = host.port_layout(synth).expect("hosted");
    assert!(synth_layout.is_instrument(), "{synth_layout:?}");
    assert!(!synth_layout.is_stereo_effect());
    assert!(!synth_layout.has_audio_input(), "an instrument gets no bus");
    assert!(synth_layout.has_note_input(), "and it must take notes");

    let gain_layout = host.port_layout(gain).expect("hosted");
    assert!(gain_layout.is_stereo_effect(), "{gain_layout:?}");
    assert!(!gain_layout.is_instrument());
    assert!(gain_layout.has_audio_input());
    assert!(!gain_layout.has_note_input());

    // The two answers agree, which is the property a picker's filter depends on.
    assert_eq!(synth_info.is_instrument(), synth_layout.is_instrument());
    assert_eq!(gain_info.is_audio_effect(), gain_layout.is_stereo_effect());

    host.shutdown();
}

#[test]
fn a_bundle_that_is_not_a_bundle_is_a_typed_scan_error() {
    // Guards the negative half of the picker's filter: a broken file must not
    // become a plugin with no features and therefore an "other" entry.
    let dir = TempDir::new("broken");
    let bogus = dir.path().join("broken.clap");
    std::fs::write(&bogus, b"not a shared library").unwrap();
    assert!(matches!(
        mydaw_plugin_host_clap::ClapHostService::scan_bundle(&bogus),
        Err(ClapScanError::Load(_) | ClapScanError::NoPluginFactory | ClapScanError::Panicked)
    ));
}

#[test]
fn destroying_an_instance_under_load_retires_its_node_and_leaks_nothing() {
    // The ownership design's whole reason for existing: a `ClapNode` is dropped
    // by the `basedrop` collector, which is not the main thread, so it must hand
    // its processor back rather than deactivate. `alloc_count` is the leak
    // detector -- it counts live `Owned` allocations, and zero at the end means
    // nothing was stranded.
    let Some((_dir, bundle)) = staged("destroy") else {
        return;
    };
    let mut collector = Collector::new();
    let mut host = host();

    let instance = host.create(&bundle, SYNTH_ID).expect("loads");
    let node = host
        .create_node(instance, SAMPLE_RATE, BLOCK)
        .expect("activates");

    // The node crosses the RT wall exactly as the graph sends it: wrapped in
    // `Owned`, dropped on the audio thread, freed by the collector.
    let mut owned =
        mydaw_rt_core::basedrop::Owned::new(&collector.handle(), Box::new(node) as Box<dyn RtNode>);
    assert_eq!(collector.alloc_count(), 1);

    let mut buffers = AudioBuffers::new(BLOCK);
    for _ in 0..8 {
        buffers.silence();
        let mut ctx = ProcessCtx::new(
            &mut buffers,
            SAMPLE_RATE,
            BLOCK,
            TransportSnapshot::default(),
        );
        owned.process(&mut ctx);
    }

    // Dropping on the "audio thread" must not deallocate.
    drop(owned);
    assert_eq!(
        collector.alloc_count(),
        1,
        "the audio thread must never free a node"
    );
    collector.collect();
    assert_eq!(collector.alloc_count(), 0, "the collector freed the node");

    // Only now can the instance be destroyed, and it must be clean.
    assert!(host.destroy(instance));
    assert_eq!(host.len(), 0);
    assert!(!host.contains(instance));

    // And the whole cycle again, to prove nothing accumulated.
    let again = host.create(&bundle, SYNTH_ID).expect("loads again");
    let node = host
        .create_node(again, SAMPLE_RATE, BLOCK)
        .expect("activates again");
    drop(node);
    assert!(host.destroy(again));
    assert_eq!(collector.alloc_count(), 0);
}

#[test]
fn an_instance_is_activated_again_after_its_node_has_been_retired() {
    // The rebuild protocol in miniature, and the reason
    // `ClapInstanceHost::create_node` waits: a plugin can only be activated
    // once, and the previous node is freed on the collector's schedule.
    let Some((_dir, bundle)) = staged("reactivate") else {
        return;
    };
    let mut host = host();
    let instance = host.create(&bundle, SYNTH_ID).expect("loads");

    // A state the re-activation has to preserve, since that is exactly what a
    // graph rebuild relies on instead of a save/load round trip.
    host.load_state(instance, &synth::state_blob(12.0))
        .expect("loads");

    for round in 0..3 {
        let mut node = host
            .create_node(instance, SAMPLE_RATE, BLOCK)
            .unwrap_or_else(|err| panic!("round {round}: {err}"));
        let pitch = render_pitch(&mut node, 69, 0);
        assert!(
            (pitch - 880.0).abs() < 6.0,
            "round {round}: state must survive deactivate/activate, got {pitch:.1} Hz"
        );
        drop(node);
    }

    assert_eq!(
        host.save_state(instance).unwrap(),
        synth::state_blob(12.0),
        "and it is still the state the host last set"
    );
    host.shutdown();
}

#[test]
fn a_plugin_asked_to_go_where_its_ports_do_not_fit_is_still_hosted_and_still_reportable() {
    // The host does not itself refuse a placement -- `ClapHost` does, using this
    // -- but the layout has to be readable before anything is activated, which is
    // what makes that refusal possible at all.
    let Some((_dir, bundle)) = staged("layout") else {
        return;
    };
    let mut host = host();
    let gain = host.create(&bundle, GAIN_ID).unwrap();
    let layout = host.port_layout(gain).unwrap();
    assert!(!layout.is_instrument());
    assert_eq!(layout.main_output_channels, CHANNELS as u32);
    // Reading the layout must not have activated anything.
    assert!(host.create_node(gain, SAMPLE_RATE, BLOCK).is_ok());
    host.shutdown();
}

#[test]
fn the_synth_fixtures_own_limits_are_what_the_tests_assume() {
    // A guard on the constants the assertions above are written against: if the
    // fixture's voice count or amplitude changed, the peaks would drift and the
    // failures would look like host bugs.
    assert_eq!(VOICES, 8);
    assert!((VOICE_GAIN - 0.2).abs() < 1e-6);
    assert_eq!(synth::state_blob(0.0).len(), 8);
}
