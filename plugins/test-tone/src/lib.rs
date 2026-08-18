//! `mydaw-plugin-test-tone` -- a sine generator, and nothing else.
//!
//! This is the vehicle for Phase 1 spike 1 ("audio-io + test-tone: RT loop, sine
//! out, no dropouts, `assert_no_alloc` clean"). Its whole job is to be the
//! simplest possible thing that makes the RT path audible, so that when the spike
//! fails the blame lands on the plumbing rather than on the DSP.
//!
//! # RT safety
//! [`TestToneNode::process`] does no allocation, no locking, no syscalls and no
//! logging. It reads three fields, writes into pre-allocated buffers and returns.
//! Everything configurable is fixed at construction time (see below).
//!
//! # Phase 0 limitation
//! Frequency and amplitude are set when the node is *created* and cannot be
//! changed afterwards. Parameter changes need the `RtCommand` queue to grow a
//! parameter variant plus a smoothing ramp (a step change in gain clicks); both
//! belong to the mixer spec in Phase 2, not to a scaffold.

use std::f64::consts::TAU;
use std::sync::Arc;

use mydaw_kernel::{KernelCtx, Plugin, PluginManifest};
use mydaw_rt_core::{ProcessCtx, RtNode, RtNodeFactory};

/// Kernel id of this plugin.
pub const PLUGIN_ID: &str = "test-tone";

/// RT node type id, as stored in project files and looked up on the graph
/// builder. Stable identifier -- renaming it invalidates saved graphs.
pub const NODE_TYPE_ID: &str = "test-tone";

/// A440. Concert pitch, and comfortably inside every speaker's usable range.
pub const DEFAULT_FREQUENCY_HZ: f64 = 440.0;

/// -14 dBFS. Loud enough to see on a meter, quiet enough to not hurt when the
/// spike accidentally runs at 3 a.m. on headphones.
pub const DEFAULT_AMPLITUDE: f32 = 0.2;

/// A sine oscillator writing the same signal into both channels.
///
/// The phase is kept in `[0, 1)` (turns, not radians) and wrapped by subtraction
/// rather than `%`: subtraction is exact here because the increment is smaller
/// than 1, and it avoids the slow path of a floating-point remainder in the inner
/// loop. `f64` phase keeps the accumulated drift below a sample over hours of
/// playback, which `f32` would not.
pub struct TestToneNode {
    phase: f64,
    phase_increment: f64,
    amplitude: f32,
}

impl TestToneNode {
    /// Create an oscillator for a given sample rate.
    ///
    /// Non-RT: call this on the graph-building thread, then hand the node over
    /// via `GraphHandle::install`.
    pub fn new(frequency_hz: f64, amplitude: f32, sample_rate: f64) -> Self {
        Self {
            phase: 0.0,
            phase_increment: phase_increment(frequency_hz, sample_rate),
            amplitude,
        }
    }

    /// Oscillator with the defaults (440 Hz, -14 dBFS).
    pub fn with_defaults(sample_rate: f64) -> Self {
        Self::new(DEFAULT_FREQUENCY_HZ, DEFAULT_AMPLITUDE, sample_rate)
    }

    /// Fixed amplitude of this oscillator (linear, not dB).
    pub fn amplitude(&self) -> f32 {
        self.amplitude
    }

    /// Phase advance per sample, in turns.
    pub fn phase_increment(&self) -> f64 {
        self.phase_increment
    }
}

/// Turns per sample for `frequency_hz` at `sample_rate`.
///
/// A non-finite or non-positive sample rate yields 0, i.e. DC -- silence rather
/// than a NaN storm propagating into the output buffers.
fn phase_increment(frequency_hz: f64, sample_rate: f64) -> f64 {
    if sample_rate.is_finite() && sample_rate > 0.0 && frequency_hz.is_finite() {
        frequency_hz / sample_rate
    } else {
        0.0
    }
}

impl RtNode for TestToneNode {
    fn process(&mut self, ctx: &mut ProcessCtx) {
        // Hoist into locals so the inner loop touches registers, not `self`.
        let mut phase = self.phase;
        let increment = self.phase_increment;
        let amplitude = self.amplitude;

        // This node is a *generator*: it overwrites rather than sums. It is only
        // ever valid at the head of a chain, which is all Phase 0's serial graph
        // has. Summing buses arrive with the mixer in Phase 2.
        let [left, right] = ctx.buffers.as_mut_slices();
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let sample = (phase * TAU).sin() as f32 * amplitude;
            *l = sample;
            *r = sample;

            phase += increment;
            if phase >= 1.0 {
                phase -= 1.0;
            }
        }

        self.phase = phase;
    }
}

/// Builds [`TestToneNode`]s for the graph builder.
pub struct TestToneFactory {
    frequency_hz: f64,
    amplitude: f32,
}

impl Default for TestToneFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl TestToneFactory {
    /// Factory producing the default tone.
    pub fn new() -> Self {
        Self {
            frequency_hz: DEFAULT_FREQUENCY_HZ,
            amplitude: DEFAULT_AMPLITUDE,
        }
    }

    /// Factory producing a custom tone.
    pub fn with_tone(frequency_hz: f64, amplitude: f32) -> Self {
        Self {
            frequency_hz,
            amplitude,
        }
    }
}

impl RtNodeFactory for TestToneFactory {
    fn type_id(&self) -> &'static str {
        NODE_TYPE_ID
    }

    fn create(&self, sample_rate: f64, _max_block: usize) -> Box<dyn RtNode> {
        // `max_block` is irrelevant here: the oscillator holds no per-block state
        // and writes only into the caller's buffers.
        Box::new(TestToneNode::new(
            self.frequency_hz,
            self.amplitude,
            sample_rate,
        ))
    }
}

/// Contributes the `"test-tone"` node factory. No services, no panels.
pub struct TestTonePlugin {
    frequency_hz: f64,
    amplitude: f32,
}

impl Default for TestTonePlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TestTonePlugin {
    pub fn new() -> Self {
        Self {
            frequency_hz: DEFAULT_FREQUENCY_HZ,
            amplitude: DEFAULT_AMPLITUDE,
        }
    }

    /// Plugin contributing a factory for a custom tone.
    pub fn with_tone(frequency_hz: f64, amplitude: f32) -> Self {
        Self {
            frequency_hz,
            amplitude,
        }
    }
}

impl Plugin for TestTonePlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest::new(PLUGIN_ID, "Test Tone")
    }

    fn register(&mut self, ctx: &mut KernelCtx) {
        ctx.register_rt_node_factory(Box::new(TestToneFactory::with_tone(
            self.frequency_hz,
            self.amplitude,
        )));

        // Also published as a service so a caller holding only the registry (the
        // app's graph builder) can spin up a tone without walking the factory
        // list. Same object either way.
        let _ = ctx.services.provide(Arc::new(TestToneFactory::with_tone(
            self.frequency_hz,
            self.amplitude,
        )));
    }

    fn start(&mut self, _ctx: &KernelCtx) {}

    fn stop(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_kernel::Kernel;
    use mydaw_rt_core::{AudioBuffers, TransportSnapshot};

    const SR: f64 = 48_000.0;

    /// Render `frames` of the node, in `block` sized chunks, into one long
    /// per-channel buffer. Chunking matters: it also proves phase continuity
    /// across block boundaries.
    fn render(node: &mut dyn RtNode, frames: usize, block: usize) -> (Vec<f32>, Vec<f32>) {
        let mut buffers = AudioBuffers::new(block);
        let (mut left, mut right) = (Vec::with_capacity(frames), Vec::with_capacity(frames));
        let mut done = 0;
        while done < frames {
            let n = block.min(frames - done);
            let mut ctx = ProcessCtx::new(&mut buffers, SR, n, TransportSnapshot::default());
            node.process(&mut ctx);
            let [l, r] = buffers.as_slices();
            left.extend_from_slice(l);
            right.extend_from_slice(r);
            done += n;
        }
        (left, right)
    }

    /// Positive-going zero crossings == number of complete cycles.
    fn positive_zero_crossings(samples: &[f32]) -> usize {
        samples
            .windows(2)
            .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
            .count()
    }

    #[test]
    fn a_block_is_not_silent_and_stays_inside_the_amplitude() {
        let mut node = TestToneNode::with_defaults(SR);
        let (left, right) = render(&mut node, 512, 512);

        assert!(
            left.iter().any(|s| s.abs() > 1e-6),
            "output should not be silent"
        );
        assert_eq!(left, right, "both channels carry the same signal");

        let peak = left.iter().fold(0.0_f32, |a, s| a.max(s.abs()));
        assert!(
            peak <= DEFAULT_AMPLITUDE + 1e-6,
            "peak {peak} exceeds amplitude {DEFAULT_AMPLITUDE}"
        );
        // 512 frames at 48 kHz is ~4.7 cycles, so the peak should be close to full
        // scale for the configured amplitude.
        assert!(
            peak > DEFAULT_AMPLITUDE * 0.9,
            "peak {peak} suspiciously low"
        );
    }

    #[test]
    fn one_second_of_output_has_440_cycles() {
        let mut node = TestToneNode::with_defaults(SR);
        // Blocked at 128 frames: the count must survive block boundaries.
        let (left, _) = render(&mut node, SR as usize, 128);

        let cycles = positive_zero_crossings(&left);
        assert!(
            (439..=441).contains(&cycles),
            "expected ~440 cycles in one second, got {cycles}"
        );
    }

    #[test]
    fn frequency_tracks_the_sample_rate() {
        // Same 440 Hz request at half the rate must still yield 440 cycles/second.
        let mut node = TestToneNode::new(DEFAULT_FREQUENCY_HZ, DEFAULT_AMPLITUDE, 24_000.0);
        let mut buffers = AudioBuffers::new(24_000);
        let mut ctx = ProcessCtx::new(&mut buffers, 24_000.0, 24_000, TransportSnapshot::default());
        node.process(&mut ctx);
        let [left, _] = buffers.as_slices();
        let cycles = positive_zero_crossings(left);
        assert!(
            (439..=441).contains(&cycles),
            "expected ~440 cycles, got {cycles}"
        );
    }

    #[test]
    fn a_custom_frequency_is_honoured() {
        let mut node = TestToneNode::new(1_000.0, 0.5, SR);
        let (left, _) = render(&mut node, SR as usize, 256);
        let cycles = positive_zero_crossings(&left);
        assert!(
            (999..=1001).contains(&cycles),
            "expected ~1000 cycles, got {cycles}"
        );
    }

    #[test]
    fn a_degenerate_sample_rate_yields_silence_not_nan() {
        let mut node = TestToneNode::new(440.0, 0.2, 0.0);
        let (left, _) = render(&mut node, 64, 64);
        assert!(left.iter().all(|s| s.is_finite()));
        assert!(left.iter().all(|s| *s == 0.0), "DC at phase 0 is silence");
    }

    #[test]
    fn the_factory_is_registered_under_its_type_id() {
        let mut kernel = Kernel::new();
        kernel.add_plugin(Box::new(TestTonePlugin::new())).unwrap();
        kernel.boot().unwrap();

        let factory = kernel
            .ctx()
            .rt_node_factory(NODE_TYPE_ID)
            .expect("factory should be contributed");
        assert_eq!(factory.type_id(), "test-tone");

        let mut node = factory.create(SR, 128);
        let (left, _) = render(node.as_mut(), 256, 128);
        assert!(left.iter().any(|s| s.abs() > 1e-6));
    }

    #[test]
    fn the_factory_is_also_available_as_a_service() {
        let mut kernel = Kernel::new();
        kernel.add_plugin(Box::new(TestTonePlugin::new())).unwrap();
        kernel.boot().unwrap();
        assert!(kernel.ctx().services.get::<TestToneFactory>().is_some());
    }
}
