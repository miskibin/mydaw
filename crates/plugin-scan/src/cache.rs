//! What was found inside a module last time, so it need not be opened again.
//!
//! Out-of-process scanning is the only design that survives a hostile plugin, and
//! it costs a process per module. On a well-stocked machine that is dozens of
//! `CreateProcess` calls plus dozens of vendor initialisers, every time the user
//! opens a plugin browser -- which is exactly the thing this was supposed to make
//! pleasant.
//!
//! So a scan writes down what it learned, keyed by a **fingerprint** of the file
//! it learned it from: length plus modification time. A later scan that finds the
//! same fingerprint answers from the cache and **loads nothing at all** -- no
//! child, no `LoadLibraryW`, no vendor code. A plugin the user upgrades or
//! reinstalls changes both fields and is scanned again.
//!
//! Failures are cached too, deliberately. A file that is not a plugin at all (a
//! stray `.vst3` text file, a 32-bit module on a 64-bit host) is just as expensive
//! to re-discover as a working one, and it fails the same way every time until
//! somebody replaces it -- which the fingerprint notices.
//!
//! # What is *not* cached
//!
//! A module that hung or crashed its child. That is the block list's job
//! ([`crate::BlockList`]), because the answer there is "never open this again"
//! rather than "here is what is inside it", and because a user who licenses the
//! plugin must be able to undo it by deleting one line.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use parking_lot::Mutex;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Bump when the on-disk shape changes. An older or newer file is discarded
/// rather than migrated: it is a cache, and re-scanning is always correct.
pub const CACHE_VERSION: u32 = 1;

/// Enough of a file to notice that it changed.
///
/// Length *and* modification time, because either alone is forgeable by an
/// installer: a patch release commonly keeps the size, and unpacking an archive
/// commonly keeps the timestamp. A content hash would be stronger and would cost
/// reading tens of megabytes per module on every start-up, which is the thing
/// being avoided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    /// File length in bytes.
    pub len: u64,
    /// Modification time in nanoseconds since the Unix epoch. `0` when the
    /// platform will not say, which simply makes the entry match less often.
    pub modified_ns: u64,
}

impl Fingerprint {
    /// Fingerprint one file, or `None` when it cannot be stat'ed.
    ///
    /// `None` is the honest answer for a module that vanished between the walk
    /// and the scan, and it means "cache nothing", not "cache a zero".
    pub fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        let modified_ns = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |since| since.as_nanos().min(u128::from(u64::MAX)) as u64);
        Some(Self {
            len: meta.len(),
            modified_ns,
        })
    }
}

/// What one module turned out to contain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome<T> {
    /// The module opened and exported these plugins. Possibly none, which is
    /// legal and is still worth remembering.
    Found(Vec<T>),
    /// The module opened and refused, with the reason as the user would see it.
    Refused(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry<T> {
    /// Module path, lossily as text. A path that does not round-trip through
    /// UTF-8 simply misses the cache and is scanned again.
    path: String,
    fingerprint: Fingerprint,
    outcome: Outcome<T>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CacheFile<T> {
    version: u32,
    entries: Vec<Entry<T>>,
}

/// Module metadata remembered across runs.
///
/// Cheap to consult and safe to share: every method takes `&self`.
#[derive(Debug)]
pub struct ScanCache<T> {
    entries: Mutex<BTreeMap<PathBuf, (Fingerprint, Outcome<T>)>>,
    /// `None` disables persistence: the cache still works for this process.
    file: Option<PathBuf>,
}

impl<T: Clone + Serialize + DeserializeOwned> ScanCache<T> {
    /// Load the cache named `file_name` from WAVE's data directory.
    ///
    /// Never fails. A missing, unreadable, truncated or version-mismatched file
    /// is an empty cache, which costs one slow scan and nothing else.
    pub fn load(file_name: &str) -> Self {
        let file = crate::blocklist::data_file(file_name);
        let entries = file.as_deref().map(Self::read_file).unwrap_or_default();
        Self {
            entries: Mutex::new(entries),
            file,
        }
    }

    /// A cache that is never written anywhere. For tests.
    pub fn in_memory() -> Self {
        Self {
            entries: Mutex::new(BTreeMap::new()),
            file: None,
        }
    }

    /// A cache backed by an explicit file.
    pub fn with_file(file: impl Into<PathBuf>) -> Self {
        let file = file.into();
        let entries = Self::read_file(&file);
        Self {
            entries: Mutex::new(entries),
            file: Some(file),
        }
    }

    fn read_file(path: &Path) -> BTreeMap<PathBuf, (Fingerprint, Outcome<T>)> {
        let Ok(text) = std::fs::read_to_string(path) else {
            return BTreeMap::new();
        };
        let parsed: CacheFile<T> = match serde_json::from_str(&text) {
            Ok(parsed) => parsed,
            Err(err) => {
                log::debug!(
                    "ignoring an unreadable scan cache at {}: {err}",
                    path.display()
                );
                return BTreeMap::new();
            }
        };
        if parsed.version != CACHE_VERSION {
            return BTreeMap::new();
        }
        parsed
            .entries
            .into_iter()
            .map(|entry| {
                (
                    PathBuf::from(entry.path),
                    (entry.fingerprint, entry.outcome),
                )
            })
            .collect()
    }

    /// What is known about `module`, if its fingerprint still matches.
    ///
    /// A stale entry answers `None` rather than being returned and corrected
    /// later: a plugin the user just upgraded must be re-read, not re-reported.
    pub fn get(&self, module: &Path, fingerprint: Fingerprint) -> Option<Outcome<T>> {
        let entries = self.entries.lock();
        let (stored, outcome) = entries.get(module)?;
        (*stored == fingerprint).then(|| outcome.clone())
    }

    /// Remember what `module` contained. Replaces any earlier entry.
    ///
    /// In memory only: [`save`](Self::save) writes the file, once, at the end of
    /// a pass. Writing per module would rewrite the whole file dozens of times
    /// during the one scan it is meant to make fast.
    pub fn put(&self, module: &Path, fingerprint: Fingerprint, outcome: Outcome<T>) {
        self.entries
            .lock()
            .insert(module.to_path_buf(), (fingerprint, outcome));
    }

    /// Forget a module -- the next scan opens it again.
    pub fn forget(&self, module: &Path) {
        self.entries.lock().remove(module);
    }

    /// Forget everything.
    pub fn clear(&self) {
        self.entries.lock().clear();
        self.save();
    }

    /// How many modules are remembered.
    pub fn len(&self) -> usize {
        self.entries.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The file this cache is written to, if any.
    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    /// Write the cache out, dropping entries whose module is gone.
    ///
    /// Best effort: a read-only data directory costs a slow scan next time, not
    /// correctness. Pruning here rather than on load is what keeps the file from
    /// growing forever as a user churns through demo plugins.
    pub fn save(&self) {
        let Some(file) = self.file.as_ref() else {
            return;
        };
        let entries: Vec<Entry<T>> = {
            let mut live = self.entries.lock();
            live.retain(|path, _| path.exists());
            live.iter()
                .map(|(path, (fingerprint, outcome))| Entry {
                    path: path.to_string_lossy().into_owned(),
                    fingerprint: *fingerprint,
                    outcome: outcome.clone(),
                })
                .collect()
        };
        let payload = CacheFile {
            version: CACHE_VERSION,
            entries,
        };
        let Ok(text) = serde_json::to_string_pretty(&payload) else {
            return;
        };
        if let Some(parent) = file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(err) = std::fs::write(file, text) {
            log::debug!(
                "could not write the plugin scan cache to {}: {err}",
                file.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::scratch_dir;

    fn fingerprint(len: u64) -> Fingerprint {
        Fingerprint {
            len,
            modified_ns: 42,
        }
    }

    #[test]
    fn a_matching_fingerprint_answers_and_a_changed_one_does_not() {
        let cache: ScanCache<String> = ScanCache::in_memory();
        let module = Path::new(r"C:\plugins\Thing.vst3");

        assert!(cache.get(module, fingerprint(10)).is_none());
        cache.put(
            module,
            fingerprint(10),
            Outcome::Found(vec!["Thing".to_owned()]),
        );

        assert_eq!(
            cache.get(module, fingerprint(10)),
            Some(Outcome::Found(vec!["Thing".to_owned()]))
        );
        assert!(
            cache.get(module, fingerprint(11)).is_none(),
            "a module the user upgraded must be read again, not re-reported"
        );
        assert_eq!(cache.len(), 1);

        cache.forget(module);
        assert!(cache.is_empty());
    }

    #[test]
    fn a_refusal_is_remembered_too() {
        // A stray text file named `.vst3` costs a whole child process to discover
        // and fails identically every time.
        let cache: ScanCache<String> = ScanCache::in_memory();
        let module = Path::new(r"C:\plugins\garbage.vst3");
        cache.put(
            module,
            fingerprint(3),
            Outcome::Refused("not a shared library".to_owned()),
        );
        assert_eq!(
            cache.get(module, fingerprint(3)),
            Some(Outcome::Refused("not a shared library".to_owned()))
        );
    }

    #[test]
    fn what_one_run_learned_the_next_one_reads() {
        let dir = scratch_dir("cache-persist");
        let file = dir.path().join("scan-cache.json");
        // A module that really exists, because `save` prunes entries whose file
        // is gone -- an uninstalled plugin must not sit in the cache forever.
        let module = dir.path().join("Thing.vst3");
        std::fs::write(&module, b"contents").unwrap();
        let print = Fingerprint::of(&module).expect("a file that exists can be fingerprinted");

        {
            let cache: ScanCache<String> = ScanCache::with_file(&file);
            cache.put(&module, print, Outcome::Found(vec!["Thing".to_owned()]));
            cache.save();
        }
        assert!(file.exists());

        let reloaded: ScanCache<String> = ScanCache::with_file(&file);
        assert_eq!(
            reloaded.get(&module, print),
            Some(Outcome::Found(vec!["Thing".to_owned()]))
        );

        // ...and a module that has since been uninstalled is dropped.
        std::fs::remove_file(&module).unwrap();
        reloaded.save();
        let after: ScanCache<String> = ScanCache::with_file(&file);
        assert!(after.is_empty(), "an uninstalled plugin must not linger");
    }

    #[test]
    fn a_corrupt_or_stale_cache_file_is_an_empty_cache_rather_than_a_failure() {
        let dir = scratch_dir("cache-corrupt");
        let file = dir.path().join("scan-cache.json");

        std::fs::write(&file, b"{not json at all").unwrap();
        assert!(ScanCache::<String>::with_file(&file).is_empty());

        std::fs::write(&file, br#"{"version":9999,"entries":[]}"#).unwrap();
        assert!(
            ScanCache::<String>::with_file(&file).is_empty(),
            "a cache written by another version is discarded, not guessed at"
        );
    }

    #[test]
    fn a_fingerprint_notices_a_rewritten_file() {
        let dir = scratch_dir("cache-fingerprint");
        let module = dir.path().join("Thing.vst3");
        std::fs::write(&module, b"one").unwrap();
        let before = Fingerprint::of(&module).unwrap();

        std::fs::write(&module, b"different length").unwrap();
        let after = Fingerprint::of(&module).unwrap();
        assert_ne!(before, after);

        assert!(Fingerprint::of(&dir.path().join("not-here.vst3")).is_none());
    }
}
