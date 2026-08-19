//! Proves the sampler allocates nothing on the audio thread.
//!
//! Same instrument as `mydaw-plugin-instruments`' own `rt_alloc.rs`, and for
//! the same reason: the zero-alloc rule is the engine's load-bearing invariant,
//! and reading the code is not evidence. A `Vec` that never grows, an `Arc` that
//! is never cloned and a trait object that never boxes are properties of the
//! *compiled* program.
//!
//! The sampler has one hazard the synthesised instruments do not: it holds an
//! `Arc<SampleBank>`, and an `Arc::clone` on the audio thread would be a pair of
//! atomic read-modify-writes rather than an allocation -- invisible to this
//! counter. The `Arc` is therefore borrowed, never cloned, in
//! [`SamplerNode::process`]; what this file catches is the other half, the
//! allocation that a lookup, a voice list or an error path might smuggle in.
//!
//! No assets: the instrument is synthesised into a temp directory before the
//! counter is armed.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;

use mydaw_plugin_sampler::test_support::{sine, InstrumentBuilder};
use mydaw_plugin_sampler::{SampleBank, SamplerNode, VOICES};
use mydaw_rt_core::{AudioBuffers, NoteEvent, ProcessCtx, RtNode, TransportSnapshot};

thread_local! {
    /// Allocator events on this thread while armed. `Cell`, not an atomic:
    /// thread-local by construction.
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
    /// Whether this thread is counting. Off by default.
    static ARMED: Cell<bool> = const { Cell::new(false) };
}

/// `System`, plus a per-thread counter of allocation events.
struct CountingAllocator;

// SAFETY: every method forwards to `System`, which upholds the `GlobalAlloc`
// contract; the counter is a side effect that touches no allocator state.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record();
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // Freeing on the audio thread is just as forbidden as allocating, and
        // it is the easier mistake to make.
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
/// caused.
fn count_allocations(body: impl FnOnce()) -> u64 {
    ALLOCATIONS.with(|count| count.set(0));
    ARMED.with(|armed| armed.set(true));
    body();
    ARMED.with(|armed| armed.set(false));
    ALLOCATIONS.with(Cell::get)
}

/// The block sizes a real device asks for, plus the awkward ones it should not.
const BLOCK_SIZES: [usize; 12] = [1, 2, 3, 7, 64, 96, 128, 129, 256, 480, 512, 1_024];

fn run_block(
    node: &mut dyn RtNode,
    buffers: &mut AudioBuffers,
    frames: usize,
    events: &[NoteEvent],
) {
    let mut ctx = ProcessCtx::with_events(
        buffers,
        48_000.0,
        frames,
        TransportSnapshot::default(),
        events,
    );
    node.process(&mut ctx);
}

/// A two-layer instrument spanning the keyboard, so layer selection, key
/// lookup and the "no sample here" branch are all reachable.
fn fixture() -> (mydaw_plugin_sampler::test_support::TempDir, Arc<SampleBank>) {
    InstrumentBuilder::new("rt-alloc")
        .release_ms(80.0)
        .sample(
            "soft.wav",
            60,
            36,
            96,
            1,
            63,
            48_000,
            sine(220.0, 48_000.0, 96_000),
        )
        .sample(
            "loud.wav",
            60,
            36,
            96,
            64,
            127,
            48_000,
            sine(440.0, 48_000.0, 96_000),
        )
        .build()
}

#[test]
fn the_allocation_counter_actually_counts() {
    let allocating = count_allocations(|| {
        let buffer: Vec<f32> = Vec::with_capacity(1_024);
        std::hint::black_box(&buffer);
    });
    assert!(
        allocating >= 1,
        "the instrument is broken: an allocation was not counted"
    );

    let arithmetic = count_allocations(|| {
        let mut total = 0.0_f32;
        for i in 0..1_000 {
            total += i as f32;
        }
        std::hint::black_box(total);
    });
    assert_eq!(arithmetic, 0, "pure arithmetic must not be counted");
}

#[test]
fn the_sampler_allocates_nothing_across_event_heavy_blocks() {
    let (_dir, bank) = fixture();
    let mut node = SamplerNode::new(bank, 48_000.0);
    let mut buffers = AudioBuffers::new(1_024);

    // Built before arming: an event slice is the host's, not the node's.
    let events: Vec<NoteEvent> = vec![
        // Both velocity layers, so the lookup takes both branches.
        NoteEvent::note_on(0, 60, 0.2),
        NoteEvent::note_on(0, 64, 0.9),
        // A retrigger, which chokes a sounding voice.
        NoteEvent::note_on(1, 60, 1.0),
        // A key the instrument does not cover, so the counting branch runs too.
        NoteEvent::note_on(2, 12, 1.0),
        NoteEvent::note_off(3, 64),
        NoteEvent::note_on(4, 67, 0.5),
    ];

    let count = count_allocations(|| {
        for size in BLOCK_SIZES {
            for pass in 0..8 {
                // Alternate loaded and empty blocks so both the "apply an
                // event" and the "voice freed itself" paths run.
                let block_events: &[NoteEvent] = if pass % 2 == 0 { &events } else { &[] };
                run_block(&mut node, &mut buffers, size, block_events);
            }
        }
    });

    assert_eq!(count, 0, "the sampler allocated {count} times");
    assert!(node.unmapped_notes() > 0, "the diagnostic path really ran");
}

/// The path most likely to allocate if voices were ever a collection: every
/// voice busy, and more notes arriving.
#[test]
fn the_sampler_allocates_nothing_while_stealing_voices_under_full_polyphony() {
    let (_dir, bank) = fixture();
    let mut node = SamplerNode::new(bank, 48_000.0);
    let mut buffers = AudioBuffers::new(512);

    // More notes than there are voices, spread across the block so each lands
    // at its own offset.
    let events: Vec<NoteEvent> = (0..(VOICES + 8))
        .map(|index| {
            NoteEvent::note_on(
                (index * 8) as u32,
                40 + (index as u8 % 48),
                if index % 2 == 0 { 0.3 } else { 1.0 },
            )
        })
        .collect();

    let count = count_allocations(|| {
        for _ in 0..16 {
            run_block(&mut node, &mut buffers, 512, &events);
            run_block(&mut node, &mut buffers, 512, &[]);
        }
    });

    assert_eq!(count, 0, "voice stealing allocated {count} times");
    assert!(node.stolen_voices() > 0, "the stealing path really ran");
    assert_eq!(node.active_voices(), VOICES, "every voice is busy");
}

/// The steady state: a chord held for a second, released, then silence. Nothing
/// may allocate when a voice frees itself, nor in the empty-buffer path.
#[test]
fn a_long_run_through_the_sampler_allocates_nothing() {
    let (_dir, bank) = fixture();
    let mut node = SamplerNode::new(bank, 48_000.0);
    let mut buffers = AudioBuffers::new(512);

    let on: Vec<NoteEvent> = [60_u8, 64, 67]
        .iter()
        .map(|key| NoteEvent::note_on(0, *key, 0.9))
        .collect();
    let off: Vec<NoteEvent> = [60_u8, 64, 67]
        .iter()
        .map(|key| NoteEvent::note_off(0, *key))
        .collect();

    let count = count_allocations(|| {
        run_block(&mut node, &mut buffers, 512, &on);
        for _ in 0..90 {
            run_block(&mut node, &mut buffers, 512, &[]);
        }
        run_block(&mut node, &mut buffers, 512, &off);
        for _ in 0..90 {
            run_block(&mut node, &mut buffers, 512, &[]);
        }
    });

    assert_eq!(count, 0, "a steady-state run allocated {count} times");
    assert_eq!(node.active_voices(), 0, "the chord released");
}

/// Building a node is *not* on the audio thread and is allowed to allocate --
/// but it must not decode anything, or changing a track's instrument would
/// stall the UI for seconds. One `Arc` clone and one voice array, nothing that
/// scales with the size of the bank.
#[test]
fn building_a_node_shares_the_bank_rather_than_copying_it() {
    let (_dir, bank) = fixture();
    let before = Arc::strong_count(&bank);
    let node = SamplerNode::new(Arc::clone(&bank), 48_000.0);
    assert_eq!(
        Arc::strong_count(&bank),
        before + 1,
        "a node must share the bank, not clone its samples"
    );
    assert!(Arc::ptr_eq(node.bank(), &bank));
}
