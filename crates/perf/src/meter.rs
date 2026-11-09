//! A counting global allocator (per-thread "armed" counters), timing statistics, RSS.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::time::Instant;

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
    static BYTES: Cell<u64> = const { Cell::new(0) };
    static FREES: Cell<u64> = const { Cell::new(0) };
}

pub struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let _ = ARMED.try_with(|a| {
            if a.get() {
                ALLOCS.with(|c| c.set(c.get() + 1));
                BYTES.with(|c| c.set(c.get() + l.size() as u64));
            }
        });
        System.alloc(l)
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let _ = ARMED.try_with(|a| {
            if a.get() {
                ALLOCS.with(|c| c.set(c.get() + 1));
                BYTES.with(|c| c.set(c.get() + l.size() as u64));
            }
        });
        System.alloc_zeroed(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        let _ = ARMED.try_with(|a| {
            if a.get() {
                FREES.with(|c| c.set(c.get() + 1));
            }
        });
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let _ = ARMED.try_with(|a| {
            if a.get() {
                ALLOCS.with(|c| c.set(c.get() + 1));
                BYTES.with(|c| c.set(c.get() + n as u64));
            }
        });
        System.realloc(p, l, n)
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct AllocCount {
    pub allocs: u64,
    pub frees: u64,
    pub bytes: u64,
}

/// Runs `f` and returns what the current thread allocated meanwhile.
pub fn count_allocs<R>(f: impl FnOnce() -> R) -> (R, AllocCount) {
    let (a0, b0, f0) = (
        ALLOCS.with(|c| c.get()),
        BYTES.with(|c| c.get()),
        FREES.with(|c| c.get()),
    );
    ARMED.with(|a| a.set(true));
    let r = f();
    ARMED.with(|a| a.set(false));
    let d = AllocCount {
        allocs: ALLOCS.with(|c| c.get()) - a0,
        bytes: BYTES.with(|c| c.get()) - b0,
        frees: FREES.with(|c| c.get()) - f0,
    };
    (r, d)
}

/// Milliseconds a closure takes.
pub fn time_ms<R>(f: impl FnOnce() -> R) -> (R, f64) {
    let t = Instant::now();
    let r = f();
    (r, t.elapsed().as_secs_f64() * 1000.0)
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub n: usize,
    pub mean: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
}

pub fn stats(mut v: Vec<f64>) -> Stats {
    if v.is_empty() {
        return Stats::default();
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let at = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize];
    Stats {
        n: v.len(),
        mean: v.iter().sum::<f64>() / v.len() as f64,
        p50: at(0.5),
        p95: at(0.95),
        p99: at(0.99),
        max: *v.last().unwrap(),
    }
}

impl Stats {
    pub fn json(&self) -> serde_json::Value {
        let r = |x: f64| (x * 1000.0).round() / 1000.0;
        serde_json::json!({ "n": self.n, "mean": r(self.mean), "p50": r(self.p50), "p95": r(self.p95), "p99": r(self.p99), "max": r(self.max) })
    }
}

/// Resident set size of this process in MiB.
pub fn rss_mib() -> f64 {
    #[cfg(windows)]
    {
        #[repr(C)]
        struct Counters {
            cb: u32,
            page_fault_count: u32,
            peak_working_set: usize,
            working_set: usize,
            rest: [usize; 6],
        }
        extern "system" {
            fn GetCurrentProcess() -> isize;
            fn K32GetProcessMemoryInfo(h: isize, c: *mut Counters, cb: u32) -> i32;
        }
        let mut c = Counters {
            cb: std::mem::size_of::<Counters>() as u32,
            page_fault_count: 0,
            peak_working_set: 0,
            working_set: 0,
            rest: [0; 6],
        };
        unsafe {
            if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) != 0 {
                return c.working_set as f64 / 1048576.0;
            }
        }
        0.0
    }
    #[cfg(not(windows))]
    {
        std::fs::read_to_string("/proc/self/statm")
            .ok()
            .and_then(|s| {
                s.split_whitespace()
                    .nth(1)
                    .and_then(|x| x.parse::<f64>().ok())
            })
            .map(|p| p * 4096.0 / 1048576.0)
            .unwrap_or(0.0)
    }
}
