//! Print what this machine has installed, module by module, as it goes.
//!
//! ```text
//! cargo run -p mydaw-plugin-host-vst3 --example scan_report                 # isolated (default)
//! cargo run -p mydaw-plugin-host-vst3 --example scan_report -- --in-process # what the app does
//! cargo run -p mydaw-plugin-host-vst3 --example scan_report -- --retry-blocked
//! cargo run -p mydaw-plugin-host-vst3 --example scan_report -- --module <path>
//! ```
//!
//! The diagnostic that produced the compatibility table in
//! `docs/superpowers/specs/2026-08-19-step7-vst3-host.md`, and -- in its default
//! mode -- **a working prototype of the out-of-process scanner** the crate docs
//! promise for phase 2.
//!
//! # Why isolation is the default here
//!
//! Measured on the development machine: a module that blocks inside `DllMain`
//! holds the Windows loader lock, and every later `LoadLibraryW` in that process
//! blocks behind it forever. An in-process scan of that machine therefore reports
//! *one* plugin and 42 false failures. One child process per module makes that
//! impossible -- a poisoned loader lock dies with the child -- and the difference
//! between the two modes is one command-line flag, which is the point: `--in-process`
//! reproduces exactly what the shipping app currently does, so the cost of not
//! having the subprocess scanner is a number rather than an argument.
//!
//! The child mode (`--module`) is the whole protocol: a path in, a report on
//! stdout, an exit code. It is deliberately the shape a `mydaw --scan` subcommand
//! would take.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use mydaw_plugin_host_vst3::scan::{scan_module_in_process, SCAN_TIMEOUT};
use mydaw_plugin_host_vst3::{BlockList, Vst3HostService};

/// How long a child gets before the parent kills it. A little more than the
/// in-process deadline, so a child that is merely slow reports for itself.
const CHILD_TIMEOUT: Duration = Duration::from_secs(SCAN_TIMEOUT.as_secs() + 5);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // --- child mode: one module, one process ------------------------------
    if let Some(index) = args.iter().position(|arg| arg == "--module") {
        let Some(path) = args.get(index + 1) else {
            eprintln!("--module needs a path");
            std::process::exit(2);
        };
        std::process::exit(scan_one(Path::new(path)));
    }

    // --- parent mode -------------------------------------------------------
    let in_process = args.iter().any(|arg| arg == "--in-process");
    let retry_blocked = args.iter().any(|arg| arg == "--retry-blocked");

    let service = Vst3HostService::new();
    let blocklist = BlockList::load();
    if retry_blocked {
        blocklist.clear();
        println!("block list cleared: every module will be tried again\n");
    }

    println!("search roots:");
    for root in service.search_paths() {
        println!(
            "  {} {}",
            if root.exists() { "*" } else { "-" },
            root.display()
        );
    }

    let modules = service.module_paths();
    println!(
        "\n{} module(s) found; mode: {}\n",
        modules.len(),
        if in_process {
            "IN-PROCESS (what the app does today)"
        } else {
            "ISOLATED (one child process per module)"
        }
    );
    if modules.is_empty() {
        println!("Nothing to scan. Set VST3_PATH to add a search root.");
        return;
    }

    let exe = std::env::current_exe().ok();
    let mut ok = 0_usize;
    let mut classes = 0_usize;
    let mut failed = 0_usize;
    let mut hung = 0_usize;
    let mut skipped = 0_usize;
    let mut poisoned = false;
    let started = Instant::now();

    for (index, module) in modules.iter().enumerate() {
        let name = module.file_name().map_or_else(
            || module.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        print!(
            "[{:>2}/{}] {:<40} ",
            index + 1,
            modules.len(),
            truncate(&name, 40)
        );
        flush();

        if blocklist.contains(module) && !retry_blocked {
            println!("SKIP  (block-listed)");
            skipped += 1;
            continue;
        }
        if poisoned {
            println!("SKIP  (the loader lock is held by an earlier module)");
            skipped += 1;
            continue;
        }

        let at = Instant::now();
        let outcome = if in_process {
            in_process_scan(module)
        } else {
            match exe.as_deref() {
                Some(exe) => isolated_scan(exe, module),
                None => in_process_scan(module),
            }
        };
        let took = at.elapsed().as_secs_f64();

        match outcome {
            Outcome::Ok(lines) => {
                ok += 1;
                classes += lines.len();
                println!("OK   {took:>6.2}s  {} class(es)", lines.len());
                for line in lines {
                    println!("         - {line}");
                }
            }
            Outcome::Failed(why) => {
                failed += 1;
                println!("FAIL {took:>6.2}s  {why}");
            }
            Outcome::Hung => {
                hung += 1;
                blocklist.block(module);
                println!(
                    "HUNG {took:>6.2}s  no answer within {}s",
                    CHILD_TIMEOUT.as_secs()
                );
                if in_process {
                    println!("         (block-listed; it holds the loader lock, so the rest of");
                    println!("          this scan cannot run -- that is the bug isolation fixes)");
                    poisoned = true;
                } else {
                    println!("         (block-listed; the child was killed, this process is fine)");
                }
            }
        }
    }

    println!(
        "\n{} module(s) in {:.1}s: {ok} opened ({classes} plugin class(es)), \
         {failed} refused, {hung} hung, {skipped} not attempted",
        modules.len(),
        started.elapsed().as_secs_f64()
    );
    if poisoned {
        println!(
            "\nThe scan stopped early: a module wedged this process's loader lock. Re-run to \
             resume past it, or drop --in-process."
        );
    }
    if !blocklist.is_empty() {
        println!("\nblock list ({} entr(ies)):", blocklist.len());
        for entry in blocklist.entries() {
            println!("  {}", entry.display());
        }
        println!("Re-run with --retry-blocked after licensing a plugin.");
    }
}

enum Outcome {
    Ok(Vec<String>),
    Failed(String),
    Hung,
}

/// Scan one module in this process. Fast, and poisons this process if it hangs.
fn in_process_scan(module: &Path) -> Outcome {
    match mydaw_plugin_host_vst3::scan::scan_module(module) {
        Ok(found) => Outcome::Ok(found.iter().map(describe).collect()),
        Err(mydaw_plugin_host_vst3::Vst3ScanError::TimedOut { .. }) => Outcome::Hung,
        Err(err) => Outcome::Failed(err.to_string()),
    }
}

/// Scan one module in a child process, killing it if it will not answer.
fn isolated_scan(exe: &Path, module: &Path) -> Outcome {
    let child = Command::new(exe)
        .arg("--module")
        .arg(module)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return in_process_scan(module);
    };

    let deadline = Instant::now() + CHILD_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut text = String::new();
                if let Some(mut out) = child.stdout.take() {
                    use std::io::Read;
                    let _ = out.read_to_string(&mut text);
                }
                let lines: Vec<String> = text
                    .lines()
                    .filter_map(|line| line.strip_prefix("CLASS "))
                    .map(str::to_owned)
                    .collect();
                return if status.success() {
                    Outcome::Ok(lines)
                } else {
                    Outcome::Failed(
                        text.lines()
                            .find_map(|line| line.strip_prefix("ERROR "))
                            .unwrap_or("the child exited without a reason")
                            .to_owned(),
                    )
                };
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    // The whole point of isolation: whatever it wedged dies here.
                    let _ = child.kill();
                    let _ = child.wait();
                    return Outcome::Hung;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(err) => return Outcome::Failed(err.to_string()),
        }
    }
}

/// Child mode: scan exactly one module and report on stdout.
fn scan_one(module: &Path) -> i32 {
    // No timeout thread here: the parent owns the deadline, and a child that
    // hangs is killed. That is the entire advantage of doing it this way.
    match scan_module_in_process(module) {
        Ok(found) => {
            for info in &found {
                println!("CLASS {}", describe(info));
            }
            0
        }
        Err(err) => {
            println!("ERROR {err}");
            1
        }
    }
}

fn describe(info: &mydaw_plugin_host_vst3::Vst3PluginInfo) -> String {
    format!(
        "{:<34} {:<18} {:<12} {:<26} {}",
        truncate(&info.name, 34),
        truncate(&info.vendor, 18),
        info.kind().id(),
        truncate(&info.subcategories, 26),
        info.cid
    )
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        text.to_owned()
    } else {
        text.chars().take(width - 1).collect::<String>() + "~"
    }
}

fn flush() {
    use std::io::Write;
    let _ = std::io::stdout().flush();
}
