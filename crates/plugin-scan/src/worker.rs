//! The scan worker: one child process per module, and the protocol it speaks.
//!
//! # Why a child process
//!
//! Reading what is inside a plugin means running the vendor's code: the OS loader
//! runs the module's entry point, then its initialiser runs. That code can crash
//! the process outright -- a segfault, a stack overflow, an `abort()` -- and no
//! amount of `catch_unwind` sees any of it. Measured on the development machine,
//! the *first* scan of a fresh install killed MYDAW before the block list had a
//! chance to learn which module did it, so the next launch did exactly the same
//! thing.
//!
//! On Windows it is worse than a crash. A module that blocks inside `DllMain`
//! holds the **loader lock**, which is per-process, so every later `LoadLibraryW`
//! from any thread blocks behind it forever. One wedged plugin does not cost one
//! plugin, it costs every module after it.
//!
//! Both problems have the same answer: open the module in a process MYDAW is
//! willing to lose. A poisoned loader lock dies with the child, and so does an
//! access violation.
//!
//! # The protocol
//!
//! Deliberately tiny -- no framing library, no handshake, no shared memory.
//!
//! ```text
//! parent                                        child
//! ------                                        -----
//! spawn(worker, env {                           run_worker_if_requested() sees
//!     MYDAW_SCAN_WORKER = "vst3"                    the two variables, scans
//!     MYDAW_SCAN_MODULE = "C:\...\Thing.vst3"       exactly that one module
//! }, stdout = pipe)
//!                                               prints one line to stdout:
//!                                               MYDAW-SCAN-RESULT {"status":...}
//!                                               exit(0)
//! read stdout until exit or deadline
//! kill on deadline
//! ```
//!
//! Three decisions are load-bearing:
//!
//! - **The request travels in the environment, not in `argv`.** The worker is
//!   normally the MYDAW binary itself, which has its own command line to parse
//!   (Tauri, the debug-capture flags); an environment variable cannot collide
//!   with any of it, and cannot be confused with a file a user dropped on the
//!   icon.
//! - **The answer is a marked line, not the whole of stdout.** Plugins print. A
//!   vendor's licensing library writing a banner to stdout must not be mistaken
//!   for a scan result, so the one line that matters carries
//!   [`RESULT_PREFIX`] and everything else is ignored.
//! - **The child exits 0 whenever it produced an answer**, including "this module
//!   refused to load". Exit status therefore means exactly one thing -- *did the
//!   process survive* -- and the parent can block-list on abnormal exit without
//!   having to distinguish a plugin's error from a plugin's crash.
//!
//! # Who the child is
//!
//! [`WorkerSpawn::Auto`] resolves, in order:
//!
//! 1. `MYDAW_SCAN_WORKER_BIN`, an explicit override (tests, and a packager who
//!    ships a dedicated worker as a sidecar).
//! 2. `mydaw-<format>-scan-worker` next to the current executable -- or one
//!    directory up when the current executable sits in `deps/` or `examples/`,
//!    which is where Cargo puts test and example binaries. This is the small
//!    dedicated worker each host crate ships; it starts faster than the whole
//!    DAW.
//! 3. The current executable itself, **but only if it has called
//!    [`run_worker_if_requested`]**. That is what makes the shipping single-binary
//!    case work with no packaging at all, and what stops a test binary or an
//!    unrelated tool from being re-executed dozens of times by accident.
//! 4. Nothing -- the caller falls back to scanning in-process, which is what the
//!    host did before this existed.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Names the format the child must scan (`vst3`, `clap`). Its presence is what
/// makes a process a scan worker.
pub const FORMAT_ENV: &str = "MYDAW_SCAN_WORKER";

/// The one module the child must scan.
pub const MODULE_ENV: &str = "MYDAW_SCAN_MODULE";

/// Overrides which executable is spawned as the worker. See the module docs.
pub const WORKER_BIN_ENV: &str = "MYDAW_SCAN_WORKER_BIN";

/// Marks the one line of the child's stdout that is a scan result.
pub const RESULT_PREFIX: &str = "MYDAW-SCAN-RESULT ";

/// Bumped when the wire shape changes. Parent and child normally come from the
/// same build; the field exists so a stale sidecar is *detected* rather than
/// misread.
pub const PROTOCOL_VERSION: u32 = 1;

/// How long one module gets before its child is killed.
///
/// Generous, because a large sample-based plugin genuinely takes seconds to
/// initialise and a licensing check may talk to the network; finite, because an
/// unlicensed one is waiting for a human who is not coming.
pub const CHILD_TIMEOUT: Duration = Duration::from_secs(15);

/// How much of a child's stdout is kept before the rest is drained and dropped.
///
/// A plugin that prints a megabyte of diagnostics should not be able to make the
/// parent allocate without bound; the result line is a few hundred bytes.
const MAX_CHILD_OUTPUT: usize = 1 << 20;

/// What the child says. One line of JSON, prefixed by [`RESULT_PREFIX`].
///
/// The plugin list crosses as an opaque [`serde_json::Value`] so this crate never
/// has to know what a VST3 class or a CLAP descriptor looks like; the host that
/// asked deserialises it into its own type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum WorkerReport {
    /// The module opened and exported these plugins.
    Ok {
        /// [`PROTOCOL_VERSION`] as the child knows it.
        v: u32,
        /// A JSON array of whatever the host's plugin-info type serialises to.
        plugins: serde_json::Value,
    },
    /// The module opened and refused, or is not a plugin at all. Not a crash, and
    /// not grounds for the block list.
    Error {
        v: u32,
        /// The reason, as a user would see it.
        message: String,
    },
}

/// How one module turned out, from the parent's side of the pipe.
#[derive(Debug)]
pub enum WorkerOutcome {
    /// The child answered with a plugin list, still in wire form.
    Scanned(serde_json::Value),
    /// The child answered "this module will not load", with the reason.
    Refused(String),
    /// The child died, or said nothing intelligible. **Block-list material**:
    /// whatever happened, it happened inside the module's own code.
    Crashed(String),
    /// The child was still running at the deadline and has been killed.
    /// **Block-list material** -- almost always a licensing dialog.
    TimedOut,
}

/// One format a worker process knows how to scan.
///
/// A `fn` pointer rather than a closure so the whole table can be a `const` in
/// the host crate, and so `main` can hand over the set of formats this build
/// supports in one line.
#[derive(Debug, Clone, Copy)]
pub struct WorkerHandler {
    /// The format tag, matched against [`FORMAT_ENV`].
    pub format: &'static str,
    /// Scan one module, returning a JSON array of plugin infos, or the reason
    /// this module could not be read.
    pub scan: fn(&Path) -> Result<serde_json::Value, String>,
}

/// Set once a process has proved it understands the protocol.
static EXE_IS_WORKER: AtomicBool = AtomicBool::new(false);

/// Declare that this executable answers [`FORMAT_ENV`], making it eligible to be
/// spawned as its own scan worker.
///
/// [`run_worker_if_requested`] calls this for you. It is public for the rare
/// binary that dispatches the protocol itself.
pub fn declare_worker_capable() {
    EXE_IS_WORKER.store(true, Ordering::Relaxed);
}

/// Whether [`declare_worker_capable`] has been called in this process.
pub fn is_worker_capable() -> bool {
    EXE_IS_WORKER.load(Ordering::Relaxed)
}

/// The scan this process was started to perform, if it was started to perform
/// one.
pub fn worker_request() -> Option<(String, PathBuf)> {
    let format = std::env::var(FORMAT_ENV).ok().filter(|s| !s.is_empty())?;
    let module = std::env::var_os(MODULE_ENV).filter(|s| !s.is_empty())?;
    Some((format, PathBuf::from(module)))
}

/// **Call this first in `main`.**
///
/// If this process was spawned as a scan worker it performs the one scan it was
/// asked for, prints the result and **exits without returning**. Otherwise it
/// records that this executable understands the protocol -- which is what lets
/// the shipping single binary act as its own worker -- and returns.
///
/// Putting it first matters: the point of the child is that it may die, and it
/// must die before it has opened an audio device or a window.
pub fn run_worker_if_requested(handlers: &[WorkerHandler]) {
    let Some((format, module)) = worker_request() else {
        declare_worker_capable();
        return;
    };
    let report = match handlers.iter().find(|handler| handler.format == format) {
        Some(handler) => match run_handler(handler, &module) {
            Ok(plugins) => WorkerReport::Ok {
                v: PROTOCOL_VERSION,
                plugins,
            },
            Err(message) => WorkerReport::Error {
                v: PROTOCOL_VERSION,
                message,
            },
        },
        None => WorkerReport::Error {
            v: PROTOCOL_VERSION,
            message: format!("this build has no scanner for plugin format '{format}'"),
        },
    };
    write_report(&report);
    // Not `return`: a worker has done its whole job, and running the DAW's
    // start-up after having loaded a stranger's DLL is exactly what this
    // process exists to avoid.
    std::process::exit(0);
}

/// Run one handler, turning a panic into a message rather than an abort.
///
/// The child would survive a panic anyway -- it is about to exit -- but a caught
/// panic can be *reported*, and "this module panicked while loading" is a far
/// better plugin-browser entry than a silent disappearance.
fn run_handler(handler: &WorkerHandler, module: &Path) -> Result<serde_json::Value, String> {
    let scan = handler.scan;
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| scan(module)))
        .unwrap_or_else(|_| Err("the module panicked while it was being read".to_owned()))
}

/// Print a report on the one line the parent reads.
fn write_report(report: &WorkerReport) {
    use std::io::Write;
    let json = serde_json::to_string(report).unwrap_or_else(|err| {
        format!(r#"{{"status":"error","v":{PROTOCOL_VERSION},"message":"unserialisable result: {err}"}}"#)
    });
    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "{RESULT_PREFIX}{json}");
    let _ = stdout.flush();
}

/// Builds a child for one `(format, module)` pair. See [`WorkerSpawn::Custom`].
pub type SpawnChild = dyn Fn(&str, &Path) -> std::io::Result<Child> + Send + Sync;

/// How the parent starts a worker.
#[derive(Clone)]
pub enum WorkerSpawn {
    /// Resolve one, as described in the module docs.
    Auto,
    /// Run exactly this executable.
    Program(PathBuf),
    /// Do not isolate anything: open modules in the calling process, on a thread
    /// with a deadline. What the hosts did before workers existed, kept so the
    /// cost of not having them stays a measurement rather than an argument --
    /// `scan_report --in-process` is the measurement.
    InProcess,
    /// Build the child by hand. The test seam: it is how "a worker that exits
    /// non-zero", "a worker that prints rubbish" and "a worker that never
    /// answers" are exercised without shipping three broken binaries.
    Custom(Arc<SpawnChild>),
}

impl std::fmt::Debug for WorkerSpawn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auto => f.write_str("WorkerSpawn::Auto"),
            Self::InProcess => f.write_str("WorkerSpawn::InProcess"),
            Self::Program(path) => write!(f, "WorkerSpawn::Program({})", path.display()),
            Self::Custom(_) => f.write_str("WorkerSpawn::Custom(..)"),
        }
    }
}

impl WorkerSpawn {
    /// Start a child for one module, or `None` when there is no worker to start.
    ///
    /// `None` is not a failure: it means "this build cannot isolate a scan", and
    /// the caller falls back to opening the module in-process.
    fn start(&self, format: &str, module: &Path) -> Option<std::io::Result<Child>> {
        match self {
            Self::Custom(build) => Some(build(format, module)),
            Self::InProcess => None,
            Self::Program(path) => Some(spawn_worker(path, format, module)),
            Self::Auto => {
                let program = resolve_worker(format)?;
                Some(spawn_worker(&program, format, module))
            }
        }
    }
}

/// The child command, with the request in its environment.
fn spawn_worker(program: &Path, format: &str, module: &Path) -> std::io::Result<Child> {
    Command::new(program)
        .env(FORMAT_ENV, format)
        .env(MODULE_ENV, module)
        // A worker inherits nothing it could be confused by, and writes nowhere
        // the parent is not reading: a plugin's stderr chatter goes to the void
        // rather than into MYDAW's console.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
}

/// Which executable [`WorkerSpawn::Auto`] would run. See the module docs.
pub fn resolve_worker(format: &str) -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os(WORKER_BIN_ENV) {
        if !explicit.is_empty() {
            return Some(PathBuf::from(explicit));
        }
    }

    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let name = format!("mydaw-{format}-scan-worker{}", std::env::consts::EXE_SUFFIX);

    let sibling = dir.join(&name);
    if sibling.is_file() {
        return Some(sibling);
    }
    // Cargo puts test binaries in `target/<profile>/deps/` and examples in
    // `target/<profile>/examples/`, one level below the dedicated workers.
    // Restricted to those two names so an installed MYDAW never goes looking
    // outside its own directory for something to execute.
    if dir
        .file_name()
        .is_some_and(|leaf| leaf == "deps" || leaf == "examples")
    {
        if let Some(parent) = dir.parent() {
            let up = parent.join(&name);
            if up.is_file() {
                return Some(up);
            }
        }
    }

    is_worker_capable().then_some(exe)
}

/// Scan one module in a child process.
///
/// Returns `None` when no worker could be started at all, which is the caller's
/// cue to fall back to an in-process scan. Every other result -- including the
/// child dying -- is a [`WorkerOutcome`], because a child dying is information.
///
/// The child is killed at `timeout`, and the parent survives whatever it was
/// doing. That is the whole point.
pub fn scan_in_child(
    spawn: &WorkerSpawn,
    format: &str,
    module: &Path,
    timeout: Duration,
) -> Option<WorkerOutcome> {
    let started = spawn.start(format, module)?;
    let mut child = match started {
        Ok(child) => child,
        Err(err) => {
            log::debug!(
                "could not start a scan worker for {}: {err}",
                module.display()
            );
            return None;
        }
    };

    // Drain stdout on its own thread. Waiting for the child *and then* reading
    // would deadlock the moment a chatty plugin filled the pipe buffer, and
    // reading to the end without waiting would never notice a hang.
    let (tx, rx) = mpsc::sync_channel(1);
    let reader = child.stdout.take().map(|mut stdout| {
        std::thread::Builder::new()
            .name("mydaw-scan-reader".to_owned())
            .spawn(move || {
                let _ = tx.send(read_capped(&mut stdout));
            })
    });
    if let Some(Err(err)) = reader {
        log::debug!("could not start a scan reader thread: {err}");
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    // The entire reason this runs in a child: whatever it wedged
                    // -- the loader lock included -- dies here.
                    let _ = child.kill();
                    let _ = child.wait();
                    return Some(WorkerOutcome::TimedOut);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(err) => {
                let _ = child.kill();
                return Some(WorkerOutcome::Crashed(format!(
                    "could not wait for the scan worker: {err}"
                )));
            }
        }
    };

    // The child has exited, so its stdout is closed and the reader is finishing.
    // The timeout is belt and braces against an inherited handle keeping the
    // pipe open (a plugin that spawned a helper of its own).
    let text = rx.recv_timeout(Duration::from_secs(2)).unwrap_or_default();
    Some(interpret(&text, status.success(), status.code()))
}

/// Read a child's output, keeping the first [`MAX_CHILD_OUTPUT`] bytes and
/// draining the rest so the child never blocks on a full pipe.
fn read_capped(source: &mut impl Read) -> String {
    let mut kept: Vec<u8> = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        match source.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                if kept.len() < MAX_CHILD_OUTPUT {
                    let room = MAX_CHILD_OUTPUT - kept.len();
                    kept.extend_from_slice(&buffer[..read.min(room)]);
                }
            }
        }
    }
    String::from_utf8_lossy(&kept).into_owned()
}

/// Turn what the child printed and how it exited into an outcome.
///
/// Split out from [`scan_in_child`] because it is the part with the rules in it,
/// and rules deserve tests that do not spawn anything.
pub fn interpret(output: &str, success: bool, code: Option<i32>) -> WorkerOutcome {
    let report = output
        .lines()
        .rev()
        .find_map(|line| line.trim_start().strip_prefix(RESULT_PREFIX))
        .map(|json| serde_json::from_str::<WorkerReport>(json.trim()));

    match report {
        Some(Ok(WorkerReport::Ok { v, plugins })) if success => {
            if v == PROTOCOL_VERSION {
                WorkerOutcome::Scanned(plugins)
            } else {
                WorkerOutcome::Crashed(format!(
                    "the scan worker speaks protocol {v}, this build speaks {PROTOCOL_VERSION}"
                ))
            }
        }
        Some(Ok(WorkerReport::Error { message, .. })) if success => WorkerOutcome::Refused(message),
        // An answer *and* a bad exit code: the module was read, then something
        // took the process down on the way out. Treat it as a crash -- a module
        // that cannot be unloaded safely is one to stop opening.
        Some(_) => WorkerOutcome::Crashed(exit_description(code)),
        None if success => WorkerOutcome::Crashed(
            "the scan worker exited without reporting a result; the module most likely took it \
             down while it was being read"
                .to_owned(),
        ),
        None => WorkerOutcome::Crashed(exit_description(code)),
    }
}

fn exit_description(code: Option<i32>) -> String {
    match code {
        // Windows reports an access violation as an unhandled exception code,
        // which is far more useful spelled out than as a bare number.
        Some(code) if code as u32 == 0xC000_0005 => {
            "the scan worker crashed reading this module (access violation)".to_owned()
        }
        Some(code) => format!("the scan worker crashed reading this module (exit code {code})"),
        None => "the scan worker was terminated while reading this module".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json_line(body: &str) -> String {
        format!("{RESULT_PREFIX}{body}\n")
    }

    #[test]
    fn a_clean_answer_is_the_plugin_list() {
        let out = json_line(r#"{"status":"ok","v":1,"plugins":[{"id":"a"}]}"#);
        let outcome = interpret(&out, true, Some(0));
        let WorkerOutcome::Scanned(value) = outcome else {
            panic!("expected a plugin list, got {outcome:?}");
        };
        assert_eq!(value.as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn a_reported_refusal_is_not_a_crash() {
        // The difference matters: a refusal is cached and shown, a crash is
        // block-listed and never retried.
        let out = json_line(r#"{"status":"error","v":1,"message":"not a shared library"}"#);
        assert!(matches!(
            interpret(&out, true, Some(0)),
            WorkerOutcome::Refused(message) if message == "not a shared library"
        ));
    }

    #[test]
    fn plugin_chatter_around_the_result_line_is_ignored() {
        // Vendors print. A licensing library's banner must not be mistaken for a
        // scan result, and must not hide one either.
        let out = format!(
            "iLok: checking licence...\n{}garbage trailing line\n",
            json_line(r#"{"status":"ok","v":1,"plugins":[]}"#)
        );
        assert!(matches!(
            interpret(&out, true, Some(0)),
            WorkerOutcome::Scanned(_)
        ));
    }

    #[test]
    fn a_worker_that_says_nothing_is_a_crash_however_it_exited() {
        // Both halves matter. Exit code 0 with no answer means something called
        // `exit()` inside the module; a non-zero code means it died outright.
        assert!(matches!(
            interpret("", true, Some(0)),
            WorkerOutcome::Crashed(_)
        ));
        assert!(matches!(
            interpret("hello from a plugin\n", false, Some(3)),
            WorkerOutcome::Crashed(message) if message.contains("exit code 3")
        ));
        assert!(matches!(
            interpret("", false, None),
            WorkerOutcome::Crashed(message) if message.contains("terminated")
        ));
    }

    #[test]
    fn an_access_violation_is_named_rather_than_numbered() {
        let outcome = interpret("", false, Some(0xC000_0005_u32 as i32));
        assert!(
            matches!(outcome, WorkerOutcome::Crashed(ref m) if m.contains("access violation")),
            "got {outcome:?}"
        );
    }

    #[test]
    fn a_truncated_or_mistyped_answer_is_a_crash_not_a_plugin_list() {
        assert!(matches!(
            interpret(&json_line("{\"status\":\"ok\","), true, Some(0)),
            WorkerOutcome::Crashed(_)
        ));
    }

    #[test]
    fn a_worker_from_another_protocol_version_is_refused_loudly() {
        let out = json_line(r#"{"status":"ok","v":99,"plugins":[]}"#);
        assert!(
            matches!(interpret(&out, true, Some(0)), WorkerOutcome::Crashed(ref m) if m.contains("protocol 99")),
        );
    }

    #[test]
    fn a_result_that_arrived_before_a_bad_exit_is_still_a_crash() {
        // The module was read and then took the process down on the way out.
        // Reporting its classes and carrying on would mean opening it again.
        let out = json_line(r#"{"status":"ok","v":1,"plugins":[]}"#);
        assert!(matches!(
            interpret(&out, false, Some(1)),
            WorkerOutcome::Crashed(_)
        ));
    }

    #[test]
    fn a_plain_test_binary_is_not_a_scan_request_and_not_a_worker() {
        // Deliberately without calling `declare_worker_capable`: this binary must
        // never become eligible to be spawned as its own worker, or one stray
        // `WorkerSpawn::Auto` would re-run the whole test suite once per module.
        assert!(worker_request().is_none());
        assert!(
            !is_worker_capable(),
            "nothing in this crate's tests may opt this executable in"
        );
    }

    #[test]
    fn capping_the_output_keeps_the_start_and_never_grows_without_bound() {
        let mut flood = std::io::Cursor::new(vec![b'x'; MAX_CHILD_OUTPUT * 2]);
        assert_eq!(read_capped(&mut flood).len(), MAX_CHILD_OUTPUT);
    }
}
