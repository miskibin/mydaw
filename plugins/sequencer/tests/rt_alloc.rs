//! Proves the scheduler and the pattern slot allocate nothing on the audio
//! thread.
//!
//! The scheduler runs *before* the graph, on the same callback, under the same
//! rule (see [`RtNode`](mydaw_rt_core::RtNode)'s zero-alloc invariant). Reading
//! the code is not evidence: a fixed array is only fixed until somebody reaches
//! for a `Vec`, and `PatternSlot::poll_swap` drops a `basedrop::Owned` on this
//! very thread -- the whole point of which is that dropping it must *not* free
//! anything.
//!
//! Same harness as `plugins/instruments/tests/rt_alloc.rs`: a counting global
//! allocator armed per-thread, so the test's own setup is not counted, and the
//! counter is proved to work before anything is asserted with it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use mydaw_plugin_sequencer::{
    pattern_channel, CompiledPattern, MelodicNote, Pattern, PatternRow, Scheduler, Step,
    TransportWindow, MAX_NOTES, MAX_ROWS, MAX_STEPS,
};
use mydaw_rt_core::basedrop::Collector;
use mydaw_rt_core::{NoteEventBuffer, DEFAULT_BPM};

thread_local! {
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
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
        // for `basedrop` it is the whole property under test.
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

fn count_allocations(body: impl FnOnce()) -> u64 {
    ALLOCATIONS.with(|count| count.set(0));
    ARMED.with(|armed| armed.set(true));
    body();
    ARMED.with(|armed| armed.set(false));
    ALLOCATIONS.with(Cell::get)
}

/// The block sizes a real device asks for, plus the awkward ones it should not.
const BLOCK_SIZES: [usize; 8] = [1, 32, 64, 127, 256, 441, 512, 4_096];

/// Every row hitting every step of the longest pattern, plus the full thousand
/// melodic notes: the densest pattern MYDAW can express, and therefore the most
/// events one block can be asked to produce.
fn dense_pattern() -> CompiledPattern {
    let mut pattern = Pattern::silent();
    pattern.length_steps = MAX_STEPS as u8;
    for row in 0..MAX_ROWS {
        let mut pattern_row = PatternRow::empty(36 + row as u8);
        for step in 0..MAX_STEPS {
            pattern_row.steps[step] = Step::accent();
        }
        pattern.rows.push(pattern_row);
    }
    // Spread across the four bars, several to a tick position, so the
    // scheduler's binary search has ties to get right and its pending array is
    // put under real pressure.
    let pattern_ticks = pattern.ticks();
    for index in 0..MAX_NOTES {
        let start = (index as u32 * 15) % pattern_ticks;
        pattern.notes.push(MelodicNote::new(
            36 + (index % 60) as u8,
            (index % 8) as u8,
            start,
            (pattern_ticks - start).clamp(1, 480),
        ));
    }
    pattern
        .validate()
        .expect("the densest legal pattern is legal");
    pattern.compile()
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
        let mut total = 0.0_f64;
        for i in 0..1_000 {
            total += i as f64;
        }
        std::hint::black_box(total);
    });
    assert_eq!(arithmetic, 0, "pure arithmetic must not be counted");
}

#[test]
fn scheduling_allocates_nothing_across_tempos_rates_and_block_sizes() {
    let pattern = dense_pattern();
    // Built before arming: the event buffer is the host's, not the scheduler's.
    let mut buffer = NoteEventBuffer::new(2_048);
    let mut scheduler = Scheduler::new();

    let count = count_allocations(|| {
        for sample_rate in [44_100.0_f64, 48_000.0] {
            for bpm in [20.0_f64, 130.0, 174.3, 999.0] {
                for frames in BLOCK_SIZES {
                    scheduler.reset();
                    let mut start = 0_u64;
                    for _ in 0..24 {
                        buffer.clear();
                        scheduler.schedule(
                            Some(&pattern),
                            &TransportWindow {
                                start_sample: start,
                                frames,
                                sample_rate,
                                bpm,
                                playing: true,
                            },
                            &mut buffer,
                        );
                        start += frames as u64;
                    }
                }
            }
        }
    });

    assert_eq!(count, 0, "the scheduler allocated {count} times");
    assert_eq!(scheduler.overflowed(), 0, "no note-off was ever lost");
}

#[test]
fn the_stop_flush_and_the_seek_flush_allocate_nothing() {
    let pattern = dense_pattern();
    let mut buffer = NoteEventBuffer::new(2_048);
    let mut scheduler = Scheduler::new();

    let count = count_allocations(|| {
        for round in 0..64_u64 {
            // Play a block, then stop (flush), then seek somewhere else and
            // play again (flush) -- the two paths that must release owed notes.
            for (start, playing) in [
                (round * 4_096, true),
                (round * 4_096 + 4_096, false),
                (round * 90_000 + 1_234, true),
            ] {
                buffer.clear();
                scheduler.schedule(
                    Some(&pattern),
                    &TransportWindow {
                        start_sample: start,
                        frames: 64,
                        sample_rate: 48_000.0,
                        bpm: DEFAULT_BPM,
                        playing,
                    },
                    &mut buffer,
                );
            }
        }
        buffer.clear();
        scheduler.flush(&mut buffer);
        scheduler.reset();
    });

    assert_eq!(count, 0, "the flush paths allocated {count} times");
}

#[test]
fn a_full_event_buffer_does_not_make_the_scheduler_allocate() {
    // The overflow branch: 256 events into a 4-slot buffer. Dropping must be a
    // returned count, never a growth.
    let pattern = dense_pattern();
    let mut buffer = NoteEventBuffer::new(4);
    let capacity = buffer.capacity();
    let mut scheduler = Scheduler::new();

    let count = count_allocations(|| {
        for block in 0..32_u64 {
            buffer.clear();
            scheduler.schedule(
                Some(&pattern),
                &TransportWindow {
                    start_sample: block * 4_096,
                    frames: 4_096,
                    sample_rate: 48_000.0,
                    bpm: 999.0,
                    playing: true,
                },
                &mut buffer,
            );
        }
    });

    assert_eq!(count, 0, "the overflow path allocated {count} times");
    assert_eq!(buffer.capacity(), capacity, "the buffer must never grow");
}

#[test]
fn swapping_the_pattern_on_the_audio_thread_neither_allocates_nor_frees() {
    // The `basedrop` claim, measured: the slot picks up a new pattern and drops
    // the old one, and neither may touch the allocator on this thread.
    let mut collector = Collector::new();
    let (mut handle, mut slot) = pattern_channel(8, collector.handle());
    let dense = dense_pattern();
    let sparse = Pattern::default().compile();

    // Installed off the "audio thread" -- that is where the allocation belongs.
    handle.install(dense.clone()).unwrap();
    handle.install(sparse).unwrap();
    handle.install(dense).unwrap();

    let mut buffer = NoteEventBuffer::new(2_048);
    let mut scheduler = Scheduler::new();

    let count = count_allocations(|| {
        for block in 0..8_u64 {
            slot.poll_swap();
            buffer.clear();
            scheduler.schedule(
                slot.current(),
                &TransportWindow {
                    start_sample: block * 512,
                    frames: 512,
                    sample_rate: 48_000.0,
                    bpm: DEFAULT_BPM,
                    playing: true,
                },
                &mut buffer,
            );
        }
    });

    assert_eq!(
        count, 0,
        "the pattern swap allocated or freed {count} times"
    );
    assert!(slot.has_pattern());
    // And the superseded patterns really were handed to the collector rather
    // than freed: they are still allocated until it runs.
    assert_eq!(collector.alloc_count(), 3);
    collector.collect();
    assert_eq!(collector.alloc_count(), 1, "only the current one survives");
}
