//! Installing the sample content from inside the app.
//!
//! `scripts/fetch-assets.ps1` does this from a shell, which is what CI and an
//! offline install want. This module does the same thing from the running
//! application, because "clone the repo, then remember to run a PowerShell
//! script, then restart" is not an install experience.
//!
//! Both read the **same file**: `assets-manifest.json` at the workspace root,
//! which lists every sample with its URL, its SHA-256 and its size. The script
//! reads it from disk; this module has it [`include_str!`]d, so the binary and
//! the script can never disagree about what "installed" means and there is no
//! path to resolve at runtime.
//!
//! ```text
//!   assets-manifest.json ──┬── scripts/fetch-assets.ps1  (shell, CI, offline)
//!                          └── AssetDownloader           (in-app, on click)
//!                                    │
//!                          GET url ──> <file>.part ──sha256──> rename <file>
//!                                                                  │
//!                                          manifest.json <─────────┘
//!                                                 │
//!                                        SamplerRegistry::rescan
//! ```
//!
//! # Consent
//! Nothing downloads on its own. A fresh install starts [`AssetState::Idle`]
//! with `installed: false`; the shell offers a button, and pressing it is the
//! consent. A DAW that pulled 138 MB on first launch because it felt like it
//! would be doing something the user did not ask for on a connection it knows
//! nothing about.
//!
//! # Atomicity
//! Every file lands as `<name>.part`, is hashed as it is written, and is
//! renamed onto its final name **only after** the hash matches. A download that
//! is interrupted -- by a crash, a network drop, or the user quitting -- leaves
//! `.part` files that the next run overwrites, and never a truncated `.flac`
//! that the loader would decode as a shorter note. A file whose hash does not
//! match is deleted rather than kept.
//!
//! # Idempotence
//! A file already on disk at its manifest size is left alone. The full hash is
//! checked when a file is *fetched*, not on every status query: re-hashing
//! 138 MB to answer "is it installed?" would make the status command cost half
//! a second, and the atomic rename above is what makes the size check
//! sufficient -- a file with the right name and size got there by passing its
//! hash.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The manifest both installers read, embedded at compile time.
///
/// `include_str!` rather than a runtime path: the script's copy and the app's
/// copy are then the same bytes by construction, and a missing file is a build
/// error rather than a puzzling runtime one.
pub const ASSET_MANIFEST_JSON: &str = include_str!("../../../assets-manifest.json");

/// The name of that file in the repository, for error messages and the script.
pub const ASSET_MANIFEST_FILE: &str = "assets-manifest.json";

/// How long a single file transfer may take before it is abandoned.
///
/// Per request, not for the whole install: a 3 MB FLAC on a slow connection is
/// normal, a 3 MB FLAC that takes two minutes is a stall.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Bytes read from the network per write. 64 KB is large enough that the
/// syscall overhead vanishes and small enough that progress updates stay smooth.
const CHUNK: usize = 64 * 1024;

/// Where one sample comes from and what it must hash to.
///
/// Deserialised from the same objects [`SampleEntry`](crate::SampleEntry) reads
/// -- each ignores the other's fields, which is what lets one file serve both.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct AssetFile {
    /// File name, relative to the instrument's directory.
    pub file: String,
    /// Where to fetch it. Pinned to a commit, so the bytes cannot change under
    /// the checksum.
    pub url: String,
    /// Lowercase hex SHA-256 of the file's contents.
    pub sha256: String,
    /// Size in bytes, used for progress and for the cheap "already there" check.
    pub bytes: u64,
}

/// Where the content came from and under what licence.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub struct AssetSource {
    #[serde(default)]
    pub repo: String,
    /// The commit every URL is pinned to.
    #[serde(default)]
    pub commit: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub license_url: String,
    #[serde(default)]
    pub author: String,
}

/// The download side of `assets-manifest.json`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AssetManifest {
    /// The instrument id, and the directory the files land in.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub attribution: String,
    #[serde(default)]
    pub source: AssetSource,
    pub samples: Vec<AssetFile>,
}

impl AssetManifest {
    /// The manifest this build was compiled with.
    ///
    /// # Errors
    /// [`DownloadError::Manifest`] if the committed file is not valid --
    /// which is a repository error, caught by this crate's own tests.
    pub fn embedded() -> Result<Self, DownloadError> {
        Self::parse(ASSET_MANIFEST_JSON.as_bytes())
    }

    /// Parse a manifest from JSON.
    ///
    /// # Errors
    /// [`DownloadError::Manifest`] for malformed JSON, an empty file list, or
    /// an entry whose name, URL or checksum cannot be used.
    pub fn parse(bytes: &[u8]) -> Result<Self, DownloadError> {
        let manifest: Self = serde_json::from_slice(bytes)
            .map_err(|err| DownloadError::Manifest(err.to_string()))?;
        if manifest.id.trim().is_empty() {
            return Err(DownloadError::Manifest("'id' is empty".to_owned()));
        }
        if manifest.samples.is_empty() {
            return Err(DownloadError::Manifest(
                "'samples' is empty; there is nothing to install".to_owned(),
            ));
        }
        for entry in &manifest.samples {
            // The same containment rule the instrument manifest enforces: a
            // downloaded list must not choose where this process writes.
            if Path::new(&entry.file).components().count() != 1 || entry.file.contains(['/', '\\'])
            {
                return Err(DownloadError::Manifest(format!(
                    "'{}' is not a plain file name",
                    entry.file
                )));
            }
            if !entry.url.starts_with("https://") {
                return Err(DownloadError::Manifest(format!(
                    "the URL for '{}' is not https",
                    entry.file
                )));
            }
            if entry.sha256.len() != 64 || !entry.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(DownloadError::Manifest(format!(
                    "the checksum for '{}' is not a SHA-256 digest",
                    entry.file
                )));
            }
        }
        Ok(manifest)
    }

    /// Total size of the install, in bytes.
    pub fn total_bytes(&self) -> u64 {
        self.samples.iter().map(|entry| entry.bytes).sum()
    }
}

/// What the downloader is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AssetState {
    /// Nothing is happening. Either nothing is installed, or everything is and
    /// [`AssetStatus::installed`] says so.
    Idle,
    /// Transferring.
    Downloading,
    /// Checking a file that is already on disk, or hashing one just written.
    Verifying,
    /// Everything is installed and the instrument manifest has been written.
    Done,
    /// The last attempt failed; [`AssetStatus::error`] says why.
    Error,
}

/// What the shell shows.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetStatus {
    /// Whether every file the manifest names is present at its stated size.
    pub installed: bool,
    /// Whether the instrument is **loaded and playable**, which is a later fact
    /// than [`installed`](Self::installed): decoding 222 MB of piano takes
    /// seconds and happens on a background thread, so there is a window in
    /// which every file is on disk and the source picker still has nothing to
    /// offer. Set by the shell, which is the only layer that can see both the
    /// downloader and the registry; the downloader itself always reports
    /// `false`.
    pub instrument_ready: bool,
    pub state: AssetState,
    pub total_bytes: u64,
    pub downloaded_bytes: u64,
    /// Files finished, of [`file_count`](Self::file_count).
    pub files_done: usize,
    pub file_count: usize,
    /// The instrument this installs, for the button's label.
    pub instrument_id: String,
    pub instrument_name: String,
    /// Empty unless [`state`](Self::state) is [`AssetState::Error`].
    pub error: String,
}

impl AssetStatus {
    /// Progress in `0.0..=1.0`.
    pub fn fraction(&self) -> f64 {
        if self.total_bytes == 0 {
            return 0.0;
        }
        (self.downloaded_bytes as f64 / self.total_bytes as f64).clamp(0.0, 1.0)
    }
}

/// Why an install failed.
#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("the asset manifest is not usable: {0}")]
    Manifest(String),
    #[error("a download is already running")]
    AlreadyRunning,
    #[error("could not create {path}: {source}")]
    Create {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("fetching {url} failed: {reason}")]
    Http { url: String, reason: String },
    /// The bytes that arrived are not the bytes that were pinned. The file is
    /// discarded; nothing partially-correct is ever left where the loader would
    /// find it.
    #[error(
        "{file} does not match its pinned checksum \
         (expected {expected}, got {actual}); the download was discarded"
    )]
    Checksum {
        file: String,
        expected: String,
        actual: String,
    },
}

/// Somewhere to fetch bytes from. The seam that makes this testable.
///
/// The real implementation is HTTPS via `ureq`; the tests hand it a reader over
/// a local file, which is what lets the checksum-mismatch path and the whole
/// state machine be exercised **without a live download in CI**.
pub trait Fetcher: Send + Sync {
    /// Open a stream of `url`'s bytes.
    ///
    /// # Errors
    /// [`DownloadError::Http`], naming the URL.
    fn fetch(&self, url: &str) -> Result<Box<dyn Read + Send>, DownloadError>;
}

/// Fetches over HTTPS.
#[derive(Debug, Default)]
pub struct HttpFetcher;

impl Fetcher for HttpFetcher {
    fn fetch(&self, url: &str) -> Result<Box<dyn Read + Send>, DownloadError> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
            .build()
            .into();
        let response = agent.get(url).call().map_err(|err| DownloadError::Http {
            url: url.to_owned(),
            reason: err.to_string(),
        })?;
        Ok(Box::new(response.into_body().into_reader()))
    }
}

/// Shared, atomically-readable progress. Split out so the worker thread writes
/// it and a Tauri command thread reads it without either waiting on the other.
#[derive(Debug)]
struct Progress {
    running: AtomicBool,
    /// [`AssetState`] as a `u8`, so a status read is three atomic loads rather
    /// than a lock a download thread might be holding across a network write.
    state: AtomicUsize,
    downloaded: AtomicU64,
    files_done: AtomicUsize,
    error: parking_lot::Mutex<String>,
}

const STATE_IDLE: usize = 0;
const STATE_DOWNLOADING: usize = 1;
const STATE_VERIFYING: usize = 2;
const STATE_DONE: usize = 3;
const STATE_ERROR: usize = 4;

fn state_of(raw: usize) -> AssetState {
    match raw {
        STATE_DOWNLOADING => AssetState::Downloading,
        STATE_VERIFYING => AssetState::Verifying,
        STATE_DONE => AssetState::Done,
        STATE_ERROR => AssetState::Error,
        _ => AssetState::Idle,
    }
}

/// Installs the sample content, on demand.
///
/// `Send + Sync`, so a Tauri command reaches it directly; the transfer runs on
/// its own thread and reports through atomics.
pub struct AssetDownloader {
    manifest: AssetManifest,
    /// `<assets>/instruments`, the directory instrument directories live in.
    instruments_root: PathBuf,
    /// The exact bytes to write as the instrument's `manifest.json`.
    manifest_json: String,
    fetcher: Arc<dyn Fetcher>,
    progress: Arc<Progress>,
}

impl AssetDownloader {
    /// A downloader for the embedded manifest, installing under
    /// `instruments_root`.
    ///
    /// # Errors
    /// [`DownloadError::Manifest`] if this build's committed manifest is not
    /// usable.
    pub fn new(instruments_root: PathBuf) -> Result<Self, DownloadError> {
        Ok(Self::with_parts(
            AssetManifest::embedded()?,
            ASSET_MANIFEST_JSON.to_owned(),
            instruments_root,
            Arc::new(HttpFetcher),
        ))
    }

    /// A downloader over an explicit manifest and fetcher. The tests' entry
    /// point, and the only way to install without a network.
    pub fn with_parts(
        manifest: AssetManifest,
        manifest_json: String,
        instruments_root: PathBuf,
        fetcher: Arc<dyn Fetcher>,
    ) -> Self {
        Self {
            manifest,
            instruments_root,
            manifest_json,
            fetcher,
            progress: Arc::new(Progress {
                running: AtomicBool::new(false),
                state: AtomicUsize::new(STATE_IDLE),
                downloaded: AtomicU64::new(0),
                files_done: AtomicUsize::new(0),
                error: parking_lot::Mutex::new(String::new()),
            }),
        }
    }

    /// What is being installed.
    pub fn manifest(&self) -> &AssetManifest {
        &self.manifest
    }

    /// The directory this instrument installs into.
    pub fn install_dir(&self) -> PathBuf {
        self.instruments_root.join(&self.manifest.id)
    }

    /// Whether every file is present at its stated size. Cheap: metadata only.
    /// See the module docs on why the size is enough.
    pub fn is_installed(&self) -> bool {
        let dir = self.install_dir();
        if !dir.join(crate::manifest::MANIFEST_FILE_NAME).is_file() {
            return false;
        }
        self.manifest
            .samples
            .iter()
            .all(|entry| present(&dir.join(&entry.file), entry.bytes))
    }

    /// Bytes already on disk, for a progress bar that starts where it left off.
    pub fn installed_bytes(&self) -> u64 {
        let dir = self.install_dir();
        self.manifest
            .samples
            .iter()
            .filter(|entry| present(&dir.join(&entry.file), entry.bytes))
            .map(|entry| entry.bytes)
            .sum()
    }

    /// What the shell shows.
    pub fn status(&self) -> AssetStatus {
        let state = state_of(self.progress.state.load(Ordering::Acquire));
        let running = self.progress.running.load(Ordering::Acquire);
        // While idle the progress counters are stale; report what is actually
        // on disk instead, so a restart shows a part-finished install honestly.
        let downloaded = if running {
            self.progress.downloaded.load(Ordering::Acquire)
        } else {
            self.installed_bytes()
        };
        AssetStatus {
            installed: self.is_installed(),
            // Always false here: only the shell can see the registry. See the
            // field's own documentation.
            instrument_ready: false,
            state,
            total_bytes: self.manifest.total_bytes(),
            downloaded_bytes: downloaded,
            files_done: self.progress.files_done.load(Ordering::Acquire),
            file_count: self.manifest.samples.len(),
            instrument_id: self.manifest.id.clone(),
            instrument_name: self.manifest.name.clone(),
            error: self.progress.error.lock().clone(),
        }
    }

    /// Whether a transfer is in flight.
    pub fn is_running(&self) -> bool {
        self.progress.running.load(Ordering::Acquire)
    }

    /// Install, on this thread. Blocking; minutes.
    ///
    /// `on_progress` is called after every chunk, which is what drives the
    /// percentage. Used directly by the tests; the app calls
    /// [`start`](Self::start).
    ///
    /// # Errors
    /// The first [`DownloadError`] that stops the install. Files already
    /// verified stay on disk, so a retry resumes rather than restarting.
    pub fn install(&self) -> Result<(), DownloadError> {
        let dir = self.install_dir();
        std::fs::create_dir_all(&dir).map_err(|source| DownloadError::Create {
            path: dir.clone(),
            source,
        })?;

        self.progress.downloaded.store(0, Ordering::Release);
        self.progress.files_done.store(0, Ordering::Release);

        for entry in &self.manifest.samples {
            let target = dir.join(&entry.file);
            if present(&target, entry.bytes) {
                // Idempotent: already installed, and the atomic rename is what
                // makes that trustworthy.
                self.progress
                    .downloaded
                    .fetch_add(entry.bytes, Ordering::AcqRel);
                self.progress.files_done.fetch_add(1, Ordering::AcqRel);
                continue;
            }
            self.progress
                .state
                .store(STATE_DOWNLOADING, Ordering::Release);
            self.fetch_one(entry, &target)?;
            self.progress.files_done.fetch_add(1, Ordering::AcqRel);
        }

        // Last, so that a manifest on disk means the samples beside it are
        // complete -- which is exactly what `is_installed` relies on.
        self.progress
            .state
            .store(STATE_VERIFYING, Ordering::Release);
        let manifest_path = dir.join(crate::manifest::MANIFEST_FILE_NAME);
        write_atomic(&manifest_path, self.manifest_json.as_bytes())?;

        self.progress.state.store(STATE_DONE, Ordering::Release);
        self.progress.error.lock().clear();
        Ok(())
    }

    /// Fetch one file into `target`, via `<target>.part`.
    fn fetch_one(&self, entry: &AssetFile, target: &Path) -> Result<(), DownloadError> {
        let part = target.with_extension("part");
        let mut reader = self.fetcher.fetch(&entry.url)?;
        let mut file = std::fs::File::create(&part).map_err(|source| DownloadError::Create {
            path: part.clone(),
            source,
        })?;

        let mut hasher = Sha256::new();
        let mut buffer = vec![0_u8; CHUNK];
        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|err| DownloadError::Http {
                    url: entry.url.clone(),
                    reason: err.to_string(),
                })?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            file.write_all(&buffer[..read])
                .map_err(|source| DownloadError::Write {
                    path: part.clone(),
                    source,
                })?;
            self.progress
                .downloaded
                .fetch_add(read as u64, Ordering::AcqRel);
        }
        file.flush().map_err(|source| DownloadError::Write {
            path: part.clone(),
            source,
        })?;
        drop(file);

        let actual = hex(&hasher.finalize());
        if !actual.eq_ignore_ascii_case(&entry.sha256) {
            // Discarded, not kept: a file that is nearly right is worse than
            // one that is missing, because the loader would decode it.
            let _ = std::fs::remove_file(&part);
            return Err(DownloadError::Checksum {
                file: entry.file.clone(),
                expected: entry.sha256.clone(),
                actual,
            });
        }

        std::fs::rename(&part, target).map_err(|source| DownloadError::Write {
            path: target.to_path_buf(),
            source,
        })?;
        Ok(())
    }

    /// Install on a background thread, calling `on_finish` with the result.
    ///
    /// `on_finish` is where the shell asks the registry to rescan, which is
    /// what makes the piano appear in the source picker with no restart.
    ///
    /// # Errors
    /// [`DownloadError::AlreadyRunning`] if one is already in flight.
    pub fn start(
        self: &Arc<Self>,
        on_finish: impl FnOnce(Result<(), DownloadError>) + Send + 'static,
    ) -> Result<(), DownloadError> {
        if self
            .progress
            .running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(DownloadError::AlreadyRunning);
        }
        self.progress
            .state
            .store(STATE_DOWNLOADING, Ordering::Release);
        self.progress.error.lock().clear();

        let downloader = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("mydaw-sampler-fetch".to_owned())
            .spawn(move || {
                let result = downloader.install();
                if let Err(err) = &result {
                    log::warn!("sampler: asset install failed: {err}");
                    *downloader.progress.error.lock() = err.to_string();
                    downloader
                        .progress
                        .state
                        .store(STATE_ERROR, Ordering::Release);
                }
                downloader.progress.running.store(false, Ordering::Release);
                on_finish(result);
            });

        if let Err(err) = spawned {
            self.progress.running.store(false, Ordering::Release);
            self.progress.state.store(STATE_ERROR, Ordering::Release);
            let message = format!("could not start the download thread: {err}");
            *self.progress.error.lock() = message.clone();
            return Err(DownloadError::Manifest(message));
        }
        Ok(())
    }
}

impl std::fmt::Debug for AssetDownloader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssetDownloader")
            .field("instrument", &self.manifest.id)
            .field("files", &self.manifest.samples.len())
            .field("dir", &self.install_dir())
            .field("running", &self.is_running())
            .finish()
    }
}

/// Whether `path` exists at exactly `bytes` bytes.
fn present(path: &Path, bytes: u64) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() == bytes)
}

/// Write `bytes` to `path` via a temporary file and a rename.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), DownloadError> {
    let part = path.with_extension("part");
    std::fs::write(&part, bytes).map_err(|source| DownloadError::Write {
        path: part.clone(),
        source,
    })?;
    std::fs::rename(&part, path).map_err(|source| DownloadError::Write {
        path: path.to_path_buf(),
        source,
    })
}

/// Lowercase hex.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    /// Serves bytes from a map. No network, so this runs in CI.
    struct MapFetcher(std::collections::HashMap<String, Vec<u8>>);

    impl Fetcher for MapFetcher {
        fn fetch(&self, url: &str) -> Result<Box<dyn Read + Send>, DownloadError> {
            self.0
                .get(url)
                .map(|bytes| Box::new(std::io::Cursor::new(bytes.clone())) as Box<dyn Read + Send>)
                .ok_or_else(|| DownloadError::Http {
                    url: url.to_owned(),
                    reason: "404".to_owned(),
                })
        }
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        hex(&Sha256::digest(bytes))
    }

    /// A two-file instrument, and a fetcher that serves it.
    fn fixture(corrupt: bool) -> (AssetManifest, String, Arc<dyn Fetcher>) {
        let first = b"first sample bytes".to_vec();
        let second = b"second sample bytes".to_vec();
        let manifest = AssetManifest {
            id: "test-piano".to_owned(),
            name: "Test Piano".to_owned(),
            attribution: "Nobody, public domain".to_owned(),
            source: AssetSource::default(),
            samples: vec![
                AssetFile {
                    file: "a.wav".to_owned(),
                    url: "https://example.invalid/a.wav".to_owned(),
                    sha256: sha256_hex(&first),
                    bytes: first.len() as u64,
                },
                AssetFile {
                    file: "b.wav".to_owned(),
                    url: "https://example.invalid/b.wav".to_owned(),
                    sha256: if corrupt {
                        // A valid-looking digest of something else entirely.
                        sha256_hex(b"not what will arrive")
                    } else {
                        sha256_hex(&second)
                    },
                    bytes: second.len() as u64,
                },
            ],
        };
        let json = serde_json::to_string(&manifest).unwrap();
        let map = [
            ("https://example.invalid/a.wav".to_owned(), first),
            ("https://example.invalid/b.wav".to_owned(), second),
        ]
        .into_iter()
        .collect();
        (manifest, json, Arc::new(MapFetcher(map)))
    }

    fn downloader(temp: &TempDir, corrupt: bool) -> AssetDownloader {
        let (manifest, json, fetcher) = fixture(corrupt);
        AssetDownloader::with_parts(manifest, json, temp.path().join("instruments"), fetcher)
    }

    /// The manifest this build ships must be usable, or nothing else matters.
    #[test]
    fn the_committed_asset_manifest_is_valid() {
        let manifest = AssetManifest::embedded().expect("assets-manifest.json must parse");
        assert!(!manifest.id.trim().is_empty());
        assert!(!manifest.samples.is_empty());
        assert!(
            !manifest.attribution.trim().is_empty(),
            "CC-BY content must carry its attribution"
        );
        assert!(
            manifest.source.commit.len() >= 7,
            "the source must be pinned to a commit, got '{}'",
            manifest.source.commit
        );
        assert!(!manifest.source.license.trim().is_empty());
    }

    /// The same file must parse as an instrument manifest too -- that is the
    /// whole reason it is copied verbatim rather than transformed.
    #[test]
    fn the_committed_manifest_is_also_a_valid_instrument_manifest() {
        let instrument = crate::manifest::InstrumentManifest::parse(
            ASSET_MANIFEST_JSON.as_bytes(),
            Path::new(ASSET_MANIFEST_FILE),
        )
        .expect("the download manifest must also be a runtime manifest");
        let download = AssetManifest::embedded().unwrap();
        assert_eq!(instrument.id, download.id);
        assert_eq!(instrument.samples.len(), download.samples.len());
        assert!(
            instrument.max_seconds.is_some(),
            "the shipped piano must declare a memory budget"
        );
    }

    #[test]
    fn a_manifest_that_names_a_path_or_a_bad_digest_is_refused() {
        let cases = [
            (r#""file":"../escape.wav""#, "plain file name"),
            (r#""file":"sub/a.wav""#, "plain file name"),
            (r#""url":"http://example.invalid/a.wav""#, "not https"),
            (r#""sha256":"abcd""#, "SHA-256"),
            (
                r#""sha256":"zz00112233445566778899aabbccddeeff00112233445566778899aabbccddee""#,
                "SHA-256",
            ),
        ];
        for (patch, needle) in cases {
            let key = patch.split(':').next().unwrap();
            let mut value: serde_json::Value = serde_json::from_str(&fixture(false).1).unwrap();
            let patched: serde_json::Value = serde_json::from_str(&format!("{{{patch}}}")).unwrap();
            let field = key.trim_matches('"');
            value["samples"][0][field] = patched[field].clone();
            let err = AssetManifest::parse(value.to_string().as_bytes()).unwrap_err();
            assert!(err.to_string().contains(needle), "{patch}: {err}");
        }
    }

    #[test]
    fn an_install_writes_every_file_and_the_instrument_manifest() {
        let temp = TempDir::new("install");
        let downloader = downloader(&temp, false);
        assert!(!downloader.is_installed());
        assert_eq!(downloader.status().state, AssetState::Idle);
        assert_eq!(downloader.status().downloaded_bytes, 0);

        downloader.install().expect("installs");

        let dir = downloader.install_dir();
        assert_eq!(
            std::fs::read(dir.join("a.wav")).unwrap(),
            b"first sample bytes"
        );
        assert_eq!(
            std::fs::read(dir.join("b.wav")).unwrap(),
            b"second sample bytes"
        );
        assert!(dir.join(crate::manifest::MANIFEST_FILE_NAME).is_file());
        assert!(downloader.is_installed());

        let status = downloader.status();
        assert_eq!(status.state, AssetState::Done);
        assert!(status.installed);
        assert_eq!(status.files_done, 2);
        assert_eq!(status.file_count, 2);
        assert_eq!(status.downloaded_bytes, status.total_bytes);
        assert!((status.fraction() - 1.0).abs() < 1e-9);
        assert!(status.error.is_empty());

        // No `.part` file survives a completed install.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "part"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// The requirement that matters most: bad bytes never reach the loader.
    #[test]
    fn a_checksum_mismatch_is_a_typed_error_and_the_file_is_discarded() {
        let temp = TempDir::new("checksum");
        let downloader = downloader(&temp, true);

        let err = downloader.install().unwrap_err();
        let DownloadError::Checksum {
            ref file,
            ref expected,
            ref actual,
        } = err
        else {
            panic!("expected a checksum error, got {err}");
        };
        assert_eq!(file, "b.wav");
        assert_ne!(expected, actual);
        assert!(err.to_string().contains("discarded"), "{err}");

        let dir = downloader.install_dir();
        assert!(
            !dir.join("b.wav").exists(),
            "a file that failed its checksum must not be left on disk"
        );
        assert!(
            !dir.join("b.part").exists(),
            "nor may the partial file survive"
        );
        assert!(
            dir.join("a.wav").exists(),
            "the file that did verify is kept, so a retry resumes"
        );
        assert!(
            !dir.join(crate::manifest::MANIFEST_FILE_NAME).exists(),
            "a failed install must not look installed"
        );
        assert!(!downloader.is_installed());
    }

    #[test]
    fn a_second_install_skips_what_is_already_there() {
        let temp = TempDir::new("idempotent");
        let downloader = downloader(&temp, false);
        downloader.install().expect("first install");

        // A fetcher that serves nothing: if the second run tried to download,
        // it would fail.
        let (manifest, json, _) = fixture(false);
        let barren = AssetDownloader::with_parts(
            manifest,
            json,
            temp.path().join("instruments"),
            Arc::new(MapFetcher(std::collections::HashMap::new())),
        );
        assert!(barren.is_installed());
        barren
            .install()
            .expect("an install with everything present must not fetch");
        assert_eq!(barren.status().files_done, 2);
    }

    /// A truncated file from an interrupted run is replaced, not trusted.
    #[test]
    fn a_file_of_the_wrong_size_is_fetched_again() {
        let temp = TempDir::new("truncated");
        let downloader = downloader(&temp, false);
        let dir = downloader.install_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.wav"), b"trunc").unwrap();

        downloader.install().expect("installs over the stub");
        assert_eq!(
            std::fs::read(dir.join("a.wav")).unwrap(),
            b"first sample bytes"
        );
    }

    #[test]
    fn a_background_install_reports_running_then_done_and_calls_back() {
        let temp = TempDir::new("background");
        let downloader = Arc::new(downloader(&temp, false));
        let (tx, rx) = std::sync::mpsc::channel();

        downloader
            .start(move |result| tx.send(result.is_ok()).unwrap())
            .expect("starts");
        let ok = rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the install must finish");
        assert!(ok);

        assert!(!downloader.is_running());
        assert_eq!(downloader.status().state, AssetState::Done);
        assert!(downloader.status().installed);
    }

    #[test]
    fn a_failed_background_install_lands_in_the_error_state_with_a_reason() {
        let temp = TempDir::new("background-error");
        let downloader = Arc::new(downloader(&temp, true));
        let (tx, rx) = std::sync::mpsc::channel();

        downloader
            .start(move |result| tx.send(result.is_ok()).unwrap())
            .unwrap();
        let ok = rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the install must finish");
        assert!(!ok);

        let status = downloader.status();
        assert_eq!(status.state, AssetState::Error);
        assert!(!status.installed);
        assert!(status.error.contains("checksum"), "{}", status.error);
    }

    #[test]
    fn hex_is_lowercase_and_the_right_length() {
        assert_eq!(hex(&[0x00, 0x0f, 0xff]), "000fff");
        assert_eq!(sha256_hex(b"").len(), 64);
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
