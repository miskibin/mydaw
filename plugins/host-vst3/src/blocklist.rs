//! Remembering which modules must not be opened again.
//!
//! # Why this exists, and why it is not optional
//!
//! On the machine step 7 was developed against, roughly twenty of the installed
//! VST3 plugins are licence-protected. Opening one whose licence does not check
//! out does not fail: it puts up a **modal activation dialog** in a separate
//! process and never returns. [`scan_module`](crate::scan::scan_module) survives
//! that -- it gives each module its own thread and a deadline -- but surviving is
//! not the same as being usable:
//!
//! - each such module costs [`SCAN_TIMEOUT`](crate::scan::SCAN_TIMEOUT) of wall
//!   clock, so a scan of that machine takes minutes rather than seconds;
//! - each one leaks the worker thread it wedged;
//! - and each one leaves a dialog on the user's screen. Twenty of them.
//!
//! A host that re-learned this every scan would be unusable on exactly the
//! machines that have the most plugins. So a module that times out is recorded,
//! and later scans skip it. This is what every shipping DAW does, and it is the
//! difference between a plugin browser that opens and one that does not.
//!
//! # The cost, stated plainly
//!
//! A plugin that is blocked *stays* blocked, including after the user licenses
//! it. [`BlockList::clear`] is the way back, and the file is plain text a user can
//! delete. Wiring that to a "deep rescan" button is step 8's business; the
//! important half -- not making the machine unusable -- is here.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;

/// File name under the application data directory.
pub const BLOCKLIST_FILE: &str = "vst3-blocklist.txt";

/// Where the block list is stored, or `None` when this platform will not say.
///
/// `%LOCALAPPDATA%\MYDAW\` on Windows, `$XDG_DATA_HOME`/`~/.local/share/mydaw`
/// elsewhere. A machine that answers neither simply gets an in-memory list,
/// which still fixes the within-one-session half of the problem.
pub fn blocklist_path() -> Option<PathBuf> {
    #[cfg(windows)]
    let root = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let root = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")));

    Some(root?.join("MYDAW").join(BLOCKLIST_FILE))
}

/// Modules that must not be opened again.
///
/// Keyed by the module path as the scanner sees it. A [`BTreeSet`] rather than a
/// hash set so the file is written in a stable order and a human can read it.
#[derive(Debug)]
pub struct BlockList {
    blocked: Mutex<BTreeSet<PathBuf>>,
    /// `None` disables persistence: the list still works for this process.
    file: Option<PathBuf>,
}

impl BlockList {
    /// Load the block list from its usual place.
    ///
    /// Never fails: a missing, unreadable or malformed file yields an empty list,
    /// because a broken block list must not stop a DAW from starting.
    pub fn load() -> Self {
        let file = blocklist_path();
        let blocked = file.as_deref().map(Self::read_file).unwrap_or_default();
        Self {
            blocked: Mutex::new(blocked),
            file,
        }
    }

    /// An in-memory list that is never written anywhere. For tests.
    pub fn in_memory() -> Self {
        Self {
            blocked: Mutex::new(BTreeSet::new()),
            file: None,
        }
    }

    /// A list backed by an explicit file. For tests, and for a caller that keeps
    /// its plugin database somewhere of its own.
    pub fn with_file(file: impl Into<PathBuf>) -> Self {
        let file = file.into();
        let blocked = Self::read_file(&file);
        Self {
            blocked: Mutex::new(blocked),
            file: Some(file),
        }
    }

    fn read_file(path: &Path) -> BTreeSet<PathBuf> {
        let Ok(text) = std::fs::read_to_string(path) else {
            return BTreeSet::new();
        };
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(PathBuf::from)
            .collect()
    }

    /// Whether `module` must be skipped.
    pub fn contains(&self, module: &Path) -> bool {
        self.blocked.lock().contains(module)
    }

    /// Record a module that must not be opened again, and persist the list.
    ///
    /// Returns whether this was news. Writing is best-effort: a read-only or
    /// missing application data directory costs persistence, not correctness.
    pub fn block(&self, module: &Path) -> bool {
        let added = self.blocked.lock().insert(module.to_path_buf());
        if added {
            self.persist();
        }
        added
    }

    /// Forget everything, so the next scan tries every module again.
    pub fn clear(&self) {
        self.blocked.lock().clear();
        self.persist();
    }

    /// How many modules are blocked.
    pub fn len(&self) -> usize {
        self.blocked.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Every blocked module, in path order.
    pub fn entries(&self) -> Vec<PathBuf> {
        self.blocked.lock().iter().cloned().collect()
    }

    fn persist(&self) {
        let Some(file) = self.file.as_ref() else {
            return;
        };
        let mut text = String::from(
            "# VST3 modules MYDAW will not open again: each one hung during a scan,\n\
             # which on Windows almost always means a licensing dialog. Delete a line\n\
             # (or this file) to make MYDAW try that module again.\n",
        );
        for path in self.blocked.lock().iter() {
            text.push_str(&path.display().to_string());
            text.push('\n');
        }
        if let Some(parent) = file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(err) = std::fs::write(file, text) {
            log::debug!(
                "could not write the VST3 block list to {}: {err}",
                file.display()
            );
        }
    }
}

impl Default for BlockList {
    fn default() -> Self {
        Self::load()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn scratch(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mydaw-vst3-block-{tag}-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch dir");
        path
    }

    #[test]
    fn an_in_memory_list_blocks_and_forgets_without_touching_the_disk() {
        let list = BlockList::in_memory();
        let module = PathBuf::from(r"C:\plugins\Wedged.vst3");

        assert!(list.is_empty());
        assert!(!list.contains(&module));
        assert!(list.block(&module), "blocking is news the first time");
        assert!(!list.block(&module), "and not the second");
        assert!(list.contains(&module));
        assert_eq!(list.len(), 1);
        assert_eq!(list.entries(), vec![module]);

        list.clear();
        assert!(list.is_empty());
    }

    #[test]
    fn a_blocked_module_survives_a_restart() {
        // The whole point: a machine with twenty unlicensed plugins must not pay
        // the timeout for them again on the next launch.
        let dir = scratch("persist");
        let file = dir.join("blocklist.txt");
        let module = PathBuf::from(r"C:\plugins\Unlicensed.vst3");

        {
            let list = BlockList::with_file(&file);
            list.block(&module);
        }
        assert!(file.exists(), "the list must have been written");

        let reloaded = BlockList::with_file(&file);
        assert!(reloaded.contains(&module));
        assert_eq!(reloaded.len(), 1);

        // ...and clearing it survives a restart too, so a user who licenses the
        // plugin gets it back.
        reloaded.clear();
        assert!(BlockList::with_file(&file).is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn comments_and_blank_lines_in_the_file_are_ignored() {
        let dir = scratch("comments");
        let file = dir.join("blocklist.txt");
        std::fs::write(
            &file,
            "# a comment\n\n   \nC:\\plugins\\One.vst3\n  C:\\plugins\\Two.vst3  \n",
        )
        .unwrap();

        let list = BlockList::with_file(&file);
        assert_eq!(list.len(), 2);
        assert!(list.contains(Path::new(r"C:\plugins\One.vst3")));
        assert!(list.contains(Path::new(r"C:\plugins\Two.vst3")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_or_unreadable_file_is_an_empty_list_rather_than_a_failure() {
        // A broken block list must never stop the DAW from starting.
        let missing = std::env::temp_dir().join("mydaw-no-such-blocklist-2f9a.txt");
        assert!(!missing.exists());
        assert!(BlockList::with_file(&missing).is_empty());
    }

    #[test]
    fn the_default_location_is_under_the_users_application_data() {
        // `None` is legal (a machine that will not say where its app data lives),
        // in which case the list is in-memory only.
        if let Some(path) = blocklist_path() {
            assert!(path.ends_with(BLOCKLIST_FILE));
            assert!(
                path.to_string_lossy().contains("MYDAW"),
                "{}",
                path.display()
            );
        }
    }
}
