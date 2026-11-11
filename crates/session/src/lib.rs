//! The live session: a running WAVE app listens on a local socket; the `wave`
//! CLI finds it through an endpoint file and sends JSON requests.
//!
//! Wire format: 4-byte little-endian length, then a JSON object. One request,
//! one reply, per connection. Every request carries the session token from the
//! endpoint file, so only processes that can read the user's files can talk to
//! the app.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use interprocess::local_socket::{prelude::*, GenericNamespaced, ListenerOptions, Stream};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 2;
const MAX_MESSAGE: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub version: u32,
    pub token: String,
    pub command: String,
    #[serde(default)]
    pub args: Value,
    #[serde(default)]
    pub if_revision: Option<u64>,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reply {
    pub ok: bool,
    pub revision: u64,
    #[serde(default)]
    pub data: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

impl ErrorBody {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    pub version: u32,
    pub session_id: String,
    pub socket: String,
    pub token: String,
    pub pid: u32,
    #[serde(default)]
    pub project: Option<String>,
}

pub fn session_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("WAVE_SESSION_DIR") {
        return PathBuf::from(d);
    }
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_RUNTIME_DIR"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("WAVE").join("sessions")
}

pub fn read_message<T: for<'a> Deserialize<'a>>(r: &mut impl Read) -> io::Result<T> {
    let mut h = [0u8; 4];
    r.read_exact(&mut h)?;
    let n = u32::from_le_bytes(h) as usize;
    if n > MAX_MESSAGE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "message too large"));
    }
    let mut b = vec![0; n];
    r.read_exact(&mut b)?;
    serde_json::from_slice(&b).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub fn write_message(w: &mut impl Write, m: &impl Serialize) -> io::Result<()> {
    let b = serde_json::to_vec(m).map_err(io::Error::other)?;
    w.write_all(&(b.len() as u32).to_le_bytes())?;
    w.write_all(&b)?;
    w.flush()
}

/// What the app answers a request with: `(data, revision)` or an error.
pub type Handler = dyn Fn(&Request) -> Result<(Value, u64), ErrorBody> + Send + Sync;

/// A listening session. Dropping it removes the endpoint file.
pub struct Server {
    endpoint_path: PathBuf,
    pub endpoint: Endpoint,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.endpoint_path);
    }
}

impl Server {
    /// Starts listening on a fresh socket and publishes the endpoint file.
    pub fn start(handler: Arc<Handler>) -> io::Result<Server> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let token = uuid::Uuid::new_v4().simple().to_string();
        let sock = format!("wave-{}.sock", &id[..12]);
        let name = sock.clone().to_ns_name::<GenericNamespaced>()?;
        let listener = ListenerOptions::new().name(name).create_sync()?;
        let dir = session_dir();
        std::fs::create_dir_all(&dir)?;
        // forget endpoints of processes that are gone
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                if let Ok(t) = std::fs::read_to_string(e.path()) {
                    if let Ok(ep) = serde_json::from_str::<Endpoint>(&t) {
                        if !pid_alive(ep.pid) {
                            let _ = std::fs::remove_file(e.path());
                        }
                    }
                }
            }
        }
        let endpoint = Endpoint { version: PROTOCOL_VERSION, session_id: id.clone(), socket: sock, token: token.clone(), pid: std::process::id(), project: None };
        let endpoint_path = dir.join(format!("{id}.json"));
        std::fs::write(&endpoint_path, serde_json::to_vec_pretty(&endpoint)?)?;
        thread::Builder::new().name("wave-session".into()).spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { continue };
                let h = handler.clone();
                let token = token.clone();
                let _ = thread::Builder::new().name("wave-session-conn".into()).spawn(move || {
                    let reply = match read_message::<Request>(&mut conn) {
                        Ok(req) if req.token != token => Reply { ok: false, revision: 0, data: Value::Null, error: Some(ErrorBody::new("UNAUTHORIZED", "bad session token")) },
                        Ok(req) if req.version != PROTOCOL_VERSION => Reply { ok: false, revision: 0, data: Value::Null, error: Some(ErrorBody::new("VERSION", format!("this app speaks protocol {PROTOCOL_VERSION}, the CLI sent {}", req.version))) },
                        Ok(req) => match h(&req) {
                            Ok((data, revision)) => Reply { ok: true, revision, data, error: None },
                            Err(e) => Reply { ok: false, revision: 0, data: Value::Null, error: Some(e) },
                        },
                        Err(e) => Reply { ok: false, revision: 0, data: Value::Null, error: Some(ErrorBody::new("BAD_REQUEST", e.to_string())) },
                    };
                    let _ = write_message(&mut conn, &reply);
                });
            }
        })?;
        Ok(Server { endpoint_path, endpoint })
    }
}

/// Live sessions on this machine, newest first.
pub fn sessions() -> Vec<Endpoint> {
    let mut out: Vec<(std::time::SystemTime, Endpoint)> = vec![];
    if let Ok(rd) = std::fs::read_dir(session_dir()) {
        for e in rd.flatten() {
            let Ok(t) = std::fs::read_to_string(e.path()) else { continue };
            let Ok(ep) = serde_json::from_str::<Endpoint>(&t) else { continue };
            if !pid_alive(ep.pid) {
                let _ = std::fs::remove_file(e.path());
                continue;
            }
            let m = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            out.push((m, ep));
        }
    }
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out.into_iter().map(|x| x.1).collect()
}

/// Sends one request to a session.
pub fn call(ep: &Endpoint, command: &str, args: Value, if_revision: Option<u64>, label: Option<String>) -> io::Result<Reply> {
    let name = ep.socket.clone().to_ns_name::<GenericNamespaced>()?;
    let mut s = Stream::connect(name)?;
    let req = Request { version: PROTOCOL_VERSION, token: ep.token.clone(), command: command.into(), args, if_revision, label };
    write_message(&mut s, &req)?;
    read_message(&mut s)
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists() || !cfg!(target_os = "linux")
}

#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    // tasklist is slow; opening the process is the cheap check
    use std::process::Command;
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_through_a_real_socket() {
        let dir = std::env::temp_dir().join(format!("wave-sess-{}", std::process::id()));
        std::env::set_var("WAVE_SESSION_DIR", &dir);
        let h: Arc<Handler> = Arc::new(|r: &Request| match r.command.as_str() {
            "echo" => Ok((r.args.clone(), 7)),
            _ => Err(ErrorBody::new("UNKNOWN_COMMAND", r.command.clone())),
        });
        let srv = Server::start(h).unwrap();
        let eps = sessions();
        assert_eq!(eps.len(), 1);
        let rep = call(&eps[0], "echo", serde_json::json!({"a": 1}), None, None).unwrap();
        assert!(rep.ok);
        assert_eq!(rep.revision, 7);
        assert_eq!(rep.data["a"], 1);
        let bad = Endpoint { token: "nope".into(), ..eps[0].clone() };
        assert_eq!(call(&bad, "echo", Value::Null, None, None).unwrap().error.unwrap().code, "UNAUTHORIZED");
        drop(srv);
        assert!(sessions().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
