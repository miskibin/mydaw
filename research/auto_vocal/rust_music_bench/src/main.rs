//! Runs WAVE's built-in analysis over a list of WAV files so the Python benchmark can
//! score it next to other methods. Research harness only.
//!
//!   music-bench <list.txt>
//!       one WAV per line -> TSV on stdout: path  bpm  key_root  minor  tuning_cents
//!       (wave_engine::music: tempo + key)
//!   music-bench --pitch yin|<rmvpe.onnx> <list.txt>
//!       lines "in.wav<TAB>out.txt" -> out.txt holds one MIDI value per 5 ms frame
//!       (NaN = unvoiced), from wave_engine::pitch::analyze_with (the app's path).

use std::io::{BufRead, Write};

use wave_engine::music::{analyze, estimate_key};
use wave_engine::pitch::{analyze_with, F0Estimator};

fn read_mono(path: &str) -> Option<(Vec<f32>, u32)> {
    let mut r = hound::WavReader::open(path).ok()?;
    let spec = r.spec();
    let ch = spec.channels as usize;
    let s: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().filter_map(Result::ok).collect(),
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u32 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>().filter_map(Result::ok).map(|v| v as f32 * scale).collect()
        }
    };
    let mono = s.chunks(ch).map(|c| c.iter().sum::<f32>() / ch as f32).collect();
    Some((mono, spec.sample_rate))
}

fn lines(path: &str) -> Vec<String> {
    let f = std::io::BufReader::new(std::fs::File::open(path).expect("list"));
    f.lines().map_while(Result::ok).filter(|l| !l.trim().is_empty()).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = std::io::stdout();
    let mut out = out.lock();
    if args.get(1).map(String::as_str) == Some("--pitch") {
        let det = &args[2];
        let est: Option<Box<dyn F0Estimator>> = if det == "yin" {
            None
        } else {
            Some(Box::new(wave_rmvpe::Rmvpe::load(std::path::Path::new(det)).expect("rmvpe model")))
        };
        for l in lines(&args[3]) {
            let (inp, dst) = l.trim().split_once('\t').expect("in<TAB>out");
            let Some((mono, sr)) = read_mono(inp) else { continue };
            let a = analyze_with(&mono, sr as f32, est.as_deref());
            let mut s = String::with_capacity(a.curve.len() * 8);
            for v in &a.curve {
                s.push_str(&format!("{v:.4}\n"));
            }
            std::fs::write(dst, s).expect("write");
            writeln!(out, "{dst}").ok();
        }
        return;
    }
    for p in lines(&args[1]) {
        let p = p.trim();
        let Some((mono, sr)) = read_mono(p) else {
            writeln!(out, "{p}\tnan\t-1\t0\t0").ok();
            continue;
        };
        let m = analyze(&mono, sr);
        let bpm = m.tempo.as_ref().map(|t| t.bpm).unwrap_or(f64::NAN);
        let (sum, _) = m.chroma_sum(0.0, f64::INFINITY);
        let (root, minor) = estimate_key(&sum, m.tuning_cents)
            .map(|k| (k.root as i32, k.minor as i32))
            .unwrap_or((-1, 0));
        writeln!(out, "{p}\t{bpm:.3}\t{root}\t{minor}\t{:.1}", m.tuning_cents).ok();
        out.flush().ok();
    }
}
