//! Proves [`ClapNode::process`] does not allocate.
//!
//! The zero-alloc rule on [`RtNode`] is the load-bearing invariant of the whole
//! engine, and "I read the code and it looks fine" is not evidence -- `clack`
//! builds its CLAP port descriptor tables inside `with_input_buffers`, and
//! whether that reallocates depends on a capacity reserved somewhere else
//! entirely. So this test installs a counting global allocator and *measures*.
//!
//! The counter is armed per-thread, so the rest of the test binary (and Rust's
//! own test harness, which allocates freely) is not counted.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use mydaw_fixture_clap_gain::PLUGIN_ID;
use mydaw_plugin_host_clap::hosting::LoadedClapPlugin;
use mydaw_plugin_host_clap::test_util::{missing_fixture_message, stage_fixture_bundle};
use mydaw_rt_core::basedrop::Collector;
use mydaw_rt_core::{
    graph_channel, AudioBuffers, GraphSpec, ProcessCtx, RtNode, TransportSnapshot,
};

thread_local! {
    /// Allocations on this thread while armed. `Cell<u64>`, not an atomic:
    /// thread-local by construction, so no synchronisation is needed.
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
    /// Whether this thread is counting. Off by default, so other test threads
    /// (and the harness) are unaffected.
    static ARMED: Cell<bool> = const { Cell::new(false) };
}

/// `System`, plus a per-thread counter of allocation and reallocation events.
struct CountingAllocator;

// SAFETY: every method forwards to `System`, which upholds the `GlobalAlloc`
// contract; the counter is a side effect that touches no allocator state.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record();
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // Deallocation is counted too: freeing on the audio thread is just as
        // forbidden as allocating, and it is the easier mistake to make.
        record();
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record();
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record();
        unsafe { System.alloc_zeroed(layout) }
    }
}

/// Bump the counter, but only on an armed thread, and never re-entrantly.
fn record() {
    // `try_with`: during thread teardown the TLS may already be destroyed, and
    // panicking inside the allocator would abort the process.
    let _ = ARMED.try_with(|armed| {
        if armed.get() {
            let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        }
    });
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Run `body` with the counter armed, returning how many allocator events it
/// caused. `body` must not itself allocate for bookkeeping.
fn count_allocations(body: impl FnOnce()) -> u64 {
    ALLOCATIONS.with(|count| count.set(0));
    ARMED.with(|armed| armed.set(true));
    body();
    ARMED.with(|armed| armed.set(false));
    ALLOCATIONS.with(Cell::get)
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mydaw-clap-alloc-{tag}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("scratch dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_counting_allocator_actually_counts() {
    // A meaningless "0 allocations" result would silently pass every test below,
    // so first prove the instrument works.
    let observed = count_allocations(|| {
        let victim: Vec<u8> = Vec::with_capacity(64);
        std::hint::black_box(&victim);
    });
    assert!(observed > 0, "the allocator hook never fired");

    let quiet = count_allocations(|| {
        let mut total = 0u64;
        for i in 0..1000u64 {
            total = total.wrapping_add(i);
        }
        std::hint::black_box(total);
    });
    assert_eq!(quiet, 0, "pure arithmetic must not allocate");
}

#[test]
fn processing_a_clap_node_allocates_nothing() {
    let dir = TempDir::new("process");
    let Some(bundle) = stage_fixture_bundle(dir.path()) else {
        eprintln!("{}", missing_fixture_message());
        return;
    };

    const MAX_BLOCK: usize = 512;
    let mut plugin = LoadedClapPlugin::load(&bundle, PLUGIN_ID).expect("load");
    let mut node = plugin.create_node(48_000.0, MAX_BLOCK).expect("activate");
    let mut buffers = AudioBuffers::new(MAX_BLOCK);
    for channel in buffers.as_mut_slices() {
        channel.fill(0.25);
    }

    // First block outside the measurement: it is where any lazily-initialised
    // state inside the plugin or clack would appear, and the invariant we care
    // about is the steady state the audio callback actually runs in.
    {
        let mut ctx = ProcessCtx::new(
            &mut buffers,
            48_000.0,
            MAX_BLOCK,
            TransportSnapshot::default(),
        );
        node.process(&mut ctx);
    }

    let observed = count_allocations(|| {
        for block in 0..64 {
            // Vary the block size: `with_input_buffers` rebuilds its descriptor
            // table every call, and a size change is where a naive implementation
            // would grow a Vec.
            let frames = 1 + (block * 7) % MAX_BLOCK;
            let mut ctx =
                ProcessCtx::new(&mut buffers, 48_000.0, frames, TransportSnapshot::default());
            node.process(&mut ctx);
        }
    });

    assert_eq!(
        observed, 0,
        "ClapNode::process allocated {observed} time(s) over 64 blocks; the audio thread \
         must never touch the allocator"
    );
    assert_eq!(node.stats().process_calls(), 65);
    assert_eq!(node.stats().process_errors(), 0);
}

#[test]
fn the_whole_graph_path_with_a_clap_node_in_it_allocates_nothing() {
    // Same claim, one level up: what the audio callback actually calls is
    // `RtGraphSlot::poll_swap` + `RtGraphSlot::process`, not `ClapNode::process`
    // directly. Measuring the node alone would miss an allocation in the graph
    // plumbing around it.
    let dir = TempDir::new("graph");
    let Some(bundle) = stage_fixture_bundle(dir.path()) else {
        eprintln!("{}", missing_fixture_message());
        return;
    };

    const BLOCK: usize = 128;
    let mut plugin = LoadedClapPlugin::load(&bundle, PLUGIN_ID).expect("load");
    let node = plugin.create_node(48_000.0, BLOCK).expect("activate");

    let mut collector = Collector::new();
    let (mut handle, mut slot) = graph_channel(2, collector.handle());
    handle
        .install(GraphSpec::new().with(Box::new(node)))
        .expect("install");

    let mut buffers = AudioBuffers::new(BLOCK);
    for channel in buffers.as_mut_slices() {
        channel.fill(0.5);
    }
    assert!(slot.poll_swap());
    {
        let mut ctx = ProcessCtx::new(&mut buffers, 48_000.0, BLOCK, TransportSnapshot::default());
        slot.process(&mut ctx);
    }

    let observed = count_allocations(|| {
        for _ in 0..32 {
            // Refilling is part of the measurement on purpose: `fill` on a
            // pre-allocated slice must not allocate either, and it keeps the
            // input identical every block so the final value is exact.
            for channel in buffers.as_mut_slices() {
                channel.fill(0.5);
            }
            // `poll_swap` on an empty queue is part of every callback too.
            slot.poll_swap();
            let mut ctx =
                ProcessCtx::new(&mut buffers, 48_000.0, BLOCK, TransportSnapshot::default());
            slot.process(&mut ctx);
        }
    });
    assert_eq!(
        observed, 0,
        "the graph path allocated {observed} time(s) over 32 blocks"
    );
    assert!(buffers
        .as_slices()
        .iter()
        .all(|channel| channel.iter().all(|s| (*s - 0.25).abs() < 1e-7)));

    drop(slot);
    drop(handle);
    collector.collect();
    assert!(plugin.shutdown());
}
