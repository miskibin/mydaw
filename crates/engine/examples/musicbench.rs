//! Benchmark tempo & key detection against FLP tempos on real songs in ~/Music.
use std::path::PathBuf;

fn flp_tempo(path: &std::path::Path) -> Option<f64> {
    let b = std::fs::read(path).ok()?;
    if b.get(0..4)? != b"FLhd" {
        return None;
    }
    let hl = u32::from_le_bytes(b.get(4..8)?.try_into().ok()?) as usize;
    let mut i = 8 + hl;
    if b.get(i..i + 4)? != b"FLdt" {
        return None;
    }
    i += 8;
    let mut tempo = None;
    while i < b.len() {
        let eid = b[i];
        i += 1;
        if eid < 64 {
            i += 1;
        } else if eid < 128 {
            if i + 2 > b.len() {
                break;
            }
            let v = u16::from_le_bytes([b[i], b[i + 1]]);
            i += 2;
            if eid == 66 && tempo.is_none() {
                tempo = Some(v as f64);
            }
        } else if eid < 192 {
            if i + 4 > b.len() {
                break;
            }
            let v = u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
            i += 4;
            if eid == 156 {
                tempo = Some(v as f64 / 1000.0);
            }
        } else {
            let mut n = 0usize;
            let mut s = 0;
            loop {
                if i >= b.len() {
                    return tempo;
                }
                let c = b[i];
                i += 1;
                n |= ((c & 0x7f) as usize) << s;
                s += 7;
                if c & 0x80 == 0 {
                    break;
                }
            }
            i += n;
        }
    }
    tempo
}

fn main() {
    let dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join("Music")
        });
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .expect("read_dir")
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());
    let (mut exact_05, mut exact_4pct, mut oct_ok, mut tot) = (0, 0, 0, 0);
    for e in entries {
        let p = e.path();
        if p.extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("flp"))
            != Some(true)
        {
            continue;
        }
        let Some(gt) = flp_tempo(&p) else { continue };
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let audio_path = ["mp3", "wav"]
            .iter()
            .map(|ext| dir.join(format!("{stem}.{ext}")))
            .find(|ap| ap.exists());
        let Some(ap) = audio_path else { continue };
        let Ok(audio) = wave_engine::sources::Audio::load(&ap) else {
            continue;
        };
        // Cap to first 90s for speed
        let max_samples = (90 * audio.sr as usize) * 2;
        let content = &audio.content()[..audio.content().len().min(max_samples)];
        let mono: Vec<f32> = content
            .as_chunks::<2>()
            .0
            .iter()
            .map(|f| (f[0] + f[1]) * 0.5)
            .collect();
        let m = wave_engine::music::analyze(&mono, audio.sr);
        let Some(ref t) = m.tempo else { continue };
        let (cs, _) = m.chroma_sum(0.0, 1e9);
        let key_str = wave_engine::music::estimate_key(&cs, m.tuning_cents)
            .map(|k| {
                format!(
                    "{}{} ({:.2})",
                    wave_engine::music::NOTE_NAMES[k.root as usize],
                    if k.minor { "m" } else { "" },
                    k.confidence
                )
            })
            .unwrap_or_else(|| "-".into());
        tot += 1;
        let diff = (t.bpm - gt).abs();
        let h05 = diff <= 0.5;
        let h4 = diff / gt < 0.04;
        let hoct = [0.5, 1.0, 2.0, 2.0 / 3.0, 1.5]
            .iter()
            .any(|r| (t.bpm * r - gt).abs() / gt < 0.04);
        if h05 {
            exact_05 += 1;
        }
        if h4 {
            exact_4pct += 1;
        }
        if hoct {
            oct_ok += 1;
        }
        let tag = if h05 {
            "EXACT"
        } else if h4 {
            "OK   "
        } else if hoct {
            "OCT  "
        } else {
            "BAD  "
        };
        println!(
            "{gt:6.1} -> {:6.2} [{tag}] conf={:.2}  key={key_str:12}  {stem}",
            t.bpm, t.confidence
        );
    }
    println!("\nSummary: ±0.5 BPM: {exact_05}/{tot}, <4%: {exact_4pct}/{tot}, with oct/3:2: {oct_ok}/{tot}");
}
