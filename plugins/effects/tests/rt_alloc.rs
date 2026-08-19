//! Proves none of the five effects allocates on the audio thread.
//!
//! The zero-alloc rule is the engine's load-bearing invariant, and reading the
//! code is not evidence: a `Vec` that never grows, an iterator that never boxes
//! and a closure that never captures by value are properties of the *compiled*
//! program, not of anybody's intentions. So this installs a counting global
//! allocator and measures -- across many block sizes, with every parameter being
//! moved, with switches thrown mid-block, and with the bypass crossing in both
//! directions.
//!
//! The counter is armed per-thread, so the harness (which allocates freely) is
//! not counted, and it is proved to work before anything is asserted with it --
//! a broken counter would pass every test in the file.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;

use mydaw_plugin_effects::EffectRegistry;
use mydaw_rt_core::{AudioBuffers, ParamBlock, ProcessCtx, RtNode, TransportSnapshot, DEFAULT_BPM};

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
const BLOCK_SIZES: [usize; 10] = [1, 2, 3, 7, 64, 96, 127, 256, 480, 512];

const SR: f64 = 48_000.0;
const MAX_BLOCK: usize = 512;

/// One block through a node. Everything it touches was allocated before the
/// counter was armed.
fn run_block(node: &mut dyn RtNode, buffers: &mut AudioBuffers, frames: usize, position: u64) {
    let transport = TransportSnapshot {
        sample_pos: position,
        playing: true,
        bpm: DEFAULT_BPM,
        ..TransportSnapshot::default()
    };
    let mut ctx = ProcessCtx::new(buffers, SR, frames, transport);
    node.process(&mut ctx);
}

/// Fill both channels with something that is not silence, so no effect can take
/// a "nothing to do" short cut the real world would not give it.
fn excite(buffers: &mut AudioBuffers, seed: &mut u32) {
    for channel in buffers.as_mut_slices() {
        for sample in channel.iter_mut() {
            *seed ^= *seed << 13;
            *seed ^= *seed >> 17;
            *seed ^= *seed << 5;
            *sample = ((*seed >> 8) as f32 * (2.0 / 16_777_215.0) - 1.0) * 0.4;
        }
    }
}

/// Every parameter of `block`, swept across its own range as `t` goes 0 -> 1.
///
/// Sweeping *everything* is the point: a coefficient recomputed only while a
/// knob moves is exactly the path most likely to allocate, and it is the path a
/// steady-state test never takes.
fn sweep(block: &ParamBlock, t: f32) {
    for (index, def) in block.defs().iter().enumerate() {
        // Skip bypass here; the bypass transition is exercised on its own so a
        // failure points at one thing.
        if def.id == mydaw_plugin_effects::BYPASS_PARAM {
            continue;
        }
        block.set(index, def.from_normalised(t));
    }
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
fn no_effect_allocates_while_every_parameter_is_being_swept() {
    let registry = EffectRegistry::new();
    for info in registry.effects() {
        let params = registry.default_params(info.type_id).expect("a block");
        let mut node = registry
            .create(info.type_id, SR, MAX_BLOCK, Arc::clone(&params))
            .expect("a node");
        let mut buffers = AudioBuffers::new(MAX_BLOCK);
        let mut seed = 0x1234_5678_u32;
        let mut position = 0_u64;

        // Prove it makes a sound before proving anything about how it does.
        excite(&mut buffers, &mut seed);
        run_block(node.as_mut(), &mut buffers, MAX_BLOCK, 0);
        assert!(
            buffers.as_slices()[0].iter().any(|s| *s != 0.0),
            "{} produced nothing to measure",
            info.type_id
        );

        let count = count_allocations(|| {
            for (pass, size) in BLOCK_SIZES.into_iter().cycle().take(160).enumerate() {
                // The sweep runs over the whole range and back, so every switch
                // takes every position and every ramp is in flight most of the
                // time.
                let t = (pass % 32) as f32 / 31.0;
                sweep(&params, if pass % 64 < 32 { t } else { 1.0 - t });
                excite(&mut buffers, &mut seed);
                run_block(node.as_mut(), &mut buffers, size, position);
                position += size as u64;
            }
        });
        assert_eq!(count, 0, "{} allocated {count} times", info.type_id);
    }
}

#[test]
fn no_effect_allocates_across_a_bypass_transition_in_either_direction() {
    let registry = EffectRegistry::new();
    for info in registry.effects() {
        let params = registry.default_params(info.type_id).expect("a block");
        // Away from the defaults, so the bypass has something to fade against.
        sweep(&params, 0.7);
        let mut node = registry
            .create(info.type_id, SR, MAX_BLOCK, Arc::clone(&params))
            .expect("a node");
        let mut buffers = AudioBuffers::new(MAX_BLOCK);
        let mut seed = 0x9E37_79B9_u32;

        let count = count_allocations(|| {
            for pass in 0..48 {
                // On, off, on: the settled-bypass path, the crossfade in both
                // directions, and the reset that comes back out of it.
                let bypassed = (pass / 8) % 2 == 1;
                params.set_by_id(
                    mydaw_plugin_effects::BYPASS_PARAM,
                    if bypassed { 1.0 } else { 0.0 },
                );
                excite(&mut buffers, &mut seed);
                run_block(node.as_mut(), &mut buffers, 256, pass * 256);
            }
        });
        assert_eq!(
            count, 0,
            "{} allocated {count} times across a bypass",
            info.type_id
        );
    }
}

#[test]
fn a_long_steady_run_through_every_effect_allocates_nothing() {
    // The state a session actually spends its time in: nobody touching
    // anything, for a few seconds, including the stretch where a delay line has
    // filled and every filter has settled into its tail.
    let registry = EffectRegistry::new();
    for info in registry.effects() {
        let params = registry.default_params(info.type_id).expect("a block");
        let mut node = registry
            .create(info.type_id, SR, MAX_BLOCK, params)
            .expect("a node");
        let mut buffers = AudioBuffers::new(MAX_BLOCK);
        let mut seed = 0x0BAD_F00D_u32;

        let count = count_allocations(|| {
            for pass in 0..400 {
                // Alternate signal and silence, so the "everything decayed away"
                // paths run too.
                if pass % 4 == 0 {
                    excite(&mut buffers, &mut seed);
                } else {
                    buffers.silence();
                }
                run_block(node.as_mut(), &mut buffers, 480, pass * 480);
            }
        });
        assert_eq!(count, 0, "{} allocated {count} times", info.type_id);
    }
}
