//! The Phase 0 audio graph: a test tone behind a transport gate.

use mydaw_plugin_audio_io::{AudioIoError, AudioIoService};
use mydaw_plugin_test_tone::TestToneFactory;
use mydaw_rt_core::{GraphSpec, ProcessCtx, RtNode, RtNodeFactory};

/// Silences the bus while the transport is stopped.
///
/// `TestToneNode` is a free-running oscillator: it writes a sine every block
/// regardless of transport state, which is right for a DSP node and wrong for a
/// DAW that is not playing. Rather than teach the oscillator about the transport
/// (Phase 2 gives nodes proper gain/automation), the shell appends this gate to
/// the end of the chain, so Play/Stop actually start and stop the sound.
///
/// # RT safety
/// Reads one `bool` from the per-block transport snapshot and fills pre-allocated
/// slices. No allocation, no locking, no syscalls.
pub struct TransportGate;

impl RtNode for TransportGate {
    fn process(&mut self, ctx: &mut ProcessCtx) {
        if ctx.transport.playing {
            return;
        }
        // `as_mut_slices` is already truncated to the block's frame count.
        for channel in ctx.buffers.as_mut_slices() {
            channel.fill(0.0);
        }
    }
}

/// Build the default graph: `test-tone -> transport gate`.
///
/// Non-RT: node construction allocates, which is exactly why it happens here and
/// not on the audio thread.
pub fn tone_graph(factory: &TestToneFactory, sample_rate: f64, max_block: usize) -> GraphSpec {
    GraphSpec::new()
        .with(factory.create(sample_rate, max_block))
        .with(Box::new(TransportGate))
}

/// Compile and publish [`tone_graph`] to the running audio thread.
///
/// # Errors
/// [`AudioIoError::NotRunning`] when no stream is open (a machine with no output
/// device still boots; it just has nothing to install into).
pub fn install_tone_graph(
    audio: &AudioIoService,
    factory: &TestToneFactory,
) -> Result<(), AudioIoError> {
    let info = audio.stream_info().ok_or(AudioIoError::NotRunning)?;
    audio.install_graph(tone_graph(
        factory,
        f64::from(info.sample_rate),
        info.max_block,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_rt_core::{AudioBuffers, TransportSnapshot};

    const SR: f64 = 48_000.0;
    const BLOCK: usize = 64;

    /// Render one block of `test-tone -> gate` with the given transport state.
    fn render(playing: bool) -> Vec<f32> {
        let factory = TestToneFactory::new();
        let mut tone = factory.create(SR, BLOCK);
        let mut gate = TransportGate;
        let mut buffers = AudioBuffers::new(BLOCK);
        let transport = TransportSnapshot {
            sample_pos: 0,
            playing,
        };

        let mut ctx = ProcessCtx::new(&mut buffers, SR, BLOCK, transport);
        tone.process(&mut ctx);
        gate.process(&mut ctx);

        buffers.as_slices()[0].to_vec()
    }

    #[test]
    fn the_gate_passes_audio_while_playing() {
        let left = render(true);
        assert!(
            left.iter().any(|s| s.abs() > 0.01),
            "a 440 Hz tone should be audible while the transport runs"
        );
    }

    #[test]
    fn the_gate_silences_a_stopped_transport() {
        let left = render(false);
        assert!(
            left.iter().all(|s| *s == 0.0),
            "nothing may reach the device while the transport is stopped"
        );
    }

    #[test]
    fn the_default_graph_is_tone_then_gate() {
        let spec = tone_graph(&TestToneFactory::new(), SR, BLOCK);
        assert_eq!(spec.len(), 2);
    }
}
