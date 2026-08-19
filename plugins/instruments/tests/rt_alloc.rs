//! Proves neither instrument allocates on the audio thread.
//!
//! The zero-alloc rule is the engine's load-bearing invariant, and reading the
//! code is not evidence: a `Vec` that never grows, an iterator that never boxes
//! and a trait object that never clones are properties of the *compiled*
//! program, not of anybody's intentions. So this installs a counting global
//! allocator and measures -- across many block sizes, with dense event traffic,
//! with voice stealing, with a choke, and with the voice-freeing paths taken.
//!
//! The counter is armed per-thread, so the harness (which allocates freely) is
//! not counted, and the instrument is proved to work before anything is
//! asserted with it -- a broken counter would pass every test in the file.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use mydaw_plugin_instruments::drum_kit::{CLOSED_HAT_KEY, KICK_KEY, OPEN_HAT_KEY, SNARE_KEY};
use mydaw_plugin_instruments::{DrumKitNode, PolySynthNode};
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

/// One block through a node, with events. Everything it touches was allocated
/// before the counter was armed.
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
fn the_drum_kit_allocates_nothing_across_event_heavy_blocks() {
    let mut node = DrumKitNode::new(48_000.0);
    let mut buffers = AudioBuffers::new(1_024);

    // Built before arming: an event slice is the host's, not the node's.
    let events: Vec<NoteEvent> = vec![
        NoteEvent::note_on(0, KICK_KEY, 1.0),
        NoteEvent::note_on(0, OPEN_HAT_KEY, 0.8),
        // A choke, in the middle of a block.
        NoteEvent::note_on(1, CLOSED_HAT_KEY, 0.9),
        NoteEvent::note_on(1, SNARE_KEY, 0.7),
        // A key the kit has no drum for, so the counting branch is taken too.
        NoteEvent::note_on(2, 60, 1.0),
        NoteEvent::note_off(2, KICK_KEY),
        NoteEvent::note_on(3, KICK_KEY, 0.5),
        NoteEvent::note_on(3, SNARE_KEY, 0.5),
    ];

    let count = count_allocations(|| {
        for size in BLOCK_SIZES {
            for pass in 0..8 {
                // Alternate loaded and empty blocks so both the "apply an
                // event" and the "voice ran out and freed itself" paths run.
                let block_events: &[NoteEvent] = if pass % 2 == 0 { &events } else { &[] };
                run_block(&mut node, &mut buffers, size, block_events);
            }
        }
    });

    assert_eq!(count, 0, "the drum kit allocated {count} times");
    assert!(node.unmapped_notes() > 0, "the diagnostic path really ran");
}

#[test]
fn the_poly_synth_allocates_nothing_across_event_heavy_blocks() {
    let mut node = PolySynthNode::new(48_000.0);
    let mut buffers = AudioBuffers::new(1_024);

    // Nine note-ons: the ninth steals a voice, which is the allocation-prone
    // path if voice allocation were ever done with a collection.
    let mut events: Vec<NoteEvent> = (0..9)
        .map(|i| NoteEvent::note_on(i, 60 + i as u8, 1.0))
        .collect();
    events.push(NoteEvent::note_on(9, 60, 0.5)); // retrigger
    events.push(NoteEvent::note_off(10, 61));

    let count = count_allocations(|| {
        for size in BLOCK_SIZES {
            for pass in 0..8 {
                let block_events: &[NoteEvent] = if pass % 2 == 0 { &events } else { &[] };
                run_block(&mut node, &mut buffers, size, block_events);
            }
        }
    });

    assert_eq!(count, 0, "the poly synth allocated {count} times");
    assert!(
        node.stolen_voices() > 0,
        "the voice-stealing path really ran"
    );
}

#[test]
fn a_long_run_through_both_instruments_allocates_nothing() {
    // The steady state: a note held for a second, then released, then silence.
    // Nothing may allocate when a voice frees itself, and nothing may allocate
    // in the empty-buffer path either.
    let mut drums = DrumKitNode::new(48_000.0);
    let mut synth = PolySynthNode::new(48_000.0);
    let mut buffers = AudioBuffers::new(512);

    let on = [NoteEvent::note_on(0, 69, 0.9)];
    let kick = [NoteEvent::note_on(0, KICK_KEY, 0.9)];
    let off = [NoteEvent::note_off(0, 69)];

    let count = count_allocations(|| {
        run_block(&mut synth, &mut buffers, 512, &on);
        run_block(&mut drums, &mut buffers, 512, &kick);
        // ~2 seconds of blocks with no events at all.
        for _ in 0..180 {
            run_block(&mut synth, &mut buffers, 512, &[]);
            run_block(&mut drums, &mut buffers, 512, &[]);
        }
        run_block(&mut synth, &mut buffers, 512, &off);
        for _ in 0..180 {
            run_block(&mut synth, &mut buffers, 512, &[]);
            run_block(&mut drums, &mut buffers, 512, &[]);
        }
    });

    assert_eq!(count, 0, "a steady-state run allocated {count} times");
    assert_eq!(synth.active_voices(), 0, "the note released");
    assert_eq!(drums.active_voices(), 0, "the kick finished");
}
