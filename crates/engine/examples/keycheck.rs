//! Key, intonation and range of a vocal WAV: `cargo run -p wave-engine --example keycheck -- file.wav`.
fn main() {
    let path = std::env::args().nth(1).expect("wav");
    let a = wave_engine::sources::Audio::load(std::path::Path::new(&path)).unwrap();
    let mono: Vec<f32> = a.content().chunks(2).map(|f| (f[0] + f[1]) * 0.5).collect();
    let an = wave_engine::pitch::analyze(&mono, a.sr as f32);
    let v: Vec<f32> = an.curve.iter().copied().filter(|x| !x.is_nan()).collect();
    let names = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    let mut pc = [0f32; 12];
    for &m in &v {
        pc[(m.round() as i32).rem_euclid(12) as usize] += 1.0;
    }
    let maj = [6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88f32];
    let min = [6.33, 2.68, 3.52, 5.38, 2.6, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17f32];
    let corr = |p: &[f32; 12], k: usize| {
        let (mx, mp) = (pc.iter().sum::<f32>() / 12.0, p.iter().sum::<f32>() / 12.0);
        let (mut s, mut a2, mut b2) = (0f32, 0f32, 0f32);
        for i in 0..12 {
            let (a, b) = (pc[i] - mx, p[(i + 12 - k) % 12] - mp);
            s += a * b;
            a2 += a * a;
            b2 += b * b;
        }
        s / (a2 * b2).sqrt()
    };
    let mut keys: Vec<(f32, String)> = (0..12).flat_map(|k| [(corr(&maj, k), format!("{} major", names[k])), (corr(&min, k), format!("{} minor", names[k]))]).collect();
    keys.sort_by(|a, b| b.0.total_cmp(&a.0));
    let dev = v.iter().map(|m| ((m - m.round()) * 100.0).abs()).sum::<f32>() / v.len().max(1) as f32;
    let mut s = v.clone();
    s.sort_by(f32::total_cmp);
    println!("voiced {:.1} s, notes {}, median midi {:.1}, range {:.1}–{:.1}", v.len() as f32 / an.rate, an.notes.len(), s[s.len() / 2], s[s.len() / 20], s[s.len() * 19 / 20]);
    println!("pitch classes: {}", (0..12).map(|i| format!("{}:{}", names[i], pc[i] as u32)).collect::<Vec<_>>().join(" "));
    println!("mean |deviation| from semitone grid: {dev:.1} cents");
    println!("best keys: {:?}", &keys[..4]);
}
