//! The real-time side of audio I/O.
//!
//! Everything the audio callback touches lives in [`RtState`], and the entire
//! body of the callback is [`run_block`]. The cpal closure in
//! [`crate::service`] is a three-line wrapper around it, which is what makes this
//! path testable: [`run_block`] needs no device, no host and no thread of its own,
//! so CI (which has neither speakers nor a sound card) still covers the logic that
//! actually matters.
//!
//! # RT rules honoured here
//! - **No allocation.** Buffers, queues and the graph are built off-thread; the
//!   callback only reads and writes them.
//! - **No locks.** Nothing in [`RtState`] is behind a mutex. Hand-off from the
//!   non-RT side happens once, by moving ownership into the closure.
//! - **No syscalls, no logging.** Anything worth reporting is an atomic counter in
//!   [`AudioIoStats`]; a non-RT thread reads it whenever it likes.
//! - **No blocking.** Meter pushes are non-blocking and drop on a full queue --
//!   a stale meter is invisible, a late buffer is a click.
//!
//! The one deliberate exception is [`RtState::ensure_realtime`], which promotes
//! the thread and disables denormals. It allocates and calls into the OS, and it
//! runs exactly once, on the first callback, before any audio is produced.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use mydaw_ipc_bridge::{MeterFrame, MeterProducer};
use mydaw_rt_core::{
    disable_denormals, drain_commands, AudioBuffers, ProcessCtx, RtCommandConsumer, RtGraphSlot,
    TransportClock, CHANNELS,
};

use crate::priority::RealtimeToken;

/// Counters the audio thread maintains for the rest of the app.
///
/// Atomics rather than logging: incrementing a relaxed counter is a handful of
/// instructions with no allocation and no lock, whereas `log::warn!` on the audio
/// thread can allocate, take the logger's lock and even touch a file.
#[derive(Debug, Default)]
pub struct AudioIoStats {
    blocks_processed: AtomicU64,
    truncated_blocks: AtomicU64,
    meter_frames_dropped: AtomicU64,
    stream_errors: AtomicU64,
    priority_promotion_failures: AtomicU64,
}

impl AudioIoStats {
    /// Blocks the callback has completed since the stream started.
    pub fn blocks_processed(&self) -> u64 {
        self.blocks_processed.load(Ordering::Relaxed)
    }

    /// Blocks the device asked for that were larger than `max_block`.
    ///
    /// Non-zero means audible glitching: the tail of those blocks was filled with
    /// silence. The fix is a larger `max_block` in [`crate::AudioIoConfig`], never
    /// an allocation on the audio thread.
    pub fn truncated_blocks(&self) -> u64 {
        self.truncated_blocks.load(Ordering::Relaxed)
    }

    /// Meter frames dropped because the queue was full (UI thread not draining).
    /// Harmless -- only the newest meter value is ever displayed.
    pub fn meter_frames_dropped(&self) -> u64 {
        self.meter_frames_dropped.load(Ordering::Relaxed)
    }

    /// Errors reported by cpal's error callback (device lost, xrun, ...).
    pub fn stream_errors(&self) -> u64 {
        self.stream_errors.load(Ordering::Relaxed)
    }

    /// Whether real-time priority promotion failed on the callback thread.
    /// Audio still runs, just at ordinary priority, so this is a warning sign for
    /// dropouts rather than an error.
    pub fn priority_promotion_failures(&self) -> u64 {
        self.priority_promotion_failures.load(Ordering::Relaxed)
    }

    /// Record a stream error. Called from cpal's error callback (audio thread).
    pub(crate) fn record_stream_error(&self) {
        self.stream_errors.fetch_add(1, Ordering::Relaxed);
    }
}

/// Constructor arguments for [`RtState`].
///
/// A struct rather than eight positional parameters: at this arity the call site
/// is unreadable and swapping two `usize`s compiles fine and sounds wrong.
pub struct RtStateParams {
    /// Receives graphs published by [`mydaw_rt_core::GraphHandle::install`].
    pub graph: RtGraphSlot,
    /// Receives transport commands from the UI.
    pub commands: RtCommandConsumer,
    /// Meter queue producer, if metering is wired up.
    pub meters: Option<MeterProducer>,
    /// Shared transport, advanced by the callback and read by everyone else.
    pub transport: Arc<TransportClock>,
    /// Device sample rate, in Hz.
    pub sample_rate: f64,
    /// Device output channel count (the interleaved stride).
    pub out_channels: usize,
    /// Largest block the callback will be asked for. Buffers are sized to this.
    pub max_block: usize,
    /// Shared counters.
    pub stats: Arc<AudioIoStats>,
}

/// Everything the audio callback owns.
///
/// Moved into the cpal closure at stream build time and dropped when the stream
/// is torn down. Nothing else may hold a reference to it -- that is the RT wall.
pub struct RtState {
    graph: RtGraphSlot,
    commands: RtCommandConsumer,
    meters: Option<MeterProducer>,
    transport: Arc<TransportClock>,
    buffers: AudioBuffers,
    sample_rate: f64,
    out_channels: usize,
    stats: Arc<AudioIoStats>,

    /// Set after the first callback, whether promotion succeeded or not: a failed
    /// promotion is not worth retrying every block.
    realtime_attempted: bool,
    /// Holds the promotion for the callback thread's lifetime; reverts on drop.
    priority: RealtimeToken,
}

impl RtState {
    /// Build the RT state. **Non-RT**: allocates the audio buffers.
    pub fn new(params: RtStateParams) -> Self {
        Self {
            graph: params.graph,
            commands: params.commands,
            meters: params.meters,
            transport: params.transport,
            buffers: AudioBuffers::new(params.max_block.max(1)),
            sample_rate: params.sample_rate,
            out_channels: params.out_channels,
            stats: params.stats,
            realtime_attempted: false,
            priority: RealtimeToken::empty(),
        }
    }

    /// Device sample rate this state was built for.
    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// Interleaved stride of the output buffer.
    pub fn out_channels(&self) -> usize {
        self.out_channels
    }

    /// Shared counters.
    pub fn stats(&self) -> &Arc<AudioIoStats> {
        &self.stats
    }

    /// Give the meter producer back so a later `start()` can reuse it.
    ///
    /// The producer can only be taken from the metering plugin once, so tearing
    /// the stream down must not swallow it -- otherwise the meters would go dead
    /// after the first stop/start cycle.
    pub fn take_meters(&mut self) -> Option<MeterProducer> {
        self.meters.take()
    }

    /// Promote this thread and disable denormals. Idempotent; call from the
    /// callback.
    ///
    /// # RT safety
    /// This is the one place in the callback that allocates and enters the kernel.
    /// It runs on the first block only, before the first sample is produced, which
    /// is the standard trade: a possible glitch on block 0 buys correct scheduling
    /// for every block after it. Denormal flushing is per-thread state, so it too
    /// has to be set here rather than on the thread that built the stream.
    pub fn ensure_realtime(&mut self, frames: usize) {
        if self.realtime_attempted {
            return;
        }
        self.realtime_attempted = true;

        disable_denormals();

        if !self
            .priority
            .promote(frames as u32, self.sample_rate as u32)
        {
            // Cannot log from here; the service reports the counter instead.
            self.stats
                .priority_promotion_failures
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Whether the callback thread is running at real-time priority.
    pub fn is_realtime(&self) -> bool {
        self.priority.is_promoted()
    }
}

/// Process one block: the entire body of the audio callback.
///
/// `output` is the device's interleaved buffer, `out_channels` samples per frame.
///
/// Order matters:
/// 1. **Swap in any pending graph** first, so a graph installed a moment ago is
///    heard in this block rather than the next.
/// 2. **Drain transport commands** before snapshotting, so a `Seek` issued with
///    the same message as a `SetPlaying` takes effect on the same block.
/// 3. **Process**, then interleave and measure in a single pass over the samples.
/// 4. **Advance the transport** by the frames actually produced, then publish the
///    meter frame carrying the post-advance position (what the playhead should
///    show once this block has been heard).
pub fn run_block(state: &mut RtState, output: &mut [f32]) {
    let out_channels = state.out_channels;
    if out_channels == 0 || output.is_empty() {
        return;
    }

    let requested = output.len() / out_channels;
    state.graph.poll_swap();
    drain_commands(&mut state.commands, &state.transport);

    let transport = state.transport.snapshot();

    // `ProcessCtx::new` clamps to the buffers' `max_block`, so `frames` may be
    // less than the device asked for. The tail is silenced below.
    let frames = {
        let mut ctx = ProcessCtx::new(&mut state.buffers, state.sample_rate, requested, transport);
        state.graph.process(&mut ctx);
        ctx.frames
    };

    if frames < requested {
        state.stats.truncated_blocks.fetch_add(1, Ordering::Relaxed);
    }

    let [left, right] = state.buffers.as_slices();
    let mut peak_l = 0.0_f32;
    let mut peak_r = 0.0_f32;
    let mut sum_sq_l = 0.0_f32;
    let mut sum_sq_r = 0.0_f32;

    for frame in 0..frames {
        let l = left[frame];
        let r = right[frame];

        peak_l = peak_l.max(l.abs());
        peak_r = peak_r.max(r.abs());
        sum_sq_l += l * l;
        sum_sq_r += r * r;

        let base = frame * out_channels;
        let slot = &mut output[base..base + out_channels];
        slot[0] = l;
        if out_channels > 1 {
            slot[1] = r;
        }
        // Surround device on a stereo engine: feed the first two channels and
        // leave the rest silent rather than smearing the mix across them.
        for extra in slot.iter_mut().skip(CHANNELS) {
            *extra = 0.0;
        }
    }

    // Only reached when the device asked for more than `max_block`.
    for sample in &mut output[frames * out_channels..] {
        *sample = 0.0;
    }

    state.transport.advance(frames);
    state.stats.blocks_processed.fetch_add(1, Ordering::Relaxed);

    if let Some(meters) = state.meters.as_mut() {
        let inv = if frames > 0 { 1.0 / frames as f32 } else { 0.0 };
        let frame = MeterFrame {
            peak_l,
            peak_r,
            rms_l: (sum_sq_l * inv).sqrt(),
            rms_r: (sum_sq_r * inv).sqrt(),
            // Post-advance: where the playhead stands once this block is audible.
            sample_pos: state.transport.snapshot().sample_pos,
            playing: transport.playing,
        };
        if meters.push(frame).is_err() {
            state
                .stats
                .meter_frames_dropped
                .fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_ipc_bridge::meter_channel;
    use mydaw_plugin_test_tone::{TestToneNode, DEFAULT_AMPLITUDE};
    use mydaw_rt_core::{graph_channel, rt_channel, GcThread, GraphSpec, RtCommand};
    use std::time::Instant;

    const SR: f64 = 48_000.0;
    const MAX_BLOCK: usize = 512;

    /// A test rig with every non-RT half kept around so the test can poke the RT
    /// state the same way the service does.
    struct Rig {
        state: RtState,
        graph: mydaw_rt_core::GraphHandle,
        commands: mydaw_rt_core::RtCommandProducer,
        throttle: mydaw_ipc_bridge::MeterThrottle,
        transport: Arc<TransportClock>,
        stats: Arc<AudioIoStats>,
        _gc: GcThread,
    }

    fn rig(out_channels: usize) -> Rig {
        let gc = GcThread::spawn();
        let (graph, slot) = graph_channel(4, gc.handle());
        let (cmd_tx, cmd_rx) = rt_channel::<RtCommand>(64);
        let (meter_tx, throttle) = meter_channel(64);
        let transport = Arc::new(TransportClock::new(SR));
        let stats = Arc::new(AudioIoStats::default());

        let state = RtState::new(RtStateParams {
            graph: slot,
            commands: cmd_rx,
            meters: Some(meter_tx),
            transport: Arc::clone(&transport),
            sample_rate: SR,
            out_channels,
            max_block: MAX_BLOCK,
            stats: Arc::clone(&stats),
        });

        Rig {
            state,
            graph,
            commands: cmd_tx,
            throttle,
            transport,
            stats,
            _gc: gc,
        }
    }

    fn install_test_tone(rig: &mut Rig) {
        rig.graph
            .install(GraphSpec::new().with(Box::new(TestToneNode::with_defaults(SR))))
            .expect("graph should install");
    }

    #[test]
    fn without_a_graph_the_output_is_silent_but_still_metered() {
        let mut rig = rig(2);
        let mut out = vec![7.0_f32; 128 * 2];
        run_block(&mut rig.state, &mut out);

        assert!(out.iter().all(|s| *s == 0.0), "no graph means silence");
        let frame = rig.throttle.poll(Instant::now()).expect("frame expected");
        assert_eq!(frame.peak_l, 0.0);
        assert_eq!(frame.rms_l, 0.0);
        assert_eq!(rig.stats.blocks_processed(), 1);
    }

    #[test]
    fn a_test_tone_graph_reaches_the_device_buffer_and_the_meters() {
        let mut rig = rig(2);
        install_test_tone(&mut rig);

        let mut out = vec![0.0_f32; 256 * 2];
        run_block(&mut rig.state, &mut out);

        assert!(
            out.iter().any(|s| s.abs() > 1e-6),
            "the tone should be audible in the interleaved buffer"
        );
        // Stereo tone: both interleaved lanes carry the same sample.
        for pair in out.chunks_exact(2) {
            assert_eq!(pair[0], pair[1]);
        }

        let frame = rig.throttle.poll(Instant::now()).expect("frame expected");
        assert!(frame.peak_l > 0.0 && frame.peak_l <= DEFAULT_AMPLITUDE + 1e-6);
        assert!(frame.rms_l > 0.0 && frame.rms_l < frame.peak_l);
        assert_eq!(frame.peak_l, frame.peak_r);
        assert!(!frame.playing, "transport is stopped until told otherwise");
    }

    #[test]
    fn transport_commands_are_applied_before_the_block_is_processed() {
        let mut rig = rig(2);
        install_test_tone(&mut rig);

        rig.commands.push(RtCommand::Seek(1_000)).unwrap();
        rig.commands.push(RtCommand::SetPlaying(true)).unwrap();

        let mut out = vec![0.0_f32; 128 * 2];
        run_block(&mut rig.state, &mut out);

        let snapshot = rig.transport.snapshot();
        assert!(snapshot.playing);
        assert_eq!(snapshot.sample_pos, 1_000 + 128, "seek then advance");

        let frame = rig.throttle.poll(Instant::now()).unwrap();
        assert!(frame.playing);
        assert_eq!(
            frame.sample_pos, 1_128,
            "meter carries the post-block position"
        );
    }

    #[test]
    fn a_stopped_transport_does_not_advance() {
        let mut rig = rig(2);
        install_test_tone(&mut rig);
        let mut out = vec![0.0_f32; 128 * 2];
        run_block(&mut rig.state, &mut out);
        run_block(&mut rig.state, &mut out);
        assert_eq!(rig.transport.snapshot().sample_pos, 0);
    }

    #[test]
    fn a_graph_installed_between_blocks_is_picked_up_on_the_next_one() {
        let mut rig = rig(2);
        let mut out = vec![0.0_f32; 128 * 2];

        run_block(&mut rig.state, &mut out);
        assert!(out.iter().all(|s| *s == 0.0));

        install_test_tone(&mut rig);
        run_block(&mut rig.state, &mut out);
        assert!(out.iter().any(|s| s.abs() > 1e-6));
    }

    #[test]
    fn oversized_blocks_are_truncated_and_counted_not_allocated() {
        let mut rig = rig(2);
        install_test_tone(&mut rig);

        // Twice `max_block`: the tail must be silence, and the glitch recorded.
        let mut out = vec![1.0_f32; MAX_BLOCK * 2 * 2];
        run_block(&mut rig.state, &mut out);

        assert!(out[..MAX_BLOCK * 2].iter().any(|s| s.abs() > 1e-6));
        assert!(
            out[MAX_BLOCK * 2..].iter().all(|s| *s == 0.0),
            "the tail past max_block must be silenced, never left stale"
        );
        assert_eq!(rig.stats.truncated_blocks(), 1);
    }

    #[test]
    fn a_mono_device_gets_the_left_channel() {
        let mut rig = rig(1);
        install_test_tone(&mut rig);
        let mut out = vec![0.0_f32; 128];
        run_block(&mut rig.state, &mut out);
        assert!(out.iter().any(|s| s.abs() > 1e-6));
    }

    #[test]
    fn surround_devices_get_silence_past_the_stereo_pair() {
        let mut rig = rig(6);
        install_test_tone(&mut rig);
        let mut out = vec![9.0_f32; 64 * 6];
        run_block(&mut rig.state, &mut out);

        for frame in out.chunks_exact(6) {
            assert_eq!(frame[0], frame[1]);
            assert!(
                frame[2..].iter().all(|s| *s == 0.0),
                "extra channels must be silent, not smeared"
            );
        }
    }

    #[test]
    fn a_full_meter_queue_drops_frames_instead_of_blocking() {
        let gc = GcThread::spawn();
        let (graph, slot) = graph_channel(4, gc.handle());
        let (_cmd_tx, cmd_rx) = rt_channel::<RtCommand>(8);
        // Capacity 1: the second block has nowhere to put its frame.
        let (meter_tx, _throttle) = meter_channel(1);
        let stats = Arc::new(AudioIoStats::default());
        let mut state = RtState::new(RtStateParams {
            graph: slot,
            commands: cmd_rx,
            meters: Some(meter_tx),
            transport: Arc::new(TransportClock::new(SR)),
            sample_rate: SR,
            out_channels: 2,
            max_block: MAX_BLOCK,
            stats: Arc::clone(&stats),
        });
        drop(graph);

        let mut out = vec![0.0_f32; 64 * 2];
        run_block(&mut state, &mut out);
        run_block(&mut state, &mut out);
        run_block(&mut state, &mut out);

        assert_eq!(stats.blocks_processed(), 3);
        assert!(stats.meter_frames_dropped() >= 1);
    }

    #[test]
    fn meters_are_handed_back_when_the_state_is_torn_down() {
        let mut rig = rig(2);
        assert!(rig.state.take_meters().is_some());
        assert!(rig.state.take_meters().is_none());

        // Still processes fine without a meter tap.
        let mut out = vec![0.0_f32; 64 * 2];
        run_block(&mut rig.state, &mut out);
        assert_eq!(rig.stats.blocks_processed(), 1);
    }

    #[test]
    fn an_empty_output_buffer_is_a_no_op() {
        let mut rig = rig(2);
        let mut out: [f32; 0] = [];
        run_block(&mut rig.state, &mut out);
        assert_eq!(rig.stats.blocks_processed(), 0);
    }
}
