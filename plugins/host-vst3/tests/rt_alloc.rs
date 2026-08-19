//! Proves [`Vst3Node::process`] does not allocate.
//!
//! The zero-alloc rule on `RtNode` is the load-bearing invariant of the whole
//! engine, and "I read the code and it looks fine" is not evidence. The VST3
//! node has three places an allocation could hide, none of them obvious:
//!
//! - the event list is a `Vec` behind a `RefCell`, and `push` reallocates the
//!   moment the capacity reserved at activation turns out to be wrong;
//! - `ComWrapper::as_com_ref` is called per block to get the `IEventList` and
//!   `IParameterChanges` pointers;
//! - the channel-pointer arrays are rewritten every block.
//!
//! So this installs a counting global allocator and *measures*. The counter is
//! armed per-thread, so the rest of the test binary (and Rust's own harness,
//! which allocates freely) is not counted.
//!
//! **Skip-if-absent**, like the rest of the VST3 hardware tests: there is no
//! fixture plugin, so with nothing installed this prints why and passes. See
//! `test_util`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use mydaw_plugin_host_vst3::hosting::Vst3BusLayout;
use mydaw_plugin_host_vst3::test_util::{
    first_workable, missing_plugins_message, scanned_effects, scanned_instruments,
};
use mydaw_rt_core::{
    AudioBuffers, NoteEvent, NoteEventBuffer, ProcessCtx, RtNode, TransportSnapshot,
};

thread_local! {
    /// Allocations on this thread while armed. `Cell<u64>`, not an atomic:
    /// thread-local by construction, so no synchronisation is needed.
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

/// Bump the counter, but only on an armed thread.
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

const SAMPLE_RATE: f64 = 48_000.0;
const MAX_BLOCK: usize = 512;

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
        let mut total = 0_u64;
        for index in 0..1000_u64 {
            total = total.wrapping_add(index);
        }
        std::hint::black_box(total);
    });
    assert_eq!(quiet, 0, "pure arithmetic must not allocate");
}

#[test]
fn processing_a_vst3_effect_node_allocates_nothing() {
    let candidates = scanned_effects();
    if candidates.is_empty() {
        eprintln!("{}", missing_plugins_message());
        return;
    }
    let (winner, _) = first_workable(
        &candidates,
        Vst3BusLayout::is_stereo_effect,
        std::env::var("MYDAW_VST3_FX").ok().as_deref(),
        SAMPLE_RATE,
        MAX_BLOCK,
    );
    let Some((info, mut plugin, ())) = winner else {
        eprintln!("SKIPPED: no VST3 effect on this machine loads and activates.");
        return;
    };

    let mut node = plugin
        .create_node(SAMPLE_RATE, MAX_BLOCK)
        .expect("it activated once already");
    let mut buffers = AudioBuffers::new(MAX_BLOCK);
    for channel in buffers.as_mut_slices() {
        channel.fill(0.25);
    }

    // First block outside the measurement: it is where any lazily-initialised
    // state inside the plugin would appear, and the invariant we care about is
    // the steady state the audio callback actually runs in.
    {
        let mut ctx = ProcessCtx::new(
            &mut buffers,
            SAMPLE_RATE,
            MAX_BLOCK,
            TransportSnapshot::default(),
        );
        node.process(&mut ctx);
    }

    let observed = count_allocations(|| {
        for block in 0..64_usize {
            // Vary the block size: a naive implementation that sized anything
            // per block would grow a `Vec` here.
            let frames = 1 + (block * 7) % MAX_BLOCK;
            let mut ctx = ProcessCtx::new(
                &mut buffers,
                SAMPLE_RATE,
                frames,
                TransportSnapshot::default(),
            );
            node.process(&mut ctx);
        }
    });

    eprintln!(
        "'{}': {} allocator event(s) over 64 blocks",
        info.name, observed
    );
    assert_eq!(
        observed, 0,
        "Vst3Node::process allocated {observed} time(s) over 64 blocks through '{}'; the audio \
         thread must never touch the allocator",
        info.name
    );
    assert_eq!(node.stats().process_calls(), 65);

    drop(node);
    assert!(plugin.shutdown());
}

#[test]
fn dispatching_note_events_to_a_vst3_instrument_allocates_nothing() {
    // The claim that matters for the note path. Translating a `NoteEvent` into
    // VST3's wire format pushes into a `Vec` reserved at activation, and *will*
    // reallocate if that reservation is wrong. Reading the code is not evidence.
    let candidates = scanned_instruments();
    if candidates.is_empty() {
        eprintln!("SKIPPED: no VST3 instrument is installed on this machine.");
        return;
    }
    let (winner, _) = first_workable(
        &candidates,
        Vst3BusLayout::is_instrument,
        std::env::var("MYDAW_VST3_INSTRUMENT").ok().as_deref(),
        SAMPLE_RATE,
        MAX_BLOCK,
    );
    let Some((info, mut plugin, ())) = winner else {
        eprintln!("SKIPPED: no VST3 instrument on this machine loads and activates.");
        return;
    };

    let mut node = plugin
        .create_node(SAMPLE_RATE, MAX_BLOCK)
        .expect("it activated once already");
    if !node.delivers_notes() {
        eprintln!(
            "SKIPPED: '{}' declares no event input bus, so there is no note path to measure.",
            info.name
        );
        drop(node);
        plugin.shutdown();
        return;
    }

    let mut buffers = AudioBuffers::new(MAX_BLOCK);
    buffers.silence();

    // Warm-up block, outside the measurement, carrying an event.
    {
        let events = [NoteEvent::note_on(0, 60, 1.0)];
        let mut ctx = ProcessCtx::with_events(
            &mut buffers,
            SAMPLE_RATE,
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
                SAMPLE_RATE,
                frames,
                TransportSnapshot::default(),
                event_buffer.as_slice(),
            );
            node.process(&mut ctx);
        }
    });

    eprintln!(
        "'{}': {} allocator event(s) over 64 event-carrying blocks",
        info.name, observed
    );
    assert_eq!(
        observed, 0,
        "note dispatch allocated {observed} time(s) through '{}'",
        info.name
    );
    assert_eq!(node.stats().events_sent(), 8 * 64 + 1);
    assert_eq!(node.stats().events_dropped(), 0);

    drop(node);
    assert!(plugin.shutdown());
}
