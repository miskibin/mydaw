//! Fixtures shared by this crate's unit tests, its integration tests and the
//! hardware-verification runs.
//!
//! Public rather than `#[cfg(test)]` for the same reason
//! [`mydaw_plugin_instruments::test_support`] is: an integration test in
//! `tests/` is a separate crate and cannot see a `#[cfg(test)]` module, and a
//! second copy of "render N frames and take the peak" is a second thing to keep
//! correct.
//!
//! # Why the WAV writer is by hand
//! The obvious answer is `hound`, which this workspace already depends on. But
//! a dev-dependency is invisible to `tests/`-facing code in a `pub` module, and
//! a 44-byte header is not worth an optional feature and a dependency edge. The
//! writer below emits IEEE-float WAV, which is what `symphonia`'s PCM decoder
//! reads and what keeps these fixtures exact: no quantisation between what a
//! test writes and what the loader gets back.

use std::path::{Path, PathBuf};

use mydaw_rt_core::{AudioBuffers, NoteEvent, ProcessCtx, RtNode, TransportSnapshot};

/// A directory under the system temp dir, removed when it drops.
///
/// Not `tempfile`: one more dependency for `create_dir_all` plus a counter is
/// not a trade worth making, and a leaked directory in a failed test is a
/// diagnostic rather than a leak.
#[derive(Debug)]
pub struct TempDir(PathBuf);

impl TempDir {
    /// A fresh directory named after `label`, plus enough entropy that parallel
    /// tests do not collide.
    pub fn new(label: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = format!(
            "mydaw-sampler-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(unique);
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a temp directory must be creatable");
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Write `samples` as an IEEE-float WAV at `path`.
///
/// `samples` is interleaved by `channels`. Panics rather than returning a
/// `Result`: this is test scaffolding, and a fixture that cannot be written is
/// not a condition a test should carry on past.
pub fn write_wav(path: &Path, sample_rate: u32, channels: u16, samples: &[f32]) {
    let bits = 32_u16;
    let block_align = channels * bits / 8;
    let byte_rate = sample_rate * u32::from(block_align);
    let data_bytes = (samples.len() * 4) as u32;

    let mut out = Vec::with_capacity(44 + samples.len() * 4);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    out.extend_from_slice(b"WAVE");

    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16_u32.to_le_bytes());
    // 3 = IEEE float. See the module docs.
    out.extend_from_slice(&3_u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());

    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_bytes.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("the fixture directory must exist");
    }
    std::fs::write(path, out).expect("the fixture WAV must be writable");
}

/// A sine of `hz` at `sample_rate`, `frames` long, mono.
pub fn sine(hz: f64, sample_rate: f64, frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|frame| {
            let phase = std::f64::consts::TAU * hz * frame as f64 / sample_rate;
            phase.sin() as f32
        })
        .collect()
}

/// A linear ramp from `0.0` to `1.0` over `frames`, mono.
///
/// The fixture the interpolation tests use: a ramp has an exact expected value
/// at every fractional position, so any curvature an interpolator invents shows
/// up as a number rather than as a judgement call.
pub fn ramp(frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|frame| frame as f32 / frames.max(1) as f32)
        .collect()
}

/// Render `frames` frames through `node` in blocks of `block`, delivering
/// `events` at the top of the first block.
///
/// Returns `(left, right)`.
pub fn render(
    node: &mut dyn RtNode,
    frames: usize,
    block: usize,
    events: &[NoteEvent],
) -> (Vec<f32>, Vec<f32>) {
    render_at(node, frames, block, events, 48_000.0)
}

/// [`render`], at an explicit sample rate.
pub fn render_at(
    node: &mut dyn RtNode,
    frames: usize,
    block: usize,
    events: &[NoteEvent],
    sample_rate: f64,
) -> (Vec<f32>, Vec<f32>) {
    let mut buffers = AudioBuffers::new(block);
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    let mut done = 0_usize;
    let mut pending = events;

    while done < frames {
        let this_block = block.min(frames - done);
        {
            let mut ctx = ProcessCtx::with_events(
                &mut buffers,
                sample_rate,
                this_block,
                TransportSnapshot::default(),
                pending,
            );
            node.process(&mut ctx);
        }
        pending = &[];
        let slices = buffers.as_slices();
        left.extend_from_slice(&slices[0][..this_block]);
        right.extend_from_slice(&slices[1][..this_block]);
        done += this_block;
    }
    (left, right)
}

/// Builds a real instrument on disk out of synthetic audio.
///
/// Every test that is *about the sampler* rather than about the piano uses
/// this: it writes WAVs and a `manifest.json` into a [`TempDir`] and loads them
/// through the ordinary [`load_instrument`](crate::load_instrument) path, so
/// the thing under test is the shipping code and not a parallel in-memory
/// constructor that could drift away from it.
///
/// Synthetic audio also makes the assertions exact. A ramp has a known value at
/// every fractional position, and a sine has one partial; neither is true of a
/// piano, which is why the piano's own tests
/// (`tests/piano_assets.rs`) assert much weaker things.
#[derive(Debug)]
pub struct InstrumentBuilder {
    id: String,
    name: String,
    release_ms: f32,
    one_shot: bool,
    max_seconds: Option<f32>,
    fade_out_seconds: Option<f32>,
    entries: Vec<(crate::manifest::SampleEntry, u32, u16, Vec<f32>)>,
}

impl InstrumentBuilder {
    /// A builder for an instrument called `id`.
    pub fn new(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            name: id.to_owned(),
            release_ms: crate::manifest::DEFAULT_RELEASE_MS,
            one_shot: false,
            max_seconds: None,
            fade_out_seconds: None,
            entries: Vec::new(),
        }
    }

    #[must_use]
    pub fn release_ms(mut self, ms: f32) -> Self {
        self.release_ms = ms;
        self
    }

    /// Make it a triggered instrument: note-off is ignored and the sample
    /// plays out. What a drum kit declares.
    #[must_use]
    pub fn one_shot(mut self, one_shot: bool) -> Self {
        self.one_shot = one_shot;
        self
    }

    #[must_use]
    pub fn truncate(mut self, max_seconds: f32, fade_out_seconds: f32) -> Self {
        self.max_seconds = Some(max_seconds);
        self.fade_out_seconds = Some(fade_out_seconds);
        self
    }

    /// Add a mono sample covering `key_lo..=key_hi` at `vel_lo..=vel_hi`.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn sample(
        mut self,
        file: &str,
        root_key: u8,
        key_lo: u8,
        key_hi: u8,
        vel_lo: u8,
        vel_hi: u8,
        sample_rate: u32,
        data: Vec<f32>,
    ) -> Self {
        self.entries.push((
            crate::manifest::SampleEntry {
                file: file.to_owned(),
                root_key,
                key_lo,
                key_hi,
                vel_lo,
                vel_hi,
                tune_cents: 0.0,
            },
            sample_rate,
            1,
            data,
        ));
        self
    }

    /// Add a sample covering every key and every velocity. The common case.
    #[must_use]
    pub fn full_range(self, file: &str, root_key: u8, sample_rate: u32, data: Vec<f32>) -> Self {
        self.sample(file, root_key, 0, 127, 1, 127, sample_rate, data)
    }

    /// Write the instrument and load it.
    ///
    /// The [`TempDir`] is returned alongside the bank because dropping it
    /// deletes the samples -- which is fine after a load (everything is in
    /// memory) but not before one.
    pub fn build(self) -> (TempDir, std::sync::Arc<crate::SampleBank>) {
        let dir = TempDir::new(&self.id);
        let home = dir.path().join(&self.id);
        std::fs::create_dir_all(&home).expect("the instrument directory must be creatable");

        let mut samples = Vec::with_capacity(self.entries.len());
        for (entry, rate, channels, data) in self.entries {
            write_wav(&home.join(&entry.file), rate, channels, &data);
            samples.push(entry);
        }

        let manifest = crate::manifest::InstrumentManifest {
            id: self.id.clone(),
            name: self.name,
            description: String::new(),
            release_ms: self.release_ms,
            one_shot: self.one_shot,
            default_params: Default::default(),
            max_seconds: self.max_seconds,
            fade_out_seconds: self.fade_out_seconds,
            attribution: String::new(),
            samples,
        };
        std::fs::write(
            home.join(crate::manifest::MANIFEST_FILE_NAME),
            serde_json::to_vec_pretty(&manifest).expect("the manifest must serialise"),
        )
        .expect("the manifest must be writable");

        let bank = crate::load_instrument(&home).expect("the synthetic instrument must load");
        (dir, bank)
    }
}

/// Largest absolute sample.
pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0_f32, |max, s| max.max(s.abs()))
}

/// Root mean square.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
    (sum / samples.len() as f64).sqrt() as f32
}
