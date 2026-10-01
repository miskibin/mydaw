//! Local session control. No audio, webview or plugin-host dependencies.
use std::io::{self, Read, Write};
use std::path::PathBuf;

use interprocess::local_socket::{prelude::*, GenericNamespaced};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Operation {
    pub command: String,
    #[serde(default = "empty_args")]
    pub args: Value,
}

pub fn empty_args() -> Value {
    serde_json::json!({})
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    #[serde(default = "version")]
    pub version: u32,
    #[serde(default)]
    pub token: Option<String>,
    pub command: String,
    #[serde(default = "empty_args")]
    pub args: Value,
    #[serde(default)]
    pub if_revision: Option<u64>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub origin: Option<String>,
    /// Guard optimistic full-pattern edits against a stale UI's local copy.
    #[serde(default)]
    pub expected_pattern: Option<Value>,
}

const fn version() -> u32 {
    PROTOCOL_VERSION
}
impl Request {
    pub fn new(command: impl Into<String>, args: Value) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            token: None,
            command: command.into(),
            args,
            if_revision: None,
            idempotency_key: None,
            label: None,
            group: None,
            origin: None,
            expected_pattern: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ControlError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}
impl ControlError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }
}
impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for ControlError {}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Reply {
    pub version: u32,
    pub session_id: String,
    pub revision: u64,
    pub data: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ControlError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEndpoint {
    pub version: u32,
    pub session_id: String,
    pub socket: String,
    pub token: String,
    pub pid: u32,
}

pub fn session_dir() -> PathBuf {
    std::env::var_os("MYDAW_CONTROL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("LOCALAPPDATA")
                .or_else(|| std::env::var_os("XDG_RUNTIME_DIR"))
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir)
                .join("WAVE Studio")
                .join("sessions")
        })
}

pub fn read_message<T: for<'a> Deserialize<'a>>(reader: &mut impl Read) -> io::Result<T> {
    let mut header = [0; 4];
    reader.read_exact(&mut header)?;
    let size = u32::from_le_bytes(header) as usize;
    if size > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "control message too large",
        ));
    }
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes)?;
    serde_json::from_slice(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub fn write_message(writer: &mut impl Write, message: &impl Serialize) -> io::Result<()> {
    let bytes = serde_json::to_vec(message).map_err(io::Error::other)?;
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "control message too large",
        ));
    }
    writer.write_all(&(bytes.len() as u32).to_le_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()
}

pub fn request(endpoint: &SessionEndpoint, mut request: Request) -> io::Result<Reply> {
    request.token = Some(endpoint.token.clone());
    let name = endpoint.socket.as_str().to_ns_name::<GenericNamespaced>()?;
    let mut stream = LocalSocketStream::connect(name)?;
    write_message(&mut stream, &request)?;
    read_message(&mut stream)
}

pub fn endpoints() -> io::Result<Vec<SessionEndpoint>> {
    let entries = match std::fs::read_dir(session_dir()) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e),
    };
    let mut result = Vec::new();
    for entry in entries.flatten() {
        if entry.path().extension().is_some_and(|e| e == "json") {
            if entry.metadata().is_ok_and(|m| m.len() > 65536) {
                continue;
            }
            if let Ok(bytes) = std::fs::read(entry.path()) {
                if let Ok(endpoint) = serde_json::from_slice(&bytes) {
                    result.push(endpoint);
                    if result.len() >= 256 {
                        break;
                    }
                }
            }
        }
    }
    result.sort_by(|a: &SessionEndpoint, b| a.session_id.cmp(&b.session_id));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framed_request_roundtrip_and_unknown_fields() {
        let mut bytes = vec![];
        write_message(&mut bytes, &Request::new("context", empty_args())).unwrap();
        let request: Request = read_message(&mut bytes.as_slice()).unwrap();
        assert_eq!(request.command, "context");
        assert!(serde_json::from_str::<Request>(r#"{"command":"context","typo":1}"#).is_err());
    }
    #[test]
    fn oversized_frame_is_rejected_before_allocation() {
        let header = ((MAX_MESSAGE_BYTES + 1) as u32).to_le_bytes();
        assert_eq!(
            read_message::<Request>(&mut header.as_slice())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}
