//! Port identity and the `midi.json` pairing file.
//!
//! WinMM identifies ports by name. USB devices without an `iSerialNumber` get
//! a new OS instance id when they move USB ports, so we never persist that id.
//! Matching is exact first, then case-insensitive; substring matching would
//! pair the wrong device when two names share a prefix.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Schema version written into a fresh `midi.json`.
pub const MIDI_SETTINGS_VERSION: u32 = 1;

/// What a paired port is willing to feed the engine. v1 only understands
/// `"notes"`; `"remote"` is reserved for MIDI Learn.
pub const ACCEPTS_NOTES: &str = "notes";

/// One row in `midi.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MidiDeviceSettings {
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_accepts")]
    pub accepts: Vec<String>,
}

fn default_accepts() -> Vec<String> {
    vec![ACCEPTS_NOTES.to_owned()]
}

impl MidiDeviceSettings {
    pub fn paired_notes(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            enabled: true,
            accepts: default_accepts(),
        }
    }

    pub fn accepts_notes(&self) -> bool {
        self.accepts.iter().any(|item| item == ACCEPTS_NOTES)
    }
}

/// App-global MIDI pairing. Missing file = empty list, same as library.json.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MidiSettings {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub devices: Vec<MidiDeviceSettings>,
}

fn default_version() -> u32 {
    MIDI_SETTINGS_VERSION
}

impl Default for MidiSettings {
    fn default() -> Self {
        Self {
            version: MIDI_SETTINGS_VERSION,
            devices: Vec::new(),
        }
    }
}

impl MidiSettings {
    pub fn device(&self, name: &str) -> Option<&MidiDeviceSettings> {
        let index = match_port_name(name, self.devices.iter().map(|d| d.name.as_str()))?;
        self.devices.get(index)
    }

    pub fn device_mut(&mut self, name: &str) -> Option<&mut MidiDeviceSettings> {
        let index = match_port_name(name, self.devices.iter().map(|d| d.name.as_str()))?;
        self.devices.get_mut(index)
    }

    /// Enable or disable `name`, inserting a new row when it has never been
    /// paired. Returns the device after the edit.
    pub fn set_enabled(&mut self, name: &str, enabled: bool) -> &MidiDeviceSettings {
        if let Some(index) = match_port_name(name, self.devices.iter().map(|d| d.name.as_str())) {
            self.devices[index].enabled = enabled;
            return &self.devices[index];
        }
        self.devices.push(MidiDeviceSettings {
            name: name.to_owned(),
            enabled,
            accepts: default_accepts(),
        });
        self.devices.last().expect("just pushed")
    }
}

/// Why `midi.json` could not be written.
#[derive(Debug, Error)]
pub enum MidiStoreError {
    #[error("could not write {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Pick `wanted` out of `available` names: exact match, then case-insensitive.
/// First hit wins, so two identical WinMM names pair the first port.
pub fn match_port_name<'a, I>(wanted: &str, available: I) -> Option<usize>
where
    I: IntoIterator<Item = &'a str>,
{
    let names: Vec<&str> = available.into_iter().collect();
    names.iter().position(|name| *name == wanted).or_else(|| {
        names
            .iter()
            .position(|name| name.eq_ignore_ascii_case(wanted))
    })
}

/// One row the Settings list draws.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MidiPortDto {
    pub name: String,
    /// The OS currently reports this port.
    pub connected: bool,
    /// The user has paired it (`enabled` in midi.json).
    pub enabled: bool,
    /// `"connected"` | `"offline"` | `"error"` | `"available"`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// Merge OS-visible names with saved pairings.
///
/// Visible if the OS lists it *or* it is saved as enabled (so an unplugged
/// controller still has a row). Unpaired, absent names are dropped.
pub fn merge_port_list(
    os_ports: &[String],
    settings: &MidiSettings,
    errors: &[(String, String)],
    open: &[String],
) -> Vec<MidiPortDto> {
    let mut rows: Vec<MidiPortDto> = Vec::new();

    for name in os_ports {
        let saved = settings.device(name);
        let enabled = saved.map(|d| d.enabled).unwrap_or(false);
        let last_error = error_for(name, errors);
        let is_open = open.iter().any(|open_name| open_name == name);
        rows.push(MidiPortDto {
            name: name.clone(),
            connected: is_open,
            enabled,
            status: port_status(enabled, true, is_open, last_error.is_some()),
            last_error,
        });
    }

    for device in &settings.devices {
        if !device.enabled {
            continue;
        }
        if match_port_name(&device.name, os_ports.iter().map(String::as_str)).is_some() {
            continue;
        }
        let last_error = error_for(&device.name, errors);
        rows.push(MidiPortDto {
            name: device.name.clone(),
            connected: false,
            enabled: true,
            status: port_status(true, false, false, last_error.is_some()),
            last_error,
        });
    }

    rows
}

fn error_for(name: &str, errors: &[(String, String)]) -> Option<String> {
    errors
        .iter()
        .find(|(port, _)| match_port_name(name, std::iter::once(port.as_str())).is_some())
        .map(|(_, message)| message.clone())
}

fn port_status(enabled: bool, present: bool, is_open: bool, has_error: bool) -> String {
    if has_error {
        return "error".to_owned();
    }
    if enabled && is_open {
        return "connected".to_owned();
    }
    if enabled && !present {
        return "offline".to_owned();
    }
    if enabled {
        return "offline".to_owned();
    }
    "available".to_owned()
}

/// Load pairings from `path`. A missing or unreadable file is an empty list.
pub fn load_settings(path: &Path) -> MidiSettings {
    let Ok(bytes) = fs::read(path) else {
        return MidiSettings::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

/// Write pairings to `path`, creating the parent directory if needed.
pub fn save_settings(path: &Path, settings: &MidiSettings) -> Result<(), MidiStoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| MidiStoreError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let json = serde_json::to_vec_pretty(settings).expect("MidiSettings is always serialisable");
    fs::write(path, json).map_err(|source| MidiStoreError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_name_wins_over_case_insensitive() {
        let names = ["MPK mini 3", "mpk mini 3"];
        assert_eq!(match_port_name("mpk mini 3", names), Some(1));
        assert_eq!(match_port_name("MPK mini 3", names), Some(0));
    }

    #[test]
    fn case_insensitive_match_when_exact_is_absent() {
        let names = ["nanoKONTROL2 MIDI 1"];
        assert_eq!(match_port_name("nanokontrol2 midi 1", names), Some(0));
    }

    #[test]
    fn substring_is_not_a_match() {
        let names = ["nanoKONTROL2 MIDI 1"];
        assert_eq!(match_port_name("nanoKONTROL", names), None);
    }

    #[test]
    fn merge_keeps_unplugged_enabled_devices_as_offline() {
        let mut settings = MidiSettings::default();
        settings.set_enabled("MPK mini 3", true);
        let rows = merge_port_list(&["loopMIDI Port".to_owned()], &settings, &[], &[]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "loopMIDI Port");
        assert_eq!(rows[0].status, "available");
        assert_eq!(rows[1].name, "MPK mini 3");
        assert_eq!(rows[1].status, "offline");
        assert!(rows[1].enabled);
        assert!(!rows[1].connected);
    }

    #[test]
    fn merge_marks_an_open_enabled_port_connected() {
        let mut settings = MidiSettings::default();
        settings.set_enabled("MPK mini 3", true);
        let rows = merge_port_list(
            &["MPK mini 3".to_owned()],
            &settings,
            &[],
            &["MPK mini 3".to_owned()],
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "connected");
        assert!(rows[0].connected);
        assert!(rows[0].enabled);
    }

    #[test]
    fn a_disabled_saved_device_that_is_unplugged_disappears() {
        let mut settings = MidiSettings::default();
        settings.set_enabled("Gone", false);
        let rows = merge_port_list(&[], &settings, &[], &[]);
        assert!(rows.is_empty());
    }

    #[test]
    fn round_trip_midi_json() {
        let dir = std::env::temp_dir().join(format!("mydaw-midi-{}", std::process::id()));
        let path = dir.join("midi.json");
        let mut settings = MidiSettings::default();
        settings.set_enabled("MPK mini 3", true);
        save_settings(&path, &settings).unwrap();
        let loaded = load_settings(&path);
        assert_eq!(loaded.devices[0].name, "MPK mini 3");
        assert!(loaded.devices[0].enabled);
        assert_eq!(loaded.devices[0].accepts, vec!["notes"]);
        let _ = fs::remove_dir_all(dir);
    }
}
