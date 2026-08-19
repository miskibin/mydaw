//! The CLAP scan really happens in another process, and finds the same thing.
//!
//! CLAP gets the same treatment as VST3 for a reason that has nothing to do with
//! the formats: `clap_entry->init()` is a vendor's code running in MYDAW's
//! address space, and it can take the process down exactly as `DllMain` can. No
//! CLAP plugin on the development machine does -- which is a statement about this
//! machine's plugin collection, not about the format -- so the proof here is that
//! the mechanism is wired up and produces identical answers, and the proof that
//! it survives a crash is in `mydaw-plugin-scan`'s own tests.
//!
//! The fixture is a real CLAP bundle built by this workspace, so unlike the VST3
//! side this runs in CI.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use mydaw_plugin_host_clap::test_util::{missing_fixture_message, stage_fixture_bundle};
use mydaw_plugin_host_clap::{ClapHostService, ClapPluginInfo};
use mydaw_plugin_scan::{ModuleScan, WorkerSpawn};

/// The dedicated worker Cargo built alongside this test.
const WORKER: &str = env!("CARGO_BIN_EXE_mydaw-clap-scan-worker");

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mydaw-clap-isolation-{tag}-{}-{unique}",
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

/// A directory holding exactly the fixture bundle, or `None` if it was not built.
fn fixture(tag: &str) -> Option<(TempDir, PathBuf)> {
    let dir = TempDir::new(tag);
    match stage_fixture_bundle(dir.path()) {
        Some(bundle) => Some((dir, bundle)),
        None => {
            println!("{}", missing_fixture_message());
            None
        }
    }
}

/// A scanner that remembers nothing and spawns the real worker binary.
fn isolated() -> ModuleScan<ClapPluginInfo> {
    ModuleScan::in_memory().with_spawn(WorkerSpawn::Program(PathBuf::from(WORKER)))
}

/// `(bundle, plugin id, name, vendor, version, features)` -- everything a browser
/// draws, so "identical" means identical rather than "the ids matched".
fn identify(
    plugins: &[ClapPluginInfo],
) -> Vec<(PathBuf, String, String, String, String, Vec<String>)> {
    let mut rows: Vec<_> = plugins
        .iter()
        .map(|plugin| {
            (
                plugin.path.clone(),
                plugin.id.clone(),
                plugin.name.clone(),
                plugin.vendor.clone(),
                plugin.version.clone(),
                plugin.features.clone(),
            )
        })
        .collect();
    rows.sort();
    rows
}

#[test]
fn the_worker_binary_exists_and_is_not_a_tool() {
    let output = Command::new(WORKER)
        .output()
        .expect("cargo built the worker next to this test");

    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("MYDAW_SCAN_WORKER"),
        "{output:?}"
    );
}

#[test]
fn a_child_process_reports_exactly_what_this_process_would_have() {
    let Some((dir, bundle)) = fixture("compare") else {
        return;
    };
    let modules = vec![bundle.clone()];

    let isolated_pass = isolated().scan_all(&modules);
    let inline_pass = ModuleScan::<ClapPluginInfo>::in_memory()
        .with_spawn(WorkerSpawn::InProcess)
        .scan_all(&modules);

    assert_eq!(
        isolated_pass.children, 1,
        "the bundle was opened in a child"
    );
    assert_eq!(isolated_pass.in_process, 0);
    assert_eq!(inline_pass.in_process, 1, "the comparison opened it here");

    // One bundle, two plugins -- the fixture ships a gain effect and a synth.
    assert_eq!(isolated_pass.found.len(), 2, "{:?}", isolated_pass.failures);
    assert_eq!(identify(&isolated_pass.found), identify(&inline_pass.found));
    assert!(
        isolated_pass
            .found
            .iter()
            .all(|plugin| plugin.path == bundle),
        "the parent stamps the bundle path back on; a child never sends one"
    );
    drop(dir);
}

#[test]
fn an_unchanged_bundle_costs_no_process_at_all() {
    let Some((_dir, bundle)) = fixture("cache") else {
        return;
    };
    let scanner = isolated();

    let first = scanner.scan_all(std::slice::from_ref(&bundle));
    assert_eq!(first.children, 1);
    assert_eq!(first.cache_hits, 0);
    assert_eq!(first.found.len(), 2);

    let second = scanner.scan_all(std::slice::from_ref(&bundle));
    assert_eq!(second.children, 0, "a cache hit must not start a child");
    assert_eq!(second.cache_hits, 1);
    assert_eq!(identify(&second.found), identify(&first.found));

    // ...until the file changes, which is the whole reason the key is a
    // fingerprint rather than a path.
    std::fs::write(&bundle, b"no longer a plugin at all").unwrap();
    let third = scanner.scan_all(std::slice::from_ref(&bundle));
    assert_eq!(third.cache_hits, 0);
    assert_eq!(third.children, 1);
    assert!(third.found.is_empty());
    assert_eq!(third.failures.len(), 1);
    assert!(
        !third.failures[0].1.is_blocked(),
        "a bundle that will not load is not a bundle that crashed"
    );
}

#[test]
fn a_file_that_is_not_a_bundle_is_refused_rather_than_blocked() {
    let dir = TempDir::new("garbage");
    let bogus = dir.path().join("garbage.clap");
    std::fs::write(&bogus, b"this is not a shared library").unwrap();

    let scanner = isolated();
    let pass = scanner.scan_all(std::slice::from_ref(&bogus));

    assert!(pass.found.is_empty());
    assert_eq!(pass.children, 1);
    assert_eq!(pass.failures.len(), 1);
    assert!(!pass.failures[0].1.is_blocked(), "{:?}", pass.failures);
    assert!(!scanner.blocklist().contains(&bogus));
}

#[test]
fn the_service_scans_out_of_process_by_default() {
    let Some((dir, bundle)) = fixture("service") else {
        return;
    };
    let service = ClapHostService::with_search_paths([dir.path().to_path_buf()]);

    let report = service.scan_detailed();
    assert_eq!(report.found.len(), 2, "{:?}", report.failures);
    assert_eq!(
        report.in_process, 0,
        "the default service must not load a bundle into the DAW's own process"
    );
    assert_eq!(report.children, 1);
    assert!(report.found.iter().all(|plugin| plugin.path == bundle));

    // And the plain `scan()` -- what every caller actually uses -- agrees.
    assert_eq!(identify(&service.scan()), identify(&report.found));
}

#[test]
fn the_clap_block_list_is_the_clap_one() {
    // Two formats sharing one list would mean a user who forgets a wedged VST3
    // plugin also forgets everything learned about their CLAP collection.
    let service = ClapHostService::new();
    if let Some(file) = service.blocklist().file() {
        assert!(file.ends_with("clap-blocklist.txt"), "{}", file.display());
    }
}
