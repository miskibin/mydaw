//! Audio graph: built off-thread, handed to the audio thread lock-free.
//!
//! # Why not `ArcSwap<CompiledGraph>`
//! The obvious "publish a new graph atomically" answer is `ArcSwap`, but
//! [`RtNode::process`] needs `&mut self`: nodes hold filter state, oscillator
//! phase, delay lines. An `Arc` only ever yields shared access, so the audio
//! thread could never call into the graph without a lock or interior mutability.
//!
//! So ownership moves instead of being shared: the audio thread **owns** the
//! current graph, and new graphs arrive by value through an `rtrb` queue as
//! [`basedrop::Owned`]. When the audio thread drops the superseded graph, the
//! `Owned` wrapper hands the allocation to a [`basedrop::Collector`], which frees
//! it later on the GC thread -- so no `free()` ever runs in the callback.
//!
//! ```text
//!  non-RT thread                       audio thread
//!  ------------                        ------------
//!  GraphSpec --compile--> Owned<CompiledGraph>
//!                    -- rtrb push --> rtrb pop --> RtGraphSlot::current
//!                                                    |  (old graph dropped)
//!  GcThread <-- basedrop collector queue <------------+
//! ```

use basedrop::{Handle, Owned};
use rtrb::{Consumer, Producer};

use crate::{ProcessCtx, RtNode};

/// A graph description assembled on a non-RT thread.
///
/// Phase 0 is a plain serial chain -- nodes run in insertion order over one
/// shared stereo bus. Real routing (fan-in/out, per-track buses, latency
/// compensation) is Phase 2+; the `GraphSpec` -> `CompiledGraph` seam exists so
/// that change stays contained.
#[derive(Default)]
pub struct GraphSpec {
    nodes: Vec<Box<dyn RtNode>>,
}

impl GraphSpec {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a node to the chain.
    pub fn push(&mut self, node: Box<dyn RtNode>) -> &mut Self {
        self.nodes.push(node);
        self
    }

    /// Builder-style append.
    #[must_use]
    pub fn with(mut self, node: Box<dyn RtNode>) -> Self {
        self.nodes.push(node);
        self
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Freeze the spec into something the audio thread can run.
    ///
    /// Non-RT: this is the last point at which allocation is allowed.
    pub fn compile(self) -> CompiledGraph {
        CompiledGraph { nodes: self.nodes }
    }
}

/// An executable graph. Owned by the audio thread while it is current.
pub struct CompiledGraph {
    nodes: Vec<Box<dyn RtNode>>,
}

impl CompiledGraph {
    /// Run every node in order over the shared bus.
    ///
    /// RT-safe as long as each node honours the zero-alloc rule; this function
    /// itself only iterates a pre-allocated `Vec`.
    pub fn process(&mut self, ctx: &mut ProcessCtx) {
        for node in &mut self.nodes {
            node.process(ctx);
        }
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }
}

/// Failure modes of [`GraphHandle::install`].
#[derive(Debug, thiserror::Error)]
pub enum GraphInstallError {
    /// The audio thread has not consumed previous graphs yet. The caller should
    /// retry; the rejected graph is dropped here, on the non-RT thread.
    #[error("graph queue is full; the audio thread has not picked up previous graphs")]
    QueueFull,
    /// The audio thread went away (queue consumer dropped).
    #[error("audio thread is gone; cannot install graph")]
    Disconnected,
}

/// Non-RT side of the graph channel.
pub struct GraphHandle {
    tx: Producer<Owned<CompiledGraph>>,
    gc: Handle,
}

impl GraphHandle {
    /// Compile `spec` and hand the result to the audio thread.
    ///
    /// All allocation (node construction, the `Owned` allocation) happens here,
    /// on the calling non-RT thread.
    pub fn install(&mut self, spec: GraphSpec) -> Result<(), GraphInstallError> {
        let graph = Owned::new(&self.gc, spec.compile());
        // On failure the rejected graph is returned inside the error and dropped
        // right here -- deliberately on this thread, never on the audio thread.
        self.tx.push(graph).map_err(|_| {
            if self.tx.is_abandoned() {
                GraphInstallError::Disconnected
            } else {
                GraphInstallError::QueueFull
            }
        })
    }

    /// Collector handle, for code that needs to allocate other deferred-drop data.
    pub fn gc_handle(&self) -> Handle {
        self.gc.clone()
    }
}

/// Audio-thread side of the graph channel: owns the current graph.
pub struct RtGraphSlot {
    rx: Consumer<Owned<CompiledGraph>>,
    current: Option<Owned<CompiledGraph>>,
}

impl RtGraphSlot {
    /// Pick up the newest pending graph, if any. Wait-free; call once per block
    /// *before* [`process`](Self::process).
    ///
    /// Drains the whole queue and keeps only the last graph, so a burst of edits
    /// costs one block, not one block each. Superseded graphs are dropped here,
    /// which only enqueues them on the collector -- no `free()` on this thread.
    ///
    /// Returns `true` if the current graph was replaced.
    pub fn poll_swap(&mut self) -> bool {
        let mut swapped = false;
        while let Ok(next) = self.rx.pop() {
            // Assigning drops the previous `Owned`, which pushes its allocation
            // onto the collector's queue (an atomic list push -- no dealloc).
            self.current = Some(next);
            swapped = true;
        }
        swapped
    }

    /// Run the current graph. Outputs silence when no graph is installed yet.
    pub fn process(&mut self, ctx: &mut ProcessCtx) {
        match self.current.as_mut() {
            Some(graph) => graph.process(ctx),
            None => ctx.buffers.silence(),
        }
    }

    pub fn has_graph(&self) -> bool {
        self.current.is_some()
    }

    /// Node count of the current graph. Diagnostics/tests.
    pub fn node_count(&self) -> usize {
        self.current.as_ref().map_or(0, |g| g.node_count())
    }
}

/// Create a graph channel. `capacity` is the number of in-flight graphs.
///
/// `gc` comes from a [`basedrop::Collector`] -- in the app, from
/// [`GcThread::handle`](crate::GcThread::handle).
pub fn graph_channel(capacity: usize, gc: Handle) -> (GraphHandle, RtGraphSlot) {
    let (tx, rx) = crate::rt_channel::<Owned<CompiledGraph>>(capacity);
    (GraphHandle { tx, gc }, RtGraphSlot { rx, current: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AudioBuffers, TransportSnapshot};
    use basedrop::Collector;

    /// Writes a constant into both channels so tests can tell graphs apart.
    struct ConstNode(f32);
    impl RtNode for ConstNode {
        fn process(&mut self, ctx: &mut ProcessCtx) {
            for ch in ctx.buffers.as_mut_slices() {
                ch.fill(self.0);
            }
        }
    }

    /// Adds a constant, to verify serial ordering / accumulation.
    struct AddNode(f32);
    impl RtNode for AddNode {
        fn process(&mut self, ctx: &mut ProcessCtx) {
            for ch in ctx.buffers.as_mut_slices() {
                for s in ch.iter_mut() {
                    *s += self.0;
                }
            }
        }
    }

    fn render(slot: &mut RtGraphSlot, buffers: &mut AudioBuffers, frames: usize) -> f32 {
        let mut ctx = ProcessCtx::new(buffers, 48_000.0, frames, TransportSnapshot::default());
        slot.process(&mut ctx);
        buffers.as_slices()[0][0]
    }

    #[test]
    fn silence_before_any_graph_is_installed() {
        let collector = Collector::new();
        let (_h, mut slot) = graph_channel(2, collector.handle());
        let mut buffers = AudioBuffers::new(16);
        for ch in buffers.as_mut_slices() {
            ch.fill(0.5);
        }
        assert!(!slot.has_graph());
        assert_eq!(render(&mut slot, &mut buffers, 16), 0.0);
    }

    #[test]
    fn graph_swap_delivers_new_graph_to_rt_side() {
        let collector = Collector::new();
        let (mut handle, mut slot) = graph_channel(4, collector.handle());
        let mut buffers = AudioBuffers::new(16);

        handle
            .install(GraphSpec::new().with(Box::new(ConstNode(0.25))))
            .unwrap();

        // Simulated audio thread: poll, then process.
        assert!(slot.poll_swap());
        assert!(slot.has_graph());
        assert_eq!(render(&mut slot, &mut buffers, 16), 0.25);

        // Nothing pending -> no swap, previous graph keeps running.
        assert!(!slot.poll_swap());
        assert_eq!(render(&mut slot, &mut buffers, 16), 0.25);

        handle
            .install(GraphSpec::new().with(Box::new(ConstNode(-1.0))))
            .unwrap();
        assert!(slot.poll_swap());
        assert_eq!(render(&mut slot, &mut buffers, 16), -1.0);
    }

    #[test]
    fn serial_chain_runs_in_order() {
        let collector = Collector::new();
        let (mut handle, mut slot) = graph_channel(2, collector.handle());
        let mut buffers = AudioBuffers::new(8);

        let mut spec = GraphSpec::new();
        spec.push(Box::new(ConstNode(1.0)));
        spec.push(Box::new(AddNode(0.5)));
        spec.push(Box::new(AddNode(0.25)));
        assert_eq!(spec.len(), 3);
        handle.install(spec).unwrap();

        slot.poll_swap();
        assert_eq!(slot.node_count(), 3);
        assert_eq!(render(&mut slot, &mut buffers, 8), 1.75);
    }

    #[test]
    fn burst_of_installs_collapses_to_the_newest_graph() {
        let collector = Collector::new();
        let (mut handle, mut slot) = graph_channel(8, collector.handle());
        let mut buffers = AudioBuffers::new(8);
        for v in [0.1_f32, 0.2, 0.3] {
            handle
                .install(GraphSpec::new().with(Box::new(ConstNode(v))))
                .unwrap();
        }
        assert!(slot.poll_swap());
        assert_eq!(render(&mut slot, &mut buffers, 8), 0.3);
    }

    #[test]
    fn full_queue_is_reported_not_silently_dropped() {
        let collector = Collector::new();
        let (mut handle, _slot) = graph_channel(1, collector.handle());
        handle.install(GraphSpec::new()).unwrap();
        let err = handle.install(GraphSpec::new()).unwrap_err();
        assert!(matches!(err, GraphInstallError::QueueFull), "got {err:?}");
    }

    /// The point of `basedrop`: dropping a graph on the "audio thread" must not
    /// free anything -- the allocation stays alive until the collector runs.
    #[test]
    fn superseded_graph_is_freed_by_the_collector_not_the_rt_thread() {
        let mut collector = Collector::new();
        let (mut handle, mut slot) = graph_channel(4, collector.handle());

        handle
            .install(GraphSpec::new().with(Box::new(ConstNode(1.0))))
            .unwrap();
        handle
            .install(GraphSpec::new().with(Box::new(ConstNode(2.0))))
            .unwrap();
        assert_eq!(collector.alloc_count(), 2, "two graphs allocated");

        // Audio thread picks up both; the first is dropped -> handed to collector.
        slot.poll_swap();
        assert_eq!(
            collector.alloc_count(),
            2,
            "dropping on the RT thread must NOT deallocate"
        );

        collector.collect();
        assert_eq!(collector.alloc_count(), 1, "collector freed the old graph");

        // Dropping the RT slot releases the last graph, freed on the next collect.
        drop(slot);
        collector.collect();
        assert_eq!(collector.alloc_count(), 0);
    }
}
