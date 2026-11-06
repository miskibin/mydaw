//! `Vst3Effect::process` must not allocate: measured with a counting global
//! allocator against a real plugin (see `hosting.rs` for the fixture), with
//! parameter changes flowing every block. One test in this binary, so no other
//! test thread allocates while it counts.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use wave_engine::ProcessCtx;

struct Counting;

static COUNTING: AtomicBool = AtomicBool::new(false);
static EVENTS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards to the system allocator; only counts.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            EVENTS.fetch_add(1, Ordering::Relaxed);
        }
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if COUNTING.load(Ordering::Relaxed) {
            EVENTS.fetch_add(1, Ordering::Relaxed);
        }
        System.dealloc(ptr, layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            EVENTS.fetch_add(1, Ordering::Relaxed);
        }
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn processing_allocates_nothing_even_with_parameter_changes() {
    let Some(bundle) = std::env::var_os("WAVE_VST3_TEST_BUNDLE") else {
        eprintln!("skipped: WAVE_VST3_TEST_BUNDLE is not set (no VST3 plugin to host)");
        return;
    };
    let root = std::path::Path::new(&bundle)
        .parent()
        .unwrap()
        .to_path_buf();
    let worker = std::path::Path::new(env!("CARGO_BIN_EXE_wave-vst3-scan-worker"));
    let (effects, _, _) = wave_vst3::testing::scan_roots(&[root], Some(worker));
    let info = effects
        .into_iter()
        .find(|effect| std::path::Path::new(&effect.path) == std::path::Path::new(&bundle))
        .expect("fixture listed");
    let (mut effect, handle) =
        wave_vst3::instantiate(&info.path, &info.cid, None, 48_000.0, 256).unwrap();
    let id = handle.params()[0].id;

    let ctx = ProcessCtx {
        sample_rate: 48_000.0,
        bpm: 128.0,
        beat: 0.0,
        playing: true,
    };
    let mut l = vec![0.25_f32; 256];
    let mut r = vec![-0.25_f32; 256];
    effect.process(&mut l, &mut r, &ctx); // warm-up

    let mut total = 0;
    for block in 0..64 {
        handle.set_param(id, block as f64 / 64.0).unwrap(); // not counted: UI side
        let frames = 1 + (block * 37) % 256; // varying block sizes
        l.fill(0.25);
        r.fill(-0.25);
        COUNTING.store(true, Ordering::SeqCst);
        effect.process(&mut l[..frames], &mut r[..frames], &ctx);
        COUNTING.store(false, Ordering::SeqCst);
        total += EVENTS.swap(0, Ordering::SeqCst);
    }
    assert_eq!(
        total, 0,
        "Vst3Effect::process allocated {total} time(s) over 64 blocks"
    );
    let expected = 63.0 / 64.0 * 0.25;
    assert!(
        (l[0] - expected).abs() < 1e-6,
        "the last parameter value arrived: {}",
        l[0]
    );
}
