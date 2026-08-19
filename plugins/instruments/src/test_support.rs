//! Offline rendering helpers, shared by this crate's unit and integration tests.
//!
//! Public because an integration test in `tests/` can only reach a crate's
//! public API, and duplicating a render loop into four files is how two of them
//! quietly stop agreeing. Nothing here is used by the engine; it is compiled
//! into the library for the same reason `mydaw-plugin-host-clap` compiles its
//! own `test_util` module.
//!
//! The loop deliberately renders in **blocks**: an instrument that only works
//! when the whole capture is one `process` call has a bug that a single-block
//! test cannot see (phase continuity, envelope state, a filter reset in the
//! wrong place).

use mydaw_rt_core::{AudioBuffers, NoteEvent, ProcessCtx, RtNode, TransportSnapshot};

/// Render `frames` frames of `node` in `block`-sized chunks.
///
/// `events` are delivered to the **first** block only, at their own frame
/// offsets, which is how the engine delivers a burst of live notes. Later
/// blocks see an empty event slice, exactly as a real callback does once the
/// queue is drained.
///
/// Returns the left and right channels, concatenated across blocks.
pub fn render(
    node: &mut dyn RtNode,
    frames: usize,
    block: usize,
    events: &[NoteEvent],
) -> (Vec<f32>, Vec<f32>) {
    render_at(node, frames, block, events, 48_000.0)
}

/// [`render`], at a chosen sample rate.
pub fn render_at(
    node: &mut dyn RtNode,
    frames: usize,
    block: usize,
    events: &[NoteEvent],
    sample_rate: f64,
) -> (Vec<f32>, Vec<f32>) {
    let block = block.max(1);
    let mut buffers = AudioBuffers::new(block);
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);

    let mut done = 0;
    let mut first = true;
    while done < frames {
        let n = block.min(frames - done);
        let block_events: &[NoteEvent] = if first { events } else { &[] };
        {
            let mut ctx = ProcessCtx::with_events(
                &mut buffers,
                sample_rate,
                n,
                TransportSnapshot::default(),
                block_events,
            );
            node.process(&mut ctx);
        }
        let [l, r] = buffers.as_slices();
        left.extend_from_slice(l);
        right.extend_from_slice(r);
        done += n;
        first = false;
    }
    (left, right)
}

/// Largest absolute sample. `0.0` for an empty slice.
pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()))
}

/// Root mean square. `0.0` for an empty slice.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
    (sum / samples.len() as f64).sqrt() as f32
}
