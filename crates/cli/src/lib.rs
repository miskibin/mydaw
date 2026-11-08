//! `wave` — the command line for a running WAVE app.
//!
//! stdout is only the JSON result; diagnostics go to stderr.
//! Exit codes: 0 ok, 1 usage, 2 no session / connection, 3 the app refused the request.

use std::process::ExitCode;

use serde_json::{json, Value};
use wave_session as ws;

const HELP: &str = r#"wave — control the open WAVE project

USAGE
  wave <command> [options]                 (add --session ID when several apps are open)

READ
  sessions                                 list running WAVE apps
  status                                   transport, revision, file, audio device
  context [--full]                         project summary (--full: every clip, note and parameter)
  catalog                                  effect types, parameters and presets
  schema                                   every op `apply` accepts, with an example
  analyze [--clip ID] [--exclude-fx ID]    detect tempo (BPM) and musical key

EDIT  (all edits are one undo step each; --if-revision N refuses to act if the project moved on)
  apply --file ops.json | --json '[...]'   apply a batch of ops atomically   [--label TEXT]
  undo | redo
  bpm <value>
  add-fx --insert ID --type eq|comp|sat|delay|reverb|limit [--preset N]
  set-param --fx ID --param NAME --value V
  preset --fx ID --index N
  import <file>... [--track N --beat B]    load audio, optionally place it

TAKES & COMPING  (times in timeline beats; add --clip ID when a track has several comps)
  takes --track N                          takes, comp regions and crossfades of the track
  comp --track N --set '[{"take":ID,"from":B,"to":B,"xfadeMs":20},...]'   replace the comp
  comp --track N --take ID                 play one whole take
  comp --track N --xfade MS                default crossfade at region boundaries
  comp --track N --remove-unused           drop takes the comp does not play
  comp --track N --flatten                 render the comp to one audio clip (removes the takes)
  comp --track N --audition ID|off         play one take instead of the comp (not an edit)

TRANSPORT
  play [--from BEAT] | stop

FILES
  render --out file.wav [--from B --to B --tail SEC --rate HZ]   bounce offline; prints level stats
  save [--path FILE] | open <FILE>

PLUGINS
  vst3 [--rescan]                          installed VST3 effects
  vst3-add --insert ID --path P --cid C    put a VST3 on an insert
  vst3-params --fx ID [--query TEXT]       parameters of a VST3 instance
  vst3-set --fx ID --id PARAM --value 0..1

  call <command> [--args JSON]             raw request (see `catalog`)
  --pretty                                 indent JSON output
"#;

struct Args {
    pos: Vec<String>,
    opts: Vec<(String, Option<String>)>,
}

impl Args {
    fn parse(raw: Vec<String>) -> Self {
        let mut pos = vec![];
        let mut opts = vec![];
        let mut it = raw.into_iter().peekable();
        while let Some(a) = it.next() {
            if let Some(k) = a.strip_prefix("--") {
                let v = match it.peek() {
                    Some(n) if !n.starts_with("--") || n.parse::<f64>().is_ok() => it.next(),
                    _ => None,
                };
                opts.push((k.to_string(), v));
            } else {
                pos.push(a);
            }
        }
        Self { pos, opts }
    }
    fn get(&self, k: &str) -> Option<&str> {
        self.opts.iter().find(|(n, _)| n == k).and_then(|(_, v)| v.as_deref())
    }
    fn has(&self, k: &str) -> bool {
        self.opts.iter().any(|(n, _)| n == k)
    }
    fn num(&self, k: &str) -> Result<Option<f64>, String> {
        self.get(k).map(|v| v.parse::<f64>().map_err(|_| format!("--{k} must be a number, got {v}"))).transpose()
    }
    fn need(&self, k: &str) -> Result<&str, String> {
        self.get(k).ok_or_else(|| format!("missing --{k}"))
    }
}

enum Fail {
    Usage(String),
    Conn(String),
    App(String),
}

/// Runs the CLI with `raw` arguments (without the program name).
pub fn run_cli(raw: Vec<String>) -> ExitCode {
    let a = Args::parse(raw);
    let pretty = a.has("pretty");
    let Some(cmd) = a.pos.first().cloned() else {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    };
    if cmd == "help" || a.has("help") {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }
    match run(&cmd, &a) {
        Ok(v) => {
            let s = if pretty { serde_json::to_string_pretty(&v) } else { serde_json::to_string(&v) };
            println!("{}", s.unwrap_or_default());
            ExitCode::SUCCESS
        }
        Err(Fail::Usage(m)) => {
            eprintln!("wave: {m}\n(run `wave help`)");
            ExitCode::from(1)
        }
        Err(Fail::Conn(m)) => {
            eprintln!("wave: {m}");
            ExitCode::from(2)
        }
        Err(Fail::App(m)) => {
            eprintln!("wave: {m}");
            ExitCode::from(3)
        }
    }
}

fn session(a: &Args) -> Result<ws::Endpoint, Fail> {
    let all = ws::sessions();
    match a.get("session") {
        Some(id) => all.into_iter().find(|e| e.session_id.starts_with(id)).ok_or_else(|| Fail::Conn(format!("no running WAVE session {id}"))),
        None => match all.len() {
            0 => Err(Fail::Conn("WAVE is not running (start the app, then try again)".into())),
            1 => Ok(all.into_iter().next().unwrap()),
            n => Err(Fail::Conn(format!("{n} WAVE apps are open; pick one with --session ID (see `wave sessions`)"))),
        },
    }
}

fn send(a: &Args, command: &str, args: Value) -> Result<Value, Fail> {
    let ep = session(a)?;
    let rev = a.num("if-revision").map_err(Fail::Usage)?.map(|v| v as u64);
    let rep = ws::call(&ep, command, args, rev, a.get("label").map(str::to_owned)).map_err(|e| Fail::Conn(format!("could not reach WAVE: {e}")))?;
    if rep.ok {
        let mut d = rep.data;
        if let Value::Object(m) = &mut d {
            m.entry("revision").or_insert(json!(rep.revision));
        }
        Ok(d)
    } else {
        let e = rep.error.unwrap_or_else(|| ws::ErrorBody::new("ERROR", "unknown error"));
        Err(Fail::App(format!("{}: {}", e.code, e.message)))
    }
}

fn run(cmd: &str, a: &Args) -> Result<Value, Fail> {
    let u = Fail::Usage;
    match cmd {
        "sessions" => Ok(json!(ws::sessions().into_iter().map(|e| json!({"sessionId": e.session_id, "pid": e.pid, "project": e.project})).collect::<Vec<_>>())),
        "status" | "catalog" | "schema" | "undo" | "redo" | "stop" => send(a, cmd, json!({})),
        "context" => send(a, "context", json!({"full": a.has("full")})),
        "analyze" => send(a, "music_analyze", json!({"clipId": a.num("clip").map_err(u)?.map(|v| v as u64), "excludeFx": a.num("exclude-fx").map_err(u)?.map(|v| v as u64)})),
        "apply" => {
            let text = match (a.get("file"), a.get("json")) {
                (Some(f), _) => std::fs::read_to_string(f).map_err(|e| u(format!("{f}: {e}")))?,
                (None, Some(j)) => j.to_string(),
                _ => return Err(u("apply needs --file ops.json or --json '[...]'".into())),
            };
            let v: Value = serde_json::from_str(&text).map_err(|e| u(format!("ops are not valid JSON: {e}")))?;
            let ops = match v {
                Value::Array(_) => v,
                Value::Object(ref m) if m.contains_key("ops") => m["ops"].clone(),
                Value::Object(_) => json!([v]),
                _ => return Err(u("ops must be an array of op objects".into())),
            };
            send(a, "apply", json!({"ops": ops}))
        }
        "bpm" => {
            let v: f64 = a.pos.get(1).and_then(|s| s.parse().ok()).ok_or_else(|| u("bpm <value>".into()))?;
            send(a, "apply", json!({"ops": [{"op": "setBpm", "bpm": v}]}))
        }
        "add-fx" => send(a, "add_fx", json!({"insertId": a.num("insert").map_err(u)?.ok_or_else(|| u("missing --insert".into()))? as u64, "type": a.need("type").map_err(u)?, "preset": a.num("preset").map_err(u)?})),
        "set-param" => send(a, "set_param", json!({"fxId": a.num("fx").map_err(u)?.ok_or_else(|| u("missing --fx".into()))? as u64, "param": a.need("param").map_err(u)?, "value": a.num("value").map_err(u)?.ok_or_else(|| u("missing --value".into()))?})),
        "preset" => send(a, "load_preset", json!({"fxId": a.num("fx").map_err(u)?.ok_or_else(|| u("missing --fx".into()))? as u64, "index": a.num("index").map_err(u)?.ok_or_else(|| u("missing --index".into()))? as u64})),
        "import" => {
            let files: Vec<String> = a.pos[1..].iter().map(|p| std::fs::canonicalize(p).map(|x| x.display().to_string()).unwrap_or_else(|_| p.clone())).collect();
            if files.is_empty() {
                return Err(u("import <file>...".into()));
            }
            send(a, "import", json!({"paths": files, "track": a.num("track").map_err(u)?, "beat": a.num("beat").map_err(u)?}))
        }
        "takes" => send(a, "takes", json!({"track": a.num("track").map_err(u)?.ok_or_else(|| u("missing --track".into()))? as u64})),
        "comp" => {
            let mut args = json!({"track": a.num("track").map_err(u)?.map(|v| v as u64), "clip": a.num("clip").map_err(u)?.map(|v| v as u64)});
            if args["track"].is_null() && args["clip"].is_null() {
                return Err(u("comp needs --track N (or --clip ID)".into()));
            }
            if a.has("flatten") {
                return send(a, "flatten_comp", args);
            }
            if a.has("audition") {
                let v = a.need("audition").map_err(u)?;
                args["take"] = if v == "off" { Value::Null } else { json!(v.parse::<u64>().map_err(|_| u(format!("--audition takes a take id or `off`, got {v}")))?) };
                return send(a, "comp_audition", args);
            }
            if let Some(s) = a.get("set") {
                args["regions"] = serde_json::from_str(s).map_err(|e| u(format!("--set is not JSON: {e}")))?;
            }
            if let Some(t) = a.num("take").map_err(u)? {
                args["take"] = json!(t as u64);
            }
            if let Some(x) = a.num("xfade").map_err(u)? {
                args["xfadeMs"] = json!(x);
            }
            if a.has("remove-unused") {
                args["removeUnused"] = json!(true);
            }
            send(a, "comp", args)
        }
        "play" => send(a, "play", json!({"from": a.num("from").map_err(u)?})),
        "render" => {
            let out = a.need("out").map_err(u)?;
            let out = std::path::absolute(out).map(|p| p.display().to_string()).unwrap_or_else(|_| out.to_string());
            send(a, "render", json!({"out": out, "from": a.num("from").map_err(u)?, "to": a.num("to").map_err(u)?, "tail": a.num("tail").map_err(u)?, "rate": a.num("rate").map_err(u)?}))
        }
        "save" => send(a, "save", json!({"path": a.get("path").map(|p| std::path::absolute(p).map(|x| x.display().to_string()).unwrap_or_else(|_| p.to_string()))})),
        "open" => {
            let p = a.pos.get(1).ok_or_else(|| u("open <file>".into()))?;
            send(a, "open", json!({"path": std::path::absolute(p).map(|x| x.display().to_string()).unwrap_or_else(|_| p.clone())}))
        }
        "vst3" => send(a, "vst3_scan", json!({"rescan": a.has("rescan")})),
        "vst3-add" => send(a, "vst3_add", json!({"insertId": a.num("insert").map_err(u)?.ok_or_else(|| u("missing --insert".into()))? as u64, "path": a.need("path").map_err(u)?, "cid": a.need("cid").map_err(u)?})),
        "vst3-params" => send(a, "vst3_params", json!({"fxId": a.num("fx").map_err(u)?.ok_or_else(|| u("missing --fx".into()))? as u64, "query": a.get("query")})),
        "vst3-set" => send(a, "vst3_set_param", json!({"fxId": a.num("fx").map_err(u)?.ok_or_else(|| u("missing --fx".into()))? as u64, "id": a.num("id").map_err(u)?.ok_or_else(|| u("missing --id".into()))? as u64, "value": a.num("value").map_err(u)?.ok_or_else(|| u("missing --value".into()))?})),
        "call" => {
            let c = a.pos.get(1).ok_or_else(|| u("call <command> [--args JSON]".into()))?;
            let args = match a.get("args") {
                Some(t) => serde_json::from_str(t).map_err(|e| u(format!("--args is not JSON: {e}")))?,
                None => json!({}),
            };
            send(a, c, args)
        }
        other => Err(u(format!("unknown command `{other}`"))),
    }
}
