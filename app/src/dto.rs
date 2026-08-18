//! Wire shapes owned by the shell.
//!
//! `ipc-bridge` defines the canonical Rust types; this module only exists where
//! the TypeScript contract in `ui/src/api/types.ts` disagrees with serde's
//! default rendering of them. Today that is exactly one type: [`MeterFrame`]
//! serialises its fields as `peak_l`/`sample_pos`, while the UI reads
//! `peakL`/`samplePos`. Renaming the fields in `ipc-bridge` would push a
//! JavaScript convention into a crate that is deliberately Tauri-free and
//! webview-agnostic, so the translation lives here, at the boundary that
//! actually cares.

use mydaw_ipc_bridge::MeterFrame;
use mydaw_plugin_host_clap::ClapPluginInfo;
use serde::Serialize;

/// [`MeterFrame`] in the shape the webview expects (camelCase).
///
/// `Copy` and 24 bytes: one of these crosses the IPC boundary up to 30 times a
/// second, so it stays a flat struct with no owned data.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeterFrameDto {
    pub peak_l: f32,
    pub peak_r: f32,
    pub rms_l: f32,
    pub rms_r: f32,
    /// Transport position in frames at the end of the measured block.
    pub sample_pos: u64,
    pub playing: bool,
    /// When the pump handed this frame to the webview, on the bench clock
    /// ([`crate::ipc_bench::epoch_ms`]).
    ///
    /// `None` -- and absent from the JSON entirely -- unless the IPC bench asked
    /// the pump to stamp frames, which is the only thing that reads it. That
    /// keeps the normal meter frame byte-for-byte what it was before spike 4.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pump_ms: Option<f64>,
}

impl From<MeterFrame> for MeterFrameDto {
    fn from(frame: MeterFrame) -> Self {
        Self {
            peak_l: frame.peak_l,
            peak_r: frame.peak_r,
            rms_l: frame.rms_l,
            rms_r: frame.rms_r,
            sample_pos: frame.sample_pos,
            playing: frame.playing,
            pump_ms: None,
        }
    }
}

/// One row of the CLAP browser panel.
///
/// `path` is a `String`, not a `PathBuf`: serde renders a `PathBuf` as whatever
/// the OS encoding happens to be, and a Windows path with a non-UTF-8 byte would
/// fail to serialise and take the whole command with it. Lossy conversion here
/// means a mangled character in a tooltip instead of an empty panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClapPluginInfoDto {
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub version: String,
    pub path: String,
}

impl From<&ClapPluginInfo> for ClapPluginInfoDto {
    fn from(info: &ClapPluginInfo) -> Self {
        Self {
            id: info.id.clone(),
            name: info.name.clone(),
            vendor: info.vendor.clone(),
            version: info.version.clone(),
            path: info.path.to_string_lossy().into_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialises_with_the_key_names_the_ui_reads() {
        let dto = MeterFrameDto::from(MeterFrame {
            peak_l: 0.5,
            peak_r: 0.25,
            rms_l: 0.1,
            rms_r: 0.2,
            sample_pos: 4_096,
            playing: true,
        });
        let json: serde_json::Value = serde_json::to_value(dto).unwrap();
        let object = json.as_object().unwrap();

        // Mirrors `MeterFrame` in ui/src/api/types.ts, key for key.
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["peakL", "peakR", "playing", "rmsL", "rmsR", "samplePos"]
        );
        assert_eq!(object["peakL"], 0.5);
        assert_eq!(object["samplePos"], 4_096);
        assert_eq!(object["playing"], true);
    }

    #[test]
    fn the_bench_timestamp_is_absent_unless_the_pump_stamped_the_frame() {
        let plain = MeterFrameDto::from(MeterFrame::SILENT);
        assert_eq!(plain.pump_ms, None);
        let json = serde_json::to_string(&plain).unwrap();
        assert!(
            !json.contains("pumpMs"),
            "an unstamped frame is exactly the pre-spike-4 wire shape: {json}"
        );

        let stamped = MeterFrameDto {
            pump_ms: Some(1234.5),
            ..plain
        };
        let json: serde_json::Value = serde_json::to_value(stamped).unwrap();
        assert_eq!(json["pumpMs"], 1234.5);
    }

    #[test]
    fn clap_plugin_info_serialises_with_the_keys_the_ui_reads() {
        let dto = ClapPluginInfoDto::from(&ClapPluginInfo {
            id: "com.acme.thing".to_owned(),
            name: "Thing".to_owned(),
            vendor: "Acme".to_owned(),
            version: "1.2.3".to_owned(),
            path: std::path::PathBuf::from(r"C:\CLAP\Thing.clap"),
        });
        let json: serde_json::Value = serde_json::to_value(dto).unwrap();
        let object = json.as_object().unwrap();

        // Mirrors `ClapPluginInfoDto` in ui/src/api/types.ts, key for key.
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["id", "name", "path", "vendor", "version"]);
        assert_eq!(object["id"], "com.acme.thing");
        assert_eq!(object["name"], "Thing");
        assert_eq!(object["vendor"], "Acme");
        assert_eq!(object["version"], "1.2.3");
        assert_eq!(object["path"], r"C:\CLAP\Thing.clap");
    }

    #[test]
    fn a_plugin_with_no_vendor_or_version_still_serialises() {
        // `scan_bundle` leaves these empty rather than dropping the plugin, so the
        // DTO has to survive it -- the UI shows a dash.
        let dto = ClapPluginInfoDto::from(&ClapPluginInfo {
            id: "x".to_owned(),
            name: String::new(),
            vendor: String::new(),
            version: String::new(),
            path: std::path::PathBuf::from("x.clap"),
        });
        let json = serde_json::to_string(&dto).unwrap();
        assert!(json.contains("\"vendor\":\"\""), "{json}");
    }
}
