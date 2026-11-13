//! `wave` — the command line: the project open in a running WAVE app, or (with `--project`,
//! given a [`Files`] provider by the app binary) a project file with no app running.
//!
//! stdout is only the JSON result; diagnostics go to stderr.
//! Exit codes: 0 ok, 1 usage, 2 no session / connection, 3 the app refused the request.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::{json, Value};
use wave_session as ws;

const HELP: &str = r#"wave — edit, render and inspect WAVE projects
docs: https://github.com/miskibin/wave-DAW/blob/main/docs/cli.md

USAGE
  wave <command> [options]                 the project open in the running app (--session ID if several are open)
  wave -p song.wave <command> [options]    a project file, no app needed: load, run, save back if it changed
                                           [--save-as FILE] [--dry-run: save nothing] [--force: even if the app has it open]
  wave new song.wave [--template empty|sketch] [--bpm N]   create a project file

FOR SCRIPTS AND AGENTS
  stdout is one JSON value, messages go to stderr. Exit: 0 ok, 1 usage, 2 app not running/unreachable,
  3 refused ("CODE: message", e.g. BAD_OPS, REVISION_CONFLICT, NEEDS_APP, FILE_OPEN).
  Read `context` first, then `schema` (ops for `apply`) and `catalog` (effect params, presets).
  Units: time in beats (4 = 1 bar), note step/len in 16ths, vol 0.7937 = 0 dB, clip gain dB, pan -1..1.
  New ids: count up from context.nextId. One `apply` batch is atomic: prefer it for related edits.
  Check a result without listening: `render --out x.wav` returns peak/RMS/crest/clipping per file.
  On a file, play/stop/undo/redo/auditions are refused (NEEDS_APP); edits to a file the app has
  open are refused (FILE_OPEN) because the app would overwrite them on its next save.

SCRIPTS
  script FILE|-                            one command per line ('…'/"…" quote, # comments, a leading
                                           `wave` is ignored); stops at the first failing line. With -p:
                                           one load and one save, nothing written if a line fails

READ
  sessions                                 list running WAVE apps
  status                                   transport, revision, file, audio device
  context [--full]                         project summary (--full: every clip, note and parameter)
  catalog                                  effect types, parameters and presets
  schema                                   every op `apply` accepts, with an example
  analyze [--clip ID] [--exclude-fx ID]    detect tempo (BPM) and musical key

EDIT  (all edits are one undo step each; --if-revision N refuses to act if the project moved on)
  apply --file ops.json | --json '[...]'   apply a batch of ops atomically   [--label TEXT]
  undo | redo                              (running app only)
  bpm <value>
  add-fx --insert ID --type eq|comp|sat|delay|half|warp|reverb|limit|... [--preset N]
  set-param --fx ID --param NAME --value V
  preset --fx ID --index N
  import <file>... [--track N --beat B]    load audio, optionally place it
  make-unique --clip ID[,ID...]            give audio clips (one sample, one track) their own copy of the
                                           sample on a new track below, routed to a free mixer insert

PATTERNS & CHANNELS  (steps: x = on, 1-9 = velocity 10-90 %, . = off; spaces and | are ignored)
  channels                                 Channel Rack: ids, instrument/sample, volume, pan, insert
  channels --add kick-808|snare|marimba…   add a sampler of a standard-library sample (--sample SOURCE_ID: of a project source)
  channels --keys                          add WAVE Keys, the sampled grand piano
  channels --synth "PRESET"                add a synth channel ("" = blank patch); --ch C --preset NAME loads one
  channels --ch C --param ID --value V     one knob of a synth (ids in `catalog`) or of WAVE Keys (release ms, dynamics %)
  channels --remove ID|NAME                remove a channel and its notes
  pattern [--id N]                         a pattern as step strings per channel
  pattern --id N --ch C --steps "x... x... x... x..." [--vel 0.8]   replace channel C's notes
  pattern --id N --ch C --notes '[{"step":0,"len":4,"p":36,"v":0.9}]'  piano-roll notes (p = MIDI key)
  pattern --id N --ch C --clear            remove channel C's notes from the pattern
  pattern --id N [--bars N (minimum)] [--swing 0..100] [--name TEXT]      (--new instead of --id: a new pattern)
  pattern --id N --play [--from B] [--loop A:B] | --stop
                                           loop the pattern on its own (stops the song), from
                                           beat B, only beats A..B of it

TAKES & COMPING  (times in timeline beats; add --clip ID when a track has several comps)
  takes --track N                          takes, comp regions and crossfades of the track
  comp --track N --set '[{"take":ID,"from":B,"to":B,"xfadeMs":20},...]'   replace the comp
  comp --track N --take ID                 play one whole take
  comp --track N --xfade MS                default crossfade at region boundaries
  comp --track N --remove-unused           drop takes the comp does not play
  comp --track N --flatten                 render the comp to one audio clip (removes the takes)
  comp --track N --audition ID|off         play one take instead of the comp (not an edit)

TRANSPORT  (running app only)
  play [--from BEAT] | stop

FILES
  render --out file.wav|.mp3 [--from B --to B --tail SEC --rate HZ --format wav|mp3 --kbps 128|192|256|320]
                                           bounce offline (mp3 chosen by .mp3 extension); prints level stats
  render --stems --out FOLDER [--no-master] [--from B --to B --tail SEC --rate HZ]
                                           one WAV per mixer insert with sound (named after it) + Master.wav
  WAV options: --wav-bits 24|32 (default 32-bit float); --dither (24-bit PCM only)
               --wrap-tail (carry effects into the start of the loop)
  save [--path FILE] | open <FILE>          (app; with -p the file is saved for you, --save-as copies)

PITCH  (Tune to Reference: pull a vocal take onto another performance of the same melody)
  tune-ref --clip ID --reference SOURCE | --melody CHANNEL [--correction 0..100 --vibrato 0..100
           --influence 0..100 --speed 0..1000 --octave auto|-24..24 --key 0..11 --scale 0..4]
                                           switch a clip to it (--off: manual); --melody: the notes that
                                           channel plays over the clip in the playlist (taken again each time)
  melody-channels --clip ID                channels playing notes over the clip, best fit to the singing first
  pitch-target --clip ID [--full]          the computed target: score, octave, notes (--full: curves)
  f0-model                                 pitch detector in use; RMVPE model install state

PITCH EDITING  (the pitch editor's notes for scripts and agents; indices come from `pitch inspect`)
  pitch inspect --clip ID [--full [--from S --to S]] [--all]
                                           notes: sung pitch, cents off, kind (stable/vibrato/glide/short/unsure),
                                           confidence, edits, resulting pitch; `base` = segmentation fingerprint
                                           (--full: frame curves; --all: notes outside the clip's region too)
  pitch compare --clip ID --source SRC | --ref-clip ID
                                           read-only: interval (any, not only octaves), note matches (match/merged/
                                           split/extra), ornaments, timing, similarity, unmatched stretches
  pitch suggest --clip ID [--source SRC] [--max-cents 60 --min-cents 15 --strength 100 --key C --scale major
                --tuning CENTS]            a cautious proposal with a reason per note and a ready `op`; read-only
  pitch edit --clip ID [--base FP] [--note I (--shift C | --cents C | --target MIDI) [--drift % --formant C
             --gain dB --speed MS|none]] [--split I@SEC] [--merge I] [--reset I,J] [--clear]
             [--json EDIT | --file edit.json] [--max-cents C]      one undo step (the editTune op)
  pitch render --clip ID --out take.wav [--original]   the clip alone with its edits (or without), 32-bit WAV
  tune-file --take u.wav --reference r.wav --out o.wav [--json target.json]
            [--model rmvpe.onnx | --yin] [same options as tune-ref]   headless; needs no running app

PLUGINS
  vst3 [--rescan]                          installed VST3 effects
  vst3-add --insert ID --path P --cid C    put a VST3 on an insert
  vst3-params --fx ID [--query TEXT]       parameters of a VST3 instance
  vst3-set --fx ID --id PARAM --value 0..1
  auto-add --fx ID --param ID [--points "0:0.2,4:0.8" | --value V] [--track N --name TEXT]
                                           automate a hosted plugin parameter (adds the playlist clip too)

NOTES  (deterministic transforms and generators for a pattern's notes)
  notes humanize --pattern ID [--timing 0..1 --velocity 0..1]
  notes quantize --pattern ID [--grid 1|2|4 --amount 0..1 --scale]
  notes euclid --pattern ID --ch CH --k K --n N [--rotate INT --vel 0..1]
  notes strum --pattern ID [--width STEPS --ramp VEL]
  notes drums --pattern ID --bars N --seed S --voice "CH:K:N[:ROT[:VEL]]" (repeatable)
  notes melody --pattern ID --ch CH [--bars N --density 0..1 --repeat N --seed S]
  notes … [--select "FROM:TO[@CH]" | --indices "1,2,5"]                limit the notes a verb touches
  key [NOTE] [SCALE] [--off]                            project key/scale (major minor pent…) for scale-aware tools

  call <command> [--args JSON]             raw request (see `catalog`)
  --pretty                                 indent JSON output
"#;

/// Opens and creates project files in this process: what `--project` and `new` need.
/// The app binary provides it; the standalone `wave` binary only talks to a running app.
pub trait Files {
    /// Loads a project file; the warnings name audio that could not be loaded.
    fn open(&self, path: &Path) -> Result<(Box<dyn Doc>, Vec<String>), String>;
    /// A new, unsaved project from a template: `empty` or `sketch`.
    fn create(&self, template: &str) -> Result<Box<dyn Doc>, String>;
}

/// A project held by this process.
pub trait Doc {
    /// The same request the running app answers; an error is `CODE: message` or a message.
    fn call(&self, command: &str, args: &Value, if_revision: Option<u64>) -> Result<Value, String>;
    fn revision(&self) -> u64;
    /// Changed since it was loaded or saved.
    fn dirty(&self) -> bool;
    fn save(&self, path: &Path) -> Result<(), String>;
}

/// Options that are only present or absent; they never take the word after them as a value.
const SWITCHES: &[&str] = &[
    "all",
    "clear",
    "dither",
    "dry-run",
    "flatten",
    "force",
    "full",
    "help",
    "keys",
    "new",
    "no-master",
    "off",
    "original",
    "play",
    "pretty",
    "remove-unused",
    "rescan",
    "stems",
    "stop",
    "wrap-tail",
    "yin",
];

#[derive(Clone)]
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
            let key = match a.as_str() {
                "-p" => Some("project"),
                _ => a.strip_prefix("--"),
            };
            if let Some(k) = key {
                // a switch never takes the next word: `--dry-run -p f.wave` or `-p f --dry-run bpm 90`
                // would otherwise turn the file or the command into its value
                let is_opt = |n: &str| (n.starts_with("--") || n == "-p") && n.parse::<f64>().is_err();
                let v = match it.peek() {
                    Some(n) if !SWITCHES.contains(&k) && !is_opt(n) => it.next(),
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
    fn get_all(&self, k: &str) -> Vec<&str> {
        self.opts.iter().filter(|(n, _)| n == k).filter_map(|(_, v)| v.as_deref()).collect()
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

#[derive(Debug)]
enum Fail {
    Usage(String),
    Conn(String),
    App(String),
}

/// Where requests go: the running app (`doc` = None) or a project held here.
struct Cx<'a> {
    doc: Option<&'a dyn Doc>,
}

/// Decimal places kept in printed numbers. The engine stores many values as `f32`, so a step of
/// 0.47 prints as 0.4699999988079071 without this; four places keep triplet positions
/// (28.3333) and fader values (0.7937 = 0 dB) exact enough to send back.
const PRINT_DECIMALS: i32 = 4;

/// Rounds every non-integer number in a reply to [`PRINT_DECIMALS`] places (integers and ids untouched).
fn tidy_numbers(v: &mut Value) {
    match v {
        Value::Number(n) if !n.is_i64() && !n.is_u64() => {
            if let Some(f) = n.as_f64() {
                let k = 10f64.powi(PRINT_DECIMALS);
                let r = (f * k).round() / k;
                if let Some(m) = serde_json::Number::from_f64(if r == 0.0 { 0.0 } else { r }) {
                    *n = m;
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(tidy_numbers),
        Value::Object(o) => o.values_mut().for_each(tidy_numbers),
        _ => {}
    }
}

/// Runs the CLI with `raw` arguments (without the program name), against the running app only.
pub fn run_cli(raw: Vec<String>) -> ExitCode {
    run_cli_with(raw, None)
}

/// Runs the CLI; with `files`, `--project FILE` and `new` work without a running app.
pub fn run_cli_with(raw: Vec<String>, files: Option<&dyn Files>) -> ExitCode {
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
    let res = if cmd == "new" {
        new_file(files, &a)
    } else if a.has("project") {
        on_file(files, &cmd, &a)
    } else {
        run(&Cx { doc: None }, &cmd, &a)
    };
    match res {
        Ok(mut v) => {
            tidy_numbers(&mut v);
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

fn need_files(files: Option<&dyn Files>) -> Result<&dyn Files, Fail> {
    files.ok_or_else(|| {
        Fail::Usage(
            "this `wave` binary only talks to a running app; project files need the `wave` the WAVE app installs (it runs `WAVE --cli`)"
                .into(),
        )
    })
}

fn abs(p: &str) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| PathBuf::from(p))
}

/// The same file, also when one of them does not exist yet (case-insensitive on Windows and macOS).
fn same_file(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        let p = std::fs::canonicalize(p).unwrap_or_else(|_| abs(&p.to_string_lossy()));
        let s = p.to_string_lossy().trim_start_matches(r"\\?\").replace('\\', "/");
        if cfg!(any(windows, target_os = "macos")) {
            s.to_lowercase()
        } else {
            s
        }
    };
    norm(a) == norm(b)
}

/// Refuses to write a file a running app has open: its next save would silently undo this edit.
fn not_open_in_app(path: &Path, a: &Args) -> Result<(), Fail> {
    if a.has("force") {
        return Ok(());
    }
    match open_in_session(path) {
        Some(id) => Err(Fail::App(format!(
            "FILE_OPEN: {} is open in WAVE (session {id}). Edit it there (drop --project; the edit joins the app's undo history), close it in the app first, or add --force (the app overwrites the file on its next save)",
            path.display(),
        ))),
        None => Ok(()),
    }
}

/// The (short) id of a running app that has `path` open.
fn open_in_session(path: &Path) -> Option<String> {
    ws::sessions().into_iter().find_map(|ep| {
        let rep = ws::call(&ep, "status", json!({}), None, None).ok()?;
        let open = rep.data.get("path").and_then(Value::as_str).map(PathBuf::from)?;
        same_file(&open, path).then(|| ep.session_id[..ep.session_id.len().min(8)].to_string())
    })
}

/// `wave --project FILE <command>`: load, run, save if it changed.
fn on_file(files: Option<&dyn Files>, cmd: &str, a: &Args) -> Result<Value, Fail> {
    let files = need_files(files)?;
    let given = a.get("project").ok_or_else(|| Fail::Usage("--project needs a file: --project song.wave".into()))?;
    let path = abs(given);
    if a.has("session") {
        return Err(Fail::Usage("--session talks to a running app and --project works on a file; pick one".into()));
    }
    if a.has("save-as") && a.get("save-as").is_none() {
        return Err(Fail::Usage("--save-as needs a file".into()));
    }
    if !path.is_file() {
        return Err(Fail::Usage(format!("{given} does not exist (create it with `wave new {given}`)")));
    }
    let (doc, warnings) = files.open(&path).map_err(|e| Fail::App(format!("OPEN_FAILED: {e}")))?;
    for w in &warnings {
        eprintln!("wave: warning: {w}");
    }
    let cx = Cx { doc: Some(&*doc) };
    let mut out = run(&cx, cmd, a)?;
    let save_as = a.get("save-as").or(if cmd == "save" { a.get("path") } else { None }).map(abs);
    let target = save_as.clone().unwrap_or(path);
    let wanted = doc.dirty() || save_as.is_some() || cmd == "save";
    let dry = a.has("dry-run");
    let saved = if wanted && !dry {
        not_open_in_app(&target, a)?;
        doc.save(&target).map_err(|e| Fail::App(format!("SAVE_FAILED: {}: {e}", target.display())))?;
        Some(target.display().to_string())
    } else {
        None
    };
    if let Value::Object(m) = &mut out {
        m.insert("saved".into(), json!(saved));
        if dry {
            m.insert("dryRun".into(), json!({ "changed": doc.dirty(), "wouldSave": wanted.then(|| target.display().to_string()) }));
        }
        if !warnings.is_empty() {
            m.insert("loadWarnings".into(), json!(warnings));
        }
    }
    Ok(out)
}

/// `wave new FILE [--template empty|sketch] [--bpm N] [--force]`
fn new_file(files: Option<&dyn Files>, a: &Args) -> Result<Value, Fail> {
    let u = Fail::Usage;
    let files = need_files(files)?;
    let raw = a.pos.get(1).ok_or_else(|| u("new <FILE> [--template empty|sketch] [--bpm N]".into()))?;
    let mut path = abs(raw);
    if path.extension().is_none() {
        path.set_extension("wave");
    }
    if path.exists() && !a.has("force") {
        return Err(Fail::App(format!("EXISTS: {} already exists (--force replaces it)", path.display())));
    }
    let template = a.get("template").unwrap_or("empty");
    let doc = files.create(template).map_err(u)?;
    if let Some(bpm) = a.num("bpm").map_err(u)? {
        doc.call("apply", &json!({"ops": [{"op": "setBpm", "bpm": bpm}]}), None).map_err(|e| Fail::App(app_error(e)))?;
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| Fail::App(format!("SAVE_FAILED: {}: {e}", dir.display())))?;
    }
    // `--force` here only means "replace an existing file": a project the app has open is never replaced
    // behind its back (its next save would write the old project over the new one)
    if let Some(id) = open_in_session(&path) {
        return Err(Fail::App(format!("FILE_OPEN: {} is open in WAVE (session {id}); close it in the app first", path.display())));
    }
    doc.save(&path).map_err(|e| Fail::App(format!("SAVE_FAILED: {}: {e}", path.display())))?;
    let ctx = doc.call("context", &json!({}), None).map_err(|e| Fail::App(app_error(e)))?;
    Ok(
        json!({ "saved": path.display().to_string(), "template": template, "bpm": ctx["bpm"], "tracks": ctx["tracks"].as_array().map(Vec::len), "nextId": ctx["nextId"] }),
    )
}

/// `CODE: message`, as the running app reports a refusal.
fn app_error(m: String) -> String {
    let coded = m.split(':').next().is_some_and(|c| c.len() > 2 && c.chars().all(|ch| ch.is_ascii_uppercase() || ch == '_'));
    if coded {
        m
    } else {
        format!("REFUSED: {m}")
    }
}

fn session(a: &Args) -> Result<ws::Endpoint, Fail> {
    let all = ws::sessions();
    match a.get("session") {
        Some(id) => {
            all.into_iter().find(|e| e.session_id.starts_with(id)).ok_or_else(|| Fail::Conn(format!("no running WAVE session {id}")))
        }
        None => match all.len() {
            0 => Err(Fail::Conn("WAVE is not running (start the app, or work on a file: wave --project song.wave …)".into())),
            1 => Ok(all.into_iter().next().unwrap()),
            n => Err(Fail::Conn(format!("{n} WAVE apps are open; pick one with --session ID (see `wave sessions`)"))),
        },
    }
}

fn send(cx: &Cx, a: &Args, command: &str, args: Value) -> Result<Value, Fail> {
    let rev = a.num("if-revision").map_err(Fail::Usage)?.map(|v| v as u64);
    if let Some(doc) = cx.doc {
        let mut d = doc.call(command, &args, rev).map_err(|e| Fail::App(app_error(e)))?;
        if let Value::Object(m) = &mut d {
            m.entry("revision").or_insert(json!(doc.revision()));
        }
        return Ok(d);
    }
    let ep = session(a)?;
    let rep = ws::call(&ep, command, args, rev, a.get("label").map(str::to_owned))
        .map_err(|e| Fail::Conn(format!("could not reach WAVE: {e}")))?;
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

/// Splits a script line like a shell: whitespace separates, '…' and "…" group. No escapes,
/// so Windows paths keep their backslashes and JSON in '…' keeps its double quotes.
fn split_line(line: &str) -> Result<Vec<String>, String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    for ch in line.chars() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => cur.push(ch),
            None if ch == '\'' || ch == '"' => {
                quote = Some(ch);
                started = true;
            }
            None if ch.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            None => {
                cur.push(ch);
                started = true;
            }
        }
    }
    if let Some(q) = quote {
        return Err(format!("unclosed {q}"));
    }
    if started {
        out.push(cur);
    }
    Ok(out)
}

/// Options of the whole invocation, not of one script line.
const OUTER_ONLY: &[&str] = &["project", "session", "save-as", "dry-run", "force", "pretty"];

/// `wave [--project FILE] script FILE|-`: one command per line, stops at the first failure.
fn script(cx: &Cx, a: &Args) -> Result<Value, Fail> {
    let u = Fail::Usage;
    let src = a.pos.get(1).ok_or_else(|| u("script <FILE> (or - for stdin)".into()))?;
    let text = if src == "-" {
        let mut t = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut t).map_err(|e| u(format!("stdin: {e}")))?;
        t
    } else {
        std::fs::read_to_string(src).map_err(|e| u(format!("{src}: {e}")))?
    };
    let mut results = vec![];
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let mut words = split_line(t).map_err(|e| u(format!("line {n}: {e}")))?;
        if words.first().map(String::as_str) == Some("wave") {
            words.remove(0);
        }
        let mut la = Args::parse(words);
        let Some(cmd) = la.pos.first().cloned() else { continue };
        if matches!(cmd.as_str(), "script" | "new" | "help") {
            return Err(u(format!("line {n}: `{cmd}` cannot run inside a script")));
        }
        if let Some((k, _)) = la.opts.iter().find(|(k, _)| OUTER_ONLY.contains(&k.as_str())) {
            return Err(u(format!("line {n}: --{k} belongs to the whole script: `wave --{k} … script FILE`")));
        }
        // on a file, where the project is written is decided once, after the script: a copy made here would be dropped
        if cx.doc.is_some() && cmd == "save" && la.has("path") {
            return Err(u(format!(
                "line {n}: `save --path` cannot write a copy from a script; use `wave --project … --save-as FILE script …`"
            )));
        }
        if let Some(s) = a.get("session") {
            la.opts.push(("session".into(), Some(s.into())));
        }
        match run(cx, &cmd, &la) {
            Ok(v) => results.push(json!({ "line": n, "command": cmd, "result": v })),
            Err(e) => {
                let done = if cx.doc.is_some() {
                    "nothing was saved".to_string()
                } else if results.is_empty() {
                    "nothing was changed".to_string()
                } else {
                    format!(
                        "the {} command(s) before it were applied to the open project (each is one undo step in the app)",
                        results.len()
                    )
                };
                let at = |m: String| format!("line {n} (`{cmd}`): {m}; {done}");
                return Err(match e {
                    Fail::Usage(m) => Fail::Usage(at(m)),
                    Fail::Conn(m) => Fail::Conn(at(m)),
                    Fail::App(m) => Fail::App(at(m)),
                });
            }
        }
    }
    Ok(json!({ "results": results }))
}

/// `12` → 12, `kick` → "kick".
fn id_or_name(s: &str) -> Value {
    s.parse::<u64>().map(|n| json!(n)).unwrap_or_else(|_| json!(s))
}

fn run(cx: &Cx, cmd: &str, a: &Args) -> Result<Value, Fail> {
    let u = Fail::Usage;
    match cmd {
        "sessions" => Ok(json!(ws::sessions()
            .into_iter()
            .map(|e| json!({"sessionId": e.session_id, "pid": e.pid, "project": e.project}))
            .collect::<Vec<_>>())),
        "status" | "catalog" | "schema" | "undo" | "redo" | "stop" => send(cx, a, cmd, json!({})),
        "context" => send(cx, a, "context", json!({"full": a.has("full")})),
        "analyze" => send(
            cx,
            a,
            "music_analyze",
            json!({"clipId": a.num("clip").map_err(u)?.map(|v| v as u64), "excludeFx": a.num("exclude-fx").map_err(u)?.map(|v| v as u64)}),
        ),
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
            send(cx, a, "apply", json!({"ops": ops}))
        }
        "bpm" => {
            let v: f64 = a.pos.get(1).and_then(|s| s.parse().ok()).ok_or_else(|| u("bpm <value>".into()))?;
            send(cx, a, "apply", json!({"ops": [{"op": "setBpm", "bpm": v}]}))
        }
        "add-fx" => send(
            cx,
            a,
            "add_fx",
            json!({"insertId": a.num("insert").map_err(u)?.ok_or_else(|| u("missing --insert".into()))? as u64, "type": a.need("type").map_err(u)?, "preset": a.num("preset").map_err(u)?}),
        ),
        "set-param" => send(
            cx,
            a,
            "set_param",
            json!({"fxId": a.num("fx").map_err(u)?.ok_or_else(|| u("missing --fx".into()))? as u64, "param": a.need("param").map_err(u)?, "value": a.num("value").map_err(u)?.ok_or_else(|| u("missing --value".into()))?}),
        ),
        "preset" => send(
            cx,
            a,
            "load_preset",
            json!({"fxId": a.num("fx").map_err(u)?.ok_or_else(|| u("missing --fx".into()))? as u64, "index": a.num("index").map_err(u)?.ok_or_else(|| u("missing --index".into()))? as u64}),
        ),
        "import" => {
            let files: Vec<String> = a.pos[1..]
                .iter()
                .map(|p| std::fs::canonicalize(p).map(|x| x.display().to_string()).unwrap_or_else(|_| p.clone()))
                .collect();
            if files.is_empty() {
                return Err(u("import <file>...".into()));
            }
            send(cx, a, "import", json!({"paths": files, "track": a.num("track").map_err(u)?, "beat": a.num("beat").map_err(u)?}))
        }
        "channels" => {
            let mut args = json!({});
            if let Some(k) = a.get("add") {
                args["add"] = json!(k);
            }
            if let Some(s) = a.num("sample").map_err(u)? {
                args["sample"] = json!(s as u64);
            }
            if let Some(r) = a.get("remove") {
                args["remove"] = id_or_name(r);
            }
            if a.has("keys") {
                args["keys"] = json!(true);
            }
            if a.has("synth") {
                args["synth"] = json!(a.get("synth").unwrap_or(""));
            }
            if let Some(c) = a.get("ch") {
                args["ch"] = id_or_name(c);
            }
            for k in ["preset", "param"] {
                if let Some(v) = a.get(k) {
                    args[k] = json!(v);
                }
            }
            if let Some(v) = a.num("value").map_err(u)? {
                args["value"] = json!(v);
            }
            send(cx, a, "channels", args)
        }
        "pattern" => {
            if a.has("play") || a.has("stop") {
                let id =
                    if a.has("stop") { None } else { Some(a.num("id").map_err(u)?.ok_or_else(|| u("--play needs --id".into()))? as u64) };
                let mut args = json!({ "id": id });
                if let Some(b) = a.num("from").map_err(u)? {
                    args["from"] = json!(b);
                }
                if let Some(v) = a.get("loop") {
                    let r = v
                        .split_once(':')
                        .and_then(|(x, y)| Some((x.trim().parse::<f64>().ok()?, y.trim().parse::<f64>().ok()?)))
                        .filter(|(x, y)| x.is_finite() && y.is_finite())
                        .ok_or_else(|| u(format!("--loop wants A:B in beats, not `{v}`")))?;
                    args["range"] = json!([r.0, r.1]);
                }
                return send(cx, a, "pattern_play", args);
            }
            let mut args = json!({ "new": a.has("new"), "clear": a.has("clear") });
            if let Some(id) = a.num("id").map_err(u)? {
                args["id"] = json!(id as u64);
            }
            for k in ["name", "color", "steps"] {
                if let Some(v) = a.get(k) {
                    args[k] = json!(v);
                }
            }
            for k in ["bars", "swing", "vel"] {
                if let Some(v) = a.num(k).map_err(u)? {
                    args[k] = json!(v);
                }
            }
            if let Some(c) = a.get("ch") {
                args["ch"] = id_or_name(c);
            }
            if let Some(n) = a.get("notes") {
                args["notes"] = serde_json::from_str(n).map_err(|e| u(format!("--notes is not JSON: {e}")))?;
            }
            send(cx, a, "pattern", args)
        }
        "make-unique" => {
            let ids = a
                .need("clip")
                .map_err(u)?
                .split(',')
                .map(|x| x.trim().parse::<u64>().map_err(|_| u(format!("--clip takes clip ids separated by commas, got {x}"))))
                .collect::<Result<Vec<_>, _>>()?;
            send(cx, a, "make_unique", json!({ "clipIds": ids }))
        }
        "takes" => send(cx, a, "takes", json!({"track": a.num("track").map_err(u)?.ok_or_else(|| u("missing --track".into()))? as u64})),
        "comp" => {
            let mut args =
                json!({"track": a.num("track").map_err(u)?.map(|v| v as u64), "clip": a.num("clip").map_err(u)?.map(|v| v as u64)});
            if args["track"].is_null() && args["clip"].is_null() {
                return Err(u("comp needs --track N (or --clip ID)".into()));
            }
            if a.has("flatten") {
                return send(cx, a, "flatten_comp", args);
            }
            if a.has("audition") {
                let v = a.need("audition").map_err(u)?;
                args["take"] = if v == "off" {
                    Value::Null
                } else {
                    json!(v.parse::<u64>().map_err(|_| u(format!("--audition takes a take id or `off`, got {v}")))?)
                };
                return send(cx, a, "comp_audition", args);
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
            send(cx, a, "comp", args)
        }
        "play" => send(cx, a, "play", json!({"from": a.num("from").map_err(u)?})),
        "render" => {
            let out = a.need("out").map_err(u)?;
            let out = std::path::absolute(out).map(|p| p.display().to_string()).unwrap_or_else(|_| out.to_string());
            let wav_bits = if a.has("wav-bits") {
                let bits = a.need("wav-bits").map_err(u)?;
                Some(match bits {
                    "24" => 24,
                    "32" => 32,
                    _ => return Err(u("--wav-bits must be 24 or 32".into())),
                })
            } else {
                None
            };
            send(
                cx,
                a,
                "render",
                json!({
                    "out": out,
                    "from": a.num("from").map_err(u)?, "to": a.num("to").map_err(u)?,
                    "tail": a.num("tail").map_err(u)?, "rate": a.num("rate").map_err(u)?,
                    "format": a.get("format"), "mp3Kbps": a.num("kbps").map_err(u)?,
                    "wavBits": wav_bits, "dither": a.has("dither"), "wrapTail": a.has("wrap-tail"),
                    "stems": a.has("stems"), "master": !a.has("no-master"),
                }),
            )
        }
        // in file mode the project is saved after the command (see `on_file`)
        "save" if cx.doc.is_some() => Ok(json!({})),
        "save" => send(
            cx,
            a,
            "save",
            json!({"path": a.get("path").map(|p| std::path::absolute(p).map(|x| x.display().to_string()).unwrap_or_else(|_| p.to_string()))}),
        ),
        "open" => {
            let p = a.pos.get(1).ok_or_else(|| u("open <file>".into()))?;
            send(cx, a, "open", json!({"path": std::path::absolute(p).map(|x| x.display().to_string()).unwrap_or_else(|_| p.clone())}))
        }
        "vst3" => send(cx, a, "vst3_scan", json!({"rescan": a.has("rescan")})),
        "vst3-add" => send(
            cx,
            a,
            "vst3_add",
            json!({"insertId": a.num("insert").map_err(u)?.ok_or_else(|| u("missing --insert".into()))? as u64, "path": a.need("path").map_err(u)?, "cid": a.need("cid").map_err(u)?}),
        ),
        "vst3-params" => send(
            cx,
            a,
            "vst3_params",
            json!({"fxId": a.num("fx").map_err(u)?.ok_or_else(|| u("missing --fx".into()))? as u64, "query": a.get("query")}),
        ),
        "vst3-set" => send(
            cx,
            a,
            "vst3_set_param",
            json!({"fxId": a.num("fx").map_err(u)?.ok_or_else(|| u("missing --fx".into()))? as u64, "id": a.num("id").map_err(u)?.ok_or_else(|| u("missing --id".into()))? as u64, "value": a.num("value").map_err(u)?.ok_or_else(|| u("missing --value".into()))?}),
        ),
        "auto-add" => {
            // an automation envelope on a hosted VST3 parameter, plus the playlist clip that plays it
            let fx = a.num("fx").map_err(u)?.ok_or_else(|| u("missing --fx".into()))? as u64;
            let param = a.num("param").map_err(u)?.ok_or_else(|| u("missing --param".into()))? as u64;
            let mut points: Vec<Value> = vec![];
            if let Some(p) = a.get("points") {
                for pair in p.split(',') {
                    let (t, v) = pair.split_once(':').ok_or_else(|| u("--points is t:v pairs, e.g. 0:0.2,4:0.8".into()))?;
                    let t: f64 = t.trim().parse().map_err(|_| u(format!("bad time `{t}`")))?;
                    let v: f64 = v.trim().parse().map_err(|_| u(format!("bad value `{v}`")))?;
                    if !(0.0..=1.0).contains(&v) {
                        return Err(u(format!("value {v} outside 0..1")));
                    }
                    points.push(json!({"t": t, "v": v}));
                }
            } else {
                let v = a.num("value").map_err(u)?.ok_or_else(|| u("needs --points \"t:v,...\" or --value 0..1".into()))?;
                if !(0.0..=1.0).contains(&v) {
                    return Err(u(format!("value {v} outside 0..1")));
                }
                points.push(json!({"t": 0.0, "v": v}));
            }
            let ctx = send(cx, a, "context", json!({}))?;
            let next = ctx.get("nextId").and_then(Value::as_u64).ok_or_else(|| u("no nextId in context".into()))?;
            let (auto_id, clip_id) = (next, next + 1);
            let track = a.num("track").map_err(u)?.unwrap_or(0.0) as u64;
            let name = a.get("name").unwrap_or("Automation").to_string();
            let len = points.iter().filter_map(|p| p["t"].as_f64()).fold(1.0, f64::max) + 1.0;
            send(
                cx,
                a,
                "apply",
                json!({"ops": [
                    {"op": "putAutomation", "automation": {"id": auto_id, "name": name, "color": "#e0a44a", "target": {"kind": "plugin", "fxId": fx, "paramId": param}, "points": points}},
                    {"op": "putClips", "clips": [{"id": clip_id, "track": track, "start": 0.0, "len": len, "offset": 0.0, "kind": "auto", "ref": auto_id, "muted": false, "gain": 0.0, "fadeIn": 0.0, "fadeOut": 0.0, "pan": 0.0, "norm": false, "rev": false}]}
                ]}),
            )
        }
        "notes" => {
            let verb = a.pos.get(1).ok_or_else(|| u("notes <humanize|quantize|euclid|strum|drums|melody> …".into()))?.clone();
            let pattern = a.num("pattern").map_err(u)?.ok_or_else(|| u("missing --pattern".into()))? as u64;
            let f = |k: &str| a.num(k);
            let (verb_json, needs_ch) = match verb.as_str() {
                "humanize" => (
                    json!({"kind": "humanize", "timing": f("timing").map_err(u)?.unwrap_or(0.5), "velocity": f("velocity").map_err(u)?.unwrap_or(0.4)}),
                    false,
                ),
                "quantize" => (
                    json!({"kind": "quantize", "grid": f("grid").map_err(u)?.unwrap_or(1.0), "amount": f("amount").map_err(u)?.unwrap_or(1.0), "scaleAware": a.has("scale")}),
                    false,
                ),
                "euclid" => (
                    json!({"kind": "euclidFill", "ch": f("ch").map_err(u)?.ok_or_else(|| u("missing --ch".into()))? as u64, "k": f("k").map_err(u)?.ok_or_else(|| u("missing --k".into()))? as u32, "n": f("n").map_err(u)?.ok_or_else(|| u("missing --n".into()))? as u32, "rotate": f("rotate").map_err(u)?.unwrap_or(0.0) as i32, "vel": f("vel").map_err(u)?.unwrap_or(0.7)}),
                    true,
                ),
                "strum" => (
                    json!({"kind": "strum", "width": f("width").map_err(u)?.unwrap_or(0.5), "ramp": f("ramp").map_err(u)?.unwrap_or(0.05)}),
                    false,
                ),
                "drums" => {
                    let mut voices = vec![];
                    for (i, v) in a.get_all("voice").iter().enumerate() {
                        let parts: Vec<&str> = v.split(':').collect();
                        if parts.len() < 3 {
                            return Err(u(format!("--voice is CH:K:N[:ROT[:VEL]] (voice {i})")));
                        }
                        let num = |i: usize, name: &str| match parts.get(i) {
                            // absent → the caller's default; present-but-malformed → an error
                            Some(x) => {
                                x.trim().parse::<f64>().map(Some).map_err(|_| u(format!("--voice {i}: bad number `{x}` for {name}")))
                            }
                            None => Ok(None),
                        };
                        voices.push(json!({"ch": num(0, "CH")?.ok_or_else(|| u(format!("--voice {i}: missing CH")))? as u64,
                            "k": num(1, "K")?.ok_or_else(|| u(format!("--voice {i}: missing K")))? as u32,
                            "n": num(2, "N")?.ok_or_else(|| u(format!("--voice {i}: missing N")))? as u32,
                            "rotate": num(3, "ROT")?.unwrap_or(0.0) as i32, "vel": num(4, "VEL")?.unwrap_or(0.7)}));
                    }
                    if voices.is_empty() {
                        return Err(u("drums needs at least one --voice CH:K:N".into()));
                    }
                    (json!({"kind": "euclidDrums", "voices": voices, "bars": f("bars").map_err(u)?.unwrap_or(2.0) as u32}), false)
                }
                "melody" => (
                    json!({"kind": "melody", "ch": f("ch").map_err(u)?.ok_or_else(|| u("missing --ch".into()))? as u64, "bars": f("bars").map_err(u)?.unwrap_or(2.0) as u32, "density": f("density").map_err(u)?.unwrap_or(0.5), "repeat": f("repeat").map_err(u)?.unwrap_or(1.0) as u32}),
                    true,
                ),
                other => return Err(u(format!("unknown notes verb `{other}` (humanize|quantize|euclid|strum|drums|melody)"))),
            };
            if needs_ch {
                // a channel id here is the Channel Rack channel: accepted as-is, validated by the engine
            }
            let mut op = json!({"op": "transformNotes", "pattern": pattern, "verb": verb_json});
            if let Some(seed) = a.get("seed") {
                // the op seed is u64: parse it directly, an f64 round-trip loses bits above 2^53
                let s = seed.trim().parse::<u64>().map_err(|_| u(format!("--seed must be an unsigned integer, got `{seed}`")))?;
                op["seed"] = json!(s);
            }
            if let Some(sel) = a.get("select") {
                let (from, rest) = sel.split_once(':').ok_or_else(|| u("--select is FROM:TO[@CH]".into()))?;
                let (to, ch) = rest.split_once('@').map_or((rest, None), |(t, c)| (t, Some(c)));
                let pf = |x: &str| x.trim().parse::<f64>().map_err(|_| u(format!("--select: bad number `{x}`")));
                let mut node = json!({"kind": "range", "from": pf(from)?, "to": pf(to)?});
                if let Some(c) = ch {
                    node["ch"] = json!(c.trim().parse::<u64>().map_err(|_| u(format!("--select: bad channel `{c}`")))?);
                }
                op["select"] = node;
            } else if let Some(ix) = a.get("indices") {
                let list: Result<Vec<u64>, _> = ix.split(',').map(|x| x.trim().parse::<u64>()).collect();
                op["select"] =
                    json!({"kind": "indices", "indices": list.map_err(|_| u("--indices is a comma list of note indices".into()))?});
            }
            send(cx, a, "apply", json!({"ops": [op]}))
        }
        "key" => {
            if a.has("off") {
                return send(cx, a, "apply", json!({"ops": [{"op": "setKey", "key": null, "scale": 1}]}));
            }
            let note_names = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
            let scales = ["chromatic", "major", "minor", "major pentatonic", "minor pentatonic"];
            let note = a.pos.get(1).ok_or_else(|| u("key <NOTE> [SCALE] | key --off  (e.g. key C minor)".into()))?;
            let key_id =
                note_names.iter().position(|n| n.eq_ignore_ascii_case(note)).ok_or_else(|| u(format!("unknown note `{note}` (C…B)")))?
                    as u64;
            let scale = a
                .pos
                .get(2)
                .map(|sc| {
                    scales
                        .iter()
                        .position(|n| n.eq_ignore_ascii_case(sc))
                        .map(|i| i as u64)
                        .ok_or_else(|| u(format!("unknown scale `{sc}` ({} )", scales.join(", "))))
                })
                .transpose()?;
            send(cx, a, "apply", json!({"ops": [{"op": "setKey", "key": key_id, "scale": scale.unwrap_or(1)}]}))
        }
        "call" => {
            let c = a.pos.get(1).ok_or_else(|| u("call <command> [--args JSON]".into()))?;
            let args = match a.get("args") {
                Some(t) => serde_json::from_str(t).map_err(|e| u(format!("--args is not JSON: {e}")))?,
                None => json!({}),
            };
            send(cx, a, c, args)
        }
        "pitch-target" => send(
            cx,
            a,
            "pitch_target",
            json!({"clipId": a.num("clip").map_err(u)?.ok_or_else(|| u("missing --clip".into()))? as u64, "full": a.has("full")}),
        ),
        "tune-ref" => {
            let mut args =
                json!({"clipId": a.num("clip").map_err(u)?.ok_or_else(|| u("missing --clip".into()))? as u64, "off": a.has("off")});
            for k in ["reference", "correction", "vibrato", "influence", "key", "scale", "speed"] {
                if let Some(v) = a.num(k).map_err(u)? {
                    args[k] = json!(v);
                }
            }
            if let Some(v) = a.num("melody").map_err(u)? {
                args["melodyChannel"] = json!(v);
            }
            match a.get("octave") {
                Some("auto") => args["octave"] = json!("auto"),
                Some(o) => args["octave"] = json!(o.parse::<i64>().map_err(|_| u("--octave is auto or semitones (-12, 0, 12…)".into()))?),
                None => {}
            }
            send(cx, a, "tune_ref", args)
        }
        "melody-channels" => {
            send(cx, a, "melody_channels", json!({"clipId": a.num("clip").map_err(u)?.ok_or_else(|| u("missing --clip".into()))? as u64}))
        }
        "f0-model" => send(cx, a, "f0_model", json!({})),
        "pitch" => pitch(cx, a),
        "tune-file" => tune_file(a),
        "script" => script(cx, a),
        "new" => Err(u("`new` creates a file; run it on its own, without --project".into())),
        other => Err(u(format!("unknown command `{other}`"))),
    }
}

/// `wave pitch inspect|compare|suggest|edit|render`: the pitch editor for scripts and agents.
fn pitch(cx: &Cx, a: &Args) -> Result<Value, Fail> {
    let u = Fail::Usage;
    let verb = a.pos.get(1).map(String::as_str).ok_or_else(|| u("pitch inspect|compare|suggest|edit|render --clip ID …".into()))?;
    let clip = a.num("clip").map_err(u)?.ok_or_else(|| u("missing --clip".into()))? as u64;
    let mut args = json!({ "clipId": clip });
    let num_into = |args: &mut Value, flag: &str, key: &str| -> Result<(), Fail> {
        if let Some(v) = a.num(flag).map_err(u)? {
            args[key] = json!(v);
        }
        Ok(())
    };
    let reference = |args: &mut Value| -> Result<(), Fail> {
        num_into(args, "source", "source")?;
        num_into(args, "ref-clip", "referenceClip")
    };
    match verb {
        "inspect" => {
            args["full"] = json!(a.has("full"));
            args["all"] = json!(a.has("all"));
            num_into(&mut args, "from", "from")?;
            num_into(&mut args, "to", "to")?;
            send(cx, a, "pitch_inspect", args)
        }
        "compare" => {
            reference(&mut args)?;
            if args.get("source").is_none() && args.get("referenceClip").is_none() {
                return Err(u("pitch compare needs --source ID (the reference's source) or --ref-clip ID".into()));
            }
            send(cx, a, "pitch_compare", args)
        }
        "suggest" => {
            reference(&mut args)?;
            for (f, k) in [("min-cents", "minCents"), ("max-cents", "maxCents"), ("strength", "strength"), ("tuning", "tuning")] {
                num_into(&mut args, f, k)?;
            }
            if let Some(k) = a.get("key") {
                args["key"] = json!(k);
            }
            if let Some(sc) = a.get("scale") {
                args["scale"] = json!(sc);
            }
            send(cx, a, "pitch_suggest", args)
        }
        "edit" => pitch_edit_args(a, clip, args).and_then(|args| send(cx, a, "pitch_edit", args)),
        "render" => {
            args["out"] = json!(a.need("out").map_err(u)?);
            args["original"] = json!(a.has("original"));
            send(cx, a, "pitch_render", args)
        }
        other => Err(u(format!("unknown pitch verb `{other}` (inspect, compare, suggest, edit, render)"))),
    }
}

/// The arguments of `pitch_edit`: a whole edit as JSON (`--json`/`--file`: the `edit` of an
/// editTune op, or the whole op as `pitch suggest` prints it), plus quick flags.
fn pitch_edit_args(a: &Args, clip: u64, mut args: Value) -> Result<Value, Fail> {
    let u = Fail::Usage;
    let text = match (a.get("json"), a.get("file")) {
        (Some(j), _) => Some(j.to_string()),
        (None, Some(f)) => Some(std::fs::read_to_string(f).map_err(|e| u(format!("{f}: {e}")))?),
        _ => None,
    };
    if let Some(t) = text {
        let mut v: Value = serde_json::from_str(&t).map_err(|e| u(format!("the edit is not JSON: {e}")))?;
        if v.get("op").and_then(Value::as_str) == Some("editTune") {
            if v.get("clip").and_then(Value::as_u64).is_some_and(|c| c != clip) {
                return Err(u(format!("the op edits clip {}, not --clip {clip}", v["clip"])));
            }
            v = v["edit"].clone();
        }
        let Value::Object(m) = v else { return Err(u("the edit is a JSON object".into())) };
        for (k, x) in m {
            args[k] = x;
        }
    }
    let push = |args: &mut Value, key: &str, x: Value| match args.get_mut(key).and_then(Value::as_array_mut) {
        Some(l) => l.push(x),
        None => args[key] = json!([x]),
    };
    if let Some(i) = a.num("note").map_err(u)? {
        let mut n = json!({ "i": i as u64 });
        for (f, k) in [("shift", "shift"), ("cents", "shiftBy"), ("target", "target"), ("formant", "formant"), ("gain", "gain")] {
            if let Some(v) = a.num(f).map_err(u)? {
                n[k] = json!(v);
            }
        }
        if let Some(d) = a.num("drift").map_err(u)? {
            if !(0.0..=100.0).contains(&d) {
                return Err(u("--drift is a percentage 0..100".into()));
            }
            n["drift"] = json!(d / 100.0);
        }
        match a.get("speed") {
            Some("none") => n["speed"] = Value::Null,
            Some(_) => n["speed"] = json!(a.num("speed").map_err(u)?),
            None => {}
        }
        if n.as_object().is_some_and(|m| m.len() == 1) {
            return Err(u(
                "--note I needs --shift C, --cents C, --target MIDI, --drift %, --formant C, --gain dB or --speed MS|none".into()
            ));
        }
        push(&mut args, "notes", n);
    }
    for sp in a.get_all("split") {
        let (i, t) = sp.split_once('@').ok_or_else(|| u("--split is NOTE@SECONDS (e.g. 4@12.35)".into()))?;
        let i: u64 = i.trim().parse().map_err(|_| u(format!("--split: bad note `{i}`")))?;
        let t: f64 = t.trim().parse().map_err(|_| u(format!("--split: bad time `{t}`")))?;
        push(&mut args, "split", json!({ "i": i, "at": t }));
    }
    for key in ["merge", "reset"] {
        for x in a.get_all(key).iter().flat_map(|x| x.split(',')) {
            let i: u64 = x.trim().parse().map_err(|_| u(format!("--{key} is a comma list of note indices")))?;
            push(&mut args, key, json!(i));
        }
    }
    if a.has("clear") {
        args["clear"] = json!(true);
    }
    if let Some(b) = a.get("base") {
        args["base"] = json!(b);
    }
    if let Some(m) = a.num("max-cents").map_err(u)? {
        args["maxCents"] = json!(m);
    }
    if args.as_object().is_some_and(|m| m.len() == 1) {
        return Err(u(
            "pitch edit: nothing to do (--note I --shift C, --split I@T, --merge I, --reset I, --clear, or --json/--file)".into()
        ));
    }
    Ok(args)
}

/// Tune to Reference on two files, offline: no app, no project.
fn tune_file(a: &Args) -> Result<Value, Fail> {
    use wave_engine::sources::{write_wav, Audio};
    let u = Fail::Usage;
    let (take_p, ref_p, out_p) = (a.need("take").map_err(u)?, a.need("reference").map_err(u)?, a.need("out").map_err(u)?);
    let take = Audio::load(std::path::Path::new(take_p)).map_err(|e| u(format!("{take_p}: {e}")))?;
    let reference = Audio::load(std::path::Path::new(ref_p)).map_err(|e| u(format!("{ref_p}: {e}")))?;
    let pct = |k: &str| -> Result<f32, Fail> {
        match a.num(k).map_err(u)? {
            Some(v) if (0.0..=100.0).contains(&v) => Ok(v as f32 / 100.0),
            Some(_) => Err(u(format!("--{k} is a percentage 0..100"))),
            None => Ok(1.0),
        }
    };
    let octave = match a.get("octave") {
        None | Some("auto") => None,
        Some(o) => Some(
            o.parse::<i32>()
                .ok()
                .filter(|o| o % 12 == 0 && o.abs() <= 36)
                .ok_or_else(|| u("--octave is auto or a multiple of 12 within ±36".into()))?,
        ),
    };
    let params = wave_engine::RefTune {
        source: 0,
        correction: pct("correction")?,
        vibrato: pct("vibrato")?,
        influence: pct("influence")?,
        octave,
        key: a.num("key").map_err(u)?.map(|k| k as u32).unwrap_or(0).min(11),
        scale: a.num("scale").map_err(u)?.map(|k| k as u32).unwrap_or(0).min(4),
        speed: None,
        melody: None,
    };
    let model = if a.has("yin") {
        None
    } else {
        let explicit = a.get("model");
        let path = explicit.map(std::path::PathBuf::from).unwrap_or_else(wave_rmvpe::model_path);
        match wave_rmvpe::Rmvpe::load(&path) {
            Ok(m) => Some(m),
            Err(e) if explicit.is_some() => return Err(Fail::App(e)),
            Err(_) => None,
        }
    };
    let est = model.as_ref().map(|m| m as &dyn wave_engine::pitch::F0Estimator);
    let (out, target, analysis) = wave_engine::reference::tune_audio(&take, &reference, &params, est);
    write_wav(std::path::Path::new(out_p), out.content(), out.sr).map_err(|e| Fail::App(format!("{out_p}: {e}")))?;
    if let Some(j) = a.get("json") {
        let text = serde_json::to_string(&target).map_err(|e| Fail::App(e.to_string()))?;
        std::fs::write(j, text).map_err(|e| Fail::App(format!("{j}: {e}")))?;
    }
    Ok(
        json!({ "out": out_p, "backend": analysis.backend, "corrected": target.reason.is_none(), "reason": target.reason, "score": target.score, "octave": target.octave, "notes": target.notes.len(), "seconds": out.seconds() }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::{Arc, Once};

    /// Keeps the tests away from WAVE apps really running on this machine.
    fn isolate() -> PathBuf {
        static ONCE: Once = Once::new();
        let root = std::env::temp_dir().join(format!("wave-cli-tests-{}", std::process::id()));
        ONCE.call_once(|| {
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("sessions")).unwrap();
            std::env::set_var("WAVE_SESSION_DIR", root.join("sessions"));
        });
        root
    }

    fn dir(name: &str) -> PathBuf {
        let d = isolate().join(name);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn args(s: &[&str]) -> Args {
        Args::parse(s.iter().map(|x| x.to_string()).collect())
    }

    /// A project held in memory: `apply` of `setBpm` changes it, `set_param` always fails.
    struct FakeDoc {
        bpm: RefCell<f64>,
        dirty: RefCell<bool>,
        saves: Rc<RefCell<Vec<PathBuf>>>,
    }

    impl Doc for FakeDoc {
        fn call(&self, command: &str, args: &Value, _: Option<u64>) -> Result<Value, String> {
            match command {
                "apply" => {
                    for op in args["ops"].as_array().cloned().unwrap_or_default() {
                        match op["op"].as_str() {
                            Some("setBpm") => *self.bpm.borrow_mut() = op["bpm"].as_f64().unwrap(),
                            other => return Err(format!("BAD_OPS: unknown op {other:?}")),
                        }
                    }
                    *self.dirty.borrow_mut() = true;
                    Ok(json!({}))
                }
                "context" => Ok(json!({ "bpm": *self.bpm.borrow(), "tracks": [], "nextId": 7 })),
                "set_param" => Err("no effect 5".into()),
                _ => Ok(json!({})),
            }
        }
        fn revision(&self) -> u64 {
            1
        }
        fn dirty(&self) -> bool {
            *self.dirty.borrow()
        }
        fn save(&self, path: &Path) -> Result<(), String> {
            std::fs::write(path, format!("bpm {}", self.bpm.borrow())).map_err(|e| e.to_string())?;
            *self.dirty.borrow_mut() = false;
            self.saves.borrow_mut().push(path.to_path_buf());
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeFiles {
        saves: Rc<RefCell<Vec<PathBuf>>>,
    }

    impl FakeFiles {
        fn doc(&self) -> Box<dyn Doc> {
            Box::new(FakeDoc { bpm: RefCell::new(120.0), dirty: RefCell::new(false), saves: self.saves.clone() })
        }
        fn saves(&self) -> Vec<PathBuf> {
            self.saves.borrow().clone()
        }
    }

    impl Files for FakeFiles {
        fn open(&self, _: &Path) -> Result<(Box<dyn Doc>, Vec<String>), String> {
            Ok((self.doc(), vec![]))
        }
        fn create(&self, template: &str) -> Result<Box<dyn Doc>, String> {
            match template {
                "empty" | "sketch" => Ok(self.doc()),
                t => Err(format!("unknown template `{t}`")),
            }
        }
    }

    fn project(d: &Path) -> String {
        let p = d.join("song.wave");
        std::fs::write(&p, "bpm 120").unwrap();
        p.display().to_string()
    }

    fn file(f: &FakeFiles, a: &[&str]) -> Result<Value, Fail> {
        let a = args(a);
        let cmd = a.pos[0].clone();
        on_file(Some(f), &cmd, &a)
    }

    #[test]
    fn short_project_flag_and_shell_like_script_lines() {
        let a = args(&["-p", "a.wave", "bpm", "120"]);
        assert_eq!(a.get("project"), Some("a.wave"));
        assert_eq!(a.pos, ["bpm", "120"]);
        assert_eq!(split_line(r#"pattern --ch kick --steps "x... x...""#).unwrap(), ["pattern", "--ch", "kick", "--steps", "x... x..."]);
        assert_eq!(
            split_line(r#"apply --json '[{"op":"setBpm","bpm":90}]'"#).unwrap(),
            ["apply", "--json", r#"[{"op":"setBpm","bpm":90}]"#]
        );
        assert_eq!(
            split_line(r#"import "C:\My Samples\kick.wav"  --beat 4"#).unwrap(),
            ["import", r"C:\My Samples\kick.wav", "--beat", "4"]
        );
        assert_eq!(split_line(r#"channels --synth """#).unwrap(), ["channels", "--synth", ""]);
        assert!(split_line("bpm '90").is_err());
    }

    #[test]
    fn switches_never_swallow_the_next_word() {
        let a = args(&["bpm", "90", "--dry-run", "-p", "song.wave"]);
        assert_eq!(a.get("project"), Some("song.wave"));
        assert!(a.has("dry-run"));
        assert_eq!(a.pos, ["bpm", "90"]);
        let a = args(&["-p", "song.wave", "--dry-run", "bpm", "90"]);
        assert_eq!(a.get("project"), Some("song.wave"));
        assert_eq!(a.pos, ["bpm", "90"]);
        let a = args(&["render", "--stems", "--out", "x.wav", "--dither", "mix"]);
        assert_eq!(a.get("out"), Some("x.wav"));
        assert_eq!(a.pos, ["render", "mix"]);
        let a = args(&["fx", "--pan", "-0.5", "--value", "-3"]);
        assert_eq!((a.get("pan"), a.get("value")), (Some("-0.5"), Some("-3")));
    }

    #[test]
    fn refusals_keep_the_apps_error_codes() {
        assert_eq!(app_error("BAD_OPS: x".into()), "BAD_OPS: x");
        assert_eq!(app_error("no effect 5".into()), "REFUSED: no effect 5");
        assert_eq!(app_error("no: such".into()), "REFUSED: no: such");
    }

    #[test]
    fn an_edit_is_saved_back_and_a_read_saves_nothing() {
        let d = dir("save");
        let p = project(&d);
        let f = FakeFiles::default();
        let v = file(&f, &["context", "--project", &p]).unwrap();
        assert_eq!(v["saved"], Value::Null);
        assert!(f.saves().is_empty());
        let v = file(&f, &["bpm", "140", "--project", &p]).unwrap();
        assert_eq!(v["saved"], json!(p));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "bpm 140");
        // `save` writes even an unchanged project; `save --path` is a save-as
        file(&f, &["save", "--project", &p]).unwrap();
        let other = d.join("copy.wave");
        file(&f, &["save", "--path", &other.display().to_string(), "--project", &p]).unwrap();
        assert_eq!(f.saves().len(), 3);
        assert!(same_file(f.saves().last().unwrap(), &other));
    }

    #[test]
    fn dry_run_reports_and_save_as_leaves_the_original() {
        let d = dir("dry");
        let p = project(&d);
        let f = FakeFiles::default();
        let v = file(&f, &["bpm", "90", "--dry-run", "--project", &p]).unwrap();
        assert_eq!(v["saved"], Value::Null);
        assert_eq!(v["dryRun"]["changed"], true);
        assert_eq!(v["dryRun"]["wouldSave"], json!(p));
        assert!(f.saves().is_empty());
        let v2 = d.join("v2.wave").display().to_string();
        let v = file(&f, &["bpm", "90", "--save-as", &v2, "--project", &p]).unwrap();
        assert_eq!(v["saved"], json!(v2));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "bpm 120");
        assert_eq!(std::fs::read_to_string(&v2).unwrap(), "bpm 90");
        assert!(matches!(file(&f, &["bpm", "90", "--save-as", "--project", &p]), Err(Fail::Usage(_))));
    }

    #[test]
    fn a_refused_command_saves_nothing() {
        let d = dir("refused");
        let p = project(&d);
        let f = FakeFiles::default();
        match file(&f, &["set-param", "--fx", "5", "--param", "mix", "--value", "1", "--project", &p]) {
            Err(Fail::App(m)) => assert_eq!(m, "REFUSED: no effect 5"),
            other => panic!("{other:?}"),
        }
        assert!(f.saves().is_empty());
    }

    #[test]
    fn file_mode_needs_an_existing_file_and_the_app_binary() {
        let d = dir("missing");
        let missing = d.join("nope.wave").display().to_string();
        match file(&FakeFiles::default(), &["context", "--project", &missing]) {
            Err(Fail::Usage(m)) => assert!(m.contains("wave new"), "{m}"),
            other => panic!("{other:?}"),
        }
        let p = project(&d);
        assert!(matches!(on_file(None, "context", &args(&["context", "--project", &p])), Err(Fail::Usage(_))));
        assert!(matches!(file(&FakeFiles::default(), &["context", "--session", "ab", "--project", &p]), Err(Fail::Usage(_))));
    }

    #[test]
    fn a_script_is_all_or_nothing_on_a_file() {
        let d = dir("script");
        let p = project(&d);
        let f = FakeFiles::default();
        let ok = d.join("ok.txt");
        std::fs::write(&ok, "# tempo first\n\nwave bpm 100\napply --json '[{\"op\":\"setBpm\",\"bpm\":101}]'\ncontext\n").unwrap();
        let v = file(&f, &["script", &ok.display().to_string(), "--project", &p]).unwrap();
        let lines: Vec<_> = v["results"].as_array().unwrap().iter().map(|r| r["line"].as_u64().unwrap()).collect();
        assert_eq!(lines, [3, 4, 5]);
        assert_eq!(v["results"][2]["result"]["bpm"], 101.0);
        assert_eq!(f.saves().len(), 1, "one save for the whole script");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "bpm 101");

        let bad = d.join("bad.txt");
        std::fs::write(&bad, "bpm 60\nset-param --fx 5 --param mix --value 1\nbpm 70\n").unwrap();
        match file(&f, &["script", &bad.display().to_string(), "--project", &p]) {
            Err(Fail::App(m)) => assert!(m.starts_with("line 2 (`set-param`): REFUSED") && m.ends_with("nothing was saved"), "{m}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(f.saves().len(), 1);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "bpm 101");

        for (body, why) in [
            ("script x.txt\n", "cannot run inside"),
            ("bpm 90 --project o.wave\n", "--project belongs"),
            ("bpm '90\n", "unclosed"),
            ("save --path copy.wave\n", "--save-as"),
        ] {
            std::fs::write(&bad, body).unwrap();
            match file(&f, &["script", &bad.display().to_string(), "--project", &p]) {
                Err(Fail::Usage(m)) => assert!(m.contains(why), "{m}"),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn new_creates_a_file_once() {
        let d = dir("new");
        let f = FakeFiles::default();
        let base = d.join("beat").display().to_string();
        let v = new_file(Some(&f), &args(&["new", &base, "--bpm", "95"])).unwrap();
        let made = d.join("beat.wave");
        assert_eq!(v["saved"], json!(made.display().to_string()));
        assert_eq!(v["bpm"], 95.0);
        assert_eq!(std::fs::read_to_string(&made).unwrap(), "bpm 95");
        match new_file(Some(&f), &args(&["new", &base])) {
            Err(Fail::App(m)) => assert!(m.starts_with("EXISTS"), "{m}"),
            other => panic!("{other:?}"),
        }
        new_file(Some(&f), &args(&["new", &base, "--force"])).unwrap();
        assert!(matches!(new_file(Some(&f), &args(&["new", &base, "--force", "--template", "nope"])), Err(Fail::Usage(_))));
    }

    #[test]
    fn a_file_the_app_has_open_is_not_overwritten() {
        let d = dir("open");
        let p = project(&d);
        let open = PathBuf::from(&p);
        let handler: Arc<ws::Handler> = Arc::new(move |_req: &ws::Request| Ok((json!({ "path": open.display().to_string() }), 1)));
        let _server = ws::Server::start(handler).unwrap();
        let f = FakeFiles::default();
        // reading (and rendering) a file the app has open is fine
        file(&f, &["context", "--project", &p]).unwrap();
        match file(&f, &["bpm", "90", "--project", &p]) {
            Err(Fail::App(m)) => assert!(m.starts_with("FILE_OPEN") && m.contains("--force"), "{m}"),
            other => panic!("{other:?}"),
        }
        if cfg!(windows) {
            // the same file, spelled differently
            match file(&f, &["bpm", "90", "--project", &p.to_uppercase()]) {
                Err(Fail::App(m)) => assert!(m.starts_with("FILE_OPEN"), "{m}"),
                other => panic!("{other:?}"),
            }
        }
        assert!(f.saves().is_empty());
        // a copy next to it, or --force, is written
        let v2 = d.join("v2.wave").display().to_string();
        file(&f, &["bpm", "90", "--save-as", &v2, "--project", &p]).unwrap();
        file(&f, &["bpm", "90", "--force", "--project", &p]).unwrap();
        assert_eq!(f.saves().len(), 2);
    }

    #[test]
    fn printed_numbers_drop_f32_noise_but_keep_ids_and_useful_precision() {
        let mut v = json!({
            "id": 11639u64, "revision": 1020, "neg": -3,
            "notes": [{"step": 28.333332061767578, "v": 0.4699999988079071, "len": 0.5}],
            "vol": 0.7936999797821045, "tiny": -0.00001, "big": 6000.000000001
        });
        tidy_numbers(&mut v);
        assert_eq!(v["id"], json!(11639u64));
        assert_eq!(v["revision"], json!(1020));
        assert_eq!(v["neg"], json!(-3));
        assert_eq!(v["notes"][0]["step"], json!(28.3333));
        assert_eq!(v["notes"][0]["v"], json!(0.47));
        assert_eq!(v["notes"][0]["len"], json!(0.5));
        assert_eq!(v["vol"], json!(0.7937));
        assert_eq!(serde_json::to_string(&v["tiny"]).unwrap(), "0.0"); // no "-0.0"
        assert_eq!(v["big"], json!(6000.0));
        assert_eq!(serde_json::to_string(&v["notes"][0]["v"]).unwrap(), "0.47");
    }
}

#[cfg(test)]
mod pitch_args_tests {
    use super::*;

    fn args(s: &[&str]) -> Args {
        Args::parse(s.iter().map(|x| x.to_string()).collect())
    }

    #[test]
    fn quick_flags_build_one_edit() {
        let a = args(&[
            "pitch",
            "edit",
            "--clip",
            "5",
            "--note",
            "3",
            "--cents",
            "-20",
            "--drift",
            "40",
            "--speed",
            "none",
            "--split",
            "2@1.5",
            "--split",
            "7@3.25",
            "--merge",
            "4,6",
            "--reset",
            "1",
            "--base",
            "abc",
            "--max-cents",
            "80",
        ]);
        let v = pitch_edit_args(&a, 5, json!({ "clipId": 5 })).unwrap();
        assert_eq!(v["notes"], json!([{ "i": 3, "shiftBy": -20.0, "drift": 0.4, "speed": null }]));
        assert_eq!(v["split"], json!([{ "i": 2, "at": 1.5 }, { "i": 7, "at": 3.25 }]));
        assert_eq!(v["merge"], json!([4, 6]));
        assert_eq!(v["reset"], json!([1]));
        assert_eq!(v["base"], "abc");
        assert_eq!(v["maxCents"], 80.0);
    }

    #[test]
    fn a_suggested_op_is_accepted_as_the_edit_and_mistakes_are_usage_errors() {
        let op = r#"{"op":"editTune","clip":5,"edit":{"base":"f00","notes":[{"i":1,"shift":-30}]}}"#;
        let v = pitch_edit_args(&args(&["pitch", "edit", "--clip", "5", "--json", op]), 5, json!({ "clipId": 5 })).unwrap();
        assert_eq!(v, json!({ "clipId": 5, "base": "f00", "notes": [{ "i": 1, "shift": -30 }] }));
        let wrong = pitch_edit_args(&args(&["pitch", "edit", "--clip", "6", "--json", op]), 6, json!({ "clipId": 6 }));
        assert!(matches!(wrong, Err(Fail::Usage(m)) if m.contains("clip 5")));
        for bad in [
            vec!["pitch", "edit", "--clip", "5"],
            vec!["pitch", "edit", "--clip", "5", "--note", "2"],
            vec!["pitch", "edit", "--clip", "5", "--split", "2-1.5"],
            vec!["pitch", "edit", "--clip", "5", "--merge", "x"],
            vec!["pitch", "edit", "--clip", "5", "--note", "1", "--drift", "150"],
            vec!["pitch", "edit", "--clip", "5", "--json", "{not json"],
        ] {
            assert!(matches!(pitch_edit_args(&args(&bad), 5, json!({ "clipId": 5 })), Err(Fail::Usage(_))), "{bad:?}");
        }
    }
}
