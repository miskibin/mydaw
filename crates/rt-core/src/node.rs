//! The RT processing contract: [`RtNode`] and its factory.

use crate::{AudioBuffers, TransportSnapshot};

/// Everything a node needs for one block.
///
/// Constructed fresh by the audio thread each block; the buffers themselves are
/// borrowed, not owned, so no allocation happens here.
pub struct ProcessCtx<'a> {
    /// Shared stereo bus. Phase 0 nodes read and write this in place.
    pub buffers: &'a mut AudioBuffers,
    pub sample_rate: f64,
    /// Frames to process. Mirrors `buffers.frames()`, kept for convenience.
    pub frames: usize,
    /// Transport state for this block, sampled once so all nodes agree.
    pub transport: TransportSnapshot,
}

impl<'a> ProcessCtx<'a> {
    /// Build a context, keeping `frames` and the buffer view in sync.
    pub fn new(
        buffers: &'a mut AudioBuffers,
        sample_rate: f64,
        frames: usize,
        transport: TransportSnapshot,
    ) -> Self {
        buffers.set_frames(frames);
        let frames = buffers.frames();
        Self {
            buffers,
            sample_rate,
            frames,
            transport,
        }
    }
}

/// A unit of audio processing living on the audio thread.
///
/// # ZERO-ALLOC RULE (hard invariant)
/// `process()` runs on the real-time audio callback. Inside it you MUST NOT:
/// - allocate or free memory (no `Vec::push` that can grow, no `Box`, no `String`,
///   no `format!`, no collection that reallocates, no `clone()` of owned data);
/// - lock a mutex or otherwise block (no `Mutex`, `RwLock`, channel `send`/`recv`);
/// - perform a syscall (no file, socket, or `println!`/`log::*`);
/// - panic (a panic across the host callback boundary is undefined behaviour);
/// - wait on another thread.
///
/// Allowed: arithmetic, atomics, `rtrb` push/pop, reads/writes of memory that was
/// allocated before processing started. Allocate everything in
/// [`RtNodeFactory::create`], which runs off the audio thread.
///
/// Violations are caught by `assert_no_alloc` around the callback in debug/test
/// builds (see the audio-io plugin) — but the rule holds regardless of tooling.
pub trait RtNode: Send {
    /// Process exactly `ctx.frames` frames. See the zero-alloc rule above.
    fn process(&mut self, ctx: &mut ProcessCtx);
}

/// Builds [`RtNode`]s off the audio thread.
///
/// Plugins contribute factories at kernel registration time; the graph builder
/// instantiates nodes when a [`GraphSpec`](crate::GraphSpec) is assembled — always
/// on a non-RT thread, which is where every allocation a node needs must happen.
pub trait RtNodeFactory: Send + Sync {
    /// Stable identifier for this node type, e.g. `"mydaw.test-tone"`.
    ///
    /// Not related to [`core::any::Any::type_id`]; this is a persistence/lookup
    /// key that must stay stable across builds because projects reference it.
    fn type_id(&self) -> &'static str;

    /// Instantiate a node. Runs on a non-RT thread: allocate freely here.
    fn create(&self, sample_rate: f64, max_block: usize) -> Box<dyn RtNode>;
}
