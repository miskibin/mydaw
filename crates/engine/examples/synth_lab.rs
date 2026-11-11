//! Measurements and demo renders of the built-in synth (`crate::poly`).
//!
//!   cargo run --release -p wave-engine --example synth_lab -- alias
//!   cargo run --release -p wave-engine --example synth_lab -- levels
//!   cargo run --release -p wave-engine --example synth_lab -- clicks
//!   cargo run --release -p wave-engine --example synth_lab -- cpu
//!   cargo run --release -p wave-engine --example synth_lab -- demos <dir>

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use wave_engine::poly::params::{ix, Patch};
use wave_engine::poly::{lab, presets, tables, voice, SynthInst};
use wave_engine::synth::Dest;
use wave_engine::*;

const SR: f32 = 48000.0;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str).unwrap_or("") {
        "alias" => alias(),
        "levels" => levels(),
        "clicks" => clicks(),
        "cpu" => cpu(),
        "clickdbg" => clickdbg(&args[1]),
        "demos" => demos(Path::new(args.get(1).map(String::as_str).unwrap_or("synth_demos"))),
        _ => eprintln!("usage: synth_lab alias|levels|clicks|cpu|demos <dir>"),
    }
}

fn mtof(k: f64) -> f64 {
    440.0 * 2f64.powf((k - 69.0) / 12.0)
}

fn alias() {
    println!("| MIDI | Hz | saw full | saw ≤20k | square full | square ≤20k | pulse 25% full |");
    println!("|---|---|---|---|---|---|---|");
    let mut worst = [f64::INFINITY; 5];
    for k in 24..=108 {
        let f = mtof(k as f64);
        let saw = voice::render_osc(tables::SAW, 0.0, f as f32, SR, SR as usize);
        let sq = voice::render_osc(tables::SQUARE, 0.0, f as f32, SR, SR as usize);
        let pw = voice::render_osc(tables::SQUARE, 0.555, f as f32, SR, SR as usize);
        let v = [
            lab::alias_snr(&saw, f, SR as f64, SR as f64 / 2.0),
            lab::alias_snr(&saw, f, SR as f64, 20000.0),
            lab::alias_snr(&sq, f, SR as f64, SR as f64 / 2.0),
            lab::alias_snr(&sq, f, SR as f64, 20000.0),
            lab::alias_snr(&pw, f, SR as f64, SR as f64 / 2.0),
        ];
        for (w, x) in worst.iter_mut().zip(v) {
            *w = w.min(x);
        }
        if k % 12 == 0 || k == 108 {
            println!("| {k} | {f:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} |", v[0], v[1], v[2], v[3], v[4]);
        }
    }
    println!("| worst 24–108 | | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} |", worst[0], worst[1], worst[2], worst[3], worst[4]);
}

/// Loudness target per category (basses and pads a little lower).
fn target(cat: &str) -> f64 {
    match cat {
        "808" | "Bas" | "Pad" => -19.0,
        _ => -18.0,
    }
}

fn levels() {
    println!("| preset | category | key | LUFS-I | dBTP | trim now | trim suggested |");
    println!("|---|---|---|---|---|---|---|");
    for p in presets::all() {
        let (l, r) = lab::probe(&p, SR);
        let (lu, tp) = lab::loudness(&l, &r, SR);
        let now = p.patch[ix::FX_OUT] as f64;
        // polyphonic patches play POLY_HEADROOM (−12 dB) lower so chords fit
        let hr = if p.patch[ix::V_MODE] == 0.0 { 20.0 * (wave_engine::poly::POLY_HEADROOM as f64).log10() } else { 0.0 };
        let mut delta = target(p.category) + hr - lu;
        if tp + delta > -3.0 + hr {
            delta = -3.0 + hr - tp;
        }
        println!("| {} | {} | {} | {:.1} | {:.1} | {:.1} | {:.1} |", p.name, p.category, p.probe_key(), lu, tp, now, now + delta);
    }
}

fn count_clicks(l: &[f32], r: &[f32]) -> usize {
    let (a, b) = (lab::click_flags(l, SR, 3.0), lab::click_flags(r, SR, 3.0));
    if std::env::var("SYNTH_DBG").is_ok() {
        for &i in &a {
            let ctx: Vec<String> = l[i.saturating_sub(4)..(i + 4).min(l.len())].iter().map(|v| format!("{v:+.4}")).collect();
            eprintln!("  flag L at {:.2} ms: {}", i as f32 / SR * 1000.0, ctx.join(" "));
        }
    }
    a.len() + b.len()
}

/// The click scenarios; returns flags per scenario.
fn click_scenarios(p: &Patch, next: &Patch, key: u8) -> [usize; 6] {
    let ms = |x: f32| (x * SR / 1000.0) as u32;
    let mut out = [0usize; 6];
    // a) note on/off
    let mut s = SynthInst::new(1, *p, SR);
    for i in 0..4 {
        s.note(ms(400.0 * i as f32), ms(300.0), key + (i * 3) as u8, 0.8, Dest::Click);
    }
    let (l, r) = lab::run(&mut s, 3.5, SR);
    out[0] = count_clicks(&l, &r);
    // b) retrigger the same key, short gaps and overlaps
    let mut s = SynthInst::new(1, *p, SR);
    for i in 0..8 {
        s.note(ms(120.0 * i as f32), ms(if i % 2 == 0 { 100.0 } else { 200.0 }), key, 0.8, Dest::Click);
    }
    let (l, r) = lab::run(&mut s, 3.0, SR);
    out[1] = count_clicks(&l, &r);
    // c) voice stealing: 20 notes stacked
    let mut s = SynthInst::new(1, *p, SR);
    for i in 0..20u32 {
        s.note(ms(30.0 * i as f32), ms(1500.0), (key as u32 - 12 + 2 * i) as u8, 0.8, Dest::Click);
    }
    let (l, r) = lab::run(&mut s, 4.0, SR);
    out[2] = count_clicks(&l, &r);
    // d) mono legato with glide (overlapping notes)
    let mut q = *p;
    q[ix::V_MODE] = 2.0;
    q[ix::V_GLIDE] = 80.0;
    let mut s = SynthInst::new(1, q, SR);
    for (i, k) in [0u8, 7, 3, 12, 5].iter().enumerate() {
        s.note(ms(250.0 * i as f32), ms(320.0), key + k, 0.8, Dest::Click);
    }
    let (l, r) = lab::run(&mut s, 3.0, SR);
    out[3] = count_clicks(&l, &r);
    // e) mono (retrigger) with glide
    q[ix::V_MODE] = 1.0;
    let mut s = SynthInst::new(1, q, SR);
    for (i, k) in [0u8, 7, 3, 12, 5].iter().enumerate() {
        s.note(ms(250.0 * i as f32), ms(320.0), key + k, 0.8, Dest::Click);
    }
    let (l, r) = lab::run(&mut s, 3.0, SR);
    out[4] = count_clicks(&l, &r);
    // f) preset change mid-note
    let mut s = SynthInst::new(1, *p, SR);
    s.note(0, ms(1500.0), key, 0.8, Dest::Click);
    s.note(ms(100.0), ms(1400.0), key + 7, 0.8, Dest::Click);
    let (mut l, mut r) = lab::run(&mut s, 0.7, SR);
    s.set_patch(next);
    let (l2, r2) = lab::run(&mut s, 2.0, SR);
    l.extend(l2);
    r.extend(r2);
    out[5] = count_clicks(&l, &r);
    out
}

fn clickdbg(name: &str) {
    let p = presets::all().into_iter().find(|p| p.name == name).unwrap();
    let ms = |x: f32| (x * SR / 1000.0) as u32;
    let key = p.probe_key();
    let mut s = SynthInst::new(1, p.patch, SR);
    for i in 0..4 {
        s.note(ms(400.0 * i as f32), ms(300.0), key + (i * 3) as u8, 0.8, Dest::Click);
    }
    let (l, _r) = lab::run(&mut s, 3.5, SR);
    for i in lab::click_flags(&l, SR, 3.0) {
        println!("on/off flag at {:.2} ms: {:?}", i as f32 / SR * 1000.0, &l[i - 3..i + 3]);
    }
    let mut s = SynthInst::new(1, p.patch, SR);
    for i in 0..8 {
        s.note(ms(120.0 * i as f32), ms(if i % 2 == 0 { 100.0 } else { 200.0 }), key, 0.8, Dest::Click);
    }
    let (l, _r) = lab::run(&mut s, 3.0, SR);
    for i in lab::click_flags(&l, SR, 3.0) {
        println!("retrig flag at {:.2} ms: {:?}", i as f32 / SR * 1000.0, &l[i - 3..i + 3]);
    }
}

fn clicks() {
    // the detector itself: a hard cut and a step must be caught, a clean sine must not
    let sine: Vec<f32> = (0..48000).map(|i| (i as f32 * 0.0137).sin() * 0.5).collect();
    let mut cut = sine.clone();
    cut[24000..].fill(0.0);
    let mut step = sine.clone();
    step[30000..].iter_mut().for_each(|v| *v += 0.05);
    let saw: Vec<f32> = voice::render_osc(tables::SAW, 0.0, 110.0, SR, 48000).iter().map(|v| v * 0.5).collect();
    let mut saw_cut = saw.clone();
    saw_cut[24011..].fill(0.0);
    let mut faded = sine.clone();
    for (i, v) in faded[24000..].iter_mut().enumerate() {
        *v *= (1.0 - i as f32 / 192.0).max(0.0);
    }
    let f = |x: &[f32]| lab::click_flags(x, SR, 3.0).len();
    println!("detector check: clean sine {}, hard cut {}, +0.05 step {}, 4 ms fade-out {}, clean saw {}, saw cut mid-ramp {}", f(&sine), f(&cut), f(&step), f(&faded), f(&saw), f(&saw_cut));
    println!("| preset | on/off | retrigger | steal (20 notes) | legato+glide | mono+glide | preset change |");
    println!("|---|---|---|---|---|---|---|");
    let all = presets::all();
    let mut total = [0usize; 6];
    for (i, p) in all.iter().enumerate() {
        let next = &all[(i + 1) % all.len()].patch;
        if std::env::var("SYNTH_DBG").is_ok() {
            eprintln!("{}", p.name);
        }
        let c = click_scenarios(&p.patch, next, p.probe_key());
        for (t, v) in total.iter_mut().zip(c) {
            *t += v;
        }
        println!("| {} | {} | {} | {} | {} | {} | {} |", p.name, c[0], c[1], c[2], c[3], c[4], c[5]);
    }
    println!("| total | {} | {} | {} | {} | {} | {} |", total[0], total[1], total[2], total[3], total[4], total[5]);
}

fn cpu() {
    let block = 64usize;
    let secs = 10.0;
    let mut s = lab::heaviest_instance(SR, secs);
    let (mut l, mut r) = (vec![0f32; block], vec![0f32; block]);
    let blocks = (secs * SR) as usize / block;
    let mut worst = 0f64;
    let mut times = Vec::with_capacity(blocks);
    let t0 = Instant::now();
    for _ in 0..blocks {
        l.fill(0.0);
        r.fill(0.0);
        let t = Instant::now();
        s.render_into(&mut l, &mut r, 120.0);
        let us = t.elapsed().as_secs_f64() * 1e6;
        worst = worst.max(us);
        times.push(us);
    }
    let total = t0.elapsed().as_secs_f64();
    let first: Vec<String> = times.iter().take(4).map(|t| format!("{t:.0}")).collect();
    let worst_after = times.iter().skip(1).fold(0f64, |m, &t| m.max(t));
    times.sort_by(|a, b| a.total_cmp(b));
    let budget = block as f64 / SR as f64 * 1e6;
    println!("voices sounding: {}", s.active_voices());
    println!("16 voices × (2 osc × 7 unison), LP24 + drive, all FX, 48 kHz, 64-sample blocks:");
    println!("  mean load {:.1} % of one core, median block {:.1} µs, p99 {:.1} µs, worst {:.1} µs (budget {:.0} µs = {:.1} %), worst after the first block {:.1} µs; first blocks µs: {}", total / secs as f64 * 100.0, times[times.len() / 2], times[times.len() * 99 / 100], worst, budget, worst / budget * 100.0, worst_after, first.join(", "));
}

// ---------------------------------------------------------------- demos

struct Song {
    bpm: f64,
    beats: f64,
    notes: Vec<(f32, f32, u8, f32)>,
    hats: bool,
}

fn synth_chan(id: Id, preset: &str, root: u8) -> Channel {
    Channel {
        id,
        name: preset.into(),
        color: "#9b7cf8".into(),
        src: ChannelSrc::Synth { synth: wave_engine::poly::preset_state(preset).unwrap() },
        vol: VOL_UNITY,
        pan: 0.0,
        mute: false,
        solo: false,
        insert: None,
        root,
    }
}

fn render_song(preset: &str, song: &Song) -> Vec<f32> {
    let mut p = Project::empty();
    p.inserts[0].fx.clear();
    p.bpm = song.bpm;
    let id = p.alloc_id();
    p.channels.push(synth_chan(id, preset, 60));
    let mut notes: Vec<Note> = song
        .notes
        .iter()
        .map(|&(step, len, key, v)| {
            let mut n = Note::new(id, step, len, Some(key));
            n.v = v;
            n
        })
        .collect();
    if song.hats {
        let hat = p.channels.iter().find(|c| c.src == ChannelSrc::Inst { inst: Inst::Hat }).unwrap().id;
        let kick = p.channels.iter().find(|c| c.src == ChannelSrc::Inst { inst: Inst::Kick }).unwrap().id;
        let clap = p.channels.iter().find(|c| c.src == ChannelSrc::Inst { inst: Inst::Clap }).unwrap().id;
        let bars = (song.beats / 4.0) as usize;
        for b in 0..bars {
            let o = b as f32 * 16.0;
            for s in (0..16).step_by(2) {
                notes.push(Note::new(hat, o + s as f32, 1.0, None));
            }
            if b % 2 == 1 {
                // a trap hat roll
                for k in 0..6 {
                    let mut n = Note::new(hat, o + 12.0 + k as f32 * 0.6667, 0.5, None);
                    n.v = 0.7;
                    notes.push(n);
                }
            }
            notes.push(Note::new(clap, o + 8.0, 1.0, None));
            notes.push(Note::new(kick, o, 1.0, None));
        }
    }
    p.patterns = vec![Pattern { id: p.alloc_id(), name: "Demo".into(), color: "#4f8ef7".into(), len: song.beats, notes, swing: 0.0 }];
    let pid = p.patterns[0].id;
    let cid = p.alloc_id();
    p.clips = vec![Clip { id: cid, track: 0, start: 0.0, len: song.beats, offset: 0.0, kind: ClipKind::Pat, source: pid, muted: false, gain: 0.0, fade_in: 0.0, fade_out: 0.0, pan: 0.0, norm: false, rev: false, tune: None }];
    Engine::new(p, Arc::new(NoPlugins)).render(0.0, song.beats, 2.5, SR as u32).unwrap()
}

/// (step, len, key, velocity) helpers
fn n(step: f32, len: f32, key: u8) -> (f32, f32, u8, f32) {
    (step, len, key, 0.85)
}

fn song_for(cat: &str) -> Song {
    let mut v = vec![];
    match cat {
        "808" => {
            // 4 bars at 140: long 808s, slides where notes overlap
            let line: [(f32, f32, u8); 10] = [(0.0, 6.0, 36), (6.0, 4.0, 36), (10.0, 6.0, 43), (16.0, 8.0, 34), (23.0, 3.0, 46), (26.0, 6.0, 34), (32.0, 6.0, 36), (38.0, 6.0, 41), (43.0, 5.0, 39), (48.0, 16.0, 31)];
            for (s, l, k) in line {
                v.push(n(s, l, k));
            }
            return Song { bpm: 140.0, beats: 16.0, notes: v, hats: true };
        }
        "Bas" => {
            let line = [36u8, 36, 48, 36, 39, 36, 43, 41];
            for bar in 0..4 {
                for (i, k) in line.iter().enumerate() {
                    v.push(n(bar as f32 * 16.0 + i as f32 * 2.0, 1.6, *k - if bar >= 2 { 4 } else { 0 }));
                }
            }
            return Song { bpm: 124.0, beats: 16.0, notes: v, hats: true };
        }
        "Pluck" | "Arp" | "Dzwonki" => {
            let arp = [60u8, 63, 67, 70, 72, 70, 67, 63];
            let roots = [0i8, -4, -7, -2];
            for (bar, r) in roots.iter().enumerate() {
                for s in 0..16 {
                    let k = (arp[s % 8] as i8 + r) as u8;
                    let step = bar as f32 * 16.0 + s as f32;
                    if cat == "Dzwonki" && s % 2 == 1 {
                        continue;
                    }
                    v.push((step, if cat == "Dzwonki" { 2.0 } else { 1.0 }, k, if s % 4 == 0 { 0.95 } else { 0.75 }));
                }
            }
        }
        "Lead" => {
            let mel: [(f32, f32, u8); 12] = [(0.0, 3.0, 72), (3.0, 3.0, 75), (6.0, 2.0, 77), (8.0, 6.0, 79), (16.0, 2.0, 79), (18.0, 2.0, 77), (20.0, 4.0, 75), (24.0, 8.0, 72), (32.0, 3.0, 70), (35.0, 5.0, 72), (40.0, 8.0, 75), (48.0, 14.0, 74)];
            for (s, l, k) in mel {
                v.push(n(s, l + 0.3, k));
            }
        }
        "Pad" | "Dęte/Smyczki" | "Klawisze" | "Lo-fi" => {
            let chords: [[u8; 4]; 4] = [[48, 58, 62, 63], [44, 55, 60, 63], [51, 58, 62, 67], [46, 55, 58, 62]];
            for (bar, c) in chords.iter().enumerate() {
                for &k in c {
                    if cat == "Klawisze" || cat == "Lo-fi" {
                        for hit in [0.0f32, 6.0, 10.0] {
                            v.push(n(bar as f32 * 16.0 + hit, 3.5, k + 12));
                        }
                    } else {
                        v.push(n(bar as f32 * 16.0, 15.5, k + 12));
                    }
                }
            }
            if cat == "Lo-fi" {
                return Song { bpm: 82.0, beats: 16.0, notes: v, hats: false };
            }
        }
        _ => {
            v.push(n(0.0, 30.0, 60));
            return Song { bpm: 120.0, beats: 8.0, notes: v, hats: false };
        }
    }
    Song { bpm: 120.0, beats: 16.0, notes: v, hats: false }
}

fn write_mp3(dir: &Path, name: &str, x: &[f32]) {
    let wav = dir.join(format!("{name}.wav"));
    wave_engine::sources::write_wav(&wav, x, SR as u32).unwrap();
    let mp3 = dir.join(format!("{name}.mp3"));
    let ok = std::process::Command::new("ffmpeg").args(["-y", "-loglevel", "error", "-i"]).arg(&wav).args(["-codec:a", "libmp3lame", "-b:a", "192k"]).arg(&mp3).status().map(|s| s.success()).unwrap_or(false);
    if ok {
        let _ = std::fs::remove_file(&wav);
        println!("{}", mp3.display());
    } else {
        println!("{} (ffmpeg failed; kept WAV)", wav.display());
    }
}

fn slug(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect()
}

fn demos(dir: &Path) {
    let probes: PathBuf = dir.join("probes");
    std::fs::create_dir_all(&probes).unwrap();
    let all = presets::all();
    for p in &all {
        let (l, r) = lab::probe(p, SR);
        let x: Vec<f32> = l.iter().zip(&r).flat_map(|(a, b)| [*a, *b]).collect();
        wave_engine::sources::write_wav(&probes.join(format!("{}.wav", slug(p.name))), &x, SR as u32).unwrap();
    }
    println!("probes: {}", probes.display());
    for cat in presets::CATEGORIES {
        let song = song_for(cat);
        let mut out: Vec<f32> = vec![];
        for p in all.iter().filter(|p| p.category == *cat) {
            let x = render_song(p.name, &song);
            out.extend_from_slice(&x);
            out.extend(std::iter::repeat_n(0.0, (SR * 0.5) as usize * 2));
        }
        let pk = out.iter().fold(0f32, |m, v| m.max(v.abs()));
        if pk > 0.97 {
            let g = 0.97 / pk;
            out.iter_mut().for_each(|v| *v *= g);
            eprintln!("{cat}: peak {pk:.2}, scaled to 0.97");
        }
        write_mp3(dir, &format!("{:02}_{}", presets::CATEGORIES.iter().position(|c| c == cat).unwrap() + 1, slug(cat)), &out);
    }
}
