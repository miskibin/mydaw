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

use mydaw_fixture_clap_gain::synth::PLUGIN_ID as SYNTH_ID;
use mydaw_fixture_clap_gain::PLUGIN_ID;
use mydaw_plugin_host_clap::hosting::LoadedClapPlugin;
use mydaw_plugin_host_clap::test_util::{missing_fixture_message, stage_fixture_bundle};
use mydaw_rt_core::basedrop::Collector;
use mydaw_rt_core::{
    drain_notes, graph_channel, note_channel, AudioBuffers, GraphSpec, NoteEvent, NoteEventBuffer,
    ProcessCtx, RtNode, TransportSnapshot,
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

#[test]
fn dispatching_note_events_to_an_instrument_allocates_nothing() {
    // The claim that matters for step 2. Translating a `NoteEvent` into CLAP's
    // wire format goes through `clack`'s `EventBuffer`, which is a `Vec`
    // underneath and *will* reallocate if the capacity reserved at activation
    // turns out to be wrong. Reading the code is not evidence; this measures.
    let dir = TempDir::new("events");
    let Some(bundle) = stage_fixture_bundle(dir.path()) else {
        eprintln!("{}", missing_fixture_message());
        return;
    };

    const MAX_BLOCK: usize = 512;
    let mut plugin = LoadedClapPlugin::load(&bundle, SYNTH_ID).expect("load the synth fixture");
    assert!(plugin.port_layout().has_note_input());
    let mut node = plugin.create_node(48_000.0, MAX_BLOCK).expect("activate");

    let mut buffers = AudioBuffers::new(MAX_BLOCK);
    buffers.silence();

    // Warm-up block, outside the measurement, carrying an event: whatever the
    // plugin or clack initialises lazily happens here rather than in the
    // steady state the audio callback actually runs in.
    {
        let events = [NoteEvent::note_on(0, 60, 1.0)];
        let mut ctx = ProcessCtx::with_events(
            &mut buffers,
            48_000.0,
            MAX_BLOCK,
            TransportSnapshot::default(),
            &events,
        );
        node.process(&mut ctx);
    }

    // A chord's worth of events per block, at varying offsets and block sizes,
    // alternating note-on and note-off so both translation paths are measured.
    let mut event_buffer = NoteEventBuffer::new(64);
    let observed = count_allocations(|| {
        for block in 0..64_usize {
            let frames = 1 + (block * 7) % MAX_BLOCK;
            event_buffer.clear();
            for voice in 0..8_u8 {
                let key = 60 + voice;
                let offset = ((block + voice as usize) % frames) as u32;
                let pushed = if block % 2 == 0 {
                    event_buffer.push(NoteEvent::note_on(offset, key, 0.75))
                } else {
                    event_buffer.push(NoteEvent::note_off(offset, key))
                };
                assert!(pushed);
            }
            let mut ctx = ProcessCtx::with_events(
                &mut buffers,
                48_000.0,
                frames,
                TransportSnapshot::default(),
                event_buffer.as_slice(),
            );
            node.process(&mut ctx);
        }
    });

    assert_eq!(
        observed, 0,
        "note dispatch allocated {observed} time(s) over 64 event-carrying blocks; \
         the audio thread must never touch the allocator"
    );
    assert_eq!(node.stats().process_calls(), 65);
    assert_eq!(node.stats().process_errors(), 0);
    assert_eq!(node.stats().events_sent(), 8 * 64 + 1);
    assert_eq!(node.stats().events_dropped(), 0);

    drop(node);
    assert!(plugin.shutdown());
}

#[test]
fn draining_the_note_queue_into_the_block_buffer_allocates_nothing() {
    // The other half of the RT note path, and the one that runs on every block
    // whether or not a plugin is loaded: queue -> per-block buffer.
    let (mut tx, mut rx) = note_channel(256);
    let mut buffer = NoteEventBuffer::new(256);

    // Warm both sides before measuring: `note_channel` and `NoteEventBuffer`
    // allocate exactly once, here.
    tx.push(mydaw_rt_core::LiveNote::note_on(60, 1.0)).unwrap();
    drain_notes(&mut rx, &mut buffer, 0);
    buffer.clear();

    let observed = count_allocations(|| {
        for block in 0..64_u8 {
            for key in 60..72_u8 {
                let _ = tx.push(mydaw_rt_core::LiveNote::note_on(key, 1.0));
            }
            buffer.clear();
            let dropped = drain_notes(&mut rx, &mut buffer, u32::from(block));
            std::hint::black_box(dropped);
        }
    });

    assert_eq!(
        observed, 0,
        "the note queue drain allocated {observed} time(s) over 64 blocks"
    );
    assert!(buffer.is_sorted());
}
