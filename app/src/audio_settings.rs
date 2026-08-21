//! App-global audio output: WASAPI vs ASIO, and the buffer we request.
//!
//! Persists in `audio.json` next to `midi.json` / `preview.json`. Missing file
//! = WASAPI, device-default buffer. That is the previous behaviour, so an
//! upgrade does not change what people hear — only the status bar starts
//! telling the truth about the callback that was already running.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use mydaw_plugin_audio_io::OutputRequest;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Schema version written into a fresh `audio.json`.
pub const AUDIO_SETTINGS_VERSION: u32 = 1;

/// Buffer sizes Settings offers, plus `0` for the device default.
pub const BUFFER_CHOICES: &[u32] = &[0, 128, 256, 512, 1024, 2048];

/// Why `audio.json` could not be written.
#[derive(Debug, Error)]
pub enum AudioStoreError {
    #[error("could not write {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// App-global output preferences. Missing file = the defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioSettings {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub prefer_asio: bool,
    /// Frames to request. `0` = host default.
    #[serde(default)]
    pub buffer_frames: u32,
}

fn default_version() -> u32 {
    AUDIO_SETTINGS_VERSION
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            version: AUDIO_SETTINGS_VERSION,
            prefer_asio: false,
            buffer_frames: 0,
        }
    }
}

impl AudioSettings {
    /// Snap a UI value onto a legal buffer. Non-finite / unknown numbers
    /// become the device default rather than a size the host will refuse.
    pub fn snap_buffer_frames(frames: u32) -> u32 {
        if frames == 0 {
            return 0;
        }
        BUFFER_CHOICES
            .iter()
            .copied()
            .filter(|choice| *choice > 0)
            .min_by_key(|choice| choice.abs_diff(frames))
            .unwrap_or(0)
    }

    pub fn set_buffer_frames(&mut self, frames: u32) {
        self.buffer_frames = Self::snap_buffer_frames(frames);
    }

    pub fn to_request(&self) -> OutputRequest {
        OutputRequest {
            prefer_asio: self.prefer_asio,
            buffer_frames: if self.buffer_frames == 0 {
                None
            } else {
                Some(self.buffer_frames)
            },
        }
    }
}

/// Config directory matching Tauri's `app_config_dir` for `com.mydaw.app`.
pub fn store_dir() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("com.mydaw.app")
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(|home| {
                PathBuf::from(home)
                    .join("Library")
                    .join("Application Support")
            })
            .unwrap_or_else(std::env::temp_dir)
            .join("com.mydaw.app")
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .unwrap_or_else(std::env::temp_dir)
            .join("com.mydaw.app")
    }
}

/// `audio.json` next to `preview.json`.
pub fn store_path() -> PathBuf {
    store_dir().join("audio.json")
}

/// Load preferences from `path`. A missing or unreadable file is the default.
pub fn load_settings(path: &Path) -> AudioSettings {
    let Ok(bytes) = fs::read(path) else {
        return AudioSettings::default();
    };
    let mut settings: AudioSettings = serde_json::from_slice(&bytes).unwrap_or_default();
    settings.version = AUDIO_SETTINGS_VERSION;
    settings.set_buffer_frames(settings.buffer_frames);
    settings
}

/// Write preferences to `path`, creating the parent directory if needed.
pub fn save_settings(path: &Path, settings: &AudioSettings) -> Result<(), AudioStoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| AudioStoreError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let json = serde_json::to_vec_pretty(settings).expect("AudioSettings is always serialisable");
    fs::write(path, json).map_err(|source| AudioStoreError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_wasapi_with_the_device_buffer() {
        let settings = load_settings(Path::new("no-such-audio-9f2a.json"));
        assert!(!settings.prefer_asio);
        assert_eq!(settings.buffer_frames, 0);
        assert_eq!(settings.to_request(), OutputRequest::default());
    }

    #[test]
    fn buffer_sizes_snap_to_the_choices_settings_shows() {
        assert_eq!(AudioSettings::snap_buffer_frames(0), 0);
        assert_eq!(AudioSettings::snap_buffer_frames(256), 256);
        assert_eq!(AudioSettings::snap_buffer_frames(300), 256);
        assert_eq!(AudioSettings::snap_buffer_frames(400), 512);
        assert_eq!(AudioSettings::snap_buffer_frames(9_000), 2048);
    }

    #[test]
    fn a_round_trip_keeps_asio_and_the_buffer() {
        let dir = std::env::temp_dir().join(format!("mydaw-audio-settings-{}", std::process::id()));
        let path = dir.join("audio.json");
        let mut settings = AudioSettings {
            prefer_asio: true,
            ..AudioSettings::default()
        };
        settings.set_buffer_frames(512);
        save_settings(&path, &settings).expect("saves");
        let loaded = load_settings(&path);
        assert!(loaded.prefer_asio);
        assert_eq!(loaded.buffer_frames, 512);
        let _ = fs::remove_dir_all(&dir);
    }
}
