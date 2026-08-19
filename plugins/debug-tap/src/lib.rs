//! `mydaw-plugin-debug-tap` -- see what the engine actually played.
//!
//! Meters answer "is there signal". This plugin answers "*what* signal": it
//! copies the post-graph output bus into a ring, hands it to a non-RT capture
//! service, and turns the result into a WAV, an annotated PNG and a block of
//! hard numbers. That is the whole point -- an agent (or a developer over a
//! remote session, or a CI job) has no ears, and "it sounds fine" is not a test
//! result.
//!
//! ```text
//!   debug-tap (register)          audio-io (start)             app / agent
//!   --------------------          ----------------             -----------
//!   debug_tap_channel()  --tap--> DebugTapHandoff::take()  ==>  DebugTapService
//!                                 (moves into the RT             ::capture()
//!                                  closure, pushed by             |-> stats()
//!                                  run_block)                     |-> write_wav()
//!                                                                 |-> render_png()
//!                                                                 '-> summary_text()
//! ```
//!
//! The hand-off mirrors `mydaw-plugin-metering`: both ends are given away
//! exactly once through a `take()`-style service, so the single-producer /
//! single-consumer invariant of the ring is enforced by construction rather than
//! by convention.
//!
//! # RT cost
//! The audio thread calls [`TapRtSide::push_block`] once per block. Disarmed --
//! which is the state during every second of normal use -- that is a single
//! relaxed atomic load and a return. Armed, it is one `rtrb` chunk reservation
//! and a copy of `frames * 2` floats: no allocation, no lock, no syscall, no
//! blocking. See the `tests/rt_alloc.rs` suite, which measures it rather than
//! asserting it by eye.
//!
//! # Using it
//! ```no_run
//! use std::time::Duration;
//! use mydaw_kernel::Kernel;
//! use mydaw_plugin_debug_tap::{DebugTapPlugin, DebugTapService};
//!
//! let mut kernel = Kernel::new();
//! kernel.add_plugin(Box::new(DebugTapPlugin::new())).unwrap();
//! kernel.boot().unwrap();
//!
//! let debug = kernel.ctx().services.get::<DebugTapService>().unwrap();
//! // Needs a running audio stream; without one this reports EngineNotRunning.
//! if let Ok(capture) = debug.capture(Duration::from_secs(1), Duration::from_secs(3)) {
//!     println!("{}", capture.summary_text());
//!     capture.write_wav("capture.wav").unwrap();
//!     capture.render_png("capture.png").unwrap();
//! }
//! ```

pub mod analysis;
pub mod capture;
pub mod error;
pub mod font;
pub mod onset;
pub mod render;
pub mod tap;
pub mod wav;

pub use analysis::{
    channel_stats, dbfs, hann_window, spectrum, CaptureStats, ChannelStats, SpectralPeak, Spectrum,
};
pub use capture::{format_db, Capture, CaptureError, DebugTapService, MAX_CAPTURE_SECONDS};
pub use error::CaptureIoError;
pub use onset::{
    align_grid, detect_onsets, grid_report, Band, GridDeviation, GridReport, Onset, OnsetConfig,
};
pub use render::{axes_sidecar_path, RenderedPng, PNG_HEIGHT, PNG_WIDTH};
pub use tap::{debug_tap_channel, TapConsumer, TapRtSide, TapShared, CHANNELS, TAP_QUEUE_CAPACITY};
pub use wav::read_wav;

use std::sync::Arc;

use mydaw_kernel::{KernelCtx, Plugin, PluginManifest};
use parking_lot::Mutex;

/// Kernel id of this plugin. Other plugins name it in their manifest deps.
pub const PLUGIN_ID: &str = "debug-tap";

/// The audio thread's end of the tap, awaiting collection.
///
/// Provided as a kernel service by [`DebugTapPlugin`]. `audio-io` calls
/// [`take`](Self::take) once while building its RT state and moves the result
/// into the audio callback; from then on this service holds `None` until the
/// stream is torn down and hands it back.
///
/// The `Mutex` exists purely so the service can be `Sync` while giving up
/// ownership of a `!Sync` producer. It is never locked from the audio thread.
pub struct DebugTapHandoff {
    rt_side: Mutex<Option<TapRtSide>>,
}

impl DebugTapHandoff {
    /// Wrap an RT side for hand-off.
    pub fn new(rt_side: TapRtSide) -> Self {
        Self {
            rt_side: Mutex::new(Some(rt_side)),
        }
    }

    /// Take the RT side. Returns `None` if somebody already took it.
    pub fn take(&self) -> Option<TapRtSide> {
        self.rt_side.lock().take()
    }

    /// Whether the RT side is still available.
    pub fn is_available(&self) -> bool {
        self.rt_side.lock().is_some()
    }

    /// Give the RT side back, so a later `start()` can reuse it.
    ///
    /// Called by `audio-io` when a stream is torn down. Returning it is what
    /// makes stop/start cycles lossless: the ring can only be handed out once.
    pub fn give_back(&self, rt_side: TapRtSide) {
        *self.rt_side.lock() = Some(rt_side);
    }
}

/// Creates the debug tap and publishes both of its ends as kernel services.
///
/// Contributes no UI panel: the capture path is an agent/developer tool driven
/// by a Tauri command and an environment variable, and a panel that duplicates
/// that would be a Phase 2 decision, not a Phase 1 one.
pub struct DebugTapPlugin {
    capacity: usize,
}

impl Default for DebugTapPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl DebugTapPlugin {
    /// Plugin with the default ring depth.
    pub fn new() -> Self {
        Self {
            capacity: TAP_QUEUE_CAPACITY,
        }
    }

    /// Plugin with an explicit ring depth, in samples (tests, long captures).
    ///
    /// A capacity too small for one stereo frame falls back to
    /// [`TAP_QUEUE_CAPACITY`].
    pub fn with_capacity(capacity: usize) -> Self {
        Self { capacity }
    }
}

impl Plugin for DebugTapPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest::new(PLUGIN_ID, "Debug Tap")
    }

    fn register(&mut self, ctx: &mut KernelCtx) {
        let (rt_side, consumer) = debug_tap_channel(self.capacity);

        // `provide` only fails on a duplicate of the same type, which would mean
        // two debug-tap plugins were registered -- a wiring bug in
        // `app/src/state.rs`, not something to paper over at runtime.
        ctx.services
            .provide(Arc::new(DebugTapHandoff::new(rt_side)))
            .expect("DebugTapHandoff provided twice: debug-tap registered more than once");
        ctx.services
            .provide(Arc::new(DebugTapService::new(consumer)))
            .expect("DebugTapService provided twice: debug-tap registered more than once");
    }

    fn start(&mut self, ctx: &KernelCtx) {
        // Nothing to start: the ring is live from `register`, and the audio
        // thread claims the RT side when a stream starts.
        if let Some(handoff) = ctx.services.get::<DebugTapHandoff>() {
            if handoff.is_available() {
                log::debug!("debug tap unclaimed; audio-io claims it when the stream starts");
            }
        }
    }

    fn stop(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_kernel::Kernel;

    fn booted() -> Kernel {
        let mut kernel = Kernel::new();
        kernel.add_plugin(Box::new(DebugTapPlugin::new())).unwrap();
        kernel.boot().unwrap();
        kernel
    }

    #[test]
    fn provides_both_ends_of_the_tap() {
        let kernel = booted();
        assert!(kernel.ctx().services.get::<DebugTapHandoff>().is_some());
        assert!(kernel.ctx().services.get::<DebugTapService>().is_some());
    }

    #[test]
    fn the_rt_side_can_only_be_taken_once_but_can_be_given_back() {
        let kernel = booted();
        let handoff = kernel.ctx().services.get::<DebugTapHandoff>().unwrap();

        assert!(handoff.is_available());
        let rt_side = handoff.take().expect("first taker gets it");
        assert!(!handoff.is_available());
        assert!(
            handoff.take().is_none(),
            "a second producer would break the SPSC invariant"
        );

        handoff.give_back(rt_side);
        assert!(handoff.is_available(), "stop/start must not lose the tap");
        assert!(handoff.take().is_some());
    }

    #[test]
    fn samples_flow_from_the_rt_side_to_the_service() {
        let kernel = booted();
        let mut rt_side = kernel
            .ctx()
            .services
            .get::<DebugTapHandoff>()
            .unwrap()
            .take()
            .unwrap();
        let service = kernel.ctx().services.get::<DebugTapService>().unwrap();

        rt_side.set_sample_rate(48_000.0);
        assert_eq!(service.sample_rate(), 48_000.0);

        service.shared().arm();
        rt_side.push_block(&[0.5, 0.25], &[-0.5, -0.25]);
        service.shared().disarm();

        // Draining is the capture path's job; here we only prove the wiring.
        assert!(!service.is_armed());
        assert_eq!(service.dropped(), 0);
    }

    #[test]
    fn the_plugin_contributes_no_panels_and_no_node_factories() {
        let kernel = booted();
        assert!(kernel.ui_panels().is_empty());
        assert!(kernel.ctx().rt_node_factories.is_empty());
    }

    #[test]
    fn the_manifest_has_no_dependencies_so_it_can_start_first() {
        let plugin = DebugTapPlugin::new();
        let manifest = plugin.manifest();
        assert_eq!(manifest.id, PLUGIN_ID);
        assert!(manifest.deps.is_empty());
    }

    #[test]
    fn a_degenerate_capacity_still_boots() {
        let mut kernel = Kernel::new();
        kernel
            .add_plugin(Box::new(DebugTapPlugin::with_capacity(0)))
            .unwrap();
        kernel.boot().unwrap();
        assert!(kernel
            .ctx()
            .services
            .get::<DebugTapHandoff>()
            .unwrap()
            .take()
            .is_some());
    }
}
