//! The scan really happens in another process, and the answers are the same.
//!
//! The shared machinery -- what a dead child means, what the cache remembers,
//! what goes on the block list -- is tested in `mydaw-plugin-scan` against
//! stand-in workers. What can only be tested here is the part that involves an
//! actual VST3 module and the actual `mydaw-vst3-scan-worker` binary:
//!
//! - the worker exists, runs, and speaks the protocol;
//! - a file that is not a module comes back as a *refusal*, remembered and shown,
//!   rather than as a crash that block-lists it;
//! - and on a machine that has real plugins, scanning them in child processes
//!   finds exactly what scanning them in-process finds. That last one is the
//!   claim the whole change rests on, and it is skip-if-absent because CI has no
//!   VST3 plugins.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use mydaw_plugin_host_vst3::scan::Vst3PluginInfo;
use mydaw_plugin_host_vst3::{BlockList, Vst3HostService, BLOCKLIST_FILE};
use mydaw_plugin_scan::{ModuleScan, WorkerSpawn};

/// The dedicated worker Cargo built alongside this test.
const WORKER: &str = env!("CARGO_BIN_EXE_mydaw-vst3-scan-worker");

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mydaw-vst3-isolation-{tag}-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A scanner that remembers nothing and spawns the real worker binary.
fn isolated() -> ModuleScan<Vst3PluginInfo> {
    ModuleScan::in_memory().with_spawn(WorkerSpawn::Program(PathBuf::from(WORKER)))
}

#[test]
fn the_worker_binary_exists_and_is_not_a_tool() {
    // Run without the environment a scan sets: it must say what it is and exit
    // non-zero, so nobody mistakes it for a way to start MYDAW.
    let output = Command::new(WORKER)
        .output()
        .expect("cargo built the worker next to this test");

    assert!(!output.status.success(), "{output:?}");
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(said.contains("MYDAW_SCAN_WORKER"), "{said}");
    assert!(
        String::from_utf8_lossy(&output.stdout).is_empty(),
        "nothing may be printed on the result channel"
    );
}

#[test]
fn a_file_that_is_not_a_module_is_refused_by_the_worker_and_remembered() {
    let dir = TempDir::new("garbage");
    let bogus = dir.path().join("garbage.vst3");
    std::fs::write(&bogus, b"this is not a shared library").unwrap();

    let scanner = isolated();
    let pass = scanner.scan_all(std::slice::from_ref(&bogus));

    assert!(pass.found.is_empty());
    assert_eq!(pass.children, 1, "the module was opened in a child");
    assert_eq!(pass.in_process, 0, "and not in this process");
    assert_eq!(pass.failures.len(), 1);
    assert!(
        !pass.failures[0].1.is_blocked(),
        "a module that answers 'I will not load' is not a crash: {:?}",
        pass.failures
    );
    assert!(
        !scanner.blocklist().contains(&bogus),
        "refusing to load must never cost a plugin its place forever"
    );

    // ...and the next pass answers from the cache, having started nothing.
    let again = scanner.scan_all(std::slice::from_ref(&bogus));
    assert_eq!(again.children, 0);
    assert_eq!(again.cache_hits, 1);
    assert_eq!(again.failures.len(), 1);
}

#[test]
fn a_module_the_worker_can_read_is_cached_and_never_opened_twice() {
    let Some(module) = one_readable_module() else {
        println!("{}", skip_message());
        return;
    };

    let scanner = isolated();
    let first = scanner.scan_all(std::slice::from_ref(&module));
    assert!(!first.found.is_empty(), "{:?}", first.failures);
    assert_eq!(first.children, 1);
    assert_eq!(first.cache_hits, 0);

    let second = scanner.scan_all(std::slice::from_ref(&module));
    assert_eq!(
        second.children, 0,
        "an unchanged module must not cost a process"
    );
    assert_eq!(second.cache_hits, 1);
    assert_eq!(second.found, first.found);
    assert!(
        second.found.iter().all(|plugin| plugin.path == module),
        "the parent stamps the module path back on; a child never sends one"
    );
}

#[test]
fn out_of_process_and_in_process_find_the_same_plugins_on_this_machine() {
    let modules = installed_modules();
    if modules.is_empty() {
        println!("{}", skip_message());
        return;
    }

    // The user's real block list on both sides, so a licence-protected plugin is
    // skipped rather than opened -- twice, with a dialog each time.
    let blocked = || BlockList::load(BLOCKLIST_FILE);

    let isolated_pass = ModuleScan::<Vst3PluginInfo>::in_memory()
        .with_blocklist(blocked())
        .with_spawn(WorkerSpawn::Program(PathBuf::from(WORKER)))
        .scan_all(&modules);

    // The old way: every module opened in *this* process. Bounded, because a
    // module that hangs holds the loader lock and the pass stops there.
    let inline_pass = ModuleScan::<Vst3PluginInfo>::in_memory()
        .with_blocklist(blocked())
        .with_spawn(WorkerSpawn::InProcess)
        .with_timeout(Duration::from_secs(15))
        .scan_all(&modules);

    if inline_pass.was_aborted() {
        println!(
            "SKIPPED the comparison: scanning in-process wedged this process at {}, which is \
             exactly the failure out-of-process scanning exists to prevent. Populate the block \
             list with `cargo run -p mydaw-plugin-host-vst3 --example scan_report` and re-run.",
            inline_pass.aborted_at.unwrap().display()
        );
        assert!(
            !isolated_pass.found.is_empty(),
            "...and the isolated pass must still have found plugins"
        );
        return;
    }

    let isolated_ids = identify(&isolated_pass.found);
    let inline_ids = identify(&inline_pass.found);
    assert_eq!(
        isolated_ids, inline_ids,
        "a child process must report exactly what this process would have"
    );
    assert!(
        !isolated_ids.is_empty(),
        "this machine has {} module(s) but neither pass found a plugin class",
        modules.len()
    );
    assert_eq!(isolated_pass.in_process, 0, "nothing was opened here");
    assert_eq!(
        inline_pass.children, 0,
        "the comparison pass spawned nothing"
    );

    println!(
        "{} module(s): {} plugin class(es), identical either way. Isolated {:.1}s in {} child \
         process(es); in-process {:.1}s.",
        modules.len(),
        isolated_ids.len(),
        isolated_pass.elapsed.as_secs_f64(),
        isolated_pass.children,
        inline_pass.elapsed.as_secs_f64(),
    );
}

#[test]
fn the_service_scans_out_of_process_by_default() {
    // Nothing installed under a scratch root, so this proves the wiring rather
    // than the plugins: an empty pass, no abort, no in-process opening.
    let dir = TempDir::new("service");
    std::fs::write(dir.path().join("a.vst3"), b"broken").unwrap();

    let service = Vst3HostService::with_search_paths([dir.path().to_path_buf()]);
    let report = service.scan_detailed();

    assert!(report.plugins.is_empty());
    assert_eq!(report.failures.len(), 1);
    assert!(report.blocked.is_empty(), "a refusal is not a block");
    assert!(!report.was_aborted());
    assert_eq!(
        report.in_process, 0,
        "the default service must not open a module in the DAW's own process"
    );
    assert_eq!(report.children, 1);
}

/// `(module path, class id)` -- the pair a project file records, and the only
/// identity that has to survive the trip through a pipe.
fn identify(plugins: &[Vst3PluginInfo]) -> Vec<(PathBuf, String)> {
    let mut ids: Vec<(PathBuf, String)> = plugins
        .iter()
        .map(|plugin| (plugin.path.clone(), plugin.cid.to_hex()))
        .collect();
    ids.sort();
    ids
}

/// Every `.vst3` on this machine, minus the ones already known to hang.
fn installed_modules() -> Vec<PathBuf> {
    let mut roots = mydaw_plugin_host_vst3::standard_search_paths();
    if let Some(extra) = std::env::var_os("MYDAW_VST3_PATH") {
        roots.extend(std::env::split_paths(&extra));
    }
    mydaw_plugin_host_vst3::scan::bundle_paths(&roots)
}

/// One module a worker can actually read, for the cache test.
fn one_readable_module() -> Option<PathBuf> {
    let blocked = BlockList::load(BLOCKLIST_FILE);
    let scanner = isolated();
    installed_modules()
        .into_iter()
        .filter(|module| !blocked.contains(module))
        .find(|module| {
            !scanner
                .scan_all(std::slice::from_ref(module))
                .found
                .is_empty()
        })
}

fn skip_message() -> String {
    format!(
        "SKIPPED: no readable VST3 module on this machine. {}",
        mydaw_plugin_host_vst3::test_util::missing_plugins_message()
    )
}
