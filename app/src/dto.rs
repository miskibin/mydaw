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
}
