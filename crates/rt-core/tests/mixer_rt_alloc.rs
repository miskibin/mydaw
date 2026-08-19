//! Proves the mixer graph does not allocate on the audio thread.
//!
//! The zero-alloc rule on [`RtNode`] is the load-bearing invariant of the whole
//! engine, and step 5 gave the graph three new things that could break it: a
//! per-track scratch buffer, a per-track note-event filter, and a peak block
//! copied out every callback. Every one of them is a `Vec` or an array that must
//! have been sized before the wall, and "I read the code and it looks fine" is
//! not evidence -- so this installs a counting global allocator and *measures*,
//! at the worst case the engine allows: sixteen tracks, a full block of events,
//! and controls moving while it renders.
//!
//! The counter is armed per-thread, so the rest of the test binary (and Rust's
//! own test harness, which allocates freely) is not counted.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;

use mydaw_rt_core::basedrop::Collector;
use mydaw_rt_core::{
    graph_channel, AudioBuffers, BusSpec, GraphLimits, GraphSpec, MixerParams, NoteEvent, NoteKind,
    ProcessCtx, RtNode, SendSpec, StripId, TrackSpec, TransportSnapshot, MAX_BUSES, MAX_TRACKS,
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
    // `try_with`: during thread teardown the TLS slot is gone, and panicking
    // inside the allocator would abort the process.
    let _ = ARMED.try_with(|armed| {
        if armed.get() {
            let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        }
    });
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Run `body` with the counter armed, returning how many times it allocated.
fn count_allocations(body: impl FnOnce()) -> u64 {
    ALLOCATIONS.with(|count| count.set(0));
    ARMED.with(|armed| armed.set(true));
    body();
    ARMED.with(|armed| armed.set(false));
    ALLOCATIONS.with(|count| count.get())
}

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;
const EVENTS: usize = 256;

/// A source that reads its whole event slice and writes a constant -- so the
/// per-track event filter is really exercised rather than skipped.
struct EventCountingNode {
    value: f32,
    seen: u64,
}

impl RtNode for EventCountingNode {
    fn process(&mut self, ctx: &mut ProcessCtx) {
        for event in ctx.events {
            if matches!(event.kind, NoteKind::NoteOn { .. }) {
                self.seen += 1;
            }
        }
        for channel in ctx.buffers.as_mut_slices() {
            channel.fill(self.value);
        }
    }
}

/// A full sixteen-track graph, every track fed by an event-reading source.
fn full_graph() -> (
    Collector,
    mydaw_rt_core::GraphHandle,
    mydaw_rt_core::RtGraphSlot,
    Arc<MixerParams>,
) {
    let collector = Collector::new();
    let (mut handle, mut slot) = graph_channel(2, collector.handle());
    let params = Arc::new(MixerParams::new());

    let mut spec = GraphSpec::new()
        .with_params(Arc::clone(&params))
        .with_limits(GraphLimits {
            sample_rate: SR,
            max_block: BLOCK,
            max_events: EVENTS,
        });
    for track in 0..MAX_TRACKS {
        spec = spec.with_track(TrackSpec::new(Box::new(EventCountingNode {
            value: 0.01 * (track + 1) as f32,
            seen: 0,
        })));
    }
    handle.install(spec).expect("install");
    slot.poll_swap();
    (collector, handle, slot, params)
}

/// One event per track, plus enough extra to make the filter do real work.
fn block_events() -> Vec<NoteEvent> {
    let mut events = Vec::new();
    for offset in 0..8_u32 {
        for track in 0..MAX_TRACKS {
            events.push(NoteEvent::note_on(offset, 60 + track as u8, 1.0).on_channel(track as u8));
        }
    }
    events
}

#[test]
fn sixteen_tracks_with_a_full_event_block_allocate_nothing() {
    let (_collector, _handle, mut slot, params) = full_graph();
    let mut buffers = AudioBuffers::new(BLOCK);
    let events = block_events();

    // One warm-up block outside the measurement: the first call is where any
    // lazily-initialised thread-local in the standard library would show up, and
    // that is not what this test is about.
    {
        let mut ctx = ProcessCtx::with_events(
            &mut buffers,
            SR,
            BLOCK,
            TransportSnapshot::default(),
            &events,
        );
        slot.process(&mut ctx);
    }

    let observed = count_allocations(|| {
        for block in 0..64 {
            // Controls moving while it renders: the ramping path has its own
            // branch, and it must not allocate either.
            params.set_track_gain(block % MAX_TRACKS, 0.5);
            params.set_track_pan(block % MAX_TRACKS, -0.5);
            params.set_track_muted(block % MAX_TRACKS, block % 3 == 0);
            params.set_master_gain(0.8);

            // `poll_swap` on an empty queue is part of every callback too.
            slot.poll_swap();
            let mut ctx = ProcessCtx::with_events(
                &mut buffers,
                SR,
                BLOCK,
                TransportSnapshot::default(),
                &events,
            );
            slot.process(&mut ctx);
            let _ = slot.track_peaks();
        }
    });

    assert_eq!(
        observed, 0,
        "the mixer graph allocated {observed} time(s) over 64 blocks of 16 tracks"
    );
    assert_eq!(slot.track_count(), MAX_TRACKS);
    assert_eq!(slot.track_peaks().count(), MAX_TRACKS);
}

/// The routing step's worst case: sixteen tracks, eight buses, **every** track
/// tapping **every** bus, and every bus tapping every later bus.
///
/// 156 send taps, 312 send strips, eight bus input buffers and one shared
/// post-fader scratch, all of which have to have been allocated before the wall.
/// This is the shape the fixed [`MAX_BUSES`] cap exists to make measurable: a
/// mixer that allocated once per tap would allocate 156 times a block here.
#[test]
fn a_full_send_matrix_over_eight_buses_allocates_nothing() {
    let collector = Collector::new();
    let (mut handle, mut slot) = graph_channel(2, collector.handle());
    let params = Arc::new(MixerParams::new());

    let mut spec = GraphSpec::new()
        .with_params(Arc::clone(&params))
        .with_limits(GraphLimits {
            sample_rate: SR,
            max_block: BLOCK,
            max_events: EVENTS,
        });
    for track in 0..MAX_TRACKS {
        spec = spec.with_track(TrackSpec::new(Box::new(EventCountingNode {
            value: 0.001 * (track + 1) as f32,
            seen: 0,
        })));
    }
    for _ in 0..MAX_BUSES {
        spec = spec.with_bus(BusSpec::new());
    }
    for track in 0..MAX_TRACKS {
        for bus in 0..MAX_BUSES {
            spec = spec.with_send(SendSpec::new(StripId::Track(track), bus));
            params.set_send_level(StripId::Track(track), bus, 0.1);
        }
    }
    for from in 0..MAX_BUSES {
        for to in from + 1..MAX_BUSES {
            spec = spec.with_send(SendSpec::new(StripId::Bus(from), to));
            params.set_send_level(StripId::Bus(from), to, 0.05);
        }
    }
    let declared = spec.send_count();
    handle.install(spec).expect("install");
    slot.poll_swap();
    assert_eq!(
        declared,
        MAX_TRACKS * MAX_BUSES + MAX_BUSES * (MAX_BUSES - 1) / 2
    );

    let mut buffers = AudioBuffers::new(BLOCK);
    let events = block_events();
    {
        let mut ctx = ProcessCtx::with_events(
            &mut buffers,
            SR,
            BLOCK,
            TransportSnapshot::default(),
            &events,
        );
        slot.process(&mut ctx);
    }

    let observed = count_allocations(|| {
        for block in 0..32 {
            // Send knobs and pre/post flags moving while it renders: both are
            // atomics, and both drive a ramp that has its own branch.
            let source = StripId::Track(block % MAX_TRACKS);
            params.set_send_level(source, block % MAX_BUSES, 0.2);
            params.set_send_pre(source, block % MAX_BUSES, block % 2 == 0);
            params.set_bus_gain(block % MAX_BUSES, 0.7);
            params.set_bus_pan(block % MAX_BUSES, -0.3);
            params.set_bus_muted(block % MAX_BUSES, block % 5 == 0);

            slot.poll_swap();
            let mut ctx = ProcessCtx::with_events(
                &mut buffers,
                SR,
                BLOCK,
                TransportSnapshot::default(),
                &events,
            );
            slot.process(&mut ctx);
            let _ = slot.track_peaks();
            let _ = slot.bus_peaks();
        }
    });

    assert_eq!(
        observed, 0,
        "a full send matrix allocated {observed} time(s) over 32 blocks"
    );
    assert_eq!(slot.bus_count(), MAX_BUSES);
    assert_eq!(slot.bus_peaks().count(), MAX_BUSES);
    // Nothing is silent and nothing has run away: the whole matrix is live.
    let master = buffers.as_slices()[0][BLOCK - 1];
    assert!(master.is_finite() && master.abs() > 0.0, "{master}");
}

#[test]
fn a_block_with_more_events_than_a_tracks_buffer_holds_does_not_grow_it() {
    // The per-track filter buffer is `max_events` long. A pathological block
    // that exceeds it must drop rather than reallocate on the audio thread.
    let collector = Collector::new();
    let (mut handle, mut slot) = graph_channel(2, collector.handle());
    handle
        .install(
            GraphSpec::new()
                .with_limits(GraphLimits {
                    sample_rate: SR,
                    max_block: BLOCK,
                    max_events: 4,
                })
                .with_track(TrackSpec::new(Box::new(EventCountingNode {
                    value: 0.5,
                    seen: 0,
                }))),
        )
        .expect("install");
    slot.poll_swap();

    // Every event on channel 0, so they all target the one track.
    let events: Vec<NoteEvent> = (0..64).map(|i| NoteEvent::note_on(i, 60, 1.0)).collect();
    let mut buffers = AudioBuffers::new(BLOCK);
    {
        let mut ctx = ProcessCtx::with_events(
            &mut buffers,
            SR,
            BLOCK,
            TransportSnapshot::default(),
            &events,
        );
        slot.process(&mut ctx);
    }

    let observed = count_allocations(|| {
        for _ in 0..16 {
            let mut ctx = ProcessCtx::with_events(
                &mut buffers,
                SR,
                BLOCK,
                TransportSnapshot::default(),
                &events,
            );
            slot.process(&mut ctx);
        }
    });
    assert_eq!(observed, 0, "an over-full event block allocated");
}

#[test]
fn a_graph_swap_on_the_audio_thread_frees_nothing() {
    // The other half of the rule: dropping the superseded graph -- sixteen
    // scratch buffers, sixteen event buffers, sixteen boxed nodes -- must be a
    // hand-off to the collector, not sixteen `free()`s in the callback.
    let mut collector = Collector::new();
    let (mut handle, mut slot) = graph_channel(4, collector.handle());
    let params = Arc::new(MixerParams::new());

    let build = || {
        let mut spec = GraphSpec::new()
            .with_params(Arc::clone(&params))
            .with_limits(GraphLimits {
                sample_rate: SR,
                max_block: BLOCK,
                max_events: EVENTS,
            });
        for track in 0..MAX_TRACKS {
            spec = spec.with_track(TrackSpec::new(Box::new(EventCountingNode {
                value: 0.01 * (track + 1) as f32,
                seen: 0,
            })));
        }
        spec
    };

    handle.install(build()).unwrap();
    slot.poll_swap();
    handle.install(build()).unwrap();

    let before = collector.alloc_count();
    let observed = count_allocations(|| {
        // The block that picks up the new graph and retires the old one.
        slot.poll_swap();
    });
    assert_eq!(observed, 0, "the swap allocated");
    assert_eq!(
        collector.alloc_count(),
        before,
        "dropping a 16-track graph on the audio thread must not deallocate"
    );

    collector.collect();
    assert!(collector.alloc_count() < before, "the collector freed it");
}
