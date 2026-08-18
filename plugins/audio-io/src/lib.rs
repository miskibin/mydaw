//! `mydaw-plugin-audio-io` -- the device, the audio thread, and the wall between
//! them.
//!
//! This is the only crate in the workspace that owns a real-time thread. It opens
//! the default output device through cpal, runs the graph inside the callback and
//! carries the results back out: audio to the device, meter frames to the UI,
//! transport position to everyone.
//!
//! Two halves, deliberately separated:
//!
//! - [`service`] -- non-RT. Opens and closes the device, owns the mutexes, and
//!   hands out [`GraphHandle`](mydaw_rt_core::GraphHandle), the
//!   [`TransportClock`](mydaw_rt_core::TransportClock) and the command producer.
//! - [`rt`] -- real-time. [`RtState`] is everything the callback owns and
//!   [`run_block`] is everything it does. No cpal types appear there, which is
//!   what lets the whole audio path be unit-tested on a machine with no sound
//!   card (see the tests in `rt.rs`).
//!
//! ```text
//!            app / UI (non-RT)                    cpal audio thread (RT)
//!   ------------------------------------   --------------------------------
//!   AudioIoService::install_graph  ---------->  RtGraphSlot::poll_swap
//!   AudioIoService::set_playing    ---------->  drain_commands -> TransportClock
//!   MeterThrottle::poll (metering) <----------  MeterFrame push
//!                                               |
//!                                               +-> run_block -> device buffer
//! ```
//!
//! # Boot behaviour
//! [`AudioIoPlugin`] registers the service and claims the meter producer, but does
//! **not** open the device. Booting the kernel on a machine without audio output
//! must not fail, and the app should decide when audio goes live. Call
//! [`AudioIoService::start`] when you are ready.
//!
//! ```no_run
//! use mydaw_kernel::Kernel;
//! use mydaw_plugin_audio_io::{AudioIoPlugin, AudioIoService};
//! use mydaw_plugin_metering::MeteringPlugin;
//!
//! let mut kernel = Kernel::new();
//! kernel.add_plugin(Box::new(MeteringPlugin::new())).unwrap();
//! kernel.add_plugin(Box::new(AudioIoPlugin::new())).unwrap();
//! kernel.boot().unwrap();
//!
//! let audio = kernel.ctx().services.get::<AudioIoService>().unwrap();
//! let info = audio.start().unwrap();
//! println!("{} @ {} Hz", info.device_name, info.sample_rate);
//! ```

mod priority;
pub mod rt;
pub mod service;

pub use rt::{run_block, AudioIoStats, RtState, RtStateParams};
pub use service::{AudioIoConfig, AudioIoError, AudioIoService, StreamInfo};

use std::sync::Arc;

use mydaw_kernel::{KernelCtx, Plugin, PluginManifest};
use mydaw_plugin_metering::MeterTap;

/// Kernel id of this plugin.
pub const PLUGIN_ID: &str = "audio-io";

/// Manifest dependencies.
///
/// The dependency on `metering` is what guarantees `MeterTap` exists by the time
/// [`Plugin::start`] runs here -- the kernel starts plugins in topological order.
const DEPS: &[&str] = &["metering"];

/// Owns the [`AudioIoService`] and wires it to the metering plugin.
pub struct AudioIoPlugin {
    config: AudioIoConfig,
    service: Option<Arc<AudioIoService>>,
}

impl Default for AudioIoPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioIoPlugin {
    /// Plugin with the default engine configuration.
    pub fn new() -> Self {
        Self::with_config(AudioIoConfig::default())
    }

    /// Plugin with an explicit engine configuration.
    pub fn with_config(config: AudioIoConfig) -> Self {
        Self {
            config,
            service: None,
        }
    }

    /// The service this plugin provided, once `register` has run.
    pub fn service(&self) -> Option<&Arc<AudioIoService>> {
        self.service.as_ref()
    }
}

impl Plugin for AudioIoPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest::with_deps(PLUGIN_ID, "Audio I/O", DEPS)
    }

    fn register(&mut self, ctx: &mut KernelCtx) {
        let service = Arc::new(AudioIoService::new(self.config));
        ctx.services
            .provide(Arc::clone(&service))
            .expect("AudioIoService provided twice: audio-io registered more than once");
        self.service = Some(service);
    }

    fn start(&mut self, ctx: &KernelCtx) {
        let Some(service) = self.service.as_ref() else {
            return;
        };

        // Claim the meter queue's producer. Guaranteed to be here by the manifest
        // dependency on "metering"; if it is missing, something in `app/main.rs`
        // registered the plugins without the kernel's ordering.
        match ctx.services.get::<MeterTap>() {
            Some(tap) => match tap.take() {
                Some(producer) => service.attach_meters(producer),
                None => {
                    log::warn!("meter producer already taken; audio-io will run without metering")
                }
            },
            None => log::warn!("MeterTap service missing; audio-io will run without metering"),
        }

        // The device is opened on demand, not here. See the crate docs.
    }

    fn stop(&mut self) {
        if let Some(service) = self.service.as_ref() {
            service.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_kernel::{Kernel, KernelError};
    use mydaw_plugin_metering::MeteringPlugin;

    fn booted() -> Kernel {
        let mut kernel = Kernel::new();
        kernel.add_plugin(Box::new(MeteringPlugin::new())).unwrap();
        kernel.add_plugin(Box::new(AudioIoPlugin::new())).unwrap();
        kernel.boot().unwrap();
        kernel
    }

    #[test]
    fn metering_starts_before_audio_io() {
        let kernel = booted();
        assert_eq!(kernel.boot_order(), vec!["metering", "audio-io"]);
    }

    #[test]
    fn registration_order_does_not_matter() {
        // Declared out of dependency order: the kernel's topological sort fixes it.
        let mut kernel = Kernel::new();
        kernel.add_plugin(Box::new(AudioIoPlugin::new())).unwrap();
        kernel.add_plugin(Box::new(MeteringPlugin::new())).unwrap();
        kernel.boot().unwrap();
        assert_eq!(kernel.boot_order(), vec!["metering", "audio-io"]);
    }

    #[test]
    fn booting_without_metering_is_a_dependency_error_not_a_silent_downgrade() {
        let mut kernel = Kernel::new();
        kernel.add_plugin(Box::new(AudioIoPlugin::new())).unwrap();
        let err = kernel.boot().unwrap_err();
        assert!(matches!(
            err,
            KernelError::MissingDependency { ref plugin, ref dependency }
                if plugin == "audio-io" && dependency == "metering"
        ));
    }

    #[test]
    fn the_service_is_registered_and_idle_after_boot() {
        let kernel = booted();
        let service = kernel
            .ctx()
            .services
            .get::<AudioIoService>()
            .expect("service should be provided");
        assert!(!service.is_running(), "boot must not open the device");
        assert!(service.transport().is_none());
    }

    #[test]
    fn starting_claims_the_meter_producer() {
        let kernel = booted();
        let tap = kernel.ctx().services.get::<MeterTap>().unwrap();
        assert!(
            !tap.is_available(),
            "audio-io should have claimed the producer during start"
        );
    }

    #[test]
    fn shutdown_is_clean_without_a_stream() {
        let mut kernel = booted();
        kernel.shutdown();
        let service = kernel.ctx().services.get::<AudioIoService>().unwrap();
        assert!(!service.is_running());
    }

    #[test]
    fn contributes_no_panels_and_no_node_factories() {
        let kernel = booted();
        // Only metering's "meters" panel.
        assert_eq!(kernel.ui_panels().len(), 1);
        assert_eq!(kernel.ui_panels()[0].component, "meters");
        assert!(kernel.ctx().rt_node_factories.is_empty());
    }
}
