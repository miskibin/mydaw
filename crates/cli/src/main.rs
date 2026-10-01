use mydaw_control_protocol::{empty_args, Reply, Request, SessionEndpoint};
use serde_json::{json, Value};
use std::time::Duration;

const HELP: &str = "MYDAW CLI — control the live, unsaved session\n\n\
mydaw sessions\n\
mydaw context [--detail summary|musical]\n\
mydaw catalog [--query pattern]\n\
mydaw schema COMMAND\n\
mydaw call COMMAND [--args JSON | --file FILE] [--if-revision N]\n\
mydaw apply --file FILE --if-revision N [--label TEXT]\n\
mydaw undo|redo --if-revision N\n\
mydaw render --args '{\"options\":{\"mode\":\"pattern\",\"bars\":4}}'\n\
mydaw analyze --args JSON\n\n\
Global options: --session ID, --timeout SECONDS, --pretty. Render/analyze wait\n\
for artifacts by default; --no-wait returns a job ID immediately. Output is JSON;\n\
logs use stderr. Edits require the revision returned by context. Command args\n\
use camelCase; schema describes exact types and musical units.\n\
apply file: {\"operations\":[{\"command\":\"set_bpm\",\"args\":{\"bpm\":140}}]}\n\
Use --idempotency-key KEY to retry a mutation safely in the same session.\n";

struct Options {
    session: Option<String>,
    timeout: u64,
    pretty: bool,
    request: Request,
    sessions: bool,
    wait: bool,
}
fn parse(arguments: Vec<String>) -> Result<Option<Options>, String> {
    if arguments.is_empty()
        || arguments
            .iter()
            .any(|s| matches!(s.as_str(), "--help" | "-h"))
    {
        return Ok(None);
    }
    let mut positionals = Vec::new();
    let mut session = None;
    let mut timeout = 120;
    let mut pretty = false;
    let mut args = empty_args();
    let mut revision = None;
    let mut label = None;
    let mut key = None;
    let mut wait = true;
    let mut iterator = arguments.into_iter();
    while let Some(argument) = iterator.next() {
        if matches!(argument.as_str(), "--json") {
            continue;
        }
        if argument == "--pretty" {
            pretty = true;
            continue;
        }
        if argument == "--no-wait" {
            wait = false;
            continue;
        }
        if argument.starts_with("--") {
            let value = iterator
                .next()
                .ok_or_else(|| format!("{argument} requires a value"))?;
            match argument.as_str() {
                "--session" => session = Some(value),
                "--timeout" => timeout = value.parse().map_err(|_| "timeout must be seconds")?,
                "--args" => args = serde_json::from_str(&value).map_err(|e| e.to_string())?,
                "--file" => {
                    if std::fs::metadata(&value).map_err(|e| e.to_string())?.len()
                        > mydaw_control_protocol::MAX_MESSAGE_BYTES as u64
                    {
                        return Err("argument file exceeds 16 MiB".into());
                    }
                    args = serde_json::from_slice(&std::fs::read(value).map_err(|e| e.to_string())?)
                        .map_err(|e| e.to_string())?
                }
                "--if-revision" => {
                    revision = Some(value.parse().map_err(|_| "revision must be an integer")?)
                }
                "--label" => label = Some(value),
                "--idempotency-key" => key = Some(value),
                "--detail" | "--query" => {
                    if !args.is_object() {
                        return Err("args must be an object".into());
                    }
                    args[argument.trim_start_matches("--")] = json!(value);
                }
                _ => return Err(format!("unknown option {argument}")),
            }
        } else {
            positionals.push(argument);
        }
    }
    let command = positionals.first().ok_or("missing command")?;
    let sessions = command == "sessions";
    let rpc = match command.as_str() {
        "call" => positionals.get(1).ok_or("call requires command")?.clone(),
        "schema" => {
            args["command"] = json!(positionals.get(1).ok_or("schema requires command")?);
            "schema".into()
        }
        "status" => "session_info".into(),
        "context" | "catalog" | "apply" | "history" | "undo" | "redo" | "render" | "analyze"
        | "sessions" => command.clone(),
        _ => {
            return Err(format!(
                "unknown command {command}; use call COMMAND for catalog operations"
            ))
        }
    };
    let expected_positionals = if matches!(command.as_str(), "call" | "schema") {
        2
    } else {
        1
    };
    if positionals.len() != expected_positionals {
        return Err("unexpected positional arguments".into());
    }
    if !(1..=3600).contains(&timeout) {
        return Err("timeout must be 1..3600 seconds".into());
    }
    let mut request = Request::new(rpc, args);
    request.if_revision = revision;
    request.label = label;
    request.idempotency_key = key.or_else(|| Some(uuid::Uuid::new_v4().to_string()));
    Ok(Some(Options {
        session,
        timeout,
        pretty,
        request,
        sessions,
        wait,
    }))
}

fn call(endpoint: SessionEndpoint, request: Request, timeout: u64) -> Result<Reply, String> {
    let key = request.idempotency_key.clone().unwrap_or_default();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(mydaw_control_protocol::request(&endpoint, request));
    });
    receiver.recv_timeout(Duration::from_secs(timeout)).map_err(|_| format!("request timed out; outcome may be unknown; retry identical arguments with --idempotency-key {key}"))?
        .map_err(|e| e.to_string())
}
fn live_endpoints() -> Result<Vec<(SessionEndpoint, Reply)>, String> {
    let mut live = vec![];
    for endpoint in mydaw_control_protocol::endpoints().map_err(|e| e.to_string())? {
        if let Ok(reply) = call(
            endpoint.clone(),
            Request::new("session_info", empty_args()),
            2,
        ) {
            if reply.error.is_none() {
                live.push((endpoint, reply));
            }
        }
    }
    Ok(live)
}
fn run(options: Options) -> Result<(Value, bool), String> {
    let live = live_endpoints()?;
    if options.sessions {
        return Ok((
            json!(live
                .into_iter()
                .map(
                    |(endpoint, reply)| json!({"sessionId":endpoint.session_id,"pid":endpoint.pid,
        "revision":reply.revision,"project":reply.data["project"]})
                )
                .collect::<Vec<_>>()),
            false,
        ));
    }
    let endpoint = if let Some(id) = options.session {
        live.into_iter()
            .find(|(e, _)| e.session_id == id)
            .map(|(e, _)| e)
            .ok_or("session is unavailable")?
    } else {
        if live.len() != 1 {
            return Err(format!(
                "found {} active sessions; select one with --session ID",
                live.len()
            ));
        }
        live.into_iter().next().unwrap().0
    };
    let worker = matches!(options.request.command.as_str(), "render" | "analyze") && options.wait;
    let mut reply = call(endpoint.clone(), options.request, options.timeout)?;
    if worker && reply.error.is_none() {
        let job = reply.data["jobId"]
            .as_str()
            .ok_or("worker returned no jobId")?
            .to_owned();
        let deadline = std::time::Instant::now() + Duration::from_secs(options.timeout);
        while reply.data["status"] == "running" {
            if std::time::Instant::now() >= deadline {
                return Err(format!(
                    "worker is still running: {job}; inspect with call job_status or cancel_job"
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
            reply = call(
                endpoint.clone(),
                Request::new("job_status", json!({"jobId":job})),
                5,
            )?;
            if reply.error.is_some() {
                break;
            }
        }
        if reply.error.is_none() && reply.data["status"] != "completed" {
            reply.error = Some(mydaw_control_protocol::ControlError::new(
                "WORKER_FAILED",
                reply.data["error"].as_str().unwrap_or("worker failed"),
            ));
        }
    }
    let failed = reply.error.is_some();
    Ok((
        serde_json::to_value(reply).map_err(|e| e.to_string())?,
        failed,
    ))
}
fn main() -> std::process::ExitCode {
    let options = match parse(std::env::args().skip(1).collect()) {
        Ok(Some(options)) => options,
        Ok(None) => {
            print!("{HELP}");
            return std::process::ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("{error}");
            return std::process::ExitCode::from(2);
        }
    };
    let pretty = options.pretty;
    match run(options) {
        Ok((value, failed)) => {
            println!(
                "{}",
                if pretty {
                    serde_json::to_string_pretty(&value)
                } else {
                    serde_json::to_string(&value)
                }
                .unwrap()
            );
            if failed {
                std::process::ExitCode::from(3)
            } else {
                std::process::ExitCode::SUCCESS
            }
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_revision_and_rejects_misspelled_flags() {
        let options = parse(vec![
            "call".into(),
            "set_bpm".into(),
            "--args".into(),
            r#"{"bpm":140}"#.into(),
            "--if-revision".into(),
            "42".into(),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(options.request.if_revision, Some(42));
        assert_eq!(options.request.args["bpm"], 140);
        assert!(parse(vec!["context".into(), "--typo".into(), "x".into()]).is_err());
    }
}
