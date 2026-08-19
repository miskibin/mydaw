//! `mydaw-plugin-scan` -- finding out what is inside a plugin without betting the
//! DAW on it.
//!
//! Both of MYDAW's plugin hosts have the same problem and it is not a format
//! problem: reading a module's plugin list means running a stranger's code in
//! your process. `mydaw-plugin-host-clap` and `mydaw-plugin-host-vst3` differ in
//! every detail of *how* they read a module and in none of the details of what to
//! do about that, so the "what to do about it" lives here once:
//!
//! | | where |
//! | --- | --- |
//! | one child process per module, and the protocol it speaks | [`worker`] |
//! | what was found last time, so nothing is opened twice | [`cache`] |
//! | what must never be opened again | [`blocklist`] |
//! | the pass that puts the three together | [`ModuleScan`] |
//!
//! A host supplies one [`ScanTarget`] implementation -- its plugin-info type,
//! its format tag, how to read one module, and which file inside a bundle to
//! fingerprint -- and gets crash-proof scanning.
//!
//! # The order a pass tries things
//!
//! ```text
//! for each module:
//!     block-listed?          -> skip, never open it
//!     fingerprint unchanged? -> answer from the cache, open nothing
//!     otherwise              -> `list_cached`: skip, wait for Rescan
//!                            -> `scan_all`: open it in a child process
//!         answered           -> remember it (success or refusal alike)
//!         crashed / hung     -> block-list it, and carry on with the next one
//! ```
//!
//! Listing the browser must not run vendor `init()`. Unlicensed plugins pop an
//! activation window the moment they load, and a tab that "just looks" is not
//! consent to do that. [`ModuleScan::list_cached`] is the default path; Rescan
//! is the only thing that calls [`ModuleScan::scan_all`].
//!
//! A warm list pays a `stat` per module. A Rescan of an unchanged machine still
//! hits the cache and opens nothing; only new or replaced files start a child.
//! The last line is what makes a Rescan survivable: before this, one plugin
//! hanging inside `DllMain` poisoned the process-wide loader lock and every
//! module after it was reported broken.

pub mod blocklist;
pub mod cache;
pub mod worker;

#[cfg(test)]
mod test_support;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::Serialize;

pub use blocklist::{data_dir, data_file, BlockList};
pub use cache::{Fingerprint, Outcome, ScanCache};
pub use worker::{
    declare_worker_capable, run_worker_if_requested, WorkerHandler, WorkerOutcome, WorkerSpawn,
    CHILD_TIMEOUT,
};

/// One plugin format, from the scanner's point of view.
///
/// Everything the shared machinery needs to know about CLAP or VST3, and nothing
/// more: the type is `Serialize` because it crosses a pipe and lands in a cache
/// file, and `Send + 'static` because the in-process fallback reads it on a
/// worker thread.
pub trait ScanTarget: Clone + Serialize + DeserializeOwned + Send + 'static {
    /// Tag identifying this format on the wire and in file names (`vst3`,
    /// `clap`). Changing it orphans a user's cache and block list.
    const FORMAT: &'static str;

    /// Read one module. **Runs third-party code**; this is the function a child
    /// process exists to contain.
    ///
    /// The error is a sentence for a user, because that is all that survives the
    /// trip through a pipe.
    fn read_module(module: &Path) -> Result<Vec<Self>, String>;

    /// The file whose length and timestamp decide whether a cached answer is
    /// still good.
    ///
    /// Not always the module path: a VST3 bundle is a *directory*, whose own
    /// metadata says nothing useful about the binary buried inside it.
    fn fingerprint_path(module: &Path) -> PathBuf {
        module.to_path_buf()
    }

    /// Stamp the module path onto an entry that came from a pipe or a cache file.
    ///
    /// Neither carries it -- the parent already knows which module it asked
    /// about, and a path is the one field a child should not be trusted to
    /// invent.
    fn with_module_path(self, module: &Path) -> Self;

    /// The block-list file for this format.
    fn blocklist_file() -> String {
        format!("{}-blocklist.txt", Self::FORMAT)
    }

    /// The metadata cache file for this format.
    fn cache_file() -> String {
        format!("{}-scan-cache.json", Self::FORMAT)
    }

    /// The worker table entry for this format, for `main` to hand to
    /// [`run_worker_if_requested`].
    fn worker_handler() -> WorkerHandler {
        WorkerHandler {
            format: Self::FORMAT,
            scan: |module| {
                Self::read_module(module)
                    .and_then(|found| serde_json::to_value(found).map_err(|err| err.to_string()))
            },
        }
    }
}

/// Why one module produced nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanFailure {
    /// The module was read and refused -- not a plugin, wrong architecture, an
    /// initialiser that returned an error. Remembered, and retried when the file
    /// changes.
    Refused(String),
    /// The module took its scan process down, or never answered. Block-listed:
    /// it will not be opened again until the user says so.
    Blocked(String),
}

impl ScanFailure {
    /// The sentence a plugin browser shows.
    pub fn message(&self) -> &str {
        match self {
            Self::Refused(message) | Self::Blocked(message) => message,
        }
    }

    /// Whether this module was added to the block list.
    pub fn is_blocked(&self) -> bool {
        matches!(self, Self::Blocked(_))
    }
}

impl std::fmt::Display for ScanFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// What one pass over a set of modules found.
#[derive(Debug)]
pub struct ScanPass<T> {
    /// Every plugin found, in module order.
    pub found: Vec<T>,
    /// Modules that produced nothing, with the reason.
    pub failures: Vec<(PathBuf, ScanFailure)>,
    /// Modules that were never opened: block-listed, unknown to the cache
    /// ([`ModuleScan::list_cached`]), or -- in the in-process fallback only --
    /// after an earlier module wedged the loader.
    pub skipped: Vec<PathBuf>,
    /// Modules block-listed *by this pass*.
    pub newly_blocked: Vec<PathBuf>,
    /// Modules answered from the cache without opening anything.
    pub cache_hits: usize,
    /// Child processes started.
    pub children: usize,
    /// Modules opened inside this process because no worker was available.
    pub in_process: usize,
    /// The module that wedged this process, if the in-process fallback was in
    /// use and one did. `Some` means the pass **stopped early**: that module
    /// holds the loader lock, so nothing after it could have been opened. Always
    /// `None` when scanning out of process, which is the point.
    pub aborted_at: Option<PathBuf>,
    /// How long the pass took.
    pub elapsed: Duration,
}

impl<T> Default for ScanPass<T> {
    fn default() -> Self {
        Self {
            found: Vec::new(),
            failures: Vec::new(),
            skipped: Vec::new(),
            newly_blocked: Vec::new(),
            cache_hits: 0,
            children: 0,
            in_process: 0,
            aborted_at: None,
            elapsed: Duration::ZERO,
        }
    }
}

impl<T> ScanPass<T> {
    /// Whether the pass stopped early because a module wedged this process.
    pub fn was_aborted(&self) -> bool {
        self.aborted_at.is_some()
    }
}

/// A configured scanner: which modules to refuse, what is already known, and how
/// to start a child.
///
/// Cheap to build and safe to share behind an `Arc`; every method takes `&self`.
#[derive(Debug)]
pub struct ModuleScan<T> {
    blocklist: BlockList,
    cache: ScanCache<T>,
    spawn: WorkerSpawn,
    timeout: Duration,
}

impl<T: ScanTarget> ModuleScan<T> {
    /// The scanner a DAW wants: the user's block list, the user's cache, and a
    /// child process per unknown module.
    pub fn persistent() -> Self {
        Self {
            blocklist: BlockList::load(&T::blocklist_file()),
            cache: ScanCache::load(&T::cache_file()),
            spawn: WorkerSpawn::Auto,
            timeout: CHILD_TIMEOUT,
        }
    }

    /// A scanner that remembers nothing between runs. The constructor tests want:
    /// a test that wrote to the developer's real block list would be a test that
    /// changed which plugins their DAW can see.
    pub fn in_memory() -> Self {
        Self {
            blocklist: BlockList::in_memory(),
            cache: ScanCache::in_memory(),
            spawn: WorkerSpawn::Auto,
            timeout: CHILD_TIMEOUT,
        }
    }

    /// Replace the block list.
    #[must_use]
    pub fn with_blocklist(mut self, blocklist: BlockList) -> Self {
        self.blocklist = blocklist;
        self
    }

    /// Replace the metadata cache.
    #[must_use]
    pub fn with_cache(mut self, cache: ScanCache<T>) -> Self {
        self.cache = cache;
        self
    }

    /// Replace how a child is started. See [`WorkerSpawn`].
    #[must_use]
    pub fn with_spawn(mut self, spawn: WorkerSpawn) -> Self {
        self.spawn = spawn;
        self
    }

    /// Replace the per-module deadline.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The modules this scanner refuses to open.
    pub fn blocklist(&self) -> &BlockList {
        &self.blocklist
    }

    /// What this scanner already knows.
    pub fn cache(&self) -> &ScanCache<T> {
        &self.cache
    }

    /// The per-module deadline.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// List plugins already known to the cache. **Never opens a module.**
    ///
    /// Unknown or changed files are skipped, not scanned. That is the path a
    /// plugin browser takes on open: showing last time's list must not run a
    /// vendor's `init()`, because that is what pops a license window.
    ///
    /// Call [`scan_all`](Self::scan_all) only when the user asks to Rescan.
    pub fn list_cached(&self, modules: &[PathBuf]) -> ScanPass<T> {
        let started = Instant::now();
        let mut pass = ScanPass::default();
        for module in modules {
            if self.blocklist.contains(module) {
                pass.skipped.push(module.clone());
                continue;
            }
            if !self.apply_cache_hit(module, &mut pass) {
                pass.skipped.push(module.clone());
            }
        }
        pass.elapsed = started.elapsed();
        pass
    }

    /// Scan every module in `modules`, in order.
    ///
    /// Never fails and never panics: a module that will not load, one that
    /// panics, one that crashes its child and one that hangs are all recorded and
    /// stepped over. The cache is written once at the end.
    ///
    /// This is Rescan: it starts a child for every module the cache does not
    /// already know. Do not call it to populate a plugin list.
    pub fn scan_all(&self, modules: &[PathBuf]) -> ScanPass<T> {
        let started = Instant::now();
        let mut pass = ScanPass::default();
        let mut learned_something = false;

        for module in modules {
            if pass.aborted_at.is_some() || self.blocklist.contains(module) {
                pass.skipped.push(module.clone());
                continue;
            }

            if self.apply_cache_hit(module, &mut pass) {
                continue;
            }

            let fingerprint = Fingerprint::of(&T::fingerprint_path(module));
            let outcome = match self.read_one(module, &mut pass) {
                Some(outcome) => outcome,
                // No worker at all: the module was opened in this process, and
                // if it hung, this process is now poisoned. Stop.
                None => {
                    self.block(module, &mut pass, in_process_hang_message());
                    pass.aborted_at = Some(module.clone());
                    continue;
                }
            };

            match outcome {
                Ok(plugins) => {
                    let plugins: Vec<T> = plugins
                        .into_iter()
                        .map(|plugin| plugin.with_module_path(module))
                        .collect();
                    if let Some(print) = fingerprint {
                        self.cache
                            .put(module, print, Outcome::Found(plugins.clone()));
                        learned_something = true;
                    }
                    pass.found.extend(plugins);
                }
                Err(ScanFailure::Refused(message)) => {
                    if let Some(print) = fingerprint {
                        self.cache
                            .put(module, print, Outcome::Refused(message.clone()));
                        learned_something = true;
                    }
                    log::debug!("skipping plugin module {}: {message}", module.display());
                    pass.failures
                        .push((module.clone(), ScanFailure::Refused(message)));
                }
                Err(ScanFailure::Blocked(message)) => self.block(module, &mut pass, message),
            }
        }

        if learned_something {
            self.cache.save();
        }
        pass.elapsed = started.elapsed();
        pass
    }

    /// Apply a cache hit for `module`. Returns `false` when the cache has
    /// nothing for this fingerprint, so the caller can skip or scan.
    fn apply_cache_hit(&self, module: &Path, pass: &mut ScanPass<T>) -> bool {
        let fingerprint = Fingerprint::of(&T::fingerprint_path(module));
        let Some(known) = fingerprint.and_then(|print| self.cache.get(module, print)) else {
            return false;
        };
        pass.cache_hits += 1;
        match known {
            Outcome::Found(plugins) => pass.found.extend(
                plugins
                    .into_iter()
                    .map(|plugin| plugin.with_module_path(module)),
            ),
            Outcome::Refused(message) => pass
                .failures
                .push((module.to_path_buf(), ScanFailure::Refused(message))),
        }
        true
    }

    /// Read one unknown module, out of process if that is possible.
    ///
    /// `None` means there was no worker and the module was opened *here*, on a
    /// worker thread with a deadline, and that deadline passed -- so this process
    /// may now be holding a poisoned loader lock.
    fn read_one(
        &self,
        module: &Path,
        pass: &mut ScanPass<T>,
    ) -> Option<Result<Vec<T>, ScanFailure>> {
        if let Some(outcome) = worker::scan_in_child(&self.spawn, T::FORMAT, module, self.timeout) {
            pass.children += 1;
            return Some(match outcome {
                WorkerOutcome::Scanned(value) => {
                    serde_json::from_value::<Vec<T>>(value).map_err(|err| {
                        ScanFailure::Refused(format!(
                            "the scan worker sent an unreadable result: {err}"
                        ))
                    })
                }
                WorkerOutcome::Refused(message) => Err(ScanFailure::Refused(message)),
                WorkerOutcome::Crashed(message) => Err(ScanFailure::Blocked(message)),
                WorkerOutcome::TimedOut => Err(ScanFailure::Blocked(timeout_message(self.timeout))),
            });
        }

        pass.in_process += 1;
        read_module_with_deadline::<T>(module, self.timeout)
            .map(|result| result.map_err(ScanFailure::Refused))
    }

    fn block(&self, module: &Path, pass: &mut ScanPass<T>, message: String) {
        log::warn!("{}: {message}", module.display());
        self.cache.forget(module);
        if self.blocklist.block(module) {
            pass.newly_blocked.push(module.to_path_buf());
        }
        pass.failures
            .push((module.to_path_buf(), ScanFailure::Blocked(message)));
    }
}

fn timeout_message(timeout: Duration) -> String {
    format!(
        "did not answer within {} s; it is most likely waiting on a licensing dialog. It will be \
         skipped from now on -- delete its line from the block list to try it again.",
        timeout.as_secs()
    )
}

/// Messages never name the module: the host prefixes them with a format-shaped
/// identification ("VST3 module <path>: ..."), and saying it twice reads badly in
/// a plugin browser.
fn in_process_hang_message() -> String {
    "did not answer within the deadline and there was no scan worker to contain it, so it is \
     holding this process's loader lock: nothing after it can be opened until MYDAW restarts."
        .to_owned()
}

/// The fallback: read a module in *this* process, on a thread with a deadline.
///
/// Only reached when no worker executable could be found. It is strictly worse
/// than a child -- a module that hangs leaks the thread and, on Windows, wedges
/// the loader lock for everything after it -- but it is what the hosts did
/// before, and it is better than reporting a machine as having no plugins.
///
/// `None` is "the deadline passed"; the results are plain data and `Send`, so
/// they come back over a channel.
fn read_module_with_deadline<T: ScanTarget>(
    module: &Path,
    timeout: Duration,
) -> Option<Result<Vec<T>, String>> {
    let owned = module.to_path_buf();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let spawned = std::thread::Builder::new()
        .name("mydaw-scan-inline".to_owned())
        .spawn(move || {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| T::read_module(&owned)))
                    .unwrap_or_else(|_| {
                        Err("the module panicked while it was being read".to_owned())
                    });
            // A caller that has already given up has dropped the receiver.
            let _ = tx.send(result);
        });

    match spawned {
        Ok(_handle) => rx.recv_timeout(timeout).ok(),
        Err(err) => {
            log::warn!("could not start a scan thread ({err}); reading inline");
            Some(T::read_module(module))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{scratch_dir, sleeping_worker, trivial_worker};
    use serde::Deserialize;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A format whose "modules" are text files: the first line is the plugin
    /// name, or the file is a refusal. Enough to exercise every branch of a pass
    /// without loading a single DLL.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct FakePlugin {
        name: String,
        #[serde(skip)]
        path: PathBuf,
    }

    impl ScanTarget for FakePlugin {
        const FORMAT: &'static str = "fake";

        fn read_module(module: &Path) -> Result<Vec<Self>, String> {
            let text = std::fs::read_to_string(module).map_err(|err| err.to_string())?;
            if text.starts_with("bad") {
                return Err("not a plugin".to_owned());
            }
            Ok(vec![FakePlugin {
                name: text.trim().to_owned(),
                path: PathBuf::new(),
            }])
        }

        fn with_module_path(mut self, module: &Path) -> Self {
            self.path = module.to_path_buf();
            self
        }
    }

    /// A scanner whose children are counted, and built by `build`.
    fn counting_scan(
        build: impl Fn(&str, &Path) -> std::io::Result<std::process::Child> + Send + Sync + 'static,
    ) -> (ModuleScan<FakePlugin>, Arc<AtomicUsize>) {
        let spawned = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&spawned);
        let scan = ModuleScan::<FakePlugin>::in_memory().with_spawn(WorkerSpawn::Custom(Arc::new(
            move |format, module| {
                counter.fetch_add(1, Ordering::Relaxed);
                build(format, module)
            },
        )));
        (scan, spawned)
    }

    fn module(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn a_worker_that_exits_non_zero_costs_one_module_and_blocks_it() {
        let dir = scratch_dir("pass-nonzero");
        let one = module(dir.path(), "one.fake", "One");
        let (scan, spawned) = counting_scan(|_, _| trivial_worker("exit 3"));

        let pass = scan.scan_all(std::slice::from_ref(&one));

        assert_eq!(spawned.load(Ordering::Relaxed), 1);
        assert!(pass.found.is_empty());
        assert_eq!(pass.failures.len(), 1);
        assert!(pass.failures[0].1.is_blocked(), "{:?}", pass.failures);
        assert_eq!(pass.newly_blocked, vec![one.clone()]);
        assert!(scan.blocklist().contains(&one));
        // ...and the parent is very much alive, which is the entire claim.
        assert!(scan
            .cache()
            .get(&one, Fingerprint::of(&one).unwrap())
            .is_none());
    }

    #[test]
    fn a_worker_that_prints_rubbish_is_a_crash_not_a_plugin() {
        let dir = scratch_dir("pass-garbage");
        let one = module(dir.path(), "one.fake", "One");
        let (scan, _) = counting_scan(|_, _| trivial_worker("echo definitely not json"));

        let pass = scan.scan_all(std::slice::from_ref(&one));

        assert!(pass.found.is_empty());
        assert_eq!(pass.newly_blocked, vec![one]);
    }

    #[test]
    fn a_worker_that_never_answers_is_killed_and_the_pass_carries_on() {
        // The finding that made all of this necessary: before out-of-process
        // scanning, the module after a hang could not be opened either.
        let dir = scratch_dir("pass-hang");
        let hangs = module(dir.path(), "hangs.fake", "Hangs");
        let after = module(dir.path(), "after.fake", "After");

        let hanging = Arc::new(hangs.clone());
        let (scan, _) = counting_scan(move |format, target| {
            if target == hanging.as_path() {
                sleeping_worker()
            } else {
                trivial_worker(&format!(
                    "echo {}{{\"status\":\"ok\",\"v\":1,\"plugins\":[{{\"name\":\"{format}\"}}]}}",
                    worker::RESULT_PREFIX
                ))
            }
        });
        let scan = scan.with_timeout(Duration::from_millis(300));

        let pass = scan.scan_all(&[hangs.clone(), after.clone()]);

        assert_eq!(pass.newly_blocked, vec![hangs]);
        assert_eq!(
            pass.found.len(),
            1,
            "the module after a hang must still be scanned: {:?}",
            pass.failures
        );
        assert_eq!(pass.found[0].path, after);
        assert!(pass.aborted_at.is_none(), "a child hanging aborts nothing");
    }

    #[test]
    fn a_block_listed_module_is_never_opened_again() {
        let dir = scratch_dir("pass-blocked");
        let one = module(dir.path(), "one.fake", "One");
        let (scan, spawned) = counting_scan(|_, _| trivial_worker("exit 3"));

        scan.scan_all(std::slice::from_ref(&one));
        let again = scan.scan_all(std::slice::from_ref(&one));

        assert_eq!(
            spawned.load(Ordering::Relaxed),
            1,
            "the second pass must not start a child for a blocked module"
        );
        assert_eq!(again.skipped, vec![one]);
        assert!(again.newly_blocked.is_empty());
    }

    #[test]
    fn a_cached_module_is_answered_without_starting_anything() {
        // The whole affordability argument: cold scans pay a process per module,
        // warm ones pay a `stat`.
        let dir = scratch_dir("pass-cache");
        let one = module(dir.path(), "one.fake", "One");
        let (scan, spawned) = counting_scan(|_, target| {
            let json = serde_json::to_string(&serde_json::json!({
                "status": "ok",
                "v": 1,
                "plugins": [{ "name": std::fs::read_to_string(target).unwrap().trim() }],
            }))
            .unwrap();
            trivial_worker(&format!("echo {}{json}", worker::RESULT_PREFIX))
        });

        let first = scan.scan_all(std::slice::from_ref(&one));
        assert_eq!(first.found.len(), 1, "{:?}", first.failures);
        assert_eq!(first.found[0].name, "One");
        assert_eq!(first.children, 1);
        assert_eq!(first.cache_hits, 0);

        let second = scan.scan_all(std::slice::from_ref(&one));
        assert_eq!(
            spawned.load(Ordering::Relaxed),
            1,
            "a cache hit must not start a child"
        );
        assert_eq!(second.cache_hits, 1);
        assert_eq!(second.found, first.found);
        assert_eq!(
            second.found[0].path, one,
            "the module path is stamped on by the parent, not sent by the child"
        );

        // ...until the file changes underneath it.
        std::fs::write(&one, "Two and then some").unwrap();
        let third = scan.scan_all(&[one]);
        assert_eq!(third.cache_hits, 0);
        assert_eq!(spawned.load(Ordering::Relaxed), 2);
        assert_eq!(third.found[0].name, "Two and then some");
    }

    #[test]
    fn listing_from_the_cache_never_opens_an_unknown_module() {
        let dir = scratch_dir("list-cached");
        let known = module(dir.path(), "known.fake", "Known");
        let unknown = module(dir.path(), "unknown.fake", "Unknown");
        let (scan, spawned) = counting_scan(|_, target| {
            let json = serde_json::to_string(&serde_json::json!({
                "status": "ok",
                "v": 1,
                "plugins": [{ "name": std::fs::read_to_string(target).unwrap().trim() }],
            }))
            .unwrap();
            trivial_worker(&format!("echo {}{json}", worker::RESULT_PREFIX))
        });

        scan.scan_all(std::slice::from_ref(&known));
        assert_eq!(spawned.load(Ordering::Relaxed), 1);

        let listed = scan.list_cached(&[known.clone(), unknown.clone()]);
        assert_eq!(listed.found.len(), 1);
        assert_eq!(listed.found[0].name, "Known");
        assert_eq!(listed.skipped, vec![unknown]);
        assert_eq!(
            spawned.load(Ordering::Relaxed),
            1,
            "an unknown module must wait for Rescan, not be opened to fill a list"
        );
        assert_eq!(listed.children, 0);
        assert_eq!(listed.cache_hits, 1);
    }

    #[test]
    fn a_refusal_is_cached_and_shown_but_never_blocked() {
        let dir = scratch_dir("pass-refused");
        let bad = module(dir.path(), "bad.fake", "bad file");
        let (scan, spawned) = counting_scan(|_, _| {
            trivial_worker(&format!(
                "echo {}{{\"status\":\"error\",\"v\":1,\"message\":\"not a plugin\"}}",
                worker::RESULT_PREFIX
            ))
        });

        let first = scan.scan_all(std::slice::from_ref(&bad));
        assert_eq!(first.failures.len(), 1);
        assert!(!first.failures[0].1.is_blocked());
        assert_eq!(first.failures[0].1.message(), "not a plugin");
        assert!(!scan.blocklist().contains(&bad));

        let second = scan.scan_all(&[bad]);
        assert_eq!(second.cache_hits, 1);
        assert_eq!(spawned.load(Ordering::Relaxed), 1);
        assert_eq!(second.failures.len(), 1);
    }

    #[test]
    fn with_no_worker_at_all_a_module_is_read_in_process() {
        // The fallback path, and the one place `aborted_at` can still be set.
        let dir = scratch_dir("pass-inline");
        let one = module(dir.path(), "one.fake", "One");
        let bad = module(dir.path(), "bad.fake", "bad file");
        let scan = ModuleScan::<FakePlugin>::in_memory().with_spawn(WorkerSpawn::Custom(Arc::new(
            |_, _| Err(std::io::Error::other("no worker in this build")),
        )));

        let pass = scan.scan_all(&[one.clone(), bad.clone()]);

        // A spawn *failure* is "this build cannot isolate a scan", so the pass
        // falls back to reading the module here, on a thread with a deadline.
        assert_eq!(pass.children, 0);
        assert_eq!(pass.in_process, 2);
        assert_eq!(pass.found.len(), 1, "{:?}", pass.failures);
        assert_eq!(pass.found[0].path, one);
        assert_eq!(pass.failures.len(), 1);
        assert!(!pass.failures[0].1.is_blocked());
        assert!(pass.aborted_at.is_none());
    }

    #[test]
    fn a_pass_over_nothing_is_an_empty_pass() {
        let scan = ModuleScan::<FakePlugin>::in_memory();
        let pass = scan.scan_all(&[]);
        assert!(pass.found.is_empty());
        assert!(!pass.was_aborted());
        assert_eq!(pass.children, 0);
    }

    #[test]
    fn the_file_names_follow_the_format_tag() {
        assert_eq!(FakePlugin::blocklist_file(), "fake-blocklist.txt");
        assert_eq!(FakePlugin::cache_file(), "fake-scan-cache.json");
        assert_eq!(FakePlugin::worker_handler().format, "fake");
    }

    #[test]
    fn the_worker_handler_serialises_what_a_child_would_send() {
        let dir = scratch_dir("handler");
        let one = module(dir.path(), "one.fake", "One");
        let handler = FakePlugin::worker_handler();
        let value = (handler.scan)(&one).expect("the fake module reads");
        assert_eq!(value, serde_json::json!([{ "name": "One" }]));
        assert!(
            (handler.scan)(Path::new("nowhere.fake")).is_err(),
            "a module that is not there is a refusal, not a panic"
        );
    }
}
