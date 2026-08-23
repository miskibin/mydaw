//! `mydaw-plugin-sampler` -- the instrument that plays recordings.
//!
//! Everything in [`mydaw_plugin_instruments`] is synthesised: no assets, no
//! licences, bit-deterministic, and silent about what a real piano sounds like.
//! This crate is the other half. It reads a directory of decoded audio and a
//! manifest describing which key and velocity each file answers for, and turns
//! that into an [`RtNode`](mydaw_rt_core::RtNode) per instrument.
//!
//! ```text
//!   assets/instruments/<id>/manifest.json
//!            │
//!   load ────┴──> SampleBank (Arc, immutable) ──> SamplerFactory ──> SamplerNode
//!  (non-RT,                                        (per instrument)   (32 voices)
//!   background
//!   thread)
//! ```
//!
//! # The three things that make this awkward, and what is done about them
//!
//! **The content is not in the repository.** 138 MB of piano is not something
//! to put in git, and the licence (CC-BY-3.0) is one that has to be honoured
//! rather than assumed. So the samples are *fetched*, pinned by commit and
//! SHA-256, by either `scripts/fetch-assets.ps1` or the in-app downloader in
//! [`download`] -- both reading the same `assets-manifest.json`. **A build with
//! no assets works**: the plugin starts, contributes zero instruments, logs a
//! hint, and every test that needs samples skips itself.
//!
//! **The instruments are discovered at runtime.** A kernel node factory's
//! `type_id` is `&'static str`, and an id read from a directory name is not.
//! [`SamplerRegistry`] interns them (see [`registry::intern`]) so each distinct
//! id leaks exactly once no matter how often the assets are rescanned.
//!
//! **Loading takes seconds.** Decoding ninety FLAC files is not something to do
//! on the boot path. [`SamplerPlugin::start`] hands it to a background thread
//! pool and publishes the result through an `ArcSwap`, so the window opens
//! immediately and the piano appears in the source picker when it is ready --
//! which is the same mechanism that lets a finished download appear without a
//! restart.
//!
//! # Using it
//! ```no_run
//! use mydaw_kernel::Kernel;
//! use mydaw_plugin_sampler::{SamplerPlugin, SamplerRegistry};
//!
//! let mut kernel = Kernel::new();
//! kernel.add_plugin(Box::new(SamplerPlugin::new())).unwrap();
//! kernel.boot().unwrap();
//!
//! let registry = kernel.ctx().services.get::<SamplerRegistry>().unwrap();
//! for instrument in registry.instruments() {
//!     println!("{} ({} samples)", instrument.name, instrument.samples);
//! }
//! ```

pub mod bank;
pub mod decode;
pub mod download;
pub mod manifest;
pub mod node;
pub mod registry;
pub mod test_support;

pub use bank::{
    file_instrument_id, file_path_of, load_from_manifest, load_instrument, LoadError, LoadedSample,
    SampleBank, ONESHOT_MAX_SECONDS,
};
pub use decode::{decode_file, DecodeError};
pub use download::{
    AssetCatalog, AssetDownloader, AssetManifest, AssetState, AssetStatus, CatalogEntry,
    DownloadError, ASSET_MANIFEST_FILE,
};
pub use manifest::{
    InstrumentManifest, ManifestError, SampleEntry, DEFAULT_RELEASE_MS, MANIFEST_FILE_NAME,
};
pub use node::{
    cubic, layer_gain, param, playback_step, SamplerFactory, SamplerNode, CHOKE_MS,
    DEFAULT_ATTACK_MS, DEFAULT_DECAY_MS, DEFAULT_SUSTAIN, LAYER_FLOOR, PARAMS, VOICES,
};
pub use registry::{instrument_defaults, InstrumentInfo, SamplerRegistry};

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mydaw_kernel::{KernelCtx, Plugin, PluginManifest};

/// Kernel id of this plugin.
pub const PLUGIN_ID: &str = "sampler";

/// Display name in the plugin list.
pub const PLUGIN_NAME: &str = "Sampler";

/// Prefix of every sampler source's wire id, so an instrument called
/// `drum-kit` can never collide with the synthesised one.
///
/// The whole id -- `sampler:salamander-piano` -- is what a project file
/// records and what a node factory answers to. It is derived from the
/// instrument's directory name, so renaming a directory invalidates saved
/// projects; that is why the id is in the manifest rather than inferred.
pub const SOURCE_PREFIX: &str = "sampler:";

/// Directory under the assets root that holds one directory per instrument.
pub const INSTRUMENTS_DIR: &str = "instruments";

/// Overrides where the sampler looks for its assets. Set by tests, and by
/// anyone running a build whose assets are not next to it.
pub const ASSETS_DIR_ENV: &str = "MYDAW_ASSETS_DIR";

/// The `sampler:` id for an instrument id.
pub fn source_id(instrument_id: &str) -> String {
    format!("{SOURCE_PREFIX}{instrument_id}")
}

/// The instrument id inside a `sampler:` source id, if it is one.
pub fn instrument_id_of(source_id: &str) -> Option<&str> {
    source_id.strip_prefix(SOURCE_PREFIX)
}

/// Where the sampler's assets live.
///
/// In order: [`ASSETS_DIR_ENV`], then an `assets` directory beside the nearest
/// enclosing workspace root (which is what makes `cargo run` and `cargo test`
/// find the same files from any package directory), then one beside the
/// executable (an installed build).
///
/// Returns a path whether or not anything is there. A missing assets directory
/// is a normal state -- it is what a fresh clone has -- and the downloader
/// needs somewhere to write before anything exists.
pub fn assets_root() -> PathBuf {
    if let Some(dir) = std::env::var_os(ASSETS_DIR_ENV) {
        return PathBuf::from(dir);
    }
    if let Ok(cwd) = std::env::current_dir() {
        // `Cargo.lock` marks the workspace root; an existing `assets/instruments`
        // catches an installed layout that has one already. Five levels is
        // deeper than this workspace nests.
        //
        // The second test looks for `<dir>/assets/instruments`, not for
        // `<dir>/instruments`: `plugins/instruments` is the *synthesised*
        // instruments crate, and matching on its name made a `cargo test` run
        // from `plugins/sampler` resolve the assets root to
        // `plugins/assets` -- so every asset-dependent test skipped itself,
        // silently, on a machine where the content was installed.
        for dir in cwd.ancestors().take(5) {
            let assets = dir.join("assets");
            if dir.join("Cargo.lock").is_file() || assets.join(INSTRUMENTS_DIR).is_dir() {
                return assets;
            }
        }
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_default()
        .join("assets")
}

/// Where instruments live: `<assets root>/instruments`.
pub fn instruments_root() -> PathBuf {
    assets_root().join(INSTRUMENTS_DIR)
}

/// Contributes the [`SamplerRegistry`] service and, once its background scan
/// finishes, one node factory per instrument found on disk.
///
/// No panels: choosing an instrument is a property of a track, and the shell's
/// source picker already lists whatever the registry reports.
#[derive(Debug, Default)]
pub struct SamplerPlugin {
    registry: Option<Arc<SamplerRegistry>>,
}

impl SamplerPlugin {
    pub const fn new() -> Self {
        Self { registry: None }
    }
}

impl Plugin for SamplerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest::new(PLUGIN_ID, PLUGIN_NAME)
    }

    fn register(&mut self, ctx: &mut KernelCtx) {
        let registry = Arc::new(SamplerRegistry::new(instruments_root()));
        self.registry = Some(Arc::clone(&registry));
        let _ = ctx.services.provide(registry);
    }

    fn start(&mut self, _ctx: &KernelCtx) {
        // Off the boot path: decoding a piano takes seconds, and a DAW whose
        // window appears late because of an instrument nobody has asked for yet
        // is a DAW that feels broken. See the crate docs.
        if let Some(registry) = self.registry.as_ref() {
            registry.rescan_in_background();
        }
    }

    fn stop(&mut self) {
        self.registry = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_kernel::Kernel;

    #[test]
    fn a_source_id_round_trips_through_its_prefix() {
        assert_eq!(source_id("salamander-piano"), "sampler:salamander-piano");
        assert_eq!(
            instrument_id_of("sampler:salamander-piano"),
            Some("salamander-piano")
        );
        assert_eq!(instrument_id_of("poly-synth"), None);
        assert_eq!(instrument_id_of("clap:com.acme.thing"), None);
    }

    /// The property that keeps CI green on a machine with no assets.
    #[test]
    fn the_plugin_boots_with_no_assets_at_all_and_contributes_nothing() {
        let empty = test_support::TempDir::new("no-assets");
        let registry = SamplerRegistry::new(empty.path().join("instruments"));
        registry.rescan();
        assert!(registry.instruments().is_empty());
        assert!(registry.create("sampler:anything", 48_000.0, 512).is_none());
    }

    #[test]
    fn the_registry_is_published_as_a_service() {
        let mut kernel = Kernel::new();
        kernel.add_plugin(Box::new(SamplerPlugin::new())).unwrap();
        kernel.boot().unwrap();
        let registry = kernel
            .ctx()
            .services
            .get::<SamplerRegistry>()
            .expect("the registry must be a service");
        // Whatever this machine has, asking is not an error.
        let _ = registry.instruments();
        kernel.shutdown();
    }

    #[test]
    fn the_manifest_names_the_plugin_stably() {
        let plugin = SamplerPlugin::new();
        assert_eq!(plugin.manifest().id, PLUGIN_ID);
        // Project files reference these; renaming one invalidates saved graphs.
        assert_eq!(PLUGIN_ID, "sampler");
        assert_eq!(SOURCE_PREFIX, "sampler:");
    }

    #[test]
    fn the_assets_root_can_be_pointed_somewhere_else() {
        // Serialised against nothing: this is the only test that reads the var,
        // and it restores it.
        let previous = std::env::var_os(ASSETS_DIR_ENV);
        std::env::set_var(ASSETS_DIR_ENV, r"D:\somewhere\else");
        assert_eq!(assets_root(), PathBuf::from(r"D:\somewhere\else"));
        assert_eq!(
            instruments_root(),
            PathBuf::from(r"D:\somewhere\else").join("instruments")
        );
        match previous {
            Some(value) => std::env::set_var(ASSETS_DIR_ENV, value),
            None => std::env::remove_var(ASSETS_DIR_ENV),
        }
    }
}
