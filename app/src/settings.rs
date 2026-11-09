//! Per-user settings that are not part of a song: audio device and library folders.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use wave_audio_io::AudioConfig;
use wave_midi_in::MidiDevice;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub audio: AudioConfig,
    /// The MIDI keyboard to reopen at startup (`None` = no device).
    pub midi_device: Option<MidiDevice>,
    pub folders: Vec<String>,
    pub last_project: Option<String>,
}

fn path() -> PathBuf {
    wave_engine::engine::scratch_dir().parent().unwrap().join("settings.json")
}

impl Settings {
    pub fn load() -> Self {
        std::fs::read_to_string(path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }
    pub fn save(&self) {
        let p = path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(t) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(p, t);
        }
    }
}
