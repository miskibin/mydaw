//! Prints input vs corrected pitch for a WAV through the Pitch Correct effect.
use wave_engine::catalog::{new_fx, param_block};
use wave_engine::pitch::{hz_to_midi, Detector};
use wave_engine::{FxKind, ProcessCtx, Processor};
fn main() {
    let path = std::env::args().nth(1).expect("wav");
    let a = wave_engine::sources::Audio::load(std::path::Path::new(&path)).unwrap();
    let sr = a.sr as f32;
    let x: Vec<f32> = a.content().chunks(2).map(|f| f[0]).collect();
    let mut fx = new_fx(1, FxKind::Tune, 120.0);
    let arg = |i: usize, d: f64| std::env::args().nth(i).and_then(|s| s.parse().ok()).unwrap_or(d);
    for (k, v) in [
        ("retune", arg(6, 0.0)),
        ("flex", 0.0),
        ("vibrato", arg(5, 0.0)),
        ("humanize", 0.0),
        ("key", arg(2, 0.0)),
        ("scale", arg(3, 0.0)),
        ("voice", arg(4, 0.0)),
    ] {
        fx.params.insert(k.into(), v);
    }
    let mut t = wave_engine::vocal::Tune::default();
    t.prepare(sr, 256);
    t.set_params(&param_block(&fx));
    let mut y = vec![];
    let mut meters = vec![];
    for c in x.chunks(256) {
        let (mut l, mut r) = (c.to_vec(), c.to_vec());
        t.process(&mut l, &mut r, &ProcessCtx { sample_rate: sr, ..Default::default() });
        let mut m = [0f32; 32];
        t.meters(&mut m);
        meters.push(m);
        y.extend(l);
    }
    // summary: how often the corrector had a confident pitch, and how far its input was off
    let (mut conf, mut tot, mut errsum, mut corrsum, mut resid) = (0usize, 0usize, 0f32, 0f32, 0f32);
    for m in &meters {
        tot += 1;
        if m[0] > 0.0 {
            conf += 1;
            errsum += m[4].abs();
            corrsum += m[5].abs();
            resid += (m[4] + m[5]).abs();
        }
    }
    let c = conf.max(1) as f32;
    eprintln!("blocks with confident pitch: {:.0} %, mean |input − target| {:.0} c, mean |correction| {:.0} c, mean |input + correction − target| {:.0} c", conf as f32 / tot as f32 * 100.0, errsum / c, corrsum / c, resid / c);
    if let Some(path) = std::env::args().nth(7) {
        let lat = wave_engine::vocal::tune_latency(sr);
        let out: Vec<f32> = y.iter().skip(lat).flat_map(|&v| [v, v]).collect();
        wave_engine::sources::write_wav(std::path::Path::new(&path), &out, a.sr).unwrap();
    }
    let (mut di, mut dout) = (Detector::new(sr), Detector::new(sr));
    let lat = wave_engine::vocal::tune_latency(sr);
    for (i, &s) in x.iter().enumerate() {
        di.push(s);
        dout.push(*y.get(i + lat).unwrap_or(&0.0));
        if i % (sr as usize / 4) == 0 && i > 0 {
            let m = meters[i / 256];
            println!(
                "{:5.2}s in {:6.2} out {:6.2}  meters in {:6.2} target {:6.2} out {:6.2}",
                i as f32 / sr,
                if di.hz > 0.0 { hz_to_midi(di.hz) } else { 0.0 },
                if dout.hz > 0.0 { hz_to_midi(dout.hz) } else { 0.0 },
                if m[0] > 0.0 { hz_to_midi(m[0]) } else { 0.0 },
                if m[1] > 0.0 { hz_to_midi(m[1]) } else { 0.0 },
                if m[2] > 0.0 { hz_to_midi(m[2]) } else { 0.0 }
            );
        }
    }
}
