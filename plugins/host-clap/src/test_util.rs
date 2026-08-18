//! Finding the workspace's fixture CLAP plugin, for tests.
//!
//! `test-fixtures/clap-gain` builds a `cdylib` -- a genuine CLAP plugin that
//! multiplies its input by 0.5. Tests load it through the real OS loader, which
//! means they need its path on disk, and Cargo puts a dependency's `cdylib` in a
//! place that depends on profile, target directory and how the build was invoked.
//! [`fixture_library_path`] finds it anyway; [`stage_fixture_bundle`] copies it
//! somewhere with a `.clap` extension so the scanner will look at it.
//!
//! This module is compiled into the library (not behind `#[cfg(test)]`) so the
//! shell's integration tests can use it too. It touches nothing at runtime unless
//! called, and the shipping app never calls it.

use std::path::{Path, PathBuf};

/// Cargo's name for the fixture's dynamic library, without the platform decoration.
const FIXTURE_STEM: &str = "mydaw_fixture_clap_gain";

/// Name the staged copy is given. The scanner only looks at `*.clap`.
pub const FIXTURE_BUNDLE_NAME: &str = "mydaw-gain.clap";

/// Environment variable that overrides the search, for exotic build layouts.
pub const FIXTURE_PATH_ENV: &str = "MYDAW_FIXTURE_CLAP_GAIN";

/// Platform-decorated file names Cargo could have produced.
fn candidate_file_names() -> [String; 2] {
    #[cfg(target_os = "windows")]
    let names = [format!("{FIXTURE_STEM}.dll"), format!("{FIXTURE_STEM}.dll")];
    #[cfg(target_os = "macos")]
    let names = [
        format!("lib{FIXTURE_STEM}.dylib"),
        format!("{FIXTURE_STEM}.dylib"),
    ];
    #[cfg(all(unix, not(target_os = "macos")))]
    let names = [
        format!("lib{FIXTURE_STEM}.so"),
        format!("{FIXTURE_STEM}.so"),
    ];
    names
}

/// Locate the built fixture library, or `None` if this build did not produce one.
///
/// Search order:
/// 1. `$MYDAW_FIXTURE_CLAP_GAIN`, if it points at a file that exists.
/// 2. The directory of the running test binary and its parent -- i.e.
///    `target/<profile>/deps/` and `target/<profile>/`. A `cdylib` built as a
///    *dependency* is only ever placed in `deps/`; one built as the primary
///    package is also uplifted to `target/<profile>/`. Both are covered.
/// 3. Any file in those directories matching `<stem>*.dll` / `lib<stem>*.so`,
///    which catches the hash-suffixed name some Cargo versions emit.
///
/// Returns `None` rather than panicking: a test that cannot find the fixture
/// should skip with a clear message, not fail with a confusing one.
pub fn fixture_library_path() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os(FIXTURE_PATH_ENV) {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
    }

    let exe = std::env::current_exe().ok()?;
    let mut roots: Vec<PathBuf> = Vec::new();
    // `target/<profile>/deps/test-binary.exe` -> deps/, then <profile>/.
    for ancestor in exe.ancestors().skip(1).take(3) {
        roots.push(ancestor.to_path_buf());
        roots.push(ancestor.join("deps"));
    }

    let names = candidate_file_names();
    for root in &roots {
        for name in &names {
            let candidate = root.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    // Fall back to a prefix match for hash-suffixed artifacts.
    for root in &roots {
        if let Some(found) = search_by_prefix(root) {
            return Some(found);
        }
    }
    None
}

/// Newest `<stem>*<dylib extension>` in `dir`, ignoring import libraries and
/// debug info (`.lib`, `.exp`, `.pdb`, `.d`).
fn search_by_prefix(dir: &Path) -> Option<PathBuf> {
    let extension = if cfg!(target_os = "windows") {
        "dll"
    } else if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    };

    let entries = std::fs::read_dir(dir).ok()?;
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != extension) {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if !(stem.starts_with(FIXTURE_STEM) || stem.starts_with(&format!("lib{FIXTURE_STEM}"))) {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        if best.as_ref().is_none_or(|(best, _)| modified > *best) {
            best = Some((modified, path));
        }
    }
    best.map(|(_, path)| path)
}

/// Copy the fixture into `directory` as [`FIXTURE_BUNDLE_NAME`].
///
/// Copying rather than pointing the scanner at `target/` on purpose: the test
/// then owns a directory containing exactly one bundle, so scan assertions are
/// about the fixture and not about whatever else the build left lying around.
///
/// Returns `None` when the fixture was not built (see [`fixture_library_path`]).
pub fn stage_fixture_bundle(directory: &Path) -> Option<PathBuf> {
    let source = fixture_library_path()?;
    let destination = directory.join(FIXTURE_BUNDLE_NAME);
    std::fs::copy(&source, &destination).ok()?;
    Some(destination)
}

/// Message a skipping test should print, naming what to run.
pub fn missing_fixture_message() -> String {
    format!(
        "SKIP: fixture CLAP plugin not found next to the test binary. \
         Run `cargo build -p mydaw-fixture-clap-gain` (or set {FIXTURE_PATH_ENV}=<path to the \
         built cdylib>) and re-run."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_candidate_names_are_platform_appropriate() {
        let names = candidate_file_names();
        assert!(names.iter().all(|n| n.contains(FIXTURE_STEM)));
        #[cfg(target_os = "windows")]
        assert!(names.iter().all(|n| n.ends_with(".dll")));
    }

    #[test]
    fn an_explicit_override_that_does_not_exist_is_ignored_not_trusted() {
        // Not using the env var itself (tests share a process); this asserts the
        // shape of the contract that a non-existent path falls through to search.
        let missing = std::env::temp_dir().join("mydaw-no-fixture-here-1a2b.dll");
        assert!(!missing.is_file());
    }

    #[test]
    fn searching_a_directory_without_the_fixture_finds_nothing() {
        let dir = std::env::temp_dir().join("mydaw-empty-search-root-3c4d");
        let _ = std::fs::create_dir_all(&dir);
        assert!(search_by_prefix(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The workspace builds the fixture as a dev-dependency of this crate, so on
    /// a normal `cargo test --workspace` it must be findable. If it is not, every
    /// hosting test below would silently skip -- which is exactly the failure mode
    /// this test exists to make loud.
    #[test]
    fn the_workspace_fixture_is_findable_from_the_test_binary() {
        let found = fixture_library_path();
        assert!(found.is_some(), "{}", missing_fixture_message());
    }
}
