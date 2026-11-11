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
    /// The app version whose release notes were last shown (`None` = fresh install).
    pub seen_version: Option<String>,
    /// True when the user turned the automatic update check off (the default is on).
    pub no_auto_update: bool,
}

fn path() -> PathBuf {
    wave_engine::engine::scratch_dir()
        .parent()
        .unwrap()
        .join("settings.json")
}

impl Settings {
    pub fn load() -> Self {
        std::fs::read_to_string(path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updates_are_checked_by_default_and_old_files_still_load() {
        let s: Settings =
            serde_json::from_str(r#"{"folders":["a"],"lastProject":"x.wave"}"#).unwrap();
        assert!(!s.no_auto_update);
        assert_eq!(s.seen_version, None);
        assert_eq!(s.folders, ["a"]);
    }

    #[test]
    fn update_fields_round_trip_in_camel_case() {
        let s = Settings {
            seen_version: Some("0.9.3".into()),
            no_auto_update: true,
            ..Default::default()
        };
        let t = serde_json::to_string(&s).unwrap();
        assert!(
            t.contains("\"seenVersion\":\"0.9.3\"") && t.contains("\"noAutoUpdate\":true"),
            "{t}"
        );
        let back: Settings = serde_json::from_str(&t).unwrap();
        assert_eq!(back.seen_version.as_deref(), Some("0.9.3"));
        assert!(back.no_auto_update);
    }
}
