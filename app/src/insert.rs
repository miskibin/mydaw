//! A swappable insert point on the master bus.
//!
//! [`SourceSlot`](crate::source::SourceSlot) made the *head* of a track
//! swappable at node granularity. This is the same idea one node further down
//! the signal path, and it exists for the same reason: the master effect has to
//! be able to change -- and to be removed -- while the engine runs, and a
//! hosted CLAP plugin cannot be part of a graph rebuild without a gap.
//!
//! ```text
//!   tracks ─▶ Σ ─▶ InsertSlot ─▶ master fader ─▶ device
//!                      ▲
//!                      └── InsertHandle::install(Some(node) | None)
//!                          (rtrb + basedrop, off the audio thread)
//! ```
//!
//! # Why the slot is always in the graph
//! Step 5 pushed the CLAP effect into the master chain only when one was
//! configured, which made "add an effect" a graph rebuild -- the exact operation
//! a live CLAP node cannot survive. The slot is therefore *unconditional*: it is
//! compiled into every graph, and it is a pure pass-through when empty. One
//! branch and one `rtrb` pop per block buys the ability to insert, replace and
//! remove a plugin with no rebuild at all.
//!
//! # RT safety
//! [`InsertSlot::process`] pops from a wait-free queue, dereferences an
//! `Option`, and runs the node. No allocation, no locking, no syscalls. A
//! superseded node is dropped on the audio thread, which under `basedrop` means
//! *queued for the collector*, not freed.

use std::sync::Arc;

use mydaw_plugin_audio_io::AudioIoError;
use mydaw_rt_core::basedrop::{Handle, Owned};
use mydaw_rt_core::{ProcessCtx, RtNode};
use parking_lot::Mutex;
use rtrb::{Consumer, Producer};

use crate::source::SourceError;

/// In-flight insert swaps. As with the source queue, four is far more than a
/// human can produce between two audio blocks; it exists so that overflow is a
/// reported error rather than an unconsidered possibility.
pub const INSERT_QUEUE_CAPACITY: usize = 4;

/// What travels across the RT wall: a node, or the absence of one.
///
/// `None` is how "remove the master effect" is expressed. Sending it through the
/// same queue as an installation is what keeps the two operations ordered with
/// respect to each other.
type SlotContents = Option<Box<dyn RtNode>>;

/// Non-RT end of the insert channel, for one open stream.
struct InsertSender {
    tx: Producer<Owned<SlotContents>>,
    gc: Handle,
}

/// Audio-thread end: one insert point in the master chain.
pub struct InsertSlot {
    rx: Consumer<Owned<SlotContents>>,
    current: Option<Owned<SlotContents>>,
}

impl InsertSlot {
    /// Pick up the newest pending contents, keeping only the last of a burst.
    fn poll_swap(&mut self) {
        while let Ok(next) = self.rx.pop() {
            self.current = Some(next);
        }
    }
}

impl RtNode for InsertSlot {
    fn process(&mut self, ctx: &mut ProcessCtx) {
        self.poll_swap();
        // An empty slot is transparent: the bus keeps whatever the sum produced.
        // That is the difference between this and `SourceSlot`, which silences.
        if let Some(contents) = self.current.as_mut() {
            if let Some(node) = contents.as_mut() {
                node.process(ctx);
            }
        }
    }
}

/// The shell's handle on one insert point. `Send + Sync`.
///
/// Created once at boot and reused across stream restarts, exactly like
/// [`SourceHandle`](crate::source::SourceHandle).
pub struct InsertHandle {
    sender: Mutex<Option<InsertSender>>,
    /// Whether a node is installed, as last published. Read by the UI and by
    /// [`TrackHost`](crate::tracks::TrackHost) to answer "is there a master
    /// effect", with no RT-side query.
    occupied: Mutex<bool>,
}

impl Default for InsertHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl InsertHandle {
    /// An empty, detached handle.
    pub fn new() -> Self {
        Self {
            sender: Mutex::new(None),
            occupied: Mutex::new(false),
        }
    }

    /// Build the channel for a freshly opened stream and take its non-RT end.
    ///
    /// The slot starts **empty**: a graph rebuild cannot carry a live CLAP node
    /// across itself, so the caller re-installs one afterwards (see
    /// [`ClapHost::rebuild`](crate::clap_host::ClapHost::rebuild)).
    pub fn open(&self, gc: Handle) -> InsertSlot {
        let (tx, rx) = mydaw_rt_core::rt_channel::<Owned<SlotContents>>(INSERT_QUEUE_CAPACITY);
        let current = Some(Owned::new(&gc, None));
        *self.sender.lock() = Some(InsertSender { tx, gc });
        *self.occupied.lock() = false;
        InsertSlot { rx, current }
    }

    /// Forget the current stream's channel. Called when audio stops.
    pub fn close(&self) {
        *self.sender.lock() = None;
        *self.occupied.lock() = false;
    }

    /// Publish new contents to the audio thread. `None` empties the slot.
    ///
    /// # Errors
    /// [`AudioIoError::NotRunning`] when no stream is open, or
    /// [`SourceError::QueueFull`] when the audio thread is behind (retryable).
    pub fn install(&self, node: SlotContents) -> Result<(), SourceError> {
        let occupied = node.is_some();
        let mut guard = self.sender.lock();
        let sender = guard.as_mut().ok_or(AudioIoError::NotRunning)?;
        let owned = Owned::new(&sender.gc, node);
        sender.tx.push(owned).map_err(|_| {
            if sender.tx.is_abandoned() {
                SourceError::Audio(AudioIoError::NotRunning)
            } else {
                SourceError::QueueFull
            }
        })?;
        drop(guard);
        *self.occupied.lock() = occupied;
        Ok(())
    }

    /// Empty the slot. Shorthand for `install(None)`.
    ///
    /// # Errors
    /// See [`install`](Self::install).
    pub fn clear(&self) -> Result<(), SourceError> {
        self.install(None)
    }

    /// Whether a node is installed, as last published.
    pub fn is_occupied(&self) -> bool {
        *self.occupied.lock()
    }

    /// Whether a stream's channel is attached. Diagnostics and tests.
    pub fn is_open(&self) -> bool {
        self.sender.lock().is_some()
    }
}

impl std::fmt::Debug for InsertHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InsertHandle")
            .field("occupied", &self.is_occupied())
            .field("open", &self.is_open())
            .finish()
    }
}

/// An [`InsertSlot`] boxed for a [`GraphSpec`](mydaw_rt_core::GraphSpec).
pub fn insert_slot(handle: &Arc<InsertHandle>, gc: Handle) -> Box<dyn RtNode> {
    Box::new(handle.open(gc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_rt_core::basedrop::Collector;
    use mydaw_rt_core::{AudioBuffers, TransportSnapshot};

    /// Halving is exact in binary floating point, so a test can assert the
    /// insert ran by comparing against the dry signal rather than a tolerance.
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

    /// Render one block with `value` on the bus, returning what came out.
    fn render(slot: &mut InsertSlot, value: f32) -> f32 {
        let mut buffers = AudioBuffers::new(8);
        for channel in buffers.as_mut_slices() {
            channel.fill(value);
        }
        {
            let mut ctx = ProcessCtx::new(&mut buffers, 48_000.0, 8, TransportSnapshot::default());
            slot.process(&mut ctx);
        }
        buffers.as_slices()[0][0]
    }

    #[test]
    fn an_empty_slot_is_transparent_rather_than_silent() {
        // The difference from `SourceSlot`, and the reason the slot can sit in
        // every graph unconditionally: an empty insert must pass the sum through.
        let collector = Collector::new();
        let handle = InsertHandle::new();
        let mut slot = handle.open(collector.handle());
        assert!(!handle.is_occupied());
        assert_eq!(render(&mut slot, 0.4), 0.4);
    }

    #[test]
    fn installing_a_node_takes_effect_on_the_next_block_and_clearing_undoes_it() {
        let collector = Collector::new();
        let handle = InsertHandle::new();
        let mut slot = handle.open(collector.handle());

        handle.install(Some(Box::new(HalveNode))).expect("installs");
        assert!(handle.is_occupied());
        assert_eq!(render(&mut slot, 0.4), 0.2);

        handle.clear().expect("clears");
        assert!(!handle.is_occupied());
        assert_eq!(render(&mut slot, 0.4), 0.4, "the bus is untouched again");
    }

    #[test]
    fn a_burst_of_swaps_collapses_to_the_last_one() {
        let collector = Collector::new();
        let handle = InsertHandle::new();
        let mut slot = handle.open(collector.handle());

        handle.install(Some(Box::new(HalveNode))).unwrap();
        handle.clear().unwrap();
        handle.install(Some(Box::new(HalveNode))).unwrap();
        assert_eq!(render(&mut slot, 0.8), 0.4);
    }

    #[test]
    fn a_handle_with_no_stream_reports_it_instead_of_pretending() {
        let handle = InsertHandle::new();
        assert!(!handle.is_open());
        assert!(matches!(
            handle.install(Some(Box::new(HalveNode))),
            Err(SourceError::Audio(AudioIoError::NotRunning))
        ));
        assert!(!handle.is_occupied());
    }

    #[test]
    fn a_full_queue_is_reported_rather_than_dropped_silently() {
        let collector = Collector::new();
        let handle = InsertHandle::new();
        // Never polled, so the queue fills and stays full.
        let _slot = handle.open(collector.handle());
        for _ in 0..INSERT_QUEUE_CAPACITY {
            handle.install(Some(Box::new(HalveNode))).unwrap();
        }
        assert!(matches!(
            handle.install(Some(Box::new(HalveNode))),
            Err(SourceError::QueueFull)
        ));
    }

    #[test]
    fn a_superseded_insert_is_freed_by_the_collector_not_the_audio_thread() {
        let mut collector = Collector::new();
        let handle = InsertHandle::new();
        let mut slot = handle.open(collector.handle());
        assert_eq!(collector.alloc_count(), 1, "the initial empty contents");

        handle.install(Some(Box::new(HalveNode))).unwrap();
        assert_eq!(collector.alloc_count(), 2);
        render(&mut slot, 1.0);
        assert_eq!(
            collector.alloc_count(),
            2,
            "dropping on the audio thread must not deallocate"
        );
        collector.collect();
        assert_eq!(collector.alloc_count(), 1);

        handle.close();
        drop(slot);
        collector.collect();
        assert_eq!(collector.alloc_count(), 0);
    }

    #[test]
    fn reopening_a_handle_starts_empty_because_a_rebuild_cannot_carry_a_node() {
        // The rebuild protocol depends on this: a fresh graph's master insert is
        // empty, and `ClapHost` re-installs the effect once the old node has
        // been retired.
        let collector = Collector::new();
        let handle = InsertHandle::new();
        let mut first = handle.open(collector.handle());
        handle.install(Some(Box::new(HalveNode))).unwrap();
        assert_eq!(render(&mut first, 1.0), 0.5);

        let mut second = handle.open(collector.handle());
        assert!(!handle.is_occupied());
        assert_eq!(render(&mut second, 1.0), 1.0);
    }
}
