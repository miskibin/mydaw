//! What instruments exist, and how to build a node for one.
//!
//! The synthesised instruments answer this with a `const` array
//! ([`mydaw_plugin_instruments::InstrumentRegistry`]): there are two of them,
//! they are compiled in, and the list cannot change. A sampler's list is read
//! from a directory that may be empty at boot and full ten minutes later,
//! because a download finished. So this registry is the same shape with two
//! differences that follow from that:
//!
//! **It is swappable.** The entry list lives behind an [`ArcSwap`]. A rescan
//! builds a whole new list off-thread and publishes it in one store; a reader
//! -- the source picker, the graph builder -- never sees a half-built list and
//! never blocks behind a load. [`generation`](SamplerRegistry::generation)
//! counts publications, which is what lets the shell notice that the picker
//! needs redrawing without polling the contents.
//!
//! **Its ids are interned.** [`RtNodeFactory::type_id`](mydaw_rt_core::RtNodeFactory::type_id)
//! is `&'static str` and an instrument id comes from a directory name. Leaking
//! the string is the only way to bridge that, so [`intern`] makes sure each
//! distinct id leaks exactly *once* however many times the assets are
//! rescanned -- a bounded, one-per-instrument cost rather than a slow leak on a
//! button the user can press twice.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;
use mydaw_rt_core::{ParamNodeFactory, RtNode, RtNodeFactory};
use parking_lot::Mutex;

use crate::bank::{load_instrument, SampleBank};
use crate::node::SamplerFactory;
use crate::source_id;

/// What the shell needs to know about one loaded instrument to offer it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstrumentInfo {
    /// The node factory's `type_id`, e.g. `"sampler:salamander-piano"`. This is
    /// what a project file records.
    pub type_id: &'static str,
    /// The instrument's own id, without the `sampler:` prefix.
    pub id: String,
    /// Name for a menu.
    pub name: String,
    /// One line for a tooltip.
    pub description: String,
    /// The attribution its licence requires be shown. Empty for content that
    /// needs none.
    pub attribution: String,
    /// Samples actually loaded.
    pub samples: usize,
    /// Samples the manifest named that could not be loaded.
    pub skipped: usize,
    /// Bytes of decoded audio this instrument holds.
    pub bytes: usize,
}

/// One instrument: what to show, and what to build.
#[derive(Clone)]
struct Entry {
    info: InstrumentInfo,
    factory: Arc<SamplerFactory>,
}

/// Every instrument found under one directory.
pub struct SamplerRegistry {
    root: PathBuf,
    entries: ArcSwap<Vec<Entry>>,
    scanning: AtomicBool,
    generation: AtomicU64,
    /// One-shots dropped onto the rack. A disk rescan must not forget them.
    user: Mutex<Vec<Entry>>,
}

impl SamplerRegistry {
    /// An empty registry over `root`, which need not exist.
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            entries: ArcSwap::from_pointee(Vec::new()),
            scanning: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            user: Mutex::new(Vec::new()),
        }
    }

    /// An empty registry over nothing, which a [`rescan`](Self::rescan) leaves
    /// empty.
    ///
    /// For tests and for the offline entry points, which want a track host
    /// without a filesystem behind it. Not merely a convenience: passing a
    /// plausible-looking path would make a test that accidentally scanned the
    /// developer's real assets pass on their machine and fail in CI.
    pub fn detached() -> Self {
        Self::new(PathBuf::new())
    }

    /// The directory this registry scans: `<assets>/instruments`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether a scan is running. The shell shows this as "loading" rather than
    /// as "no instruments", which are very different facts.
    pub fn is_scanning(&self) -> bool {
        self.scanning.load(Ordering::Acquire)
    }

    /// How many times the instrument list has been published.
    ///
    /// Changes exactly when the contents do, so the shell can tell "the picker
    /// is stale" from "nothing happened" without comparing lists.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Every loaded instrument, in directory order.
    pub fn instruments(&self) -> Vec<InstrumentInfo> {
        self.entries
            .load()
            .iter()
            .map(|entry| entry.info.clone())
            .collect()
    }

    /// Whether `type_id` names a loaded instrument.
    pub fn contains(&self, type_id: &str) -> bool {
        self.entries
            .load()
            .iter()
            .any(|entry| entry.info.type_id == type_id)
    }

    /// Description of one instrument.
    pub fn info(&self, type_id: &str) -> Option<InstrumentInfo> {
        self.entries
            .load()
            .iter()
            .find(|entry| entry.info.type_id == type_id)
            .map(|entry| entry.info.clone())
    }

    /// The bank behind one instrument, for tests and diagnostics.
    pub fn bank(&self, type_id: &str) -> Option<Arc<SampleBank>> {
        self.entries
            .load()
            .iter()
            .find(|entry| entry.info.type_id == type_id)
            .map(|entry| Arc::clone(entry.factory.bank()))
    }

    /// Bytes of decoded audio every loaded instrument holds together. What the
    /// memory budget is measured against.
    pub fn bytes(&self) -> usize {
        self.entries
            .load()
            .iter()
            .map(|entry| entry.info.bytes)
            .sum()
    }

    /// Instantiate an instrument. **Non-RT**, but cheap: the bank was decoded
    /// at scan time, so this clones an `Arc` and zeroes a voice array.
    ///
    /// `None` for a `type_id` this registry does not have -- which, unlike the
    /// synthesised instruments, is a *runtime* condition rather than a wiring
    /// bug: the assets may simply not be installed.
    pub fn create(
        &self,
        type_id: &str,
        sample_rate: f64,
        max_block: usize,
    ) -> Option<Box<dyn RtNode>> {
        self.entries
            .load()
            .iter()
            .find(|entry| entry.info.type_id == type_id)
            .map(|entry| entry.factory.create(sample_rate, max_block))
    }

    /// The same, reading the knobs in `params`.
    ///
    /// The block belongs to the track and outlives the node, which is what
    /// makes a graph rebuild pick the instrument up where it was -- the same
    /// contract the synthesised instruments have.
    pub fn create_with_params(
        &self,
        type_id: &str,
        sample_rate: f64,
        max_block: usize,
        params: Arc<mydaw_rt_core::ParamBlock>,
    ) -> Option<Box<dyn RtNode>> {
        self.entries
            .load()
            .iter()
            .find(|entry| entry.info.type_id == type_id)
            .map(|entry| {
                entry
                    .factory
                    .create_with_params(sample_rate, max_block, params)
            })
    }

    /// Decode `path` into a oneshot instrument and keep it across rescans.
    ///
    /// The same path is installed once: a second drop of the same file reuses
    /// the bank and the caller adds another channel on top of it.
    pub fn install_file(&self, path: &Path) -> Result<InstrumentInfo, crate::LoadError> {
        let id = crate::file_instrument_id(path);
        if let Some(info) = self.instruments().into_iter().find(|info| info.id == id) {
            return Ok(info);
        }

        let bank = SampleBank::oneshot_from_path(path)?;
        Ok(self.install_bank(bank))
    }

    /// Load one bank synchronously for an offline worker, without scanning the
    /// rest of the library. Never called from the audio callback.
    pub fn install_instrument(&self, id: &str) -> Result<InstrumentInfo, String> {
        let mut components = Path::new(id).components();
        if !matches!(components.next(), Some(std::path::Component::Normal(_)))
            || components.next().is_some()
        {
            return Err("instrument ID must be one directory name".into());
        }
        if let Some(info) = self.instruments().into_iter().find(|info| info.id == id) {
            if info.skipped != 0 {
                return Err(format!(
                    "instrument {id} is missing {} samples",
                    info.skipped
                ));
            }
            return Ok(info);
        }
        let bank = load_instrument(&self.root.join(id)).map_err(|e| e.to_string())?;
        if bank.id() != id || !bank.skipped().is_empty() {
            return Err(format!(
                "instrument {id} has a mismatched ID or missing samples"
            ));
        }
        Ok(self.install_bank(bank))
    }

    fn install_bank(&self, bank: Arc<SampleBank>) -> InstrumentInfo {
        let entry = entry_for(bank);
        let info = entry.info.clone();
        let type_id = info.type_id;

        let mut user = self.user.lock();
        if let Some(existing) = user.iter().find(|entry| entry.info.type_id == type_id) {
            return existing.info.clone();
        }
        user.push(entry.clone());

        let mut merged: Vec<Entry> = self
            .entries
            .load()
            .iter()
            .filter(|entry| entry.info.type_id != type_id)
            .cloned()
            .collect();
        merged.push(entry);
        self.entries.store(Arc::new(merged));
        self.generation.fetch_add(1, Ordering::AcqRel);
        info
    }

    /// Rescan the root and publish what is there. **Blocking**, seconds for a
    /// piano; call it off any thread a user is waiting on.
    ///
    /// Re-entrant in the sense that matters: two concurrent scans both publish,
    /// and the last one wins. Nothing is torn.
    pub fn rescan(&self) {
        self.scanning.store(true, Ordering::Release);
        let mut entries = scan(&self.root);
        {
            let user = self.user.lock();
            for entry in user.iter() {
                if !entries
                    .iter()
                    .any(|existing| existing.info.type_id == entry.info.type_id)
                {
                    entries.push(entry.clone());
                }
            }
            let count = entries.len();
            let bytes: usize = entries.iter().map(|entry| entry.info.bytes).sum();
            self.entries.store(Arc::new(entries));
            self.generation.fetch_add(1, Ordering::AcqRel);
            self.scanning.store(false, Ordering::Release);

            if count == 0 {
                log::info!(
                    "sampler: no instruments under {}; \
                     run scripts/fetch-assets.ps1 or use the in-app downloader to install them",
                    self.root.display()
                );
            } else {
                log::info!(
                    "sampler: {count} instrument(s) loaded from {} ({:.1} MB of samples)",
                    self.root.display(),
                    bytes as f64 / 1e6
                );
            }
        }
    }

    /// Rescan on a background thread, unless one is already running.
    ///
    /// The boot path and the "download finished" path both take this: neither
    /// has a thread it can afford to spend seconds decoding on.
    pub fn rescan_in_background(self: &Arc<Self>) {
        if self
            .scanning
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            // Already scanning. The running scan will publish whatever is on
            // disk when it reads it, which is at least as new as this request.
            return;
        }
        // `rescan` sets the flag itself; it was set above only to claim the
        // slot, and is cleared here on the spawn failure path.
        let registry = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("mydaw-sampler-scan".to_owned())
            .spawn(move || registry.rescan());
        if let Err(err) = spawned {
            log::warn!("sampler: could not start the instrument scan: {err}");
            self.scanning.store(false, Ordering::Release);
        }
    }
}

impl std::fmt::Debug for SamplerRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SamplerRegistry")
            .field("root", &self.root)
            .field("scanning", &self.is_scanning())
            .field("generation", &self.generation())
            .field(
                "instruments",
                &self
                    .entries
                    .load()
                    .iter()
                    .map(|entry| entry.info.type_id)
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// Load every instrument directory under `root`.
///
/// A directory that fails to load is logged and passed over: one broken
/// instrument must not cost the user the others.
fn scan(root: &Path) -> Vec<Entry> {
    let Ok(read_dir) = std::fs::read_dir(root) else {
        // Not an error. A fresh clone has no assets directory at all.
        return Vec::new();
    };

    let mut directories: Vec<PathBuf> = read_dir
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    // Directory order is filesystem order, which differs between machines; a
    // source picker whose entries move between runs is a bug report waiting to
    // happen.
    directories.sort();

    let mut entries = Vec::new();
    for dir in directories {
        match load_instrument(&dir) {
            Ok(bank) => entries.push(entry_for(bank)),
            Err(err) => log::warn!("sampler: skipping {}: {err}", dir.display()),
        }
    }
    entries
}

fn entry_for(bank: Arc<SampleBank>) -> Entry {
    let type_id = intern(&source_id(bank.id()));
    publish_defaults(type_id, bank.default_params());
    let info = InstrumentInfo {
        type_id,
        id: bank.id().to_owned(),
        name: bank.name().to_owned(),
        description: bank.description().to_owned(),
        attribution: bank.attribution().to_owned(),
        samples: bank.len(),
        skipped: bank.skipped().len(),
        bytes: bank.bytes(),
    };
    if !bank.skipped().is_empty() {
        log::warn!(
            "sampler: instrument '{}' is missing {} of its samples; \
             notes they covered fall through to a neighbouring layer",
            info.id,
            bank.skipped().len()
        );
    }
    Entry {
        info,
        factory: Arc::new(SamplerFactory::new(type_id, bank)),
    }
}

/// Every loaded instrument's starting knob positions, by `type_id`.
///
/// Process-wide for the same reason [`intern`] is: the shell builds a track's
/// parameter block in `TrackDef::params_for` (`app/src/project.rs`) -- a pure
/// data type with no registry to ask -- and the answer is a property
/// of the *instrument*, discovered when its manifest was read. Published on
/// load rather than looked up through a service so that the two places a track
/// can come from (a new channel, and a project file being restored) cannot
/// disagree about where an 808's knobs start.
///
/// Bounded by the number of instruments installed, and overwritten rather than
/// appended to on a rescan.
static DEFAULTS: Mutex<Option<HashMap<&'static str, InstrumentDefaults>>> = Mutex::new(None);

/// One instrument's starting knob positions: `(parameter id, value)` pairs.
pub type InstrumentDefaults = Arc<[(String, f32)]>;

fn publish_defaults(type_id: &'static str, params: &[(String, f32)]) {
    let mut guard = DEFAULTS.lock();
    guard
        .get_or_insert_with(HashMap::new)
        .insert(type_id, params.into());
}

/// Where the instrument `type_id` wants the sampler's knobs, by parameter id.
///
/// Empty for an instrument that is not loaded -- which is not an error: a
/// project can name an instrument whose samples are not installed yet, and its
/// track keeps the sampler's own defaults until they are.
pub fn instrument_defaults(type_id: &str) -> InstrumentDefaults {
    DEFAULTS
        .lock()
        .as_ref()
        .and_then(|map| map.get(type_id).cloned())
        .unwrap_or_else(|| Arc::from([]))
}

/// Leak `id` once and return the same `&'static str` for it ever after.
///
/// The bridge between runtime-discovered instrument ids and
/// [`RtNodeFactory::type_id`]'s `&'static str`. Interning rather than leaking
/// per scan is what keeps "press Download, then Rescan, then Rescan again" from
/// being a slow leak.
pub fn intern(id: &str) -> &'static str {
    static INTERNED: Mutex<Option<HashMap<String, &'static str>>> = Mutex::new(None);
    let mut guard = INTERNED.lock();
    let map = guard.get_or_insert_with(HashMap::new);
    if let Some(existing) = map.get(id) {
        return existing;
    }
    let leaked: &'static str = Box::leak(id.to_owned().into_boxed_str());
    map.insert(id.to_owned(), leaked);
    leaked
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{InstrumentManifest, SampleEntry};
    use crate::test_support::{write_wav, TempDir};

    /// Write a one-sample instrument into `root/<id>` and return `root`.
    fn instrument(dir: &Path, id: &str, name: &str) {
        let home = dir.join(id);
        std::fs::create_dir_all(&home).unwrap();
        write_wav(&home.join("a.wav"), 48_000, 1, &vec![0.5_f32; 480]);
        let manifest = InstrumentManifest {
            id: id.to_owned(),
            name: name.to_owned(),
            description: String::new(),
            release_ms: 120.0,
            one_shot: false,
            default_params: Default::default(),
            max_seconds: None,
            fade_out_seconds: None,
            attribution: "Test content".to_owned(),
            samples: vec![SampleEntry {
                file: "a.wav".to_owned(),
                root_key: 60,
                key_lo: 0,
                key_hi: 127,
                vel_lo: 1,
                vel_hi: 127,
                tune_cents: 0.0,
            }],
        };
        std::fs::write(
            home.join(crate::manifest::MANIFEST_FILE_NAME),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn a_missing_root_is_an_empty_registry_and_not_an_error() {
        let registry = SamplerRegistry::new(PathBuf::from("no-such-dir-7a3f/instruments"));
        registry.rescan();
        assert!(registry.instruments().is_empty());
        assert_eq!(registry.bytes(), 0);
        assert!(!registry.is_scanning());
        assert_eq!(registry.generation(), 1, "an empty scan still publishes");
    }

    #[test]
    fn instruments_are_found_named_and_buildable() {
        let temp = TempDir::new("registry");
        let root = temp.path().join("instruments");
        instrument(&root, "zeta-bells", "Zeta Bells");
        instrument(&root, "alpha-piano", "Alpha Piano");

        let registry = SamplerRegistry::new(root);
        registry.rescan();

        let found = registry.instruments();
        assert_eq!(found.len(), 2);
        assert_eq!(
            found.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            vec!["alpha-piano", "zeta-bells"],
            "the list must be sorted, not in filesystem order"
        );

        let piano = &found[0];
        assert_eq!(piano.type_id, "sampler:alpha-piano");
        assert_eq!(piano.name, "Alpha Piano");
        assert_eq!(piano.samples, 1);
        assert_eq!(piano.skipped, 0);
        assert_eq!(piano.attribution, "Test content");
        assert!(piano.bytes > 0);
        assert!(!piano.description.is_empty(), "a tooltip is substituted");

        assert!(registry.contains("sampler:alpha-piano"));
        assert!(!registry.contains("sampler:nothing"));
        assert_eq!(registry.info("sampler:alpha-piano").as_ref(), Some(piano));
        assert!(registry
            .create("sampler:alpha-piano", 48_000.0, 512)
            .is_some());
        assert!(registry.create("sampler:nothing", 48_000.0, 512).is_none());
        assert!(registry.bank("sampler:alpha-piano").is_some());
    }

    #[test]
    fn a_worker_loads_only_the_requested_complete_bank() {
        let temp = TempDir::new("selective-bank");
        let root = temp.path().join("instruments");
        instrument(&root, "drums", "Drums");
        instrument(&root, "unrelated", "Unrelated");
        let registry = SamplerRegistry::new(root.clone());
        let drums = registry.install_instrument("drums").unwrap();
        assert_eq!(drums.id, "drums");
        assert_eq!(registry.instruments().len(), 1);
        assert!(!registry.contains("sampler:unrelated"));
        let generation = registry.generation();
        registry.install_instrument("drums").unwrap();
        assert_eq!(registry.generation(), generation);
        assert!(registry.install_instrument("../unrelated").is_err());
        std::fs::remove_file(root.join("unrelated/a.wav")).unwrap();
        assert!(registry.install_instrument("unrelated").is_err());
    }

    /// The hot-load path: instruments appear without anything being rebuilt.
    #[test]
    fn a_rescan_picks_up_an_instrument_that_was_not_there_before() {
        let temp = TempDir::new("rescan");
        let root = temp.path().join("instruments");
        std::fs::create_dir_all(&root).unwrap();

        let registry = SamplerRegistry::new(root.clone());
        registry.rescan();
        assert!(registry.instruments().is_empty());
        let first = registry.generation();

        instrument(&root, "late-piano", "Late Piano");
        registry.rescan();

        assert_eq!(registry.instruments().len(), 1);
        assert!(registry.contains("sampler:late-piano"));
        assert!(
            registry.generation() > first,
            "a publication must bump the generation"
        );
    }

    #[test]
    fn one_broken_instrument_does_not_cost_the_user_the_others() {
        let temp = TempDir::new("broken");
        let root = temp.path().join("instruments");
        instrument(&root, "good", "Good");
        let bad = root.join("bad");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join(crate::manifest::MANIFEST_FILE_NAME), b"{ nope").unwrap();

        let registry = SamplerRegistry::new(root);
        registry.rescan();
        let found = registry.instruments();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "good");
    }

    #[test]
    fn a_dropped_file_survives_a_rescan_and_is_not_decoded_twice() {
        let temp = TempDir::new("user-oneshot");
        let path = temp.path().join("rim.wav");
        write_wav(&path, 8_000, 1, &[0.4_f32; 40]);

        let registry = SamplerRegistry::detached();
        let first = registry.install_file(&path).expect("installs");
        assert_eq!(first.id, crate::file_instrument_id(&path));
        assert!(first.type_id.starts_with("sampler:file:"));
        assert_eq!(registry.instruments().len(), 1);

        registry.rescan();
        assert_eq!(
            registry.instruments().len(),
            1,
            "a disk scan must not drop user oneshots"
        );
        assert!(registry.contains(first.type_id));

        let again = registry.install_file(&path).expect("reuses");
        assert_eq!(again.type_id, first.type_id);
        assert_eq!(registry.instruments().len(), 1);
    }

    /// A directory whose manifest is fine but whose samples are all missing is
    /// not an instrument, and must not appear as an empty one.
    #[test]
    fn an_instrument_whose_samples_are_all_missing_is_not_offered() {
        let temp = TempDir::new("no-samples");
        let root = temp.path().join("instruments");
        instrument(&root, "ghost", "Ghost");
        std::fs::remove_file(root.join("ghost").join("a.wav")).unwrap();

        let registry = SamplerRegistry::new(root);
        registry.rescan();
        assert!(registry.instruments().is_empty());
    }

    #[test]
    fn interning_returns_the_same_pointer_for_the_same_id() {
        let first = intern("sampler:interned-test");
        let second = intern("sampler:interned-test");
        assert!(
            std::ptr::eq(first, second),
            "a rescan must not leak the same id twice"
        );
        assert_ne!(
            intern("sampler:interned-other").as_ptr(),
            first.as_ptr(),
            "different ids are different strings"
        );
    }
}
