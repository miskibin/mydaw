//! Offline rendering helpers, shared by this crate's unit and integration tests.
//!
//! Public because an integration test in `tests/` can only reach a crate's
//! public API, and duplicating a render loop into six files is how two of them
//! quietly stop agreeing. Nothing here is used by the engine; it is compiled
//! into the library for the same reason `mydaw-plugin-instruments` compiles its
//! own `test_support`.
//!
//! The loop deliberately renders in **blocks**, and the default block size is
//! deliberately not a round number: an effect that only works when the whole
//! capture arrives in one `process` call has a bug a single-block test cannot
//! see -- a smoother that restarts, a delay line that resets, a bypass that
//! forgets where its fade got to.

use std::sync::Arc;

use mydaw_rt_core::{
    AudioBuffers, ParamBlock, ParamNodeFactory, ProcessCtx, RtNode, TransportSnapshot, CHANNELS,
};

/// Block size the harness renders in unless told otherwise.
///
/// 96 frames: smaller than the 480-frame parameter ramp, so every test that
/// measures a ramp crosses at least five block boundaries, and not a power of
/// two, so an off-by-one that only shows up at 64 or 128 has nowhere to hide.
pub const DEFAULT_BLOCK: usize = 96;

/// One effect node, its parameter block, and a transport to drive it with.
pub struct Harness {
    /// The shared parameter block. Writing to it is what the shell does.
    pub params: Arc<ParamBlock>,
    node: Box<dyn RtNode>,
    buffers: AudioBuffers,
    sample_rate: f64,
    block: usize,
    /// Tempo the transport reports. The delay reads it.
    pub bpm: f64,
    /// Whether the transport reports itself as running.
    pub playing: bool,
    position: u64,
}

impl Harness {
    /// Build a node from `factory` at its default parameters.
    pub fn new(factory: &dyn ParamNodeFactory, sample_rate: f64, max_block: usize) -> Self {
        let params = factory.default_params();
        let node = factory.create_with_params(sample_rate, max_block, Arc::clone(&params));
        Self {
            params,
            node,
            buffers: AudioBuffers::new(max_block),
            sample_rate,
            block: DEFAULT_BLOCK.min(max_block),
            bpm: mydaw_rt_core::DEFAULT_BPM,
            playing: true,
            position: 0,
        }
    }

    /// Render in `block`-sized chunks instead of [`DEFAULT_BLOCK`].
    #[must_use]
    pub fn with_block(mut self, block: usize) -> Self {
        self.block = block.max(1).min(self.buffers.max_block());
        self
    }

    /// Run the transport at `bpm`.
    #[must_use]
    pub fn at_bpm(mut self, bpm: f64) -> Self {
        self.bpm = bpm;
        self
    }

    /// Set a parameter by id, panicking on an id this node does not declare.
    pub fn set(&mut self, id: &str, value: f32) {
        assert!(
            self.params.set_by_id(id, value).is_some(),
            "no parameter called '{id}'"
        );
    }

    /// Read a parameter by id.
    pub fn get(&self, id: &str) -> f32 {
        self.params
            .get_by_id(id)
            .unwrap_or_else(|| panic!("no parameter called '{id}'"))
    }

    /// Read a meter by id.
    pub fn readout(&self, id: &str) -> f32 {
        let index = self
            .params
            .readout_index_of(id)
            .unwrap_or_else(|| panic!("no readout called '{id}'"));
        self.params.readout(index)
    }

    /// The sample rate this harness renders at.
    pub const fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// Push one block of `frames` interleaved stereo frames through the node.
    fn render_block(&mut self, input: &[[f32; 2]], out: &mut Vec<[f32; 2]>) {
        let frames = input.len();
        {
            let channels = self.buffers.as_mut_slices_for(frames);
            for (frame, sample) in input.iter().enumerate() {
                channels[0][frame] = sample[0];
                channels[1][frame] = sample[1];
            }
        }
        let transport = TransportSnapshot {
            sample_pos: self.position,
            playing: self.playing,
            bpm: self.bpm,
            ..TransportSnapshot::default()
        };
        {
            let mut ctx = ProcessCtx::new(&mut self.buffers, self.sample_rate, frames, transport);
            self.node.process(&mut ctx);
        }
        let [left, right] = self.buffers.as_slices();
        for (l, r) in left[..frames].iter().zip(&right[..frames]) {
            out.push([*l, *r]);
        }
        self.position += frames as u64;
    }
}

/// Helper so the harness can address a partial block without borrowing twice.
trait BufferView {
    fn as_mut_slices_for(&mut self, frames: usize) -> [&mut [f32]; CHANNELS];
}

impl BufferView for AudioBuffers {
    fn as_mut_slices_for(&mut self, frames: usize) -> [&mut [f32]; CHANNELS] {
        self.set_frames(frames);
        self.as_mut_slices()
    }
}

/// Push `input` through the harness in blocks, returning the output.
pub fn render_stereo(harness: &mut Harness, input: &[[f32; 2]]) -> Vec<[f32; 2]> {
    let block = harness.block;
    let mut out = Vec::with_capacity(input.len());
    for chunk in input.chunks(block) {
        harness.render_block(chunk, &mut out);
    }
    out
}

/// Push a mono signal through both channels, returning the left channel.
pub fn render_mono(harness: &mut Harness, input: &[f32]) -> Vec<f32> {
    let stereo: Vec<[f32; 2]> = input.iter().map(|s| [*s, *s]).collect();
    render_stereo(harness, &stereo)
        .into_iter()
        .map(|frame| frame[0])
        .collect()
}

/// A sine at `hz`, `frames` long, at amplitude `amplitude`.
pub fn sine(hz: f64, frames: usize, sample_rate: f64, amplitude: f32) -> Vec<f32> {
    (0..frames)
        .map(|n| {
            (amplitude as f64 * (std::f64::consts::TAU * hz * n as f64 / sample_rate).sin()) as f32
        })
        .collect()
}

/// Silence, `frames` long.
pub fn silence(frames: usize) -> Vec<f32> {
    vec![0.0; frames]
}

/// Root-mean-square of a stereo capture, both channels together.
pub fn rms(frames: &[[f32; 2]]) -> f32 {
    if frames.is_empty() {
        return 0.0;
    }
    let energy: f64 = frames
        .iter()
        .map(|f| f64::from(f[0]).powi(2) + f64::from(f[1]).powi(2))
        .sum();
    (energy / (2.0 * frames.len() as f64)).sqrt() as f32
}

/// Root-mean-square of a mono capture.
pub fn rms_mono(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let energy: f64 = samples.iter().map(|s| f64::from(*s).powi(2)).sum();
    (energy / samples.len() as f64).sqrt() as f32
}

/// Largest absolute sample of a mono capture.
pub fn peak_mono(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()))
}

/// Amplitude of the component at `hz`, by a single-bin Goertzel-style
/// correlation.
///
/// A whole FFT would be overkill for "how much 3 kHz is in this": the tests know
/// exactly which frequency they are asking about, and correlating against one
/// complex exponential is four multiplies a sample with no windowing decisions
/// to get wrong. The window is a whole number of the *analysis* frequency's
/// cycles wherever the caller can arrange it, which keeps leakage below the
/// tolerances these tests use.
pub fn amplitude_at(samples: &[f32], hz: f64, sample_rate: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let (mut real, mut imag) = (0.0_f64, 0.0_f64);
    for (n, s) in samples.iter().enumerate() {
        let w = std::f64::consts::TAU * hz * n as f64 / sample_rate;
        real += f64::from(*s) * w.cos();
        imag += f64::from(*s) * w.sin();
    }
    2.0 * (real * real + imag * imag).sqrt() / samples.len() as f64
}

/// [`amplitude_at`] in decibels, floored so silence is a number.
pub fn level_db_at(samples: &[f32], hz: f64, sample_rate: f64) -> f64 {
    20.0 * amplitude_at(samples, hz, sample_rate).max(1.0e-12).log10()
}

/// dB of a linear amplitude, floored.
pub fn db(linear: f32) -> f32 {
    20.0 * linear.max(1.0e-12).log10()
}
