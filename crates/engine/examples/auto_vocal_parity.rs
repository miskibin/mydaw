//! Parity of the app's auto-vocal dynamics chain with the research prototype.
//!
//! 1. Export the research test set (50 degraded MUSDB test songs, same seeds as
//!    `av.dyn_calib`) with `auto_vocal_parity_export.py` (see its docstring).
//! 2. `cargo run --release -p wave-engine --example auto_vocal_parity -- D:/wave-data/auto_vocal_parity [--short] [--n 50]`
//!
//! Each song's pre-dynamics signal (gate → HPF → EQ, done in Python) goes through the engine
//! chain calibrated by `autovocal::calibrate_dynamics` (rider → FET → compressor → de-esser).
//! The research metrics (400 ms loudness IQR / P90−P10 and per-word IQR / P90−P10 on the raw
//! take's vocal windows and words, breath lift, gain motion) are computed with the Rust port
//! and compared with the Python numbers stored by the export.

use std::path::{Path, PathBuf};

use wave_engine::autovocal::{self, metrics, percentile, Style};

fn read_wav(p: &Path) -> (Vec<f32>, f32) {
    let mut r = hound::WavReader::open(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    let sr = r.spec().sample_rate as f32;
    (r.samples::<f32>().map(|s| s.unwrap()).collect(), sr)
}

/// Minimal .npy reader for a little-endian float64 array; returns (shape, data).
fn read_npy(p: &Path) -> (Vec<usize>, Vec<f64>) {
    let b = std::fs::read(p).unwrap();
    let hl = u16::from_le_bytes([b[8], b[9]]) as usize;
    let header = String::from_utf8_lossy(&b[10..10 + hl]).to_string();
    let sh = header.split("'shape': (").nth(1).unwrap().split(')').next().unwrap();
    let shape: Vec<usize> = sh.split(',').filter_map(|s| s.trim().parse().ok()).collect();
    let data = b[10 + hl..].as_chunks::<8>().0.iter().map(|c| f64::from_le_bytes(*c)).collect();
    (shape, data)
}

fn med(v: &[f64]) -> f64 {
    percentile(v, 50.0)
}

#[derive(Default, Clone)]
struct Row {
    rust: [f64; 12],
    py: [f64; 12],
    gain_rms_diff: f64,
    secs: f64,
}

const NAMES: [&str; 12] = [
    "raw 400ms IQR",
    "raw word P90-P10",
    "rider 400ms IQR",
    "rider word P90-P10",
    "after 400ms IQR",
    "after 400ms P90-P10",
    "after word IQR",
    "after word P90-P10",
    "breath lift median",
    "breath lift P95",
    "gain motion P95 dB/s",
    "metric check: py_after IQR",
];

fn song(dir: &Path, long: bool) -> Row {
    let (raw, sr) = read_wav(&dir.join("raw.wav"));
    let (eq, _) = read_wav(&dir.join("eq.wav"));
    let (pya, _) = read_wav(&dir.join("py_after.wav"));
    let js: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("python.json")).unwrap().replace("NaN", "null")).unwrap();
    let t0 = std::time::Instant::now();
    let d = autovocal::calibrate_dynamics(&eq, sr, Style::Natural, long);
    let secs = t0.elapsed().as_secs_f64();
    let mask = metrics::vocal_windows(&raw, sr);
    let segs = metrics::words(&raw, sr);
    let m1 = |x: &[f32]| metrics::m1(x, sr, &mask);
    let m2 = |x: &[f32]| metrics::m2(x, sr, &segs);
    let (r1, r2) = (m1(&raw), m2(&raw));
    let (d1, d2) = (m1(&d.after_rider), m2(&d.after_rider));
    let (a1, a2) = (m1(&d.after), m2(&d.after));
    if std::env::var("AV_STAGES").is_ok() {
        // stage-by-stage: research stages (Rust port) vs engine stages, on the same rider output
        let rs = autovocal::research_stages(&d.after_rider, sr);
        let fet = autovocal::render(wave_engine::FxKind::Fet, &d.fet, &d.after_rider, sr);
        let comp = autovocal::render(wave_engine::FxKind::Comp, &d.comp, &fet, sr);
        eprintln!(
            "  stages IQR/wordP90: research fet {:.2}/{:.2} opto {:.2}/{:.2} deess {:.2}/{:.2} | engine fet {:.2}/{:.2} comp {:.2}/{:.2} deess {:.2}/{:.2}",
            m1(&rs[0]).1, m2(&rs[0]).1, m1(&rs[1]).1, m2(&rs[1]).1, m1(&rs[2]).1, m2(&rs[2]).1,
            m1(&fet).1, m2(&fet).1, m1(&comp).1, m2(&comp).1, a1.1, a2.1
        );
    }

    // rider gain per 10 ms frame (energy ratio), against the research curve and its masks
    let (shape, g) = read_npy(&dir.join("py_rider_gain.npy"));
    let nf = shape[1];
    let (pg, conf, active) = (&g[..nf], &g[nf..2 * nf], &g[2 * nf..]);
    // research-style stats on the rider's own curve and masks
    let rc = &d.rider_curve;
    let n = rc.len().min(nf);
    let rg: Vec<f64> = rc.iter().map(|f| f[0] as f64).collect();
    let non: Vec<f64> = (0..n).filter(|&i| rc[i][1] < 0.2 || rc[i][2] < 0.5).map(|i| rg[i]).collect();
    let motion: Vec<f64> = (1..n).filter(|&i| rc[i][2] > 0.5).map(|i| (rg[i] - rg[i - 1]).abs() / 0.01).collect();
    let on: Vec<usize> = (0..n).filter(|&i| conf[i] > 0.5).collect();
    if std::env::var("AV_DBG").is_ok() {
        let nonp: Vec<f64> = (0..n).filter(|&i| conf[i] < 0.2 || active[i] < 0.5).map(|i| rg[i]).collect();
        let (mut bi, mut bd) = (0, 0.0);
        for i in 0..n {
            let d = (rg[i] - pg[i]).abs();
            if d > bd {
                bd = d;
                bi = i;
            }
        }
        let a = bi.saturating_sub(5);
        let b = (bi + 5).min(n);
        let r1 = |v: &[f64]| v.iter().map(|x| (x * 10.0).round() / 10.0).collect::<Vec<_>>();
        let oc: Vec<f64> = rc[a..b].iter().map(|v| v[1] as f64).collect();
        let oa: Vec<f64> = rc[a..b].iter().map(|v| v[2] as f64).collect();
        eprintln!(
            "{} lift p95 pymask {:.2} ours {:.2} | maxdiff {bd:.2} at {bi}/{n}\n ours {:?}\n py   {:?}\n pyconf {:?} pyact {:?}\n ourconf {:?} ouract {:?}",
            dir.display(),
            percentile(&nonp, 95.0),
            percentile(&non, 95.0),
            r1(&rg[a..b]),
            r1(&pg[a..b]),
            r1(&conf[a..b]),
            r1(&active[a..b]),
            r1(&oc),
            r1(&oa)
        );
    }
    let gain_rms_diff = (on.iter().map(|&i| (rg[i] - pg[i]).powi(2)).sum::<f64>() / on.len().max(1) as f64).sqrt();

    let f = |a: &str, b: &str| js[a][b].as_f64().unwrap_or(f64::NAN);
    let rust = [r1.1, r2.1, d1.1, d2.1, a1.1, a1.2, a2.0, a2.1, med(&non), percentile(&non, 95.0), percentile(&motion, 95.0), m1(&pya).1];
    let py = [
        f("raw_m1", "iqr"),
        f("raw_m2", "p90_p10"),
        f("rider_m1", "iqr"),
        f("rider_m2", "p90_p10"),
        f("after_m1", "iqr"),
        f("after_m1", "p90_p10"),
        f("after_m2", "iqr"),
        f("after_m2", "p90_p10"),
        f("rider", "lift_median_db"),
        f("rider", "lift_p95_db"),
        f("rider", "motion_p95_db_s"),
        f("after_m1", "iqr"),
    ];
    eprintln!(
        "{}  after IQR rust {:.2} py {:.2} | word P90-P10 rust {:.2} py {:.2} | FET {:?} comp {:?} sib {:?} deess thr {:.1} | {:.1}s",
        dir.file_name().unwrap().to_string_lossy(),
        a1.1,
        py[4],
        a2.1,
        py[7],
        d.stats.fet_p99_gr_db,
        d.stats.comp_mean_gr_db,
        d.stats.sibilance_after_db,
        d.deess["thr"],
        secs
    );
    Row { rust, py, gain_rms_diff, secs }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root = PathBuf::from(args.get(1).map(String::as_str).unwrap_or("D:/wave-data/auto_vocal_parity"));
    let long = !args.iter().any(|a| a == "--short");
    let n: usize = args.iter().position(|a| a == "--n").and_then(|i| args.get(i + 1)).and_then(|s| s.parse().ok()).unwrap_or(50);
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.join("eq.wav").exists()).collect();
    dirs.sort();
    dirs.truncate(n);
    let threads = std::thread::available_parallelism().map(|v| v.get()).unwrap_or(4).saturating_sub(2).clamp(1, 8);
    let rows: Vec<Row> = std::thread::scope(|s| {
        let chunks: Vec<Vec<PathBuf>> = (0..threads).map(|t| dirs.iter().skip(t).step_by(threads).cloned().collect()).collect();
        let hs: Vec<_> = chunks.into_iter().map(|c| s.spawn(move || c.iter().map(|d| song(d, long)).collect::<Vec<_>>())).collect();
        hs.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    println!("songs: {}  look-ahead: {}", rows.len(), if long { "long" } else { "short" });
    println!("{:<28} {:>8} {:>8} {:>8}", "median over songs", "python", "rust", "diff");
    for (k, name) in NAMES.iter().enumerate() {
        let p = med(&rows.iter().map(|r| r.py[k]).collect::<Vec<_>>());
        let r = med(&rows.iter().map(|r| r.rust[k]).collect::<Vec<_>>());
        println!("{name:<28} {p:>8.2} {r:>8.2} {:>+8.2}", r - p);
    }
    for (k, name) in [(4, "after 400ms IQR"), (7, "after word P90-P10")] {
        let d: Vec<f64> = rows.iter().map(|r| (r.rust[k] - r.py[k]).abs()).collect();
        println!("per song |rust - python|, {name}: median {:.2}, p90 {:.2} dB", med(&d), percentile(&d, 90.0));
    }
    println!("rider gain vs research curve, RMS diff on confident frames (median): {:.2} dB", med(&rows.iter().map(|r| r.gain_rms_diff).collect::<Vec<_>>()));
    println!("calibration time per song (median): {:.1} s", med(&rows.iter().map(|r| r.secs).collect::<Vec<_>>()));
}
