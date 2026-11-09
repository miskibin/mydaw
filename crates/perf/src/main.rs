//! `wave-perf`: fixture generator and engine benchmark runner.
//!
//!   wave-perf gen   [--cache DIR] [sizes...]
//!   wave-perf bench [--cache DIR] [--sizes small,medium,large] [--quick] [--out FILE]
//!
//! Results are one JSON document on stdout / `--out`. Everything runs on the CPU, single
//! process; the "audio thread" is a plain thread that calls `Rt::process` the way the
//! device callback would.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use wave_engine::catalog::new_fx;
use wave_engine::midi::MidiMsg;
use wave_engine::*;
use wave_perf::fixture::{self, Spec};
use wave_perf::meter::*;

#[global_allocator]
static ALLOC: Counting = Counting;

const SR: f32 = 48000.0;

static ONLY: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
/// `--only fx,synth,rt,ops,export,analysis,edit,midi,telemetry,undo` limits the run to some sections.
fn want(sec: &str) -> bool {
    ONLY.get()
        .map(|o| o.is_empty() || o.iter().any(|x| x == sec))
        .unwrap_or(true)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("bench");
    let mut cache = PathBuf::from("perf/.cache");
    let mut sizes: Vec<String> = vec![];
    let mut out: Option<PathBuf> = None;
    let mut quick = false;
    let mut full_export = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--cache" => {
                cache = PathBuf::from(&args[i + 1]);
                i += 1;
            }
            "--sizes" => {
                sizes = args[i + 1].split(',').map(String::from).collect();
                i += 1;
            }
            "--out" => {
                out = Some(PathBuf::from(&args[i + 1]));
                i += 1;
            }
            "--only" => {
                let _ = ONLY.set(args[i + 1].split(',').map(String::from).collect());
                i += 1;
            }
            "--quick" => quick = true,
            "--full-export" => full_export = true,
            s => sizes.push(s.to_string()),
        }
        i += 1;
    }
    if sizes.is_empty() {
        sizes = vec!["small".into(), "medium".into(), "large".into()];
    }
    match cmd {
        "gen" => {
            for s in &sizes {
                let sp = fixture::spec(s).unwrap_or_else(|| panic!("unknown size {s}"));
                let (g, ms) = time_ms(|| fixture::generate(&sp, &cache));
                eprintln!(
                    "{s}: {} clips, {} tracks -> {} ({ms:.0} ms)",
                    g.project.clips.len(),
                    g.project.tracks.len(),
                    g.wave.display()
                );
            }
        }
        _ => {
            let doc = bench_all(&cache, &sizes, quick, full_export);
            let text = serde_json::to_string_pretty(&doc).unwrap();
            if let Some(o) = out {
                std::fs::create_dir_all(o.parent().unwrap_or(Path::new("."))).ok();
                std::fs::write(&o, &text).unwrap();
                eprintln!("wrote {}", o.display());
            } else {
                println!("{text}");
            }
        }
    }
}

fn r2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn bench_all(cache: &Path, sizes: &[String], quick: bool, full_export: bool) -> Value {
    let mut doc = json!({
        "meta": { "sample_rate": SR, "block": 512, "quick": quick, "cpus": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
                  "engine_version": env!("CARGO_PKG_VERSION") },
        "fixtures": {}, "rt_project": {}, "ops": {}, "io": {}, "export": {},
    });
    // ---- per-effect cost (independent of project size)
    if want("fx") {
        eprintln!("[fx] per-effect real-time cost");
        doc["rt_fx"] = bench_fx(cache, quick);
    }
    if want("synth") {
        eprintln!("[synth] built-in synth, heaviest patch");
        doc["rt_synth"] = bench_synth(quick);
    }

    for name in sizes {
        let sp = fixture::spec(name).unwrap_or_else(|| panic!("unknown size {name}"));
        let dir = cache.join(sp.name);
        let wave = dir.join(format!("{}.wave", sp.name));
        if !wave.exists() {
            eprintln!("[{name}] generating fixture");
            fixture::generate(&sp, cache);
        }
        let wav_mb: f64 = std::fs::read_dir(dir.join("audio"))
            .map(|d| {
                d.flatten()
                    .map(|e| e.metadata().map(|m| m.len()).unwrap_or(0))
                    .sum::<u64>() as f64
                    / 1048576.0
            })
            .unwrap_or(0.0);
        let wave_bytes = std::fs::metadata(&wave).map(|m| m.len()).unwrap_or(0);
        let proj_bytes = std::fs::metadata(dir.join("project.json"))
            .map(|m| m.len())
            .unwrap_or(0);

        // ---- project load
        eprintln!("[{name}] load");
        let rss0 = rss_mib();
        let e = Engine::new(Project::empty(), Arc::new(NoPlugins));
        let (warn, open_ms) = time_ms(|| e.open(&wave).expect("open"));
        let rss1 = rss_mib();
        let p = e.project();
        doc["fixtures"][name] = json!({
            "tracks": p.tracks.len(), "inserts": p.inserts.len(), "fx": p.inserts.iter().map(|i| i.fx.len()).sum::<usize>(),
            "clips": p.clips.len(), "audio_clips": p.clips.iter().filter(|c| c.kind == ClipKind::Audio).count(),
            "patterns": p.patterns.len(), "notes": p.patterns.iter().map(|x| x.notes.len()).sum::<usize>(),
            "sources": p.sources.len(), "automations": p.automations.len(), "comps": p.comps.len(),
            "beats": p.song_end(), "minutes": p.song_end() * 60.0 / p.bpm / 60.0, "bpm": p.bpm,
            "wav_mib": r2(wav_mb), "wave_file_kib": wave_bytes / 1024, "project_json_kib": proj_bytes / 1024,
        });
        let (_, save_ms) = time_ms(|| e.save(&dir.join("resave.wave")).unwrap());
        let (_, state_ms) = time_ms(|| e.state());
        let (bytes, ser_ms) = time_ms(|| serde_json::to_vec(&e.state()).unwrap());
        let (_, clone_ms) = time_ms(|| e.project());
        let (reparse, parse_ms) = time_ms(|| {
            let t = std::fs::read_to_string(&wave).unwrap();
            let v: Value = serde_json::from_str(&t).unwrap();
            serde_json::from_value::<Project>(v["project"].clone()).unwrap()
        });
        let _ = reparse;
        let (e2, new_ms) = time_ms(|| {
            let e2 = Engine::new(Project::empty(), Arc::new(NoPlugins));
            e2.new_project(p.clone());
            e2
        });
        drop(e2);
        // load with a device attached: sync builds graph, timeline and renders tuned clips
        let e3 = Engine::new(Project::empty(), Arc::new(NoPlugins));
        let _rt = e3.create_rt(SR, 512);
        let (_, open_rt_ms) = time_ms(|| e3.open(&wave).unwrap());
        drop(_rt);
        drop(e3);
        doc["io"][name] = json!({
            "open_ms_no_device": r2(open_ms), "open_ms_with_device": r2(open_rt_ms), "open_warnings": warn.len(),
            "open_rss_delta_mib": r2(rss1 - rss0), "save_ms": r2(save_ms), "state_snapshot_ms": r2(state_ms),
            "state_json_ms": r2(ser_ms), "state_json_kib": bytes.len() / 1024, "project_clone_ms": r2(clone_ms),
            "json_parse_ms": r2(parse_ms), "new_project_ms": r2(new_ms),
        });
        drop(e);

        // ---- full-project real-time rendering
        if want("rt") {
            eprintln!("[{name}] real-time render");
            doc["rt_project"][name] = bench_rt_project(&wave, quick);
        }
        // ---- ops
        if want("ops") {
            eprintln!("[{name}] ops");
            doc["ops"][name] = bench_ops(&wave, &sp, quick);
        }
        // ---- offline export
        if want("export") {
            eprintln!("[{name}] export");
            doc["export"][name] = bench_export(&wave, quick, full_export);
        }
    }
    if sizes.iter().any(|s| s == "large") || sizes.iter().any(|s| s == "medium") {
        let name = if sizes.iter().any(|s| s == "large") {
            "large"
        } else {
            "medium"
        };
        let wave = cache.join(name).join(format!("{name}.wave"));
        if want("analysis") {
            eprintln!("[{name}] analysis / comp / peaks");
            doc["analysis"] = bench_analysis(&wave, name);
        }
        if want("edit") {
            eprintln!("[{name}] audio-thread safety under edits");
            doc["rt_under_edit"] = bench_rt_under_edit(&wave, quick);
        }
        if want("midi") {
            eprintln!("[{name}] midi");
            doc["midi"] = bench_midi(&wave, quick);
        }
        if want("telemetry") {
            eprintln!("[{name}] telemetry / housekeeping");
            doc["telemetry"] = bench_telemetry(&wave, quick);
        }
        if want("undo") {
            eprintln!("[{name}] undo memory");
            doc["undo_memory"] = bench_undo_memory(&wave, quick);
        }
    }
    doc
}

// ---------------------------------------------------------------- audio-thread helpers

struct Sim {
    stop: Arc<AtomicBool>,
    h: std::thread::JoinHandle<(SimResult, Rt)>,
}
struct SimResult {
    times_us: Vec<f64>,
    allocs: u64,
    alloc_bytes: u64,
    late: usize,
    period_us: f64,
    audio_secs: f64,
}

/// A thread that calls `Rt::process` once per period, like a device callback.
fn start_sim(mut rt: Rt, block: usize, realtime: bool) -> Sim {
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    let h = std::thread::Builder::new()
        .name("sim-audio".into())
        .spawn(move || {
            let period = Duration::from_secs_f64(block as f64 / SR as f64);
            let mut l = vec![0f32; block];
            let mut r = vec![0f32; block];
            let mut times = vec![];
            let (mut allocs, mut bytes, mut late) = (0u64, 0u64, 0usize);
            let mut next = Instant::now() + period;
            let mut blocks = 0usize;
            while !s2.load(Ordering::Relaxed) {
                let t = Instant::now();
                let (_, a) = count_allocs(|| rt.process(&mut l, &mut r, None));
                let dt = t.elapsed();
                times.push(dt.as_secs_f64() * 1e6);
                allocs += a.allocs;
                bytes += a.bytes;
                blocks += 1;
                if dt > period {
                    late += 1;
                }
                if realtime {
                    let now = Instant::now();
                    if next > now {
                        std::thread::sleep(next - now);
                    }
                    next += period;
                }
            }
            (
                SimResult {
                    times_us: times,
                    allocs,
                    alloc_bytes: bytes,
                    late,
                    period_us: period.as_secs_f64() * 1e6,
                    audio_secs: blocks as f64 * block as f64 / SR as f64,
                },
                rt,
            )
        })
        .unwrap();
    Sim { stop, h }
}

impl Sim {
    fn finish(self) -> SimResult {
        self.finish_keep().0
    }
    fn finish_keep(self) -> (SimResult, Rt) {
        self.stop.store(true, Ordering::Relaxed);
        self.h.join().unwrap()
    }
}

impl SimResult {
    fn json(&self) -> Value {
        let n = self.times_us.len().max(1) as f64;
        let total: f64 = self.times_us.iter().sum();
        let st = stats(self.times_us.clone());
        json!({
            "blocks": self.times_us.len(), "block_us": st.json(), "deadline_us": r2(self.period_us),
            "late_blocks": self.late, "cpu_load_mean_pct": r2(total / n / self.period_us * 100.0),
            "cpu_load_p99_pct": r2(st.p99 / self.period_us * 100.0), "cpu_load_max_pct": r2(st.max / self.period_us * 100.0),
            "audio_allocs": self.allocs, "audio_alloc_bytes": self.alloc_bytes, "audio_secs": r2(self.audio_secs),
        })
    }
}

/// Renders `blocks` blocks synchronously (as fast as possible) and returns the stats.
fn rt_sync(e: &Arc<Engine>, block: usize, from: f64, secs: f64) -> Value {
    let mut rt = e.create_rt(SR, block);
    if from >= 0.0 {
        e.play(from); // a negative `from` leaves the transport stopped (idle graph cost)
    }
    let mut l = vec![0f32; block];
    let mut r = vec![0f32; block];
    let warm = (0.5 * SR as f64 / block as f64) as usize;
    for _ in 0..warm {
        rt.process(&mut l, &mut r, None);
    }
    let blocks = (secs * SR as f64 / block as f64) as usize;
    let mut times = Vec::with_capacity(blocks);
    let (mut allocs, mut bytes) = (0, 0);
    let t0 = Instant::now();
    for _ in 0..blocks {
        let t = Instant::now();
        let (_, a) = count_allocs(|| rt.process(&mut l, &mut r, None));
        times.push(t.elapsed().as_secs_f64() * 1e6);
        allocs += a.allocs;
        bytes += a.bytes;
    }
    let wall = t0.elapsed().as_secs_f64();
    let period = block as f64 / SR as f64 * 1e6;
    let st = stats(times.clone());
    let silent = l.iter().chain(r.iter()).all(|x| *x == 0.0);
    e.stop();
    drop(rt);
    json!({
        "block": block, "blocks": blocks, "block_us": st.json(), "deadline_us": r2(period),
        "rtf": r2(blocks as f64 * block as f64 / SR as f64 / wall), "cpu_load_mean_pct": r2(st.mean / period * 100.0),
        "cpu_load_p99_pct": r2(st.p99 / period * 100.0), "cpu_load_max_pct": r2(st.max / period * 100.0),
        "late_blocks": times.iter().filter(|t| **t > period).count(), "audio_allocs": allocs, "audio_alloc_bytes": bytes,
        "last_block_silent": silent,
    })
}

// ---------------------------------------------------------------- per-effect cost

fn bench_fx(cache: &Path, quick: bool) -> Value {
    let dir = cache.join("fxbench");
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join("vox.wav");
    if !wav.exists() {
        let n = 30 * 48000;
        let mut v = vec![0f32; n * 2];
        // voice-like: reuse the project synth through a tiny local copy
        let mut ph = 0f32;
        for i in 0..n {
            let t = i as f32 / 48000.0;
            let f0 = 150.0
                * 2f32.powf(((t * 1.7).floor() % 9.0) / 12.0)
                * (1.0 + 0.012 * (std::f32::consts::TAU * 5.5 * t).sin());
            ph += std::f32::consts::TAU * f0 / 48000.0;
            let mut s = 0.0;
            for h in 1..=12 {
                let hf = f0 * h as f32;
                s += (ph * h as f32).sin() * (-((hf - 800.0) / 600.0).powi(2)).exp() / h as f32;
            }
            let env = if (t * 1.7).fract() < 0.9 { 1.0 } else { 0.0 };
            let noise = ((i as u32).wrapping_mul(2654435761) >> 16) as f32 / 65536.0 - 0.5;
            let x = s * 0.4 * env + noise * 0.01;
            v[i * 2] = x;
            v[i * 2 + 1] = x;
        }
        wave_engine::sources::write_wav(&wav, &v, 48000).unwrap();
    }
    let e = Engine::new(Project::empty(), Arc::new(NoPlugins));
    let paths: Vec<PathBuf> = (0..8).map(|_| wav.clone()).collect();
    e.import_audio(&paths, Some((0, 0.0))).unwrap();
    // 8 tracks, each with the voice clip; the loop is the whole 30 s
    let secs = if quick { 3.0 } else { 8.0 };
    let base = rt_sync(&e, 512, 0.0, secs);
    let base_ms = base["cpu_load_mean_pct"].as_f64().unwrap();
    let mut out = json!({ "baseline_8_tracks_no_fx": base, "per_kind": {} });
    let kinds = [
        FxKind::Eq,
        FxKind::Comp,
        FxKind::Fet,
        FxKind::Deess,
        FxKind::Sat,
        FxKind::Delay,
        FxKind::Reverb,
        FxKind::Limit,
        FxKind::Tune,
        FxKind::Denoise,
        FxKind::Voice,
        FxKind::Rider,
    ];
    let mut next_id = 90_000;
    for k in kinds {
        let p = e.project();
        let mut ops = vec![];
        for i in 1..=8u64 {
            next_id += 1;
            ops.push(Op::PutFx {
                insert_id: i,
                fx: new_fx(next_id, k, p.bpm),
                index: None,
            });
        }
        e.apply(&ops, false, None).unwrap();
        let m = rt_sync(&e, 512, 0.0, secs);
        let cpu = m["cpu_load_mean_pct"].as_f64().unwrap();
        let per_inst = ((cpu - base_ms) / 8.0).max(0.0);
        out["per_kind"][format!("{k:?}").to_lowercase()] = json!({
            "cpu_load_mean_pct_8_instances": cpu, "cpu_pct_per_instance": r2(per_inst),
            "instances_for_100pct": if per_inst > 0.0 { r2(100.0 / per_inst) } else { 0.0 },
            "rtf_per_instance": if per_inst > 0.0 { r2(100.0 / per_inst) } else { 0.0 },
            "block_us": m["block_us"], "late_blocks": m["late_blocks"], "audio_allocs": m["audio_allocs"], "cpu_load_max_pct": m["cpu_load_max_pct"],
        });
        let rm: Vec<Op> = e
            .project()
            .inserts
            .iter()
            .skip(1)
            .take(8)
            .flat_map(|i| i.fx.iter().map(|f| Op::RemoveFx { fx_id: f.id }))
            .collect();
        e.apply(&rm, false, None).unwrap();
    }
    out
}

/// The built-in synth's worst case: 16 voices of the heaviest patch (2 oscillators × 7 unison,
/// LP24 with drive, all effects), 48 kHz, 64-sample blocks, one instance on this thread.
fn bench_synth(quick: bool) -> Value {
    let block = 64usize;
    let secs = if quick { 3.0 } else { 10.0 };
    let mut s = wave_engine::poly::lab::heaviest_instance(SR, secs);
    let (mut l, mut r) = (vec![0f32; block], vec![0f32; block]);
    let blocks = (secs * SR) as usize / block;
    let mut us = Vec::with_capacity(blocks);
    // the first block is outside the allocation count (nothing should allocate there either)
    s.render_into(&mut l, &mut r, 120.0);
    let (_, allocs) = count_allocs(|| {
        for _ in 1..blocks {
            let t = Instant::now();
            s.render_into(&mut l, &mut r, 120.0);
            us.push(t.elapsed().as_secs_f64() * 1e6);
        }
    });
    let st = stats(us);
    let budget = block as f64 / SR as f64 * 1e6;
    json!({
        "voices": s.active_voices(), "block": block, "budget_us": r2(budget),
        "cpu_pct_mean": r2(st.mean / budget * 100.0), "cpu_pct_p99": r2(st.p99 / budget * 100.0), "cpu_pct_max": r2(st.max / budget * 100.0),
        "block_us": st.json(), "audio_allocs": allocs.allocs,
    })
}

// ---------------------------------------------------------------- whole project

fn bench_rt_project(wave: &Path, quick: bool) -> Value {
    let e = Engine::new(Project::empty(), Arc::new(NoPlugins));
    e.open(wave).unwrap();
    e.apply(&[Op::SetLoop { range: None }], false, None)
        .unwrap();
    let secs = if quick { 4.0 } else { 12.0 };
    let end = e.project().song_end();
    let mut out = json!({});
    out["stopped_idle_512"] = rt_sync(&e, 512, -1.0, secs);
    out["from_start_512"] = rt_sync(&e, 512, 0.0, secs);
    out["from_middle_512"] = rt_sync(&e, 512, end * 0.5, secs);
    if !quick {
        out["block_64"] = rt_sync(&e, 64, end * 0.5, 6.0);
        out["block_128"] = rt_sync(&e, 128, end * 0.5, 8.0);
        out["block_1024"] = rt_sync(&e, 1024, end * 0.5, secs);
    }
    // voices in the middle: how many clips sound at the chosen beat
    let b = end * 0.5;
    let p = e.project();
    out["clips_sounding_at_middle"] = json!(p
        .clips
        .iter()
        .filter(|c| c.start <= b && b < c.start + c.len && c.kind != ClipKind::Auto)
        .count());
    out
}

// ---------------------------------------------------------------- ops

fn bench_ops(wave: &Path, _sp: &Spec, quick: bool) -> Value {
    let n = if quick { 8 } else { 30 };
    // an audio thread must exist, otherwise nothing is synced
    let e = Engine::new(Project::empty(), Arc::new(NoPlugins));
    let rt = e.create_rt(SR, 512);
    let sim = start_sim(rt, 512, true);
    e.open(wave).unwrap();
    let p = e.project();
    let clip = p
        .clips
        .iter()
        .find(|c| c.kind == ClipKind::Audio && c.tune.is_none())
        .cloned()
        .unwrap();
    let tuned = p.clips.iter().find(|c| c.tune.is_some()).cloned();
    let track = p.tracks[p.tracks.len() / 2].clone();
    let comp_fx = p
        .inserts
        .iter()
        .flat_map(|i| i.fx.iter())
        .find(|f| f.kind == FxKind::Comp)
        .cloned()
        .unwrap();
    let eq_ins = p
        .inserts
        .iter()
        .find(|i| i.fx.iter().any(|f| f.kind == FxKind::Eq) && i.id != 0)
        .cloned()
        .unwrap();
    let mut res = json!({});

    let time_op = |mut f: Box<dyn FnMut(usize) -> Vec<Op>>, transient: bool| -> Value {
        let mut t = vec![];
        for k in 0..n {
            let ops = f(k);
            let (_, ms) = time_ms(|| e.apply(&ops, transient, None).unwrap());
            t.push(ms);
        }
        stats(t).json()
    };
    let mut c = clip.clone();
    res["move_clip_ms"] = time_op(
        Box::new(move |k| {
            c.start = 10.0 + (k % 7) as f64;
            vec![Op::PutClips {
                clips: vec![c.clone()],
            }]
        }),
        false,
    );
    e.begin_gesture();
    let mut c = clip.clone();
    res["drag_clip_transient_ms"] = time_op(
        Box::new(move |k| {
            c.start = 20.0 + k as f64 * 0.25;
            vec![Op::PutClips {
                clips: vec![c.clone()],
            }]
        }),
        true,
    );
    e.end_gesture();
    e.begin_gesture();
    let mut t = track.clone();
    res["fader_drag_transient_ms"] = time_op(
        Box::new(move |k| {
            t.vol = 0.5 + (k % 20) as f32 * 0.01;
            vec![Op::PutTrack { track: t.clone() }]
        }),
        true,
    );
    e.end_gesture();
    e.begin_gesture();
    let mut f = comp_fx.clone();
    res["fx_param_drag_transient_ms"] = time_op(
        Box::new(move |k| {
            f.params.insert("thr".into(), -30.0 + (k % 20) as f64);
            vec![Op::PutFx {
                insert_id: 0,
                fx: f.clone(),
                index: None,
            }]
        }),
        true,
    );
    e.end_gesture();
    let mut ins = eq_ins.clone();
    res["insert_vol_ms"] = time_op(
        Box::new(move |k| {
            ins.vol = 0.5 + (k % 20) as f32 * 0.01;
            vec![Op::PutInsert {
                insert: ins.clone(),
            }]
        }),
        false,
    );
    let mut next = 70_000u64;
    let eq_id = eq_ins.id;
    let bpm = p.bpm;
    let mut added: Vec<Id> = vec![];
    let mut t = vec![];
    for _ in 0..n {
        next += 1;
        let (_, ms) = time_ms(|| {
            e.apply(
                &[Op::PutFx {
                    insert_id: eq_id,
                    fx: new_fx(next, FxKind::Sat, bpm),
                    index: None,
                }],
                false,
                None,
            )
            .unwrap()
        });
        t.push(ms);
        added.push(next);
    }
    res["add_fx_graph_rebuild_ms"] = stats(t).json();
    let mut t = vec![];
    for id in added {
        let (_, ms) = time_ms(|| e.apply(&[Op::RemoveFx { fx_id: id }], false, None).unwrap());
        t.push(ms);
    }
    res["remove_fx_graph_rebuild_ms"] = stats(t).json();
    res["set_bpm_ms"] = time_op(
        Box::new(|k| {
            vec![Op::SetBpm {
                bpm: 100.0 + k as f64,
            }]
        }),
        false,
    );
    // a no-op apply costs clone + equality only
    let same = Op::SetBpm {
        bpm: e.project().bpm,
    };
    let mut t = vec![];
    for _ in 0..n {
        let (_, ms) = time_ms(|| e.apply(std::slice::from_ref(&same), false, None).unwrap());
        t.push(ms);
    }
    res["noop_apply_ms_clone_and_compare"] = stats(t).json();
    // undo / redo of the structural steps above
    let mut t = vec![];
    for _ in 0..n.min(20) {
        let (_, ms) = time_ms(|| e.undo());
        t.push(ms);
    }
    res["undo_ms"] = stats(t).json();
    let mut t = vec![];
    for _ in 0..n.min(20) {
        let (_, ms) = time_ms(|| e.redo());
        t.push(ms);
    }
    res["redo_ms"] = stats(t).json();
    if let Some(tc) = tuned {
        // moving a tuned clip: hash of the edit list (JSON) per tuned clip on every timeline build
        let mut c = tc.clone();
        let mut t = vec![];
        for k in 0..n {
            c.start = 30.0 + (k % 5) as f64;
            let (_, ms) = time_ms(|| {
                e.apply(
                    &[Op::PutClips {
                        clips: vec![c.clone()],
                    }],
                    false,
                    None,
                )
                .unwrap()
            });
            t.push(ms);
        }
        res["move_tuned_clip_ms"] = stats(t).json();
        // editing the tune of one clip: re-render
        let mut t = vec![];
        for k in 0..5 {
            let mut c2 = tc.clone();
            if let Some(tu) = c2.tune.as_mut() {
                tu.notes[0].shift = 10.0 * (k + 1) as f32;
            }
            let (_, ms) = time_ms(|| {
                e.apply(&[Op::PutClips { clips: vec![c2] }], false, None)
                    .unwrap()
            });
            t.push(ms);
        }
        res["edit_tune_rerender_ms"] = stats(t).json();
    }
    // IPC cost of every change: what the app emits on the "state" event
    let mut t = vec![];
    let mut sz = 0;
    for _ in 0..10 {
        let (b, ms) = time_ms(|| serde_json::to_vec(&e.state()).unwrap());
        sz = b.len();
        t.push(ms);
    }
    res["state_event_serialize_ms"] = stats(t).json();
    res["state_event_kib"] = json!(sz / 1024);
    // the same apply with the app's state listener attached (serialises the whole project)
    let bytes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let b2 = bytes.clone();
    e.on_change(move |dto| {
        b2.fetch_add(serde_json::to_vec(dto).unwrap().len(), Ordering::Relaxed);
    });
    e.begin_gesture();
    let mut t = t_vec();
    let mut tr = track.clone();
    for k in 0..n {
        tr.vol = 0.4 + (k % 20) as f32 * 0.01;
        let (_, ms) = time_ms(|| {
            e.apply(&[Op::PutTrack { track: tr.clone() }], true, None)
                .unwrap()
        });
        t.push(ms);
    }
    e.end_gesture();
    res["fader_drag_with_state_event_ms"] = stats(t).json();
    res["state_event_bytes_per_op_kib"] = json!(bytes.load(Ordering::Relaxed) / 1024 / (n + 1));
    // undo stack memory: every undo step is a full project copy
    res["undo_limit"] = json!(200);
    res["project_json_kib"] = json!(serde_json::to_vec(&e.project()).unwrap().len() / 1024);
    let s = sim.finish();
    res["audio_thread_during_ops"] = s.json();
    res
}

fn t_vec() -> Vec<f64> {
    vec![]
}

// ---------------------------------------------------------------- export

fn bench_export(wave: &Path, quick: bool, full: bool) -> Value {
    let e = Engine::new(Project::empty(), Arc::new(NoPlugins));
    e.open(wave).unwrap();
    let p = e.project();
    let end = p.song_end();
    // a slice from the middle of the song (dense region); the whole song with --full-export
    let len = if full {
        end
    } else if quick {
        8.0
    } else {
        32.0
    };
    let from = if full { 0.0 } else { (end * 0.5).floor() };
    let (out, ms) = time_ms(|| e.render(from, from + len, 1.0, 48000).unwrap());
    let audio = out.len() as f64 / 2.0 / 48000.0;
    json!({ "from_beat": from, "beats": len, "audio_secs": r2(audio), "ms": r2(ms), "rtf": r2(audio / (ms / 1000.0)), "full_song": full,
            "mem_mib_out": r2(out.len() as f64 * 4.0 / 1048576.0) })
}

// ---------------------------------------------------------------- analysis, comp, peaks

fn bench_analysis(wave: &Path, _name: &str) -> Value {
    let e = Engine::new(Project::empty(), Arc::new(NoPlugins));
    e.open(wave).unwrap();
    let p = e.project();
    let mut res = json!({});
    let vocal = p
        .clips
        .iter()
        .filter(|c| c.kind == ClipKind::Audio && c.tune.is_none())
        .find(|c| {
            p.sources
                .iter()
                .find(|s| s.id == c.source)
                .map(|s| s.name.starts_with("Vocal"))
                .unwrap_or(false)
        })
        .cloned()
        .unwrap();
    let src = p.sources.iter().find(|s| s.id == vocal.source).unwrap();
    let (a, ms) = time_ms(|| e.pitch_analysis(vocal.id).unwrap());
    let (_, ms2) = time_ms(|| e.pitch_analysis(vocal.id).unwrap());
    res["pitch_analysis"] = json!({ "source_secs": r2(src.dur), "first_ms": r2(ms), "rtf": r2(src.dur / (ms / 1000.0)), "cached_ms": r2(ms2), "notes": a.notes.len(), "estimator": e.f0_backend() });
    let (_, ms) = time_ms(|| e.music_analysis(Some(vocal.id), None).unwrap());
    res["music_analysis_clip_ms"] = json!(r2(ms));
    let (_, ms) = time_ms(|| e.music_analysis(None, None));
    res["music_analysis_project_ms"] = json!(r2(ms));
    // tune render: put an edit on the clip
    let mut c = vocal.clone();
    c.tune = Some(ClipTune {
        notes: a
            .notes
            .iter()
            .take(6)
            .map(|n| NoteEdit {
                start: n.start,
                end: n.end,
                shift: 50.0,
                drift: 0.0,
                formant: 0.0,
                gain: 0.0,
            })
            .collect(),
        pencil: vec![],
        reference: None,
    });
    let rt = e.create_rt(SR, 512);
    let sim = start_sim(rt, 512, true);
    let (_, ms) = time_ms(|| {
        e.apply(
            &[Op::PutClips {
                clips: vec![c.clone()],
            }],
            false,
            None,
        )
        .unwrap()
    });
    res["tune_render_apply_ms"] =
        json!({ "source_secs": r2(src.dur), "ms": r2(ms), "rtf": r2(src.dur / (ms / 1000.0)) });
    // peaks
    let (pk, ms) = time_ms(|| e.source_peaks(vocal.source, false, false).unwrap());
    res["source_peaks_ms"] =
        json!({ "pairs": pk.len() / 2, "ms": r2(ms), "kib": pk.len() * 4 / 1024 });
    let (_, ms) = time_ms(|| e.source_peaks(vocal.source, true, false).unwrap());
    res["source_peaks_normalized_first_ms"] = json!(r2(ms));
    let (_, ms) = time_ms(|| e.clip_peaks(vocal.id).unwrap());
    res["clip_peaks_tuned_ms"] = json!(r2(ms));
    // decoding
    let dir = wave.parent().unwrap().join("audio");
    let files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|f| f.path())
        .filter(|p| p.extension().map(|x| x == "wav").unwrap_or(false))
        .take(10)
        .collect();
    let mb: f64 = files
        .iter()
        .map(|f| std::fs::metadata(f).map(|m| m.len()).unwrap_or(0) as f64)
        .sum::<f64>()
        / 1048576.0;
    let (_, ms) = time_ms(|| {
        for f in &files {
            let _ = wave_engine::sources::Audio::load(f).unwrap();
        }
    });
    res["decode_wav"] = json!({ "files": files.len(), "mib": r2(mb), "ms": r2(ms), "mib_per_s": r2(mb / (ms / 1000.0)) });
    // import path (decode + sources + undo snapshot)
    let (_, ms) = time_ms(|| {
        e.import_audio(&files[..3.min(files.len())], Some((0, 0.0)))
            .unwrap()
    });
    res["import_3_files_ms"] = json!(r2(ms));
    // comp / flatten
    if let Some(cc) = e
        .project()
        .clips
        .iter()
        .find(|c| c.kind == ClipKind::Comp)
        .cloned()
    {
        let (r, ms) = time_ms(|| e.flatten_comp(cc.id));
        res["flatten_comp_ms"] = json!({ "ms": r2(ms), "ok": r.is_ok() });
    }
    let s = sim.finish();
    res["audio_thread_during_analysis"] = s.json();
    res
}

// ---------------------------------------------------------------- audio thread under edits

fn bench_rt_under_edit(wave: &Path, quick: bool) -> Value {
    let e = Engine::new(Project::empty(), Arc::new(NoPlugins));
    e.open(wave).unwrap();
    e.apply(&[Op::SetLoop { range: None }], false, None)
        .unwrap();
    let rt = e.create_rt(SR, 512);
    e.play(e.project().song_end() * 0.5);
    // control: the same playback, paced like a device callback, with no edits at all
    let ctl = start_sim(rt, 512, true);
    std::thread::sleep(Duration::from_millis(if quick { 1500 } else { 4000 }));
    let (ctl_res, rt) = ctl.finish_keep();
    let sim = start_sim(rt, 512, true);
    std::thread::sleep(Duration::from_millis(300));
    let p = e.project();
    let clip = p
        .clips
        .iter()
        .find(|c| c.kind == ClipKind::Audio && c.tune.is_none())
        .cloned()
        .unwrap();
    let ins = p
        .inserts
        .iter()
        .find(|i| i.id != 0 && !i.fx.is_empty())
        .cloned()
        .unwrap();
    let n = if quick { 10 } else { 40 };
    let mut out = json!({});
    // while playing: timeline edits (every clip move rebuilds the whole timeline)
    let mut c = clip.clone();
    let mut t = vec![];
    for k in 0..n {
        c.start = 40.0 + (k % 9) as f64;
        let (_, ms) = time_ms(|| {
            e.apply(
                &[Op::PutClips {
                    clips: vec![c.clone()],
                }],
                false,
                None,
            )
            .unwrap()
        });
        t.push(ms);
        std::thread::sleep(Duration::from_millis(15));
    }
    out["apply_clip_move_while_playing_ms"] = stats(t).json();
    let mut t = vec![];
    for next in (80_001u64..).take(n / 2) {
        let (_, ms) = time_ms(|| {
            e.apply(
                &[Op::PutFx {
                    insert_id: ins.id,
                    fx: new_fx(next, FxKind::Reverb, p.bpm),
                    index: None,
                }],
                false,
                None,
            )
            .unwrap()
        });
        t.push(ms);
        std::thread::sleep(Duration::from_millis(30));
        e.apply(&[Op::RemoveFx { fx_id: next }], false, None)
            .unwrap();
        std::thread::sleep(Duration::from_millis(30));
    }
    out["apply_add_reverb_while_playing_ms"] = stats(t).json();
    let r = sim.finish();
    out["audio_thread"] = r.json();
    out["audio_thread_control_no_edits"] = ctl_res.json();
    e.stop();
    out
}

// ---------------------------------------------------------------- MIDI

fn bench_midi(wave: &Path, quick: bool) -> Value {
    let e = Engine::new(Project::empty(), Arc::new(NoPlugins));
    e.open(wave).unwrap();
    let rt = e.create_rt(SR, 512);
    let sim = start_sim(rt, 512, true);
    e.midi_configure(None, true);
    let n = if quick { 500 } else { 3000 };
    let call = |e: &Arc<Engine>, k: usize| {
        let t = Instant::now();
        e.midi_event(if k.is_multiple_of(2) {
            MidiMsg::NoteOn {
                ch: 0,
                key: 60 + (k % 12) as u8,
                vel: 100,
            }
        } else {
            MidiMsg::NoteOff {
                ch: 0,
                key: 60 + ((k - 1) % 12) as u8,
            }
        });
        t.elapsed().as_secs_f64() * 1e6
    };
    let mut idle = vec![];
    for k in 0..n {
        idle.push(call(&e, k));
        std::thread::sleep(Duration::from_micros(300));
    }
    // now with the UI thread editing the project (core lock held while apply clones and rebuilds)
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    let e2 = e.clone();
    let clip = e
        .project()
        .clips
        .iter()
        .find(|c| c.kind == ClipKind::Audio && c.tune.is_none())
        .cloned()
        .unwrap();
    let editor = std::thread::spawn(move || {
        let mut c = clip;
        let mut k = 0;
        while !s2.load(Ordering::Relaxed) {
            c.start = 10.0 + (k % 11) as f64;
            let _ = e2.apply(
                &[Op::PutClips {
                    clips: vec![c.clone()],
                }],
                false,
                None,
            );
            k += 1;
        }
        k
    });
    let mut busy = vec![];
    for k in 0..n {
        busy.push(call(&e, k));
        std::thread::sleep(Duration::from_micros(300));
    }
    stop.store(true, Ordering::Relaxed);
    let edits = editor.join().unwrap();
    let r = sim.finish();
    json!({ "note_event_us_idle": stats(idle).json(), "note_event_us_while_editing": stats(busy).json(), "edits_during_test": edits, "audio_thread": r.json() })
}

// ---------------------------------------------------------------- telemetry (housekeeping at 60 Hz)

fn bench_telemetry(wave: &Path, quick: bool) -> Value {
    let e = Engine::new(Project::empty(), Arc::new(NoPlugins));
    e.open(wave).unwrap();
    e.apply(&[Op::SetLoop { range: None }], false, None)
        .unwrap();
    let p = e.project();
    let fx_ids: Vec<Id> = p
        .inserts
        .iter()
        .flat_map(|i| i.fx.iter())
        .filter(|f| {
            matches!(
                f.kind,
                FxKind::Eq | FxKind::Comp | FxKind::Tune | FxKind::Reverb
            )
        })
        .map(|f| f.id)
        .take(8)
        .collect();
    let rt = e.create_rt(SR, 512);
    let sim = start_sim(rt, 512, true);
    let frames = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let bytes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (f2, b2) = (frames.clone(), bytes.clone());
    e.set_frame_sink(Some(Box::new(move |v: Vec<u8>| {
        f2.fetch_add(1, Ordering::Relaxed);
        b2.fetch_add(v.len(), Ordering::Relaxed);
    })));
    e.play(p.song_end() * 0.5);
    let n = if quick { 40 } else { 120 };
    let mut track = p.tracks[10].clone();
    let mut res = json!({});
    for (label, ids) in [("no_windows", vec![]), ("8_fx_windows", fx_ids.clone())] {
        e.watch_fx(&ids);
        std::thread::sleep(Duration::from_millis(300));
        let (f0, b0) = (
            frames.load(Ordering::Relaxed),
            bytes.load(Ordering::Relaxed),
        );
        let t0 = Instant::now();
        let mut t = vec![];
        for k in 0..n {
            track.vol = 0.5 + (k % 20) as f32 * 0.01;
            let (_, ms) = time_ms(|| {
                e.apply(
                    &[Op::PutTrack {
                        track: track.clone(),
                    }],
                    false,
                    None,
                )
                .unwrap()
            });
            t.push(ms);
            std::thread::sleep(Duration::from_millis(12));
        }
        let secs = t0.elapsed().as_secs_f64();
        let (f1, b1) = (
            frames.load(Ordering::Relaxed),
            bytes.load(Ordering::Relaxed),
        );
        res[label] = json!({ "frames_per_s": r2((f1 - f0) as f64 / secs), "kib_per_frame": r2((b1 - b0) as f64 / (f1 - f0).max(1) as f64 / 1024.0), "mib_per_s": r2((b1 - b0) as f64 / secs / 1048576.0),
                             "apply_fader_ms_while_housekeeping": stats(t).json() });
    }
    e.set_frame_sink(None);
    e.stop();
    let r = sim.finish();
    res["audio_thread"] = r.json();
    res
}

// ---------------------------------------------------------------- undo memory

fn bench_undo_memory(wave: &Path, quick: bool) -> Value {
    let e = Engine::new(Project::empty(), Arc::new(NoPlugins));
    e.open(wave).unwrap();
    let n = if quick { 40 } else { 200 };
    let r0 = rss_mib();
    let t0 = Instant::now();
    for k in 0..n {
        e.apply(
            &[Op::SetBpm {
                bpm: 60.0 + k as f64 * 0.1,
            }],
            false,
            None,
        )
        .unwrap();
    }
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    let r1 = rss_mib();
    json!({ "undo_steps": n, "rss_growth_mib": r2(r1 - r0), "mib_per_step": r2((r1 - r0) / n as f64), "avg_apply_ms": r2(ms / n as f64), "project_json_kib": serde_json::to_vec(&e.project()).unwrap().len() / 1024 })
}
