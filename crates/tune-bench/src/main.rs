//! tune-bench — measures Tune to Reference against the MVP acceptance criteria.
//! stdout is the JSON report; progress and the PASS/FAIL table go to stderr.

mod ab;
mod agent;
mod agent_vocalset;
mod cases;
mod eval;
mod gtsinger;
mod listen;
mod medley;
mod synth;
mod vocalset;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::{json, Value};
use wave_engine::pitch::F0Estimator;
use wave_engine::RefTune;

const HELP: &str = "tune-bench — Tune to Reference benchmarks

  tune-bench synth [--songs 6] [--only 'song2 ±400'] [--write DIR]
  tune-bench vocalset --root DIR [--mode self|cross|mismatch|all] [--limit 40]
  tune-bench ab --out DIR [--root DIR] [--fragments 20] [--baseline snap|take] [--seed 1]
  tune-bench ab-score --kit DIR --ratings ratings.csv

  agent workflow (pitch inspect/compare/suggest → editTune → render), scored on the re-analysed render,
  with existing Tune to Reference on the same cases for comparison:
  tune-bench agent-vocalset --root DIR [--limit 30]      Annotated VocalSet (controlled + cross-singer + other key + unrelated)
  tune-bench agent-gtsinger --root DIR [--limit 16]      GTSinger subset (scripts/fetch_gtsinger.py): ornaments, tenor/alto
  tune-bench agent-medley --root DIR [--limit 12]        MedleyDB_Sample: a song's lead vocal, backing vocals as references
  tune-bench estimation [--vocalset DIR] [--gtsinger DIR] [--medley DIR] [--limit 40]   detector vs annotations
  tune-bench listen --out DIR [--vocalset DIR] [--gtsinger DIR] [--fragments 12] [--seed 1]
                                                         blind 3-way kit: original / tune-ref / agent
  tune-bench listen-score --kit DIR --ratings ratings.csv
  agent options: --max-cents 60 --min-cents 15 (planner limits), --no-tune-ref (skip the comparison),
                 --dump DIR (each case's take/reference/output WAV and the planner's decisions)

common: --rmvpe model.onnx | --yin   (default: the installed RMVPE model, else YIN)
        --correction --vibrato --influence 0..100   --json report.json
VocalSet: the folder holding `Annotated VocalSet` and `audio` (Zenodo 7061507).
Exit code 0 when every criterion passes, 1 when one fails, 2 on usage errors.
";

/// Deterministic xorshift, shared by the suites.
pub struct Rng(pub u32);
impl Rng {
    pub fn unit(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0 as f32 / u32::MAX as f32
    }
    pub fn sym(&mut self) -> f32 {
        self.unit() * 2.0 - 1.0
    }
}

struct Args(Vec<String>);
impl Args {
    fn get(&self, k: &str) -> Option<&str> {
        let i = self.0.iter().position(|a| a == &format!("--{k}"))?;
        self.0.get(i + 1).map(String::as_str).filter(|v| !v.starts_with("--"))
    }
    fn has(&self, k: &str) -> bool {
        self.0.iter().any(|a| a == &format!("--{k}"))
    }
    fn num(&self, k: &str, d: f64) -> Result<f64, String> {
        self.get(k).map(|v| v.parse::<f64>().map_err(|_| format!("--{k} must be a number"))).unwrap_or(Ok(d))
    }
}

fn params(a: &Args) -> Result<RefTune, String> {
    let pct = |k: &str| a.num(k, 100.0).map(|v| (v / 100.0).clamp(0.0, 1.0) as f32);
    Ok(RefTune {
        source: 0,
        correction: pct("correction")?,
        vibrato: pct("vibrato")?,
        influence: pct("influence")?,
        octave: None,
        key: 0,
        scale: 0,
        speed: None,
        melody: None,
    })
}

fn estimator(a: &Args) -> Result<Option<wave_rmvpe::Rmvpe>, String> {
    if a.has("yin") {
        return Ok(None);
    }
    match a.get("rmvpe") {
        Some(p) => wave_rmvpe::Rmvpe::load(Path::new(p)).map(Some),
        None => Ok(wave_rmvpe::Rmvpe::load(&wave_rmvpe::model_path()).ok()),
    }
}

/// Every scored summary passes, and there is something to pass: a run with no summary, a summary
/// with no case or no criterion, or a failed case is a failure, never a silent PASS.
pub fn all_pass(v: &Value) -> bool {
    let scored: Vec<&Value> =
        v["summaries"].as_array().into_iter().flatten().filter(|s| s["informational"].as_bool() != Some(true)).collect();
    !scored.is_empty()
        && scored.iter().all(|s| {
            let checks = s["checks"].as_array().map(Vec::as_slice).unwrap_or(&[]);
            s["cases"].as_u64().unwrap_or(0) > 0
                && !checks.is_empty()
                && checks.iter().all(|c| c["pass"].as_bool() == Some(true))
                && s["failed"].as_array().is_none_or(|f| f.is_empty())
        })
}

fn report(a: &Args, v: Value) -> Result<bool, String> {
    let all_pass = all_pass(&v);
    let text = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
    if let Some(p) = a.get("json") {
        std::fs::write(p, &text).map_err(|e| format!("{p}: {e}"))?;
    }
    println!("{text}");
    Ok(all_pass)
}

fn summaries(label: &str, os: &[eval::Outcome]) -> Vec<Value> {
    let mut groups: Vec<&str> = os.iter().map(|o| o.group.as_str()).collect();
    groups.dedup();
    groups.sort();
    groups.dedup();
    let mut out = vec![eval::summarize(&format!("{label} (all)"), &os.iter().collect::<Vec<_>>())];
    for g in groups {
        out.push(eval::summarize(&format!("{label} {g}"), &os.iter().filter(|o| o.group == g).collect::<Vec<_>>()));
    }
    out
}

fn run(cmd: &str, a: &Args) -> Result<bool, String> {
    let p = params(a)?;
    match cmd {
        "synth" => {
            let model = estimator(a)?;
            let est = model.as_ref().map(|m| m as &dyn F0Estimator);
            let backend = if est.is_some() { "rmvpe" } else { "yin" };
            eprintln!("synthetic suite, detector {backend}");
            let os = synth::run(a.num("songs", 6.0)? as u32, a.get("only"), &p, est, a.get("write").map(Path::new));
            let s = summaries("synthetic", &os);
            s.iter().for_each(eval::print);
            report(
                a,
                json!({ "suite": "synthetic", "detector": backend, "summaries": s, "cases": os.iter().map(eval::outcome_json).collect::<Vec<_>>() }),
            )
        }
        "vocalset" => {
            let root = PathBuf::from(a.get("root").ok_or("missing --root")?);
            let model = estimator(a)?;
            let est = model.as_ref().map(|m| m as &dyn F0Estimator);
            let backend = if est.is_some() { "rmvpe" } else { "yin" };
            let items = vocalset::index(&root)?;
            let limit = a.num("limit", 40.0)? as usize;
            let mode = a.get("mode").unwrap_or("all");
            eprintln!("Annotated VocalSet: {} annotated recordings, detector {backend}", items.len());
            let mut s = vec![];
            let mut cases = vec![];
            if mode == "self" || mode == "all" {
                let os = vocalset::run_self(&vocalset::spread(&items, limit), &p, est);
                s.extend(summaries("vocalset self", &os));
                cases.extend(os.iter().map(eval::outcome_json));
            }
            if mode == "cross" || mode == "all" {
                let pairs = vocalset::cross_pairs(&items);
                let os = vocalset::run_cross(&vocalset::spread(&pairs, limit), &p, est);
                s.push(eval::summarize("vocalset cross-singer", &os.iter().collect::<Vec<_>>()));
                cases.extend(os.iter().map(eval::outcome_json));
            }
            if mode == "mismatch" || mode == "all" {
                let os = vocalset::run_mismatch(&items, limit.min(20), &p, est);
                s.push(eval::summarize("vocalset mismatch", &os.iter().collect::<Vec<_>>()));
                cases.extend(os.iter().map(eval::outcome_json));
            }
            s.iter().for_each(eval::print);
            report(a, json!({ "suite": "vocalset", "detector": backend, "summaries": s, "cases": cases }))
        }
        "ab" => {
            let model = estimator(a)?;
            let est = model.as_ref().map(|m| m as &dyn F0Estimator);
            let baseline = a.get("baseline").unwrap_or("snap");
            if baseline != "snap" && baseline != "take" {
                return Err("--baseline is snap or take".into());
            }
            let v = ab::make(
                Path::new(a.get("out").ok_or("missing --out")?),
                a.get("root").map(Path::new),
                a.num("fragments", 20.0)? as usize,
                baseline,
                a.num("seed", 1.0)? as u32,
                &p,
                est,
            )?;
            println!("{v}");
            Ok(true)
        }
        "agent-vocalset" | "agent-gtsinger" | "agent-medley" => {
            let root = PathBuf::from(a.get("root").ok_or("missing --root")?);
            let model = estimator(a)?;
            let est = model.as_ref().map(|m| m as &dyn F0Estimator);
            let backend = if est.is_some() { "rmvpe" } else { "yin" };
            let sp = wave_engine::pitch_api::SuggestParams {
                max_cents: a.num("max-cents", 60.0)? as f32,
                min_cents: a.num("min-cents", 15.0)? as f32,
                ..Default::default()
            };
            let mut failed: Vec<(String, String)> = vec![];
            let mut skipped: Vec<String> = vec![];
            let (suite, cases, dataset) = match cmd {
                "agent-vocalset" => {
                    let items = vocalset::index(&root)?;
                    let cs = agent_vocalset::cases(&items, a.num("limit", 30.0)? as usize, est, &mut failed);
                    (
                        "vocalset",
                        cs,
                        json!({ "name": "Annotated VocalSet", "zenodo": ["7061507", "1442513"], "licence": "CC BY 4.0", "annotated": items.len() }),
                    )
                }
                "agent-gtsinger" => {
                    let segs = gtsinger::index(&root)?;
                    let cs = gtsinger::cases(&segs, a.num("limit", 16.0)? as usize, est, &mut failed, &mut skipped);
                    (
                        "gtsinger",
                        cs,
                        json!({ "name": "GTSinger (English subset)", "source": "huggingface.co/datasets/GTSinger/GTSinger", "licence": "CC BY-NC-SA 4.0", "segments": segs.len() }),
                    )
                }
                _ => {
                    let songs = medley::index(&root)?;
                    let cs = medley::cases(&songs, a.num("limit", 12.0)? as usize, est, &mut failed, &mut skipped);
                    (
                        "medleydb",
                        cs,
                        json!({ "name": "MedleyDB_Sample", "zenodo": "1438309", "licence": "CC BY-SA 4.0", "songs": songs.iter().map(|s| s.name.clone()).collect::<Vec<_>>() }),
                    )
                }
            };
            eprintln!("{suite}: {} cases, {} failed to build, detector {backend}", cases.len(), failed.len());
            let mut methods = vec![agent::Method::Agent(sp.clone())];
            if !a.has("no-tune-ref") {
                methods.push(agent::Method::TuneRef(p.clone()));
            }
            let mut os: Vec<agent::Outcome> = vec![];
            for c in &cases {
                let mut ms = methods.clone();
                // the large controlled errors also with the limit raised, as an agent that knows the take is far off would
                if c.group.contains("large") {
                    ms.push(agent::Method::Agent(wave_engine::pitch_api::SuggestParams { max_cents: 250.0, ..sp.clone() }));
                }
                for m in &ms {
                    if matches!(m, agent::Method::TuneRef(_)) && c.reference.is_none() {
                        continue;
                    }
                    let o = agent::run(&c.case(), m, est, a.get("dump").map(Path::new));
                    eprintln!(
                        "  {:<12} {:<44} {:<40} slips {:5.1}→{:5.1} ct  moved {}/{} kept {}/{} {}",
                        o.method,
                        o.group,
                        o.name.chars().take(40).collect::<String>(),
                        o.slip_before.iter().map(|x| x.abs()).sum::<f32>() / o.slip_before.len().max(1) as f32,
                        o.slip_after.iter().map(|x| x.abs()).sum::<f32>() / o.slip_after.len().max(1) as f32,
                        o.in_tune_change.iter().filter(|x| **x > 15.0).count(),
                        o.in_tune_change.len(),
                        o.intended_change.iter().filter(|x| **x < 20.0).count(),
                        o.intended_change.len(),
                        o.failed.as_deref().unwrap_or("")
                    );
                    os.push(o);
                }
            }
            // cases that could not even be built are failures of every method's run
            for (name, why) in &failed {
                for m in &methods {
                    os.push(agent::Outcome {
                        name: name.clone(),
                        group: "could not be built".into(),
                        method: m.label(),
                        agent: m.is_agent(),
                        failed: Some(why.clone()),
                        ..Default::default()
                    });
                }
            }
            let s = agent::summaries(suite, &os);
            s.iter().for_each(eval::print);
            report(
                a,
                json!({
                    "suite": format!("agent-{suite}"), "detector": backend, "dataset": dataset,
                    "planner": { "maxCents": sp.max_cents, "minCents": sp.min_cents, "strength": sp.strength },
                    "tuneRef": { "correction": p.correction, "vibrato": p.vibrato, "influence": p.influence },
                    "skipped": skipped,
                    "summaries": s, "cases": os.iter().map(agent::outcome_json).collect::<Vec<_>>(),
                }),
            )
        }
        "estimation" => {
            let model = estimator(a)?;
            let est = model.as_ref().map(|m| m as &dyn F0Estimator);
            let limit = a.num("limit", 40.0)? as usize;
            let mut v = json!({ "detector": if est.is_some() { "rmvpe" } else { "yin" } });
            if let Some(r) = a.get("vocalset") {
                v["vocalset"] = agent_vocalset::estimation(&vocalset::index(Path::new(r))?, limit, est);
            }
            if let Some(r) = a.get("gtsinger") {
                v["gtsingerScoreAgreement"] = gtsinger::score_agreement(&gtsinger::index(Path::new(r))?, limit, est);
            }
            if let Some(r) = a.get("medley") {
                v["medleydb"] = medley::estimation(&medley::index(Path::new(r))?, est);
            }
            let text = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
            if let Some(p) = a.get("json") {
                std::fs::write(p, &text).map_err(|e| format!("{p}: {e}"))?;
            }
            println!("{text}");
            Ok(true)
        }
        "listen" => {
            let model = estimator(a)?;
            let est = model.as_ref().map(|m| m as &dyn F0Estimator);
            let v = listen::make(
                Path::new(a.get("out").ok_or("missing --out")?),
                a.get("vocalset").map(Path::new),
                a.get("gtsinger").map(Path::new),
                a.num("fragments", 12.0)? as usize,
                a.num("seed", 1.0)? as u32,
                &p,
                est,
            )?;
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            Ok(true)
        }
        "listen-score" => {
            let v =
                listen::score(Path::new(a.get("kit").ok_or("missing --kit")?), Path::new(a.get("ratings").ok_or("missing --ratings")?))?;
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            Ok(true)
        }
        "ab-score" => {
            let v = ab::score(Path::new(a.get("kit").ok_or("missing --kit")?), Path::new(a.get("ratings").ok_or("missing --ratings")?))?;
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            Ok(true)
        }
        _ => Err(format!("unknown command `{cmd}`\n\n{HELP}")),
    }
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = raw.first().cloned().filter(|c| c != "help" && c != "--help") else {
        eprint!("{HELP}");
        return ExitCode::SUCCESS;
    };
    match run(&cmd, &Args(raw)) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("tune-bench: {e}");
            ExitCode::from(2)
        }
    }
}
