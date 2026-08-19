//! `mydaw-plugin-metering` -- the meter channel owner.
//!
//! This plugin owns exactly one thing: the single-producer/single-consumer queue
//! that carries `MeterFrame`s from the audio callback to the UI. It deliberately
//! does *not* measure anything itself -- the levels are computed where the samples
//! already are, inside the audio callback owned by `mydaw-plugin-audio-io`, so
//! there is no extra pass over the buffers and no extra queue hop.
//!
//! Splitting ownership this way keeps the RT wall clean:
//!
//! ```text
//!   metering (register)        audio-io (start)            app (IPC pump)
//!   -------------------        ----------------            --------------
//!   meter_channel()  --tap-->  MeterTap::take()  ==push==>  MeterOutput::take()
//!                              (moves into the              (MeterThrottle,
//!                               RT closure)                  non-RT, 30 Hz)
//! ```
//!
//! Both halves are handed over exactly once, through a `take()`-style service, so
//! the SPSC invariant is enforced by construction: a second taker gets `None`
//! rather than a second producer.
//!
//! # RT safety
//! Nothing in this crate runs on the audio thread. The services here lock a mutex
//! and are only ever touched during boot (and, for [`MeterOutput`], on the IPC
//! pump thread). The audio thread only ever sees the bare `rtrb` producer that
//! [`MeterTap::take`] handed it.

use std::sync::Arc;

use mydaw_ipc_bridge::{
    meter_channel, spectrum_channel, MeterProducer, MeterThrottle, SpectrumConsumer,
    SpectrumProducer, SpectrumWatch,
};
use mydaw_kernel::{KernelCtx, PanelManifest, Plugin, PluginManifest};
use parking_lot::Mutex;

/// Kernel id of this plugin. Other plugins name it in their manifest deps.
pub const PLUGIN_ID: &str = "metering";

/// Panel id and frontend component key. Must match the key in
/// `ui/src/panels/registry.ts`.
pub const METERS_PANEL: &str = "meters";

/// Meter queue depth, in frames.
///
/// One frame is pushed per audio block (~344 Hz at 48 kHz / 128 frames) and the
/// throttle drains the queue completely on every poll (~30 Hz), so ~12 frames is
/// the steady-state high-water mark. 64 gives an order of magnitude of headroom
/// for a stalled UI thread; overflow is handled by dropping the newest frame,
/// which is invisible in a meter.
pub const METER_QUEUE_CAPACITY: usize = 64;

/// The audio thread's end of the meter queue, awaiting collection.
///
/// Provided as a kernel service by [`MeteringPlugin`]. `audio-io` calls
/// [`take`](Self::take) once while building its RT state and moves the producer
/// into the audio callback; from then on this service holds `None`.
///
/// The `Mutex` exists purely so the service can be `Sync` while giving up
/// ownership of a `!Sync` producer. It is never locked from the audio thread.
pub struct MeterTap {
    producer: Mutex<Option<MeterProducer>>,
}

impl MeterTap {
    /// Wrap a producer for hand-off.
    pub fn new(producer: MeterProducer) -> Self {
        Self {
            producer: Mutex::new(Some(producer)),
        }
    }

    /// Take the producer. Returns `None` if somebody already took it.
    ///
    /// A single-producer queue can only have one owner; returning `None` instead
    /// of handing out a second producer is what makes that a runtime guarantee.
    pub fn take(&self) -> Option<MeterProducer> {
        self.producer.lock().take()
    }

    /// Whether the producer is still available.
    pub fn is_available(&self) -> bool {
        self.producer.lock().is_some()
    }
}

/// The UI's end of the meter queue, awaiting collection.
///
/// Holds a [`MeterThrottle`] rather than a bare `rtrb::Consumer`: `ipc-bridge`
/// hands out the consumer already wrapped in its rate limiter, and that is exactly
/// what the IPC pump wants (drain fully, emit at most 30 Hz). Unwrapping it back
/// to a raw consumer would only throw the throttling away.
pub struct MeterOutput {
    throttle: Mutex<Option<MeterThrottle>>,
}

impl MeterOutput {
    /// Wrap a throttle for hand-off.
    pub fn new(throttle: MeterThrottle) -> Self {
        Self {
            throttle: Mutex::new(Some(throttle)),
        }
    }

    /// Take the throttle. Returns `None` if somebody already took it.
    pub fn take(&self) -> Option<MeterThrottle> {
        self.throttle.lock().take()
    }

    /// Whether the throttle is still available.
    pub fn is_available(&self) -> bool {
        self.throttle.lock().is_some()
    }
}

/// Hop queue depth. The UI polls ~25 Hz and a hop is ~21 ms, so 4 is already
/// more than a stalled frame or two.
pub const SPECTRUM_QUEUE_CAPACITY: usize = 4;

/// Audio-thread producer plus the shared watch word.
///
/// Same take-once contract as [`MeterTap`]: audio-io claims the producer at
/// stream start. The watch is an `Arc` and can be cloned by anyone.
pub struct SpectrumHandoff {
    producer: Mutex<Option<SpectrumProducer>>,
    watch: Arc<SpectrumWatch>,
}

impl SpectrumHandoff {
    pub fn new(producer: SpectrumProducer, watch: Arc<SpectrumWatch>) -> Self {
        Self {
            producer: Mutex::new(Some(producer)),
            watch,
        }
    }

    pub fn take(&self) -> Option<SpectrumProducer> {
        self.producer.lock().take()
    }

    pub fn watch(&self) -> Arc<SpectrumWatch> {
        Arc::clone(&self.watch)
    }

    pub fn is_available(&self) -> bool {
        self.producer.lock().is_some()
    }
}

/// The shell's end of the hop queue.
pub struct SpectrumOutput {
    consumer: Mutex<Option<SpectrumConsumer>>,
}

impl SpectrumOutput {
    pub fn new(consumer: SpectrumConsumer) -> Self {
        Self {
            consumer: Mutex::new(Some(consumer)),
        }
    }

    pub fn take(&self) -> Option<SpectrumConsumer> {
        self.consumer.lock().take()
    }

    pub fn is_available(&self) -> bool {
        self.consumer.lock().is_some()
    }
}

/// Creates the meter queue and contributes the Meters panel.
pub struct MeteringPlugin {
    capacity: usize,
}

impl Default for MeteringPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl MeteringPlugin {
    /// Plugin with the default queue depth.
    pub fn new() -> Self {
        Self {
            capacity: METER_QUEUE_CAPACITY,
        }
    }

    /// Plugin with an explicit queue depth (tests, low-latency profiling).
    ///
    /// A capacity of 0 falls back to [`METER_QUEUE_CAPACITY`]; `rtrb` would panic
    /// on an empty ring, and a dead meter is not worth crashing the boot for.
    pub fn with_capacity(capacity: usize) -> Self {
        Self { capacity }
    }

    fn capacity(&self) -> usize {
        if self.capacity == 0 {
            METER_QUEUE_CAPACITY
        } else {
            self.capacity
        }
    }
}

impl Plugin for MeteringPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest::new(PLUGIN_ID, "Metering")
    }

    fn register(&mut self, ctx: &mut KernelCtx) {
        let (producer, throttle) = meter_channel(self.capacity());
        let (spectrum_tx, spectrum_rx) = spectrum_channel(SPECTRUM_QUEUE_CAPACITY);
        let watch = Arc::new(SpectrumWatch::new());

        // `provide` only fails on a duplicate of the same type, which would mean
        // two metering plugins were registered -- a wiring bug in `app/main.rs`,
        // not something to paper over at runtime.
        ctx.services
            .provide(Arc::new(MeterTap::new(producer)))
            .expect("MeterTap provided twice: metering plugin registered more than once");
        ctx.services
            .provide(Arc::new(MeterOutput::new(throttle)))
            .expect("MeterOutput provided twice: metering plugin registered more than once");
        ctx.services
            .provide(Arc::new(SpectrumHandoff::new(spectrum_tx, watch)))
            .expect("SpectrumHandoff provided twice: metering plugin registered more than once");
        ctx.services
            .provide(Arc::new(SpectrumOutput::new(spectrum_rx)))
            .expect("SpectrumOutput provided twice: metering plugin registered more than once");

        ctx.contribute_panel(PanelManifest::new(METERS_PANEL, "Meters", METERS_PANEL));
    }

    fn start(&mut self, ctx: &KernelCtx) {
        // Nothing to start: the queue is live from `register`. Audio I/O claims
        // the producer when its stream starts, which is after boot.
        if let Some(tap) = ctx.services.get::<MeterTap>() {
            if tap.is_available() {
                log::debug!("meter producer unclaimed; audio-io claims it when the stream starts");
            }
        }
    }

    fn stop(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_ipc_bridge::MeterFrame;
    use mydaw_kernel::Kernel;
    use std::time::Instant;

    fn booted() -> Kernel {
        let mut kernel = Kernel::new();
        kernel.add_plugin(Box::new(MeteringPlugin::new())).unwrap();
        kernel.boot().unwrap();
        kernel
    }

    #[test]
    fn contributes_the_meters_panel_matching_the_ui_registry() {
        let kernel = booted();
        let panels = kernel.ui_panels();
        assert_eq!(panels.len(), 1);
        assert_eq!(panels[0].id, "meters");
        assert_eq!(panels[0].title, "Meters");
        // Key into ui/src/panels/registry.ts -- renaming this breaks the UI.
        assert_eq!(panels[0].component, "meters");
    }

    #[test]
    fn provides_both_ends_of_the_queue() {
        let kernel = booted();
        assert!(kernel.ctx().services.get::<MeterTap>().is_some());
        assert!(kernel.ctx().services.get::<MeterOutput>().is_some());
        assert!(kernel.ctx().services.get::<SpectrumHandoff>().is_some());
        assert!(kernel.ctx().services.get::<SpectrumOutput>().is_some());
    }

    #[test]
    fn each_end_can_only_be_taken_once() {
        let kernel = booted();
        let tap = kernel.ctx().services.get::<MeterTap>().unwrap();
        assert!(tap.is_available());
        assert!(tap.take().is_some());
        assert!(!tap.is_available());
        assert!(tap.take().is_none(), "a second producer would break SPSC");

        let out = kernel.ctx().services.get::<MeterOutput>().unwrap();
        assert!(out.take().is_some());
        assert!(out.take().is_none());

        let spectrum = kernel.ctx().services.get::<SpectrumHandoff>().unwrap();
        assert!(spectrum.take().is_some());
        assert!(
            spectrum.take().is_none(),
            "a second hop producer would break SPSC"
        );
        let spectrum_out = kernel.ctx().services.get::<SpectrumOutput>().unwrap();
        assert!(spectrum_out.take().is_some());
        assert!(spectrum_out.take().is_none());
    }

    #[test]
    fn frames_flow_from_the_tap_to_the_output() {
        let kernel = booted();
        let mut producer = kernel
            .ctx()
            .services
            .get::<MeterTap>()
            .unwrap()
            .take()
            .unwrap();
        let mut throttle = kernel
            .ctx()
            .services
            .get::<MeterOutput>()
            .unwrap()
            .take()
            .unwrap();

        producer
            .push(MeterFrame {
                peak_l: 0.75,
                sample_pos: 4096,
                playing: true,
                ..MeterFrame::SILENT
            })
            .unwrap();

        let frame = throttle.poll(Instant::now()).expect("frame should arrive");
        assert_eq!(frame.peak_l, 0.75);
        assert_eq!(frame.sample_pos, 4096);
        assert!(frame.playing);
    }

    #[test]
    fn zero_capacity_falls_back_to_the_default() {
        let mut kernel = Kernel::new();
        kernel
            .add_plugin(Box::new(MeteringPlugin::with_capacity(0)))
            .unwrap();
        kernel.boot().unwrap();
        assert!(kernel
            .ctx()
            .services
            .get::<MeterTap>()
            .unwrap()
            .take()
            .is_some());
    }
}
