//! Callback load with a heavy mixer: `cargo run -p wave-engine --example bench --release`.
//! 8 tracks playing, every insert with EQ + compressor + reverb, master limiter, 48 kHz / 128 frames.

use std::sync::Arc;
use std::time::Instant;

use wave_engine::catalog::new_fx;
use wave_engine::*;

fn main() {
    let mut p = Project::sketch();
    let mut id = 20_000;
    for m in p.inserts.iter_mut().skip(1) {
        m.fx.clear();
        for k in [FxKind::Eq, FxKind::Comp, FxKind::Reverb] {
            m.fx.push(new_fx(id, k, p.bpm));
            id += 1;
        }
    }
    // put a clip on every lane so all eight are busy
    let pats = [1u64, 2, 3];
    p.clips = (0..8u32)
        .map(|t| Clip { id: 30_000 + t as u64, track: t, start: 0.0, len: 64.0, offset: 0.0, kind: ClipKind::Pat, source: pats[t as usize % 3], muted: false, gain: 0.0, fade_in: 0.0, fade_out: 0.0, pan: 0.0, norm: false, rev: false, tune: None, stretch: 1.0, resample: false })
        .collect();
    let e = Engine::new(p, Arc::new(NoPlugins));
    let block = 128;
    let mut rt = e.create_rt(48000.0, block);
    e.watch_fx(&[20_002, 20_005, 20_008]); // three reverb windows open
    e.play(0.0);
    let (mut l, mut r) = (vec![0f32; block], vec![0f32; block]);
    let blocks = 48000 * 20 / block;
    let mut worst = 0f64;
    let t0 = Instant::now();
    for _ in 0..blocks {
        let t = Instant::now();
        rt.process(&mut l, &mut r, None);
        worst = worst.max(t.elapsed().as_secs_f64());
    }
    let total = t0.elapsed().as_secs_f64();
    let period = block as f64 / 48000.0;
    println!("20 s of audio in {:.3} s → average load {:.1} %, worst block {:.1} % of the {:.2} ms period",
        total, total / 20.0 * 100.0, worst / period * 100.0, period * 1000.0);
}
