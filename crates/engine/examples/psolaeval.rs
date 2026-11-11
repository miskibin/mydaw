//! Shift a vocal by a constant interval through the corrector's detector + PSOLA path:
//! `psolaeval in.wav out.wav 0.5` (semitones). For A/B quality checks against a reference shifter.
use wave_engine::pitch::{Detector, Psola};
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let au = wave_engine::sources::Audio::load(std::path::Path::new(&a[1])).unwrap();
    let st: f32 = a[3].parse().unwrap();
    let sr = au.sr as f32;
    let x: Vec<f32> = au.content().chunks(2).map(|f| (f[0] + f[1]) * 0.5).collect();
    let mut det = Detector::new(sr);
    det.set_range(70.0, 400.0);
    let mut ps = Psola::new(sr);
    let delay = det.latency().saturating_sub(ps.half_grain());
    let lat = delay + ps.latency();
    let mut dl = vec![0f32; delay.max(1)];
    let mut w = 0;
    let mut period = 0f32;
    let ratio = 2f32.powf(st / 12.0);
    let mut y = Vec::with_capacity(x.len() + lat);
    for i in 0..x.len() + lat {
        let s = x.get(i).copied().unwrap_or(0.0);
        if det.push(s) {
            period = if det.hz > 0.0 && det.confidence > 0.55 { sr / det.hz } else { 0.0 };
            det.hint = if period > 0.0 { det.hz } else { 0.0 };
        }
        let d = if delay > 0 { let v = dl[w]; dl[w] = s; w = (w + 1) % dl.len(); v } else { s };
        y.push(ps.process(d, period, ratio, 1.0));
    }
    let out: Vec<f32> = y[lat..].iter().flat_map(|&v| [v, v]).collect();
    wave_engine::sources::write_wav(std::path::Path::new(&a[2]), &out, au.sr).unwrap();
}
