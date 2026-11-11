use wave_engine::poly::{lab, presets, SynthInst};
use wave_engine::synth::Dest;
fn main() {
    let name = std::env::args().nth(1).unwrap();
    let all = presets::all();
    let p = all.iter().find(|p| p.name == name).unwrap();
    let sr = 48000.0;
    let ms = |x: f32| (x * sr / 1000.0) as u32;
    let key = p.probe_key();
    let mut s = SynthInst::new(1, p.patch, sr);
    for i in 0..20u32 {
        s.note(
            ms(30.0 * i as f32),
            ms(1500.0),
            (key as u32 - 12 + 2 * i) as u8,
            0.8,
            Dest::Click,
        );
    }
    let (l, _) = lab::run(&mut s, 4.0, sr);
    for i in lab::click_flags(&l, sr, 3.0) {
        println!("flag at {:.2} ms", i as f32 / sr * 1000.0);
        for j in i - 40..i + 8 {
            println!(
                "  {j} {:+.5} d2 {:+.5}",
                l[j],
                l[j] - 2.0 * l[j - 1] + l[j - 2]
            );
        }
    }
}
