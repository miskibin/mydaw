//! App-global preview level: how loud a browser audition is relative to master.
//!
//! Preview already sums into the master bus *before* the master fader, so this
//! is a linear fraction of whatever VOL is set to. 80% is quiet enough that a
//! 0 dBFS one-shot does not jump out of a mix, and loud enough to judge a hit.
//! Persists in `preview.json` next to `midi.json` / `library.json`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Schema version written into a fresh `preview.json`.
pub const PREVIEW_SETTINGS_VERSION: u32 = 1;

/// Default audition level, as a percent of master. 80% ≈ −1.9 dB.
pub const DEFAULT_VOLUME_PERCENT: f32 = 80.0;

/// Linear gain that matches [`DEFAULT_VOLUME_PERCENT`].
pub const DEFAULT_PREVIEW_GAIN: f32 = DEFAULT_VOLUME_PERCENT / 100.0;

/// Why `preview.json` could not be written.
#[derive(Debug, Error)]
pub enum PreviewStoreError {
    #[error("could not write {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// App-global preview preferences. Missing file = the defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSettings {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default = "default_volume")]
    pub volume_percent: f32,
}

fn default_version() -> u32 {
    PREVIEW_SETTINGS_VERSION
}

fn default_volume() -> f32 {
    DEFAULT_VOLUME_PERCENT
}

impl Default for PreviewSettings {
    fn default() -> Self {
        Self {
            version: PREVIEW_SETTINGS_VERSION,
            volume_percent: DEFAULT_VOLUME_PERCENT,
        }
    }
}

impl PreviewSettings {
    /// Clamp a UI percent into `0..=100`. Non-finite values become the default
    /// rather than silence, so a corrupt slider cannot mute audition forever.
    pub fn clamp_volume(percent: f32) -> f32 {
        if !percent.is_finite() {
            return DEFAULT_VOLUME_PERCENT;
        }
        percent.clamp(0.0, 100.0)
    }

    pub fn set_volume_percent(&mut self, percent: f32) {
        self.volume_percent = Self::clamp_volume(percent);
    }

    /// Linear multiplier applied on the preview voice, `0..=1`.
    pub fn linear_gain(&self) -> f32 {
        Self::clamp_volume(self.volume_percent) / 100.0
    }
}

/// Load preferences from `path`. A missing or unreadable file is the default.
pub fn load_settings(path: &Path) -> PreviewSettings {
    let Ok(bytes) = fs::read(path) else {
        return PreviewSettings::default();
    };
    let mut settings: PreviewSettings = serde_json::from_slice(&bytes).unwrap_or_default();
    settings.set_volume_percent(settings.volume_percent);
    settings
}

/// Write preferences to `path`, creating the parent directory if needed.
///
/// # Errors
/// [`PreviewStoreError::Io`] when the parent cannot be created or the file
/// cannot be written.
pub fn save_settings(path: &Path, settings: &PreviewSettings) -> Result<(), PreviewStoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| PreviewStoreError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let json = serde_json::to_vec_pretty(settings).expect("PreviewSettings is always serialisable");
    fs::write(path, json).map_err(|source| PreviewStoreError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_eighty_percent() {
        let settings = load_settings(Path::new("no-such-preview-9f2a.json"));
        assert_eq!(settings.volume_percent, 80.0);
        assert!((settings.linear_gain() - 0.8).abs() < 1e-6);
    }

    #[test]
    fn non_finite_and_out_of_range_percents_are_clamped() {
        assert_eq!(PreviewSettings::clamp_volume(f32::NAN), 80.0);
        assert_eq!(PreviewSettings::clamp_volume(-12.0), 0.0);
        assert_eq!(PreviewSettings::clamp_volume(150.0), 100.0);
        let mut settings = PreviewSettings::default();
        settings.set_volume_percent(33.0);
        assert!((settings.linear_gain() - 0.33).abs() < 1e-6);
    }

    #[test]
    fn a_round_trip_keeps_the_percent() {
        let dir =
            std::env::temp_dir().join(format!("mydaw-preview-settings-{}", std::process::id()));
        let path = dir.join("preview.json");
        let mut settings = PreviewSettings::default();
        settings.set_volume_percent(25.0);
        save_settings(&path, &settings).expect("saves");
        let loaded = load_settings(&path);
        assert_eq!(loaded.volume_percent, 25.0);
        let _ = fs::remove_dir_all(&dir);
    }
}
