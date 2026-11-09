//! `wave-perf-vst3`: cost of hosted VST3 effects (instantiate time, per-block process time, audio-thread
//! allocations). Built only with `--features vst3`; it is its own scan worker, like the app.
//!
//!   cargo run --release -p wave-perf --features vst3 --bin wave-perf-vst3 -- [--max 3] [--match substr] [--secs 6]
//!
//! Plugins that open licence dialogs or need a device may hang or fail: they are reported, not hidden.

use std::time::Instant;

use serde_json::json;
use wave_engine::ProcessCtx;
use wave_perf::meter::*;

#[global_allocator]
static ALLOC: Counting = Counting;

fn main() {
    if wave_vst3::run_scan_worker_if_requested() {
        return;
    }
    let args: Vec<String> = std::env::args().collect();
    let arg = |k: &str| {
        args.iter()
            .position(|a| a == k)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let max: usize = arg("--max").and_then(|v| v.parse().ok()).unwrap_or(3);
    let secs: f64 = arg("--secs").and_then(|v| v.parse().ok()).unwrap_or(6.0);
    let filter = arg("--match");
    let (list, scan_ms) = time_ms(|| wave_vst3::scan(false));
    eprintln!("scan: {} effects in {scan_ms:.0} ms", list.len());
    let mut out = vec![];
    let mut vendors = std::collections::HashSet::new();
    for p in list.iter().filter(|p| {
        filter
            .as_ref()
            .map(|f| p.name.to_lowercase().contains(&f.to_lowercase()))
            .unwrap_or(true)
    }) {
        if filter.is_none() && !vendors.insert(p.vendor.clone()) {
            continue; // one plugin per vendor unless asked for a specific one
        }
        if out.len() >= max {
            break;
        }
        eprintln!("instantiating {} ({})", p.name, p.vendor);
        let (r, inst_ms) = time_ms(|| wave_vst3::instantiate(&p.path, &p.cid, None, 48000.0, 512));
        let (mut proc, _handle) = match r {
            Ok(x) => x,
            Err(e) => {
                out.push(json!({ "name": p.name, "vendor": p.vendor, "error": e.to_string() }));
                continue;
            }
        };
        let ctx = ProcessCtx {
            sample_rate: 48000.0,
            bpm: 120.0,
            beat: 0.0,
            playing: true,
        };
        let mut l = vec![0f32; 512];
        let mut r = vec![0f32; 512];
        let blocks = (secs * 48000.0 / 512.0) as usize;
        let mut times = Vec::with_capacity(blocks);
        let (mut allocs, mut bytes) = (0, 0);
        for b in 0..blocks + 40 {
            for i in 0..512 {
                let t = (b * 512 + i) as f32 / 48000.0;
                l[i] = (t * 220.0 * std::f32::consts::TAU).sin() * 0.3
                    + (t * 1760.0 * std::f32::consts::TAU).sin() * 0.1;
                r[i] = l[i];
            }
            let t0 = Instant::now();
            let (_, a) = count_allocs(|| proc.process(&mut l, &mut r, &ctx));
            if b >= 40 {
                times.push(t0.elapsed().as_secs_f64() * 1e6);
                allocs += a.allocs;
                bytes += a.bytes;
            }
        }
        let period = 512.0 / 48000.0 * 1e6;
        let st = stats(times);
        out.push(json!({ "name": p.name, "vendor": p.vendor, "instantiate_ms": (inst_ms * 10.0).round() / 10.0, "block_us": st.json(),
            "cpu_pct_per_instance": (st.mean / period * 1000.0).round() / 10.0, "cpu_load_max_pct": (st.max / period * 1000.0).round() / 10.0,
            "audio_allocs": allocs, "audio_alloc_bytes": bytes, "audio_allocs_per_block": allocs as f64 / blocks as f64 }));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({ "scan_ms": scan_ms.round(), "effects_found": list.len(), "measured": out })
        )
        .unwrap()
    );
}
