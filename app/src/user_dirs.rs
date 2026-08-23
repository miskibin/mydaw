//! Where WAVE writes the audio it generates.

use std::path::PathBuf;

/// The root for generated audio: destructive sample edits
/// ([`crate::sample_edit`]) and pattern renders ([`crate::resample`]).
///
/// One root for both so a user who wants to clear the cache has one folder to
/// delete, and so a project that references a generated file finds it in the
/// same place on the next run.
pub fn sample_cache_dir() -> PathBuf {
    let root = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_CACHE_HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    root.join("WAVE Studio")
}
