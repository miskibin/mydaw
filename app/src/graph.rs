//! One track's chain, and the transport gate at the end of it.
//!
//! ```text
//!   per track:  SourceSlot -> [insert..] -> TransportGate -> [strip] --\
//!                                                                       sum
//!   master:                        InsertSlot -> [master fader] <------/
//! ```
//!
//! The tracks themselves are assembled by [`TrackHost::build_spec`], which calls
//! [`track_chain`] once per track; this module owns the one piece that is not
//! the mixer's business -- the gate.
//!
//! A track's chain is installed once per stream. What changes at runtime is the
//! node *inside* [`SourceSlot`](crate::source::SourceSlot) -- see `crate::source`
//! for why the swap happens there and not by rebuilding the graph -- and the node
//! inside the master's [`InsertSlot`](crate::insert::InsertSlot).
//!
//! [`TrackHost::build_spec`]: crate::tracks::TrackHost::build_spec
//!
//! # Why the gate is conditional rather than absent
//!
//! [`TransportGate`] exists because `TestToneNode` is a free-running
//! oscillator: it makes a sine whether or not the DAW is playing, so the shell
//! has to silence it when the transport is stopped. An instrument has the
//! opposite problem. It is silent until it is *played*, and it is played live,
//! by a human, whose expectation is that pressing a key makes a sound -- exactly
//! as it does in every DAW, with the transport stopped, before anything has been
//! recorded. Gating it on the transport would make the instrument mute unless
//! you pressed Play first, which is not a stricter engine, just a broken one.
//!
//! So an instrument is not gated -- and neither is a hosted CLAP instrument, for
//! the same reason. Live notes are gated by their own note-offs, which is the
//! correct mechanism. The sequencer gates *pattern playback* on the transport
//! (no transport, no scheduled events) while leaving live input alone; that is a
//! property of the event source, not of the audio chain, so it belongs there and
//! not here.
//!
//! The gate node itself stays **last** in the chain either way, so Stop
//! silences whatever the track's inserts produced -- including a reverb tail --
//! rather than only the generator. Whether it is armed is a flag the source
//! publishes as it is installed, so the two can never disagree about which
//! source is playing.
//!
//! # What happened to `MYDAW_CLAP_FX` and `MYDAW_CLAP_INSTRUMENT`
//!
//! Retired in step 6, along with the single-instance model they implied. They
//! were spike affordances: a way to get *one* plugin into the graph before there
//! was a UI for choosing one. There is a UI now -- a picker over the scan, a
//! source dropdown per track and a master insert selector -- and it does
//! strictly more (any number of instruments, on any track, changeable while the
//! engine runs). Keeping the variables would have meant maintaining a second,
//! weaker path into the same machinery for no user.
//!
//! `CLAP_PATH` -- the CLAP specification's own search-path override, which is how
//! a developer points MYDAW at a build directory -- is unaffected and still
//! honoured by
//! [`standard_search_paths`](mydaw_plugin_host_clap::standard_search_paths).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use mydaw_rt_core::{GraphSpec, ProcessCtx, RtNode, TrackSpec};

/// Silences the bus while the transport is stopped -- when the current source
/// asks to be silenced.
///
/// `TestToneNode` is a free-running oscillator: it writes a sine every block
/// regardless of transport state, which is right for a DSP node and wrong for a
/// DAW that is not playing. Rather than teach the oscillator about the transport
/// (Phase 2 gives nodes proper gain/automation), the shell appends this gate to
/// the end of the chain, so Play/Stop actually start and stop the sound.
///
/// An instrument must *not* be gated (see the module docs), and the source can
/// change while the stream is running, so the decision is a shared flag rather
/// than the gate's presence in the graph. The flag is written by
/// [`SourceSlot`](crate::source::SourceSlot) at the head of the same chain, in
/// the same block, so it always describes the source that just rendered.
///
/// # RT safety
/// Reads one atomic and one `bool` from the per-block transport snapshot, then
/// fills pre-allocated slices. No allocation, no locking, no syscalls.
pub struct TransportGate {
    armed: Arc<AtomicBool>,
}

impl TransportGate {
    /// A gate armed by `armed`, which the source slot publishes.
    pub fn new(armed: Arc<AtomicBool>) -> Self {
        Self { armed }
    }

    /// A gate that is always armed, for chains built by hand (tests, and the
    /// offline renders in `tests/clap_graph.rs`) rather than by a source slot.
    pub fn always() -> Self {
        Self::new(Arc::new(AtomicBool::new(true)))
    }

    /// Whether this gate is currently silencing a stopped transport.
    pub fn is_armed(&self) -> bool {
        self.armed.load(Ordering::Acquire)
    }
}

impl RtNode for TransportGate {
    fn process(&mut self, ctx: &mut ProcessCtx) {
        if !self.is_armed() || ctx.transport.playing {
            return;
        }
        // `as_mut_slices` is already truncated to the block's frame count.
        for channel in ctx.buffers.as_mut_slices() {
            channel.fill(0.0);
        }
    }
}

/// Build a plain serial chain: `source -> [effect] -> [gate]`.
///
/// The effect sits after the source, because that is where an insert lives, and
/// the gate goes last, so Stop silences whatever the effect produced.
///
/// One track's worth of the shape the engine runs, minus the source slot -- for
/// the offline renders that want a chain without a stream behind it.
///
/// Non-RT: node construction allocates, which is exactly why it happens here and
/// not on the audio thread.
pub fn engine_graph(
    source: Box<dyn RtNode>,
    effect: Option<Box<dyn RtNode>>,
    gated: bool,
) -> GraphSpec {
    let mut spec = GraphSpec::new().with(source);
    if let Some(effect) = effect {
        spec.push(effect);
    }
    if gated {
        spec.push(Box::new(TransportGate::always()));
    }
    spec
}

/// Assemble one track: `SourceSlot -> [effect] -> TransportGate`.
///
/// The gate goes **last**, so Stop silences whatever the track's inserts
/// produced -- including a tail -- rather than only its generator. It is armed
/// by the flag the slot publishes, so a later swap to (or from) the test tone
/// arms or disarms it with no graph install at all.
///
/// This is the function [`TrackHost::build_spec`] calls once per track, so the
/// offline tests build the chain the engine actually runs rather than a copy of
/// it that can drift.
///
/// [`TrackHost::build_spec`]: crate::tracks::TrackHost::build_spec
pub fn track_chain(
    slot: crate::source::SourceSlot,
    effect: Option<Box<dyn RtNode>>,
    gate: Arc<AtomicBool>,
) -> TrackSpec {
    let mut track = TrackSpec::new(Box::new(slot) as Box<dyn RtNode>);
    if let Some(effect) = effect {
        track.inserts.push(effect);
    }
    track.inserts.push(Box::new(TransportGate::new(gate)));
    track
}

/// [`track_chain`] as a whole one-track graph, for offline renders that drive a
/// [`RtGraphSlot`](mydaw_rt_core::RtGraphSlot) by hand.
pub fn source_chain(
    slot: crate::source::SourceSlot,
    effect: Option<Box<dyn RtNode>>,
    gate: Arc<AtomicBool>,
) -> GraphSpec {
    GraphSpec::new().with_track(track_chain(slot, effect, gate))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_plugin_test_tone::TestToneFactory;
    use mydaw_rt_core::{AudioBuffers, RtNodeFactory, TransportSnapshot};

    const SR: f64 = 48_000.0;
    const BLOCK: usize = 64;

    /// Render one block of `test-tone -> [effect] -> gate` with the given
    /// transport state, returning the left channel.
    fn render(playing: bool, effect: Option<Box<dyn RtNode>>) -> Vec<f32> {
        let factory = TestToneFactory::new();
        let mut nodes: Vec<Box<dyn RtNode>> = vec![factory.create(SR, BLOCK)];
        if let Some(effect) = effect {
            nodes.push(effect);
        }
        nodes.push(Box::new(TransportGate::always()));

        let mut buffers = AudioBuffers::new(BLOCK);
        let transport = TransportSnapshot {
            sample_pos: 0,
            playing,
            ..TransportSnapshot::default()
        };
        let mut ctx = ProcessCtx::new(&mut buffers, SR, BLOCK, transport);
        for node in &mut nodes {
            node.process(&mut ctx);
        }

        buffers.as_slices()[0].to_vec()
    }

    /// Stand-in for a CLAP effect in tests that are about the graph, not CLAP.
    struct HalveNode;
    impl RtNode for HalveNode {
        fn process(&mut self, ctx: &mut ProcessCtx) {
            for channel in ctx.buffers.as_mut_slices() {
                for sample in channel.iter_mut() {
                    *sample *= 0.5;
                }
            }
        }
    }

    /// Stand-in for a CLAP instrument with nothing to play.
    struct SilentNode;
    impl RtNode for SilentNode {
        fn process(&mut self, ctx: &mut ProcessCtx) {
            ctx.buffers.silence();
        }
    }

    /// Stand-in for a CLAP instrument that has been played.
    struct ConstNode(f32);
    impl RtNode for ConstNode {
        fn process(&mut self, ctx: &mut ProcessCtx) {
            for channel in ctx.buffers.as_mut_slices() {
                channel.fill(self.0);
            }
        }
    }

    #[test]
    fn the_gate_passes_audio_while_playing() {
        let left = render(true, None);
        assert!(
            left.iter().any(|s| s.abs() > 0.01),
            "a 440 Hz tone should be audible while the transport runs"
        );
    }

    #[test]
    fn the_gate_silences_a_stopped_transport() {
        let left = render(false, None);
        assert!(
            left.iter().all(|s| *s == 0.0),
            "nothing may reach the device while the transport is stopped"
        );
    }

    #[test]
    fn an_effect_sits_between_the_generator_and_the_gate() {
        let plain = render(true, None);
        let halved = render(true, Some(Box::new(HalveNode)));

        let plain_peak = plain.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()));
        let halved_peak = halved.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()));
        assert!((halved_peak - plain_peak / 2.0).abs() < 1e-6);

        // And the gate still wins: an insert must not make Stop leak audio.
        assert!(render(false, Some(Box::new(HalveNode)))
            .iter()
            .all(|s| *s == 0.0));
    }

    #[test]
    fn the_tone_chain_is_tone_then_gate_and_grows_by_one_with_an_effect() {
        let tone = || TestToneFactory::new().create(SR, BLOCK);
        assert_eq!(engine_graph(tone(), None, true).len(), 2);
        assert_eq!(
            engine_graph(tone(), Some(Box::new(HalveNode)), true).len(),
            3
        );
    }

    #[test]
    fn an_ungated_chain_is_just_the_source_and_its_insert() {
        // An instrument alone: one node, and it is *not* the gate.
        let spec = engine_graph(Box::new(SilentNode), None, false);
        assert_eq!(spec.len(), 1, "no gate -- just the instrument");

        let spec = engine_graph(Box::new(SilentNode), Some(Box::new(HalveNode)), false);
        assert_eq!(spec.len(), 2);
    }

    #[test]
    fn an_instrument_sounds_with_the_transport_stopped() {
        // The whole point of the gate being conditional. A stopped transport
        // must not silence a live instrument, or a user could not audition one
        // without pressing Play first.
        let mut nodes: Vec<Box<dyn RtNode>> = vec![Box::new(ConstNode(0.3))];
        let mut buffers = AudioBuffers::new(BLOCK);
        let mut ctx = ProcessCtx::new(
            &mut buffers,
            SR,
            BLOCK,
            TransportSnapshot {
                sample_pos: 0,
                playing: false,
                ..TransportSnapshot::default()
            },
        );
        for node in &mut nodes {
            node.process(&mut ctx);
        }
        assert!(
            buffers.as_slices()[0].iter().all(|s| *s == 0.3),
            "an ungated chain has nothing to silence it"
        );

        // And the tone chain still behaves the old way, for contrast.
        assert!(render(false, None).iter().all(|s| *s == 0.0));
    }

    #[test]
    fn a_disarmed_gate_passes_a_stopped_transport_through_untouched() {
        // The runtime half of the same decision: one gate node, two behaviours,
        // chosen by the flag the source slot publishes.
        let armed = Arc::new(AtomicBool::new(true));
        let mut nodes: Vec<Box<dyn RtNode>> = vec![
            Box::new(ConstNode(0.3)),
            Box::new(TransportGate::new(Arc::clone(&armed))),
        ];
        let stopped = TransportSnapshot {
            sample_pos: 0,
            playing: false,
            ..TransportSnapshot::default()
        };

        let mut buffers = AudioBuffers::new(BLOCK);
        {
            let mut ctx = ProcessCtx::new(&mut buffers, SR, BLOCK, stopped);
            for node in &mut nodes {
                node.process(&mut ctx);
            }
        }
        assert!(buffers.as_slices()[0].iter().all(|s| *s == 0.0), "armed");

        armed.store(false, Ordering::Release);
        {
            let mut ctx = ProcessCtx::new(&mut buffers, SR, BLOCK, stopped);
            for node in &mut nodes {
                node.process(&mut ctx);
            }
        }
        assert!(
            buffers.as_slices()[0].iter().all(|s| *s == 0.3),
            "a disarmed gate must be transparent"
        );
    }
}
