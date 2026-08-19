//! Changing instrument on a running engine, through the real chain.
//!
//! No device and no webview: the graph is `mydaw-rt-core`'s own -- `GraphSpec`
//! -> `GraphHandle::install` -> `RtGraphSlot::poll_swap`/`process`, with
//! `basedrop` retiring what the audio thread drops -- and the chain is built by
//! [`source_chain`](mydaw_app::source_chain), the same function the shell calls
//! when it opens a device. So what runs here is what the audio callback runs,
//! driven by this thread instead of by cpal.
//!
//! Three claims, all of them about the decision to swap at *node* granularity
//! rather than by rebuilding the graph (see `app/src/source.rs`):
//!
//! 1. an insert survives a source change untouched -- the reason the design is
//!    shaped this way at all;
//! 2. the transport gate follows the source, arming for the free-running test
//!    tone and disarming for an instrument, with no second graph install;
//! 3. the swapped-in instrument really plays the notes the block carries.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use mydaw_app::{source_chain, Source, SourceHandle};
use mydaw_plugin_instruments::drum_kit::KICK_KEY;
use mydaw_plugin_instruments::{DrumKitNode, PolySynthNode};
use mydaw_plugin_test_tone::{TestToneFactory, DEFAULT_AMPLITUDE};
use mydaw_rt_core::basedrop::Collector;
use mydaw_rt_core::{
    graph_channel, AudioBuffers, GraphHandle, NoteEvent, ProcessCtx, RtGraphSlot, RtNode,
    RtNodeFactory, TransportSnapshot,
};

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

/// Stand-in for a CLAP insert. Halving is exact in binary floating point, so a
/// test can assert the effect ran by comparing against the dry signal rather
/// than against a tolerance.
struct HalveNode {
    blocks: Arc<AtomicBool>,
}

impl RtNode for HalveNode {
    fn process(&mut self, ctx: &mut ProcessCtx) {
        self.blocks.store(true, Ordering::Release);
        for channel in ctx.buffers.as_mut_slices() {
            for sample in channel.iter_mut() {
                *sample *= 0.5;
            }
        }
    }
}

/// The rig: a handle, the chain it heads, and the "audio thread" that runs it.
struct Rig {
    collector: Collector,
    handle: SourceHandle,
    gate: Arc<AtomicBool>,
    graph: RtGraphSlot,
    buffers: AudioBuffers,
    ran_effect: Arc<AtomicBool>,
    // Kept alive so the graph channel is not abandoned mid-test.
    _graph_handle: GraphHandle,
}

impl Rig {
    /// Build `source -> halve -> gate` and install it, exactly as the shell
    /// does when a device opens.
    fn new(source: Source, node: Box<dyn RtNode>) -> Self {
        let collector = Collector::new();
        let gate = Arc::new(AtomicBool::new(source.gated()));
        let handle = SourceHandle::new(source.clone());
        let slot = handle.open(node, source, collector.handle(), Arc::clone(&gate));

        let ran_effect = Arc::new(AtomicBool::new(false));
        let effect = Box::new(HalveNode {
            blocks: Arc::clone(&ran_effect),
        });
        let spec = source_chain(slot, Some(effect), Arc::clone(&gate));
        assert_eq!(spec.len(), 3, "slot, insert, gate");

        let (mut graph_handle, mut graph) = graph_channel(4, collector.handle());
        graph_handle.install(spec).expect("install");
        assert!(graph.poll_swap());

        Self {
            collector,
            handle,
            gate,
            graph,
            buffers: AudioBuffers::new(BLOCK),
            ran_effect,
            _graph_handle: graph_handle,
        }
    }

    /// Run one block and return the left channel.
    fn block(&mut self, playing: bool, events: &[NoteEvent]) -> Vec<f32> {
        let mut ctx = ProcessCtx::with_events(
            &mut self.buffers,
            SR,
            BLOCK,
            TransportSnapshot {
                sample_pos: 0,
                playing,
                ..TransportSnapshot::default()
            },
            events,
        );
        self.graph.process(&mut ctx);
        self.buffers.as_slices()[0].to_vec()
    }

    /// Run `count` blocks with no events, returning the last one.
    fn blocks(&mut self, count: usize, playing: bool) -> Vec<f32> {
        let mut last = Vec::new();
        for _ in 0..count {
            last = self.block(playing, &[]);
        }
        last
    }
}

fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()))
}

#[test]
fn an_insert_survives_a_source_change_and_keeps_processing() {
    // This is the whole reason the swap happens at node granularity: rebuilding
    // the graph would mean rebuilding the effect node, and a hosted CLAP plugin
    // cannot be activated twice at once.
    let mut rig = Rig::new(Source::TestTone, TestToneFactory::new().create(SR, BLOCK));

    let tone = rig.block(true, &[]);
    assert!(
        (peak(&tone) - DEFAULT_AMPLITUDE / 2.0).abs() < 1e-3,
        "the insert should have halved the tone, peak {}",
        peak(&tone)
    );
    assert!(rig.ran_effect.load(Ordering::Acquire));

    // Swap to the drum kit and hit it. The same `HalveNode` instance is still
    // in the chain -- it was never rebuilt.
    rig.handle
        .install(Box::new(DrumKitNode::new(SR)), Source::DrumKit)
        .expect("the swap installs");
    let hit = rig.block(false, &[NoteEvent::note_on(0, KICK_KEY, 1.0)]);

    // Compare against the same kit, same event, rendered on its own: the
    // instruments are bit-deterministic, so "the insert ran" is an exact claim.
    let mut bare = DrumKitNode::new(SR);
    let mut buffers = AudioBuffers::new(BLOCK);
    {
        let events = [NoteEvent::note_on(0, KICK_KEY, 1.0)];
        let mut ctx = ProcessCtx::with_events(
            &mut buffers,
            SR,
            BLOCK,
            TransportSnapshot::default(),
            &events,
        );
        bare.process(&mut ctx);
    }
    let dry = buffers.as_slices()[0].to_vec();

    assert!(peak(&dry) > 0.1, "the kick must actually sound");
    for (index, (wet, dry)) in hit.iter().zip(dry.iter()).enumerate() {
        assert_eq!(
            wet.to_bits(),
            (dry * 0.5).to_bits(),
            "sample {index}: the insert did not survive the swap ({wet} vs {})",
            dry * 0.5
        );
    }
}

#[test]
fn the_gate_follows_the_source_without_a_second_graph_install() {
    let mut rig = Rig::new(Source::TestTone, TestToneFactory::new().create(SR, BLOCK));

    // The tone is free-running, so a stopped transport must be silent.
    assert!(rig.gate.load(Ordering::Acquire), "the tone arms the gate");
    assert!(
        rig.block(false, &[]).iter().all(|s| *s == 0.0),
        "Stop must silence the test tone"
    );

    // An instrument must sound with the transport stopped, or a user could not
    // audition one without pressing Play first.
    rig.handle
        .install(Box::new(PolySynthNode::new(SR)), Source::PolySynth)
        .expect("the swap installs");
    let played = rig.block(false, &[NoteEvent::note_on(0, 69, 1.0)]);
    assert!(
        !rig.gate.load(Ordering::Acquire),
        "an instrument must disarm the gate"
    );
    assert!(
        peak(&played) > 0.0,
        "a key press must make a sound with the transport stopped"
    );

    // ... and back again, still without touching the graph.
    rig.handle
        .install(TestToneFactory::new().create(SR, BLOCK), Source::TestTone)
        .expect("the swap installs");
    assert!(rig.block(false, &[]).iter().all(|s| *s == 0.0));
    assert!(rig.gate.load(Ordering::Acquire));
    assert_eq!(rig.handle.current(), Source::TestTone);
}

#[test]
fn a_swapped_in_instrument_plays_the_notes_the_block_carries() {
    let mut rig = Rig::new(Source::PolySynth, Box::new(PolySynthNode::new(SR)));

    assert!(
        rig.block(false, &[]).iter().all(|s| *s == 0.0),
        "an instrument with nothing held is silent"
    );

    let attack = rig.block(false, &[NoteEvent::note_on(0, 60, 1.0)]);
    assert!(peak(&attack) > 0.0, "the note must start");
    let held = rig.blocks(20, false);
    assert!(
        peak(&held) > 0.01,
        "the note must sustain, got {}",
        peak(&held)
    );

    rig.block(false, &[NoteEvent::note_off(0, 60)]);
    let released = rig.blocks(60, false);
    assert!(
        released.iter().all(|s| *s == 0.0),
        "the release must reach digital silence, peak {}",
        peak(&released)
    );
}

#[test]
fn a_superseded_source_is_freed_by_the_collector_and_not_by_the_audio_thread() {
    let mut rig = Rig::new(Source::TestTone, TestToneFactory::new().create(SR, BLOCK));
    // One graph + one source, both `Owned`.
    let before = rig.collector.alloc_count();
    assert!(before >= 2);

    rig.handle
        .install(Box::new(DrumKitNode::new(SR)), Source::DrumKit)
        .expect("installs");
    assert_eq!(rig.collector.alloc_count(), before + 1);

    rig.block(false, &[]);
    assert_eq!(
        rig.collector.alloc_count(),
        before + 1,
        "the audio thread must not have freed the old source"
    );

    rig.collector.collect();
    assert_eq!(
        rig.collector.alloc_count(),
        before,
        "the collector frees it, on its own thread"
    );
}
