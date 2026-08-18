//! Serde DTOs mirroring kernel types.
//!
//! Why mirror instead of deriving `Serialize` on the kernel types directly: the
//! kernel stays free of a serde dependency and free of any obligation towards the
//! wire format. The DTOs are the contract with the TypeScript side and may evolve
//! (renames, extra display fields) without touching the kernel.

use mydaw_kernel::{Kernel, PanelManifest, PluginState, PluginStatus};
use serde::{Deserialize, Serialize};

/// Lifecycle state as seen by the frontend.
///
/// Serialised in lowercase so TS can use a plain string union:
/// `type PluginState = "registered" | "started" | "stopped"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginStateDto {
    Registered,
    Started,
    Stopped,
}

impl From<PluginState> for PluginStateDto {
    fn from(state: PluginState) -> Self {
        match state {
            PluginState::Registered => Self::Registered,
            PluginState::Started => Self::Started,
            PluginState::Stopped => Self::Stopped,
        }
    }
}

/// One row of the kernel status panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginStatusDto {
    pub id: String,
    pub name: String,
    pub state: PluginStateDto,
}

impl From<&PluginStatus> for PluginStatusDto {
    fn from(status: &PluginStatus) -> Self {
        Self {
            id: status.id.clone(),
            name: status.name.clone(),
            state: status.state.into(),
        }
    }
}

impl From<PluginStatus> for PluginStatusDto {
    fn from(status: PluginStatus) -> Self {
        Self {
            id: status.id,
            name: status.name,
            state: status.state.into(),
        }
    }
}

/// A panel the frontend should be able to open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelManifestDto {
    pub id: String,
    pub title: String,
    /// Key into the frontend's panel component registry.
    pub component: String,
}

impl From<&PanelManifest> for PanelManifestDto {
    fn from(manifest: &PanelManifest) -> Self {
        Self {
            id: manifest.id.clone(),
            title: manifest.title.clone(),
            component: manifest.component.clone(),
        }
    }
}

impl From<PanelManifest> for PanelManifestDto {
    fn from(manifest: PanelManifest) -> Self {
        Self {
            id: manifest.id,
            title: manifest.title,
            component: manifest.component,
        }
    }
}

/// Everything the UI needs to draw itself after boot.
///
/// Sent once when the webview asks for it; meter/playhead updates stream
/// separately through [`MeterThrottle`](crate::MeterThrottle).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct UiSnapshot {
    pub plugins: Vec<PluginStatusDto>,
    pub panels: Vec<PanelManifestDto>,
}

impl UiSnapshot {
    /// Build a snapshot from the live kernel. Non-RT (clones strings).
    pub fn from_kernel(kernel: &Kernel) -> Self {
        Self {
            plugins: kernel
                .plugin_statuses()
                .iter()
                .map(PluginStatusDto::from)
                .collect(),
            panels: kernel
                .ui_panels()
                .iter()
                .map(PanelManifestDto::from)
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_kernel::{KernelCtx, Plugin, PluginManifest};

    #[test]
    fn plugin_state_maps_one_to_one() {
        assert_eq!(
            PluginStateDto::from(PluginState::Registered),
            PluginStateDto::Registered
        );
        assert_eq!(
            PluginStateDto::from(PluginState::Started),
            PluginStateDto::Started
        );
        assert_eq!(
            PluginStateDto::from(PluginState::Stopped),
            PluginStateDto::Stopped
        );
    }

    #[test]
    fn plugin_status_converts_by_value_and_by_reference() {
        let status = PluginStatus {
            id: "mydaw.audio-io".to_owned(),
            name: "Audio I/O".to_owned(),
            state: PluginState::Started,
        };
        let by_ref = PluginStatusDto::from(&status);
        let by_val = PluginStatusDto::from(status);
        assert_eq!(by_ref, by_val);
        assert_eq!(by_ref.id, "mydaw.audio-io");
        assert_eq!(by_ref.name, "Audio I/O");
        assert_eq!(by_ref.state, PluginStateDto::Started);
    }

    #[test]
    fn panel_manifest_converts() {
        let manifest = PanelManifest::new("mydaw.panel.status", "Kernel Status", "StatusPanel");
        let dto = PanelManifestDto::from(&manifest);
        assert_eq!(dto.id, "mydaw.panel.status");
        assert_eq!(dto.title, "Kernel Status");
        assert_eq!(dto.component, "StatusPanel");
        assert_eq!(dto, PanelManifestDto::from(manifest));
    }

    #[test]
    fn state_serialises_lowercase_for_typescript() {
        let json = serde_json::to_string(&PluginStateDto::Started).unwrap();
        assert_eq!(json, "\"started\"");
        let back: PluginStateDto = serde_json::from_str("\"stopped\"").unwrap();
        assert_eq!(back, PluginStateDto::Stopped);
    }

    struct Demo;
    impl Plugin for Demo {
        fn manifest(&self) -> PluginManifest {
            PluginManifest::new("mydaw.demo", "Demo")
        }
        fn register(&mut self, ctx: &mut KernelCtx) {
            ctx.contribute_panel(PanelManifest::new("mydaw.panel.demo", "Demo", "DemoPanel"));
        }
        fn start(&mut self, _ctx: &KernelCtx) {}
        fn stop(&mut self) {}
    }

    #[test]
    fn snapshot_mirrors_a_booted_kernel() {
        let mut kernel = Kernel::new();
        kernel.add_plugin(Box::new(Demo)).unwrap();
        kernel.boot().unwrap();

        let snapshot = UiSnapshot::from_kernel(&kernel);
        assert_eq!(snapshot.plugins.len(), 1);
        assert_eq!(snapshot.plugins[0].id, "mydaw.demo");
        assert_eq!(snapshot.plugins[0].state, PluginStateDto::Started);
        assert_eq!(snapshot.panels.len(), 1);
        assert_eq!(snapshot.panels[0].component, "DemoPanel");

        // Round-trips through JSON unchanged -- the actual IPC payload.
        let json = serde_json::to_string(&snapshot).unwrap();
        let back: UiSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back, snapshot);
    }
}
