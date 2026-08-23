//! The one test that actually talks to the network.
//!
//! `src/download.rs` tests the whole state machine against an injected fetcher:
//! success, checksum mismatch, resume, atomicity, the background thread. All of
//! that runs in CI. What none of it can prove is that [`HttpFetcher`] works --
//! that the TLS stack is wired up, that the pinned URLs resolve, and that the
//! bytes GitHub serves today still hash to what `assets-manifest.json` recorded.
//!
//! So this fetches the smallest file of **each** instrument, verifies it, and
//! throws it away -- one file per pinned repository proves that repository is
//! still serving what the manifest recorded, and a few hundred kilobytes proves
//! it as well as a hundred megabytes would. `#[ignore]`d, so
//! `cargo test --workspace` never touches the network:
//!
//! ```sh
//! cargo test -p mydaw-plugin-sampler --test live_download -- --ignored --nocapture
//! ```
//!
//! A failure here means the supply chain is broken, not the sampler: either the
//! upstream repository moved, or the pinned commit was rewritten, or this
//! machine cannot reach it.

use std::io::Read;

use mydaw_plugin_sampler::download::{Fetcher, HttpFetcher};
use mydaw_plugin_sampler::AssetCatalog;
use sha2::{Digest, Sha256};

#[test]
#[ignore = "downloads a few megabytes from GitHub"]
fn the_pinned_urls_still_serve_the_bytes_their_checksums_promise() {
    let catalog = AssetCatalog::embedded().expect("the committed manifest must parse");

    // `new` rather than `with_verification`, so this test exercises exactly the
    // configuration the app ships -- including `MYDAW_TLS_NO_VERIFY` when the
    // person running it is behind a proxy the OS store does not know either.
    let fetcher = HttpFetcher::new();

    for instrument in catalog.entries() {
        let manifest = &instrument.manifest;
        eprintln!(
            "{}: {} @ {}",
            manifest.id, manifest.source.repo, manifest.source.commit
        );

        // The smallest file in this instrument: it proves the mechanism, and a
        // hundred and thirty megabytes would prove nothing more.
        let entry = manifest
            .samples
            .iter()
            .min_by_key(|entry| entry.bytes)
            .expect("the manifest has samples");
        eprintln!(
            "  fetching {} ({:.2} MB)\n    {}",
            entry.file,
            entry.bytes as f64 / 1e6,
            entry.url
        );

        let mut reader = fetcher
            .fetch(&entry.url)
            .unwrap_or_else(|err| panic!("the pinned URL must be reachable: {err}"));

        let mut hasher = Sha256::new();
        let mut buffer = vec![0_u8; 64 * 1024];
        let mut total = 0_u64;
        loop {
            let read = reader
                .read(&mut buffer)
                .expect("the transfer must not fail");
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            total += read as u64;
        }

        let actual: String = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        eprintln!("    {total} bytes, sha256 {actual}");

        assert_eq!(
            total, entry.bytes,
            "{} is {total} bytes upstream but {} in the manifest",
            entry.file, entry.bytes
        );
        assert_eq!(
            actual, entry.sha256,
            "{} does not hash to its pinned checksum; the upstream content changed \
             under a pinned commit, which should be impossible",
            entry.file
        );
    }
}
