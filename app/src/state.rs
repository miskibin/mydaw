//! Kernel boot and the state the Tauri commands run against.

use std::sync::Arc;

use mydaw_ipc_bridge::{PanelManifestDto, UiSnapshot};
use mydaw_kernel::{Kernel, KernelError};
use mydaw_plugin_audio_io::{AudioIoError, AudioIoPlugin, AudioIoService, StreamInfo};
use mydaw_plugin_host_clap::HostClapPlugin;
use mydaw_plugin_host_vst3::HostVst3Plugin;
use mydaw_plugin_metering::{MeterOutput, MeteringPlugin};
use mydaw_plugin_test_tone::{TestToneFactory, TestTonePlugin};
use mydaw_plugin_windows::PluginWindowsPlugin;

use crate::graph::install_tone_graph;
use crate::meters::MeterPump;

/// Panel key of the kernel status view, mirrored in `ui/src/panels/registry.ts`.
pub const KERNEL_STATUS_PANEL: &str = "kernel-status";

/// Startup failures that are not the kernel's own.
#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    /// The kernel booted but a plugin did not publish the service the shell needs.
    /// Always a wiring bug in [`boot_kernel`], never a runtime condition.
    #[error("plugin service '{0}' is missing after boot")]
    MissingService(&'static str),
}

/// Register the Phase 0 plugin set and boot the kernel.
///
/// Registration order is irrelevant -- the kernel topologically sorts by manifest
/// dependencies -- so the list is written in the order a reader wants it:
/// infrastructure, then generators, then hosts.
///
/// # Errors
/// Propagates [`KernelError`] from `boot`: a duplicate id, a missing dependency
/// or a cycle. All three are wiring bugs, and all three fail before any plugin
/// code runs.
pub fn boot_kernel() -> Result<Kernel, KernelError> {
    let mut kernel = Kernel::new();
    kernel.add_plugin(Box::new(MeteringPlugin::new()))?;
    kernel.add_plugin(Box::new(TestTonePlugin::new()))?;
    kernel.add_plugin(Box::new(AudioIoPlugin::new()))?;
    kernel.add_plugin(Box::new(HostClapPlugin::new()))?;
    kernel.add_plugin(Box::new(HostVst3Plugin::new()))?;
    kernel.add_plugin(Box::new(PluginWindowsPlugin::new()))?;
    kernel.boot()?;
    Ok(kernel)
}

/// Snapshot of the booted kernel, plus the panels the shell itself contributes.
///
/// The kernel status panel has no owning plugin -- it displays the plugin
/// registry, so a plugin contributing it would be circular. The shell injects it
/// first so it is the leftmost tab in the dock.
pub fn ui_snapshot(kernel: &Kernel) -> UiSnapshot {
    let mut snapshot = UiSnapshot::from_kernel(kernel);
    snapshot.panels.insert(
        0,
        PanelManifestDto {
            id: KERNEL_STATUS_PANEL.to_owned(),
            title: "Kernel".to_owned(),
            component: KERNEL_STATUS_PANEL.to_owned(),
        },
    );
    snapshot
}

/// What the Tauri commands are allowed to touch.
///
/// Note what is *not* here: the [`Kernel`] itself. `Box<dyn Plugin>` is not
/// `Send`, so the kernel cannot live in Tauri's managed state; it stays on the
/// main thread (owned by the run loop, which also shuts it down). Everything the
/// UI needs is either an immutable snapshot taken at boot or an `Arc` to a
/// service that was designed to be shared.
pub struct AppState {
    snapshot: UiSnapshot,
    audio: Arc<AudioIoService>,
    tone: Arc<TestToneFactory>,
    meters: Arc<MeterPump>,
}

impl AppState {
    /// Collect the shell's state from a booted kernel.
    ///
    /// Claims the metering plugin's throttle here, once, so the pump owns it for
    /// the process lifetime regardless of how often the webview resubscribes.
    ///
    /// # Errors
    /// [`StartupError::MissingService`] if a required plugin service is absent.
    pub fn from_kernel(kernel: &Kernel) -> Result<Self, StartupError> {
        let services = &kernel.ctx().services;
        let audio = services
            .get::<AudioIoService>()
            .ok_or(StartupError::MissingService("AudioIoService"))?;
        let tone = services
            .get::<TestToneFactory>()
            .ok_or(StartupError::MissingService("TestToneFactory"))?;
        let meter_output = services
            .get::<MeterOutput>()
            .ok_or(StartupError::MissingService("MeterOutput"))?;

        let throttle = meter_output.take();
        if throttle.is_none() {
            log::warn!("meter throttle was already claimed; the UI meters will stay silent");
        }

        Ok(Self {
            snapshot: ui_snapshot(kernel),
            audio,
            tone,
            meters: MeterPump::new(throttle),
        })
    }

    /// Open the output device and publish the test-tone graph.
    ///
    /// Callers treat failure as a warning, not a fatal error: a machine with no
    /// output device (headless CI, RDP session) must still get a working window.
    ///
    /// # Errors
    /// Whatever cpal reports, plus [`AudioIoError::NotRunning`] if the stream died
    /// between opening it and installing the graph.
    pub fn start_audio(&self) -> Result<StreamInfo, AudioIoError> {
        // Order is forced by the service: `install_graph` needs a live session,
        // because the graph channel is created together with the stream.
        let info = self.audio.start()?;
        install_tone_graph(&self.audio, &self.tone)?;
        Ok(info)
    }

    /// Start or stop the transport.
    ///
    /// # Errors
    /// [`AudioIoError::NotRunning`] when no device was opened.
    pub fn set_playing(&self, playing: bool) -> Result<(), AudioIoError> {
        self.audio.set_playing(playing)
    }

    /// The snapshot handed to the webview. Cheap to clone (a few short strings).
    pub fn snapshot(&self) -> UiSnapshot {
        self.snapshot.clone()
    }

    pub fn meters(&self) -> &Arc<MeterPump> {
        &self.meters
    }

    pub fn audio(&self) -> &Arc<AudioIoService> {
        &self.audio
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_ipc_bridge::PluginStateDto;

    #[test]
    fn boots_the_full_plugin_set_in_dependency_order() {
        let kernel = boot_kernel().expect("the Phase 0 plugin set must boot");
        assert_eq!(
            kernel.boot_order(),
            vec![
                "metering",
                "test-tone",
                "host-clap",
                "host-vst3",
                "plugin-windows",
                "audio-io",
            ],
            "audio-io waits for metering; Kahn's algorithm keeps the rest in \
             registration order"
        );
        assert!(kernel
            .plugin_statuses()
            .iter()
            .all(|p| p.state == mydaw_kernel::PluginState::Started));
    }

    #[test]
    fn the_snapshot_lists_six_plugins_and_the_shell_panel() {
        let kernel = boot_kernel().unwrap();
        let snapshot = ui_snapshot(&kernel);

        assert_eq!(snapshot.plugins.len(), 6);
        assert!(snapshot
            .plugins
            .iter()
            .all(|p| p.state == PluginStateDto::Started));

        // Kernel status first (shell-contributed), then the plugins' own panels.
        let components: Vec<&str> = snapshot
            .panels
            .iter()
            .map(|p| p.component.as_str())
            .collect();
        assert_eq!(components, vec!["kernel-status", "meters"]);
    }

    #[test]
    fn state_claims_the_meter_throttle_exactly_once() {
        let kernel = boot_kernel().unwrap();
        let state = AppState::from_kernel(&kernel).expect("all services present");
        assert!(state.meters().has_throttle());

        let output = kernel.ctx().services.get::<MeterOutput>().unwrap();
        assert!(
            !output.is_available(),
            "the pump owns the throttle after boot"
        );
    }

    #[test]
    fn transport_commands_fail_cleanly_without_a_device() {
        let kernel = boot_kernel().unwrap();
        let state = AppState::from_kernel(&kernel).unwrap();
        // No `start_audio` call: this is the headless path.
        assert!(matches!(
            state.set_playing(true),
            Err(AudioIoError::NotRunning)
        ));
    }
}
