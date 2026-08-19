//! Scratch directories and stand-in worker processes, for this crate's tests.
//!
//! The stand-ins are shell commands rather than fixture binaries on purpose. The
//! behaviours that have to be tested -- a worker that exits non-zero, one that
//! prints something that is not a result, one that never answers -- are exactly
//! the behaviours nobody wants to build, ship and keep working as real
//! executables, and the parent cannot tell the difference: it spawns a process,
//! reads its stdout and watches its exit status either way.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

/// A directory that deletes itself. Avoids a `tempfile` dependency for what is
/// three lines of `std::fs`.
pub struct ScratchDir(PathBuf);

impl ScratchDir {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A fresh scratch directory, unique across processes and calls.
pub fn scratch_dir(tag: &str) -> ScratchDir {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "mydaw-plugin-scan-{tag}-{}-{unique}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("scratch dir");
    ScratchDir(path)
}

/// Run one shell command as if it were a scan worker.
///
/// The command line is handed to the interpreter verbatim -- `raw_arg` on
/// Windows -- because the interesting stand-ins print JSON, and JSON is quotes
/// all the way down.
pub fn trivial_worker(script: &str) -> std::io::Result<Child> {
    let mut command = shell(script);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
}

/// A worker that will still be running when any sane deadline passes.
pub fn sleeping_worker() -> std::io::Result<Child> {
    #[cfg(windows)]
    let script = "ping -n 120 127.0.0.1";
    #[cfg(not(windows))]
    let script = "sleep 120";
    trivial_worker(script)
}

#[cfg(windows)]
fn shell(script: &str) -> Command {
    use std::os::windows::process::CommandExt;
    let mut command = Command::new("cmd");
    // `arg` would quote the string for a C runtime's rules, which `cmd.exe` does
    // not follow: every `"` in a JSON payload would come back with a backslash
    // in front of it.
    command.raw_arg(format!("/C {script}"));
    command
}

#[cfg(not(windows))]
fn shell(script: &str) -> Command {
    let mut command = Command::new("sh");
    command.arg("-c").arg(script);
    command
}
