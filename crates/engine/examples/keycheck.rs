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
    println!("best keys (frames): {:?}", &keys[..4]);
    // note-based: each note's median pitch, weighted by its length; share in each major scale
    let mut w = [0f32; 12];
    for nb in &an.notes {
        let (i0, i1) = ((nb.start * an.rate as f64) as usize, ((nb.end * an.rate as f64) as usize).min(an.curve.len()));
        let mut v: Vec<f32> = an.curve[i0..i1].iter().copied().filter(|x| !x.is_nan()).collect();
        let len = (nb.end - nb.start) as f32;
        if v.len() < 3 || len < 0.1 {
            continue;
        }
        v.sort_by(f32::total_cmp);
        w[(v[v.len() / 2].round() as i32).rem_euclid(12) as usize] += len;
    }
    let total: f32 = w.iter().sum();
    {
        let mut ps: Vec<f32> = an.notes.iter().map(|n| n.pitch).collect();
        ps.sort_by(f32::total_cmp);
        if !ps.is_empty() {
            let med = ps[ps.len() / 2];
            let far = an.notes.iter().filter(|n| (n.pitch - med).abs() > 9.0).count();
            println!("notes pitch min {:.1} max {:.1} median {:.1}; outlier notes (>9 st from median): {far}", ps[0], ps[ps.len() - 1], med);
        }
    }
    // intonation of the notes themselves (median of each note, ignoring glides into it)
    let mut nd = vec![];
    for nb in &an.notes {
        let (i0, i1) = ((nb.start * an.rate as f64) as usize, ((nb.end * an.rate as f64) as usize).min(an.curve.len()));
        let mut v: Vec<f32> = an.curve[i0..i1].iter().copied().filter(|x| !x.is_nan()).collect();
        if v.len() >= 5 {
            v.sort_by(f32::total_cmp);
            let m = v[v.len() / 2];
            nd.push(((m - m.round()) * 100.0).abs());
        }
    }
    nd.sort_by(f32::total_cmp);
    if !nd.is_empty() {
        println!("note medians: {} notes, median |off| {:.1} cents, 80th pct {:.1} cents", nd.len(), nd[nd.len() / 2], nd[nd.len() * 4 / 5]);
    }
    let major = [0usize, 2, 4, 5, 7, 9, 11];
    let mut fit: Vec<(f32, &str)> = (0..12).map(|k| (major.iter().map(|d| w[(k + d) % 12]).sum::<f32>() / total.max(1e-6), names[k])).collect();
    fit.sort_by(|a, b| b.0.total_cmp(&a.0));
    println!("note weights (s): {}", (0..12).map(|i| format!("{}:{:.1}", names[i], w[i])).collect::<Vec<_>>().join(" "));
    println!("major scales by share of sung time in scale (relative minor = 6th degree): {:?}", &fit[..4]);
}
