//! Print what this machine has installed, module by module, as it goes.
//!
//! ```text
//! cargo run -p mydaw-plugin-host-vst3 --example scan_report                  # what the app does
//! cargo run -p mydaw-plugin-host-vst3 --example scan_report -- --in-process  # the old way, for comparison
//! cargo run -p mydaw-plugin-host-vst3 --example scan_report -- --cold        # ignore the metadata cache
//! cargo run -p mydaw-plugin-host-vst3 --example scan_report -- --retry-blocked
//! cargo run -p mydaw-plugin-host-vst3 --example scan_report -- --module <path>
//! ```
//!
//! The diagnostic that produced the compatibility table in
//! `docs/superpowers/specs/2026-08-19-step7-vst3-host.md`, and the way the
//! scanner's cost is measured rather than argued about.
//!
//! # It drives the real thing
//!
//! Every mode here goes through [`Vst3HostService`] and
//! [`ModuleScan`](mydaw_plugin_scan::ModuleScan) -- the same block list, the same
//! metadata cache, the same worker protocol the app uses. Progress is printed by
//! scanning one module per call rather than by a private code path, so there is
//! no second implementation to drift.
//!
//! `--in-process` selects [`WorkerSpawn::InProcess`], which is exactly what the
//! shipping app did before workers existed. On a machine with a licence-protected
//! plugin the difference is not subtle: the first hang wedges the process-wide
//! loader lock and every module after it is reported broken.

use std::path::PathBuf;
use std::time::Instant;

use mydaw_plugin_host_vst3::scan::{bundle_paths, standard_search_paths, Vst3PluginInfo};
use mydaw_plugin_scan::{ModuleScan, ScanPass, WorkerSpawn};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let in_process = args.iter().any(|arg| arg == "--in-process");
    let cold = args.iter().any(|arg| arg == "--cold");
    let retry_blocked = args.iter().any(|arg| arg == "--retry-blocked");

    let mut scanner = ModuleScan::<Vst3PluginInfo>::persistent();
    if in_process {
        scanner = scanner.with_spawn(WorkerSpawn::InProcess);
    }

    // --- one module, on demand ---------------------------------------------
    if let Some(index) = args.iter().position(|arg| arg == "--module") {
        let Some(path) = args.get(index + 1) else {
            eprintln!("--module needs a path");
            std::process::exit(2);
        };
        let module = PathBuf::from(path);
        report_line(&scanner.scan_all(std::slice::from_ref(&module)));
        return;
    }

    let roots = standard_search_paths();
    if retry_blocked {
        scanner.blocklist().clear();
        println!("block list cleared: every module will be tried again\n");
    }
    if cold || retry_blocked {
        scanner.cache().clear();
        println!("metadata cache cleared: every module will be opened again\n");
    }

    println!("search roots:");
    for root in &roots {
        println!(
            "  {} {}",
            if root.exists() { "*" } else { "-" },
            root.display()
        );
    }

    let modules = bundle_paths(&roots);
    println!(
        "\n{} module(s) found; mode: {}\n",
        modules.len(),
        if in_process {
            "IN-PROCESS (what the app did before out-of-process scanning)"
        } else {
            "ISOLATED (one child process per unknown module)"
        }
    );
    if modules.is_empty() {
        println!("Nothing to scan. Set VST3_PATH to add a search root.");
        return;
    }

    let mut totals = Totals::default();
    let started = Instant::now();

    for (index, module) in modules.iter().enumerate() {
        let name = module.file_name().map_or_else(
            || module.display().to_string(),
            |leaf| leaf.to_string_lossy().into_owned(),
        );
        print!(
            "[{:>2}/{}] {:<40} ",
            index + 1,
            modules.len(),
            truncate(&name, 40)
        );
        flush();

        // One module at a time, through the production scanner: the block list
        // and the cache live in `scanner` and carry across the calls, so this is
        // the same pass the app makes, printed as it happens.
        let pass = scanner.scan_all(std::slice::from_ref(module));
        totals.add(&pass);
        report_line(&pass);

        if pass.was_aborted() {
            println!("         (block-listed; it holds this process's loader lock, so the rest");
            println!("          of this scan cannot run -- that is the bug isolation fixes)");
            for later in &modules[index + 1..] {
                println!("        - {} not attempted", later.display());
            }
            break;
        }
    }

    println!(
        "\n{} module(s) in {:.1}s: {} opened ({} plugin class(es)), {} refused, {} \
         blocked, {} not attempted; {} answered from cache, {} child process(es), {} opened here",
        modules.len(),
        started.elapsed().as_secs_f64(),
        totals.opened,
        totals.classes,
        totals.refused,
        totals.blocked,
        totals.skipped,
        totals.cache_hits,
        totals.children,
        totals.in_process,
    );

    let blocklist = scanner.blocklist();
    if !blocklist.is_empty() {
        println!("\nblock list ({} entr(ies)):", blocklist.len());
        for entry in blocklist.entries() {
            println!("  {}", entry.display());
        }
        println!("Re-run with --retry-blocked after licensing a plugin.");
    }
}

#[derive(Default)]
struct Totals {
    opened: usize,
    classes: usize,
    refused: usize,
    blocked: usize,
    skipped: usize,
    cache_hits: usize,
    children: usize,
    in_process: usize,
}

impl Totals {
    fn add(&mut self, pass: &ScanPass<Vst3PluginInfo>) {
        self.classes += pass.found.len();
        if !pass.found.is_empty() {
            self.opened += 1;
        }
        self.blocked += pass.newly_blocked.len();
        self.refused += pass
            .failures
            .iter()
            .filter(|(_, failure)| !failure.is_blocked())
            .count();
        self.skipped += pass.skipped.len();
        self.cache_hits += pass.cache_hits;
        self.children += pass.children;
        self.in_process += pass.in_process;
    }
}

/// One module's outcome, in the shape the compatibility table wants.
fn report_line(pass: &ScanPass<Vst3PluginInfo>) {
    let took = pass.elapsed.as_secs_f64();
    if !pass.skipped.is_empty() {
        println!("SKIP  (block-listed)");
        return;
    }
    // One module per pass, so there is at most one of each of these.
    if let Some((_, failure)) = pass.failures.first() {
        if failure.is_blocked() {
            println!("BAD  {took:>6.2}s  {failure}");
        } else {
            println!("FAIL {took:>6.2}s  {failure}");
        }
        return;
    }
    let how = if pass.cache_hits > 0 {
        "cached"
    } else {
        "read"
    };
    println!("OK   {took:>6.2}s  {} class(es) [{how}]", pass.found.len());
    for info in &pass.found {
        println!("         - {}", describe(info));
    }
}

fn describe(info: &Vst3PluginInfo) -> String {
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
