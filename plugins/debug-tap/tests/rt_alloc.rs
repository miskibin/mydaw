//! Proves the audio thread's half of the debug tap does not allocate.
//!
//! The zero-alloc rule is the load-bearing invariant of the engine, and reading
//! the code is not evidence: whether `rtrb`'s chunk API and the interleaving
//! iterator stay allocation-free is a property of their implementations, not of
//! our intentions. So this test installs a counting global allocator and
//! *measures* -- armed and disarmed, across many block sizes.
//!
//! The counter is armed per-thread, so the rest of the binary (and the test
//! harness, which allocates freely) is not counted. It first proves the
//! instrument works, because a broken counter would pass everything.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use mydaw_plugin_debug_tap::debug_tap_channel;

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
fn a_disarmed_tap_allocates_nothing() {
    let (mut rt, _consumer) = debug_tap_channel(1 << 14);
    let left = vec![0.25_f32; 512];
    let right = vec![-0.25_f32; 512];

    let events = count_allocations(|| {
        for _ in 0..256 {
            rt.push_block(&left, &right);
        }
    });
    assert_eq!(events, 0, "a disarmed tap must be free");
}

#[test]
fn an_armed_tap_allocates_nothing_across_varying_block_sizes() {
    // Large enough that the ring never fills during the measured run.
    let (mut rt, consumer) = debug_tap_channel(1 << 16);
    consumer.shared().arm();

    let left = vec![0.25_f32; 4_096];
    let right = vec![-0.25_f32; 4_096];

    let events = count_allocations(|| {
        // A spread of block sizes, including the awkward ones a device can ask
        // for. 64 blocks averaging ~500 frames stays well inside the ring.
        for size in [1_usize, 2, 3, 7, 64, 96, 128, 129, 256, 480, 512, 1_024] {
            for _ in 0..4 {
                rt.push_block(&left[..size], &right[..size]);
            }
        }
    });
    assert_eq!(events, 0, "the armed push path allocated {events} times");

    // And it really did copy something -- a no-op would also allocate nothing.
    assert!(consumer.available() > 0);
}

#[test]
fn overflowing_the_ring_allocates_nothing_either() {
    // Deliberately tiny: every push after the first few is a pure drop-count.
    let (mut rt, consumer) = debug_tap_channel(64);
    consumer.shared().arm();

    let left = vec![0.5_f32; 512];
    let right = vec![0.5_f32; 512];

    let events = count_allocations(|| {
        for _ in 0..64 {
            rt.push_block(&left, &right);
        }
    });
    assert_eq!(events, 0, "the overflow path allocated {events} times");
    assert!(rt.dropped() > 0, "the ring should have overflowed");
}
