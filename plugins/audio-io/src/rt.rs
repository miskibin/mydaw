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
use mydaw_plugin_debug_tap::TapRtSide;
use mydaw_plugin_sequencer::{PatternSlot, Scheduler, TransportWindow};
use mydaw_rt_core::{
    disable_denormals, drain_commands, drain_notes, AudioBuffers, NoteConsumer, NoteEventBuffer,
    ProcessCtx, RtCommandConsumer, RtGraphSlot, TransportClock, CHANNELS,
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
    note_events_dropped: AtomicU64,
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

    /// Note events dropped because the block's event buffer was full.
    ///
    /// Audible: a dropped note-on is a note that never sounds, and a dropped
    /// note-off is a note that never stops. Counts both sources that fill the
    /// buffer -- live input and the sequencer -- because the buffer they
    /// overflow is the same one and the symptom is identical.
    ///
    /// The buffer is sized to the note queue's capacity, so on the live path
    /// this can only be non-zero if something is generating more than
    /// [`AudioIoConfig::note_queue_capacity`](crate::AudioIoConfig::note_queue_capacity)
    /// events in a single block; a 16-row pattern contributes at most 32.
    pub fn note_events_dropped(&self) -> u64 {
        self.note_events_dropped.load(Ordering::Relaxed)
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
    /// Receives live note events (a keyboard, a pad, the UI's piano).
    pub notes: NoteConsumer,
    /// Events the block's note buffer can hold. Sized to the note queue's
    /// capacity so a full queue always fits in one block.
    pub note_capacity: usize,
    /// Receives patterns published by
    /// [`PatternHandle::install`](mydaw_plugin_sequencer::PatternHandle::install).
    pub pattern: PatternSlot,
    /// Meter queue producer, if metering is wired up.
    pub meters: Option<MeterProducer>,
    /// Debug capture tap, if `debug-tap` is wired up. Costs one relaxed atomic
    /// load per block while nobody is capturing.
    pub debug_tap: Option<TapRtSide>,
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
    notes: NoteConsumer,
    /// This block's note events. Allocated once, refilled every block.
    note_events: NoteEventBuffer,
    /// The pattern the sequencer is playing, swapped in like the graph.
    pattern: PatternSlot,
    /// Turns that pattern into events. Fixed-size state; see the sequencer's
    /// `schedule` module for the timing argument.
    scheduler: Scheduler,
    meters: Option<MeterProducer>,
    debug_tap: Option<TapRtSide>,
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
    ///
    /// Also publishes the negotiated sample rate to the debug tap. The capture
    /// service has no other way to learn it -- it never sees the device -- and
    /// doing it here means every stream, including a restart on a different
    /// device, reports the rate its samples were actually produced at.
    pub fn new(params: RtStateParams) -> Self {
        if let Some(tap) = params.debug_tap.as_ref() {
            tap.set_sample_rate(params.sample_rate);
        }
        Self {
            graph: params.graph,
            commands: params.commands,
            notes: params.notes,
            note_events: NoteEventBuffer::new(params.note_capacity),
            pattern: params.pattern,
            scheduler: Scheduler::new(),
            meters: params.meters,
            debug_tap: params.debug_tap,
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

    /// Give the debug tap back so a later `start()` can reuse it.
    ///
    /// Same reasoning as [`take_meters`](Self::take_meters): the tap's RT side
    /// is handed out once, so swallowing it on teardown would leave the capture
    /// harness dead for the rest of the process.
    pub fn take_debug_tap(&mut self) -> Option<TapRtSide> {
        self.debug_tap.take()
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

    /// Note-offs the sequencer still owes. Diagnostics and tests: a non-zero
    /// value after the transport has stopped would mean a stuck note.
    pub fn pending_note_offs(&self) -> usize {
        self.scheduler.pending()
    }

    /// Whether a pattern has reached the audio thread yet.
    pub fn has_pattern(&self) -> bool {
        self.pattern.has_pattern()
    }
}

/// Process one block: the entire body of the audio callback.
///
/// `output` is the device's interleaved buffer, `out_channels` samples per frame.
///
/// Order matters:
/// 1. **Swap in any pending graph and pattern** first, so an edit made a moment
///    ago is heard in this block rather than the next.
/// 2. **Drain transport commands** before snapshotting, so a `Seek` issued with
///    the same message as a `SetPlaying` takes effect on the same block -- and
///    so a tempo change is already in the snapshot the scheduler reads.
/// 3. **Drain live notes** into the block's event buffer, all at frame offset 0:
///    a key pressed between callbacks sounds from the first sample of the next
///    block, which is the least latency a block-based engine can offer.
/// 4. **Schedule the pattern** into the same buffer, at real frame offsets.
///    After the live notes, so that at a shared offset the key the user is
///    pressing right now is not pre-empted by the grid (the buffer's insert is
///    stable, so push order *is* the tie-break -- see the sequencer's `schedule`
///    module).
/// 5. **Process**, then interleave and measure in a single pass over the samples.
/// 6. **Advance the transport** by the frames actually produced, then publish the
///    meter frame carrying the post-advance position (what the playhead should
///    show once this block has been heard).
pub fn run_block(state: &mut RtState, output: &mut [f32]) {
    let out_channels = state.out_channels;
    if out_channels == 0 || output.is_empty() {
        return;
    }

    let requested = output.len() / out_channels;
    state.graph.poll_swap();
    state.pattern.poll_swap();
    drain_commands(&mut state.commands, &state.transport);

    // Live input has no position within the block, so every event lands at
    // offset 0.
    state.note_events.clear();
    let mut dropped = drain_notes(&mut state.notes, &mut state.note_events, 0);

    let transport = state.transport.snapshot();

    // The graph will be asked for `requested` frames but can only produce
    // `max_block` of them, and the transport advances by what it produced. The
    // scheduler has to agree, or a truncated block would leave a gap in the
    // grid; so the window is the clamped count, computed here rather than read
    // back out of `ProcessCtx`.
    let frames = requested.min(state.buffers.max_block());
    dropped += state.scheduler.schedule(
        state.pattern.current(),
        &TransportWindow {
            start_sample: transport.sample_pos,
            frames,
            sample_rate: state.sample_rate,
            bpm: transport.bpm,
            playing: transport.playing,
        },
        &mut state.note_events,
    );

    if dropped > 0 {
        state
            .stats
            .note_events_dropped
            .fetch_add(dropped as u64, Ordering::Relaxed);
    }

    // `ProcessCtx::with_events` clamps to the buffers' `max_block`, which is the
    // same clamp `frames` already applied.
    let frames = {
        let mut ctx = ProcessCtx::with_events(
            &mut state.buffers,
            state.sample_rate,
            frames,
            transport,
            state.note_events.as_slice(),
        );
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

    // The debug capture tap sees exactly what the graph produced, before the
    // device's channel layout is applied: a capture is always stereo, even on a
    // 5.1 card. Disarmed (the normal case) this is one relaxed atomic load.
    //
    // Disjoint field borrows: `buffers` is read while `debug_tap` is mutated.
    if let Some(tap) = state.debug_tap.as_mut() {
        let [left, right] = state.buffers.as_slices();
        tap.push_block(left, right);
    }

    state.transport.advance(frames);
    state.stats.blocks_processed.fetch_add(1, Ordering::Relaxed);

    if let Some(meters) = state.meters.as_mut() {
        let inv = if frames > 0 { 1.0 / frames as f32 } else { 0.0 };
        let mut frame = MeterFrame {
            peak_l,
            peak_r,
            rms_l: (sum_sq_l * inv).sqrt(),
            rms_r: (sum_sq_r * inv).sqrt(),
            // Post-advance: where the playhead stands once this block is audible.
            sample_pos: state.transport.snapshot().sample_pos,
            playing: transport.playing,
            ..MeterFrame::SILENT
        };
        // The per-track peaks the mixer measured during its sum pass. A fixed
        // -size copy out of the graph -- no second pass over the audio, and no
        // queue of its own.
        frame.set_track_peaks(&state.graph.track_peaks());
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
    use mydaw_plugin_debug_tap::{debug_tap_channel, TapConsumer};
    use mydaw_plugin_sequencer::{
        pattern_channel, Pattern, PatternHandle, PatternRow, Step, PATTERN_QUEUE_CAPACITY,
    };
    use mydaw_plugin_test_tone::{TestToneNode, DEFAULT_AMPLITUDE};
    use mydaw_rt_core::{
        graph_channel, note_channel, rt_channel, GcThread, GraphSpec, LiveNote, NoteEvent,
        NoteKind, NoteProducer, RtCommand, RtNode, DEFAULT_BPM,
    };
    use std::time::Instant;

    const SR: f64 = 48_000.0;
    const MAX_BLOCK: usize = 512;
    /// Samples, not frames. Comfortably larger than any block these tests
    /// push, so a capture assertion never fails for lack of ring space.
    const TAP_CAPACITY: usize = 16_384;

    /// A test rig with every non-RT half kept around so the test can poke the RT
    /// state the same way the service does.
    struct Rig {
        state: RtState,
        graph: mydaw_rt_core::GraphHandle,
        commands: mydaw_rt_core::RtCommandProducer,
        notes: NoteProducer,
        pattern: PatternHandle,
        throttle: mydaw_ipc_bridge::MeterThrottle,
        tap: TapConsumer,
        transport: Arc<TransportClock>,
        stats: Arc<AudioIoStats>,
        _gc: GcThread,
    }

    fn rig(out_channels: usize) -> Rig {
        rig_with_note_capacity(out_channels, 64)
    }

    fn rig_with_note_capacity(out_channels: usize, note_capacity: usize) -> Rig {
        let gc = GcThread::spawn();
        let (graph, slot) = graph_channel(4, gc.handle());
        let (cmd_tx, cmd_rx) = rt_channel::<RtCommand>(64);
        let (note_tx, note_rx) = note_channel(note_capacity);
        let (meter_tx, throttle) = meter_channel(64);
        let (pattern_tx, pattern_rx) = pattern_channel(PATTERN_QUEUE_CAPACITY, gc.handle());
        let (tap_rt, tap) = debug_tap_channel(TAP_CAPACITY);
        let transport = Arc::new(TransportClock::new(SR));
        let stats = Arc::new(AudioIoStats::default());

        let state = RtState::new(RtStateParams {
            graph: slot,
            commands: cmd_rx,
            notes: note_rx,
            note_capacity,
            pattern: pattern_rx,
            meters: Some(meter_tx),
            debug_tap: Some(tap_rt),
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
            notes: note_tx,
            pattern: pattern_tx,
            throttle,
            tap,
            transport,
            stats,
            _gc: gc,
        }
    }

    /// Records the note events each block hands it, so a test can assert what
    /// reached the graph without involving a plugin.
    ///
    /// It locks a mutex inside `process`, which a real node must never do; the
    /// node is inside the graph and therefore unreachable from the test thread
    /// any other way. Nothing here runs on a real audio callback.
    #[derive(Clone, Default)]
    struct NoteSpy(Arc<parking_lot::Mutex<Vec<Vec<NoteEvent>>>>);

    impl NoteSpy {
        fn blocks(&self) -> Vec<Vec<NoteEvent>> {
            self.0.lock().clone()
        }
    }

    impl RtNode for NoteSpy {
        fn process(&mut self, ctx: &mut ProcessCtx) {
            self.0.lock().push(ctx.events.to_vec());
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
    fn the_meter_frame_carries_one_peak_per_track() {
        // The per-track meter path, end to end on the RT side: the mixer
        // measures each track during its sum pass, the graph keeps the block in
        // a fixed array, and `run_block` copies it into the frame the UI reads.
        use mydaw_rt_core::{GraphLimits, MixerParams, TrackSpec};

        struct ConstNode(f32);
        impl RtNode for ConstNode {
            fn process(&mut self, ctx: &mut ProcessCtx) {
                for channel in ctx.buffers.as_mut_slices() {
                    channel.fill(self.0);
                }
            }
        }

        let mut rig = rig(2);
        let params = Arc::new(MixerParams::new());
        params.set_track_gain(1, 0.0);
        rig.graph
            .install(
                GraphSpec::new()
                    .with_limits(GraphLimits {
                        sample_rate: SR,
                        max_block: MAX_BLOCK,
                        max_events: 64,
                    })
                    .with_params(Arc::clone(&params))
                    .with_track(TrackSpec::new(Box::new(ConstNode(0.5))))
                    .with_track(TrackSpec::new(Box::new(ConstNode(0.25)))),
            )
            .unwrap();

        let mut out = vec![0.0_f32; 128 * 2];
        run_block(&mut rig.state, &mut out);

        let frame = rig.throttle.poll(Instant::now()).expect("frame expected");
        assert_eq!(frame.track_count, 2, "only the tracks that exist");
        assert_eq!(frame.track_meters().len(), 2);
        assert_eq!(frame.tracks[0].peak_l, 0.5);
        assert_eq!(frame.tracks[0].peak_r, 0.5);
        assert_eq!(
            frame.tracks[1],
            mydaw_ipc_bridge::TrackMeter::SILENT,
            "track 1's fader is at zero, and a channel meter is post-fader"
        );
        assert_eq!(
            frame.peak_l, 0.5,
            "the master carries only what was audible"
        );
    }

    #[test]
    fn a_graph_with_no_tracks_reports_no_track_meters() {
        let mut rig = rig(2);
        rig.graph.install(GraphSpec::new()).unwrap();
        let mut out = vec![0.0_f32; 64 * 2];
        run_block(&mut rig.state, &mut out);

        let frame = rig.throttle.poll(Instant::now()).unwrap();
        assert_eq!(frame.track_count, 0);
        assert!(frame.track_meters().is_empty());
        assert!(out.iter().all(|s| *s == 0.0));
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
        let (_note_tx, note_rx) = note_channel(8);
        let (_pattern_tx, pattern_rx) = pattern_channel(2, gc.handle());
        // Capacity 1: the second block has nowhere to put its frame.
        let (meter_tx, _throttle) = meter_channel(1);
        let stats = Arc::new(AudioIoStats::default());
        let mut state = RtState::new(RtStateParams {
            graph: slot,
            commands: cmd_rx,
            notes: note_rx,
            note_capacity: 8,
            pattern: pattern_rx,
            meters: Some(meter_tx),
            debug_tap: None,
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
    fn the_debug_tap_learns_the_sample_rate_when_the_state_is_built() {
        let rig = rig(2);
        assert_eq!(
            rig.tap.shared().sample_rate(),
            SR,
            "the capture service has no other way to learn the device rate"
        );
    }

    #[test]
    fn a_disarmed_debug_tap_receives_nothing() {
        let mut rig = rig(2);
        install_test_tone(&mut rig);
        let mut out = vec![0.0_f32; 128 * 2];
        for _ in 0..8 {
            run_block(&mut rig.state, &mut out);
        }
        assert_eq!(rig.tap.available(), 0, "capture must be opt-in");
        assert_eq!(rig.tap.shared().dropped(), 0);
    }

    #[test]
    fn an_armed_debug_tap_receives_the_post_graph_stereo_bus() {
        let mut rig = rig(2);
        install_test_tone(&mut rig);
        rig.tap.shared().arm();

        let mut out = vec![0.0_f32; 128 * 2];
        run_block(&mut rig.state, &mut out);
        rig.tap.shared().disarm();

        let mut captured = Vec::new();
        rig.tap.drain_into(&mut captured, usize::MAX);
        assert_eq!(captured.len(), 128 * 2, "one stereo frame per device frame");
        assert_eq!(
            captured, out,
            "on a stereo device the capture is the device buffer, sample for sample"
        );
        assert!(captured.iter().any(|s| s.abs() > 1e-6));
    }

    #[test]
    fn the_debug_tap_stays_stereo_on_a_surround_device() {
        let mut rig = rig(6);
        install_test_tone(&mut rig);
        rig.tap.shared().arm();

        let mut out = vec![0.0_f32; 64 * 6];
        run_block(&mut rig.state, &mut out);
        rig.tap.shared().disarm();

        let mut captured = Vec::new();
        rig.tap.drain_into(&mut captured, usize::MAX);
        assert_eq!(
            captured.len(),
            64 * 2,
            "the tap mirrors the engine bus, not the device layout"
        );
        for (frame, pair) in captured.chunks_exact(2).enumerate() {
            assert_eq!(pair[0], out[frame * 6]);
            assert_eq!(pair[1], out[frame * 6 + 1]);
        }
    }

    #[test]
    fn the_debug_tap_captures_exactly_the_frames_that_were_processed() {
        let mut rig = rig(2);
        install_test_tone(&mut rig);
        rig.tap.shared().arm();

        // Twice max_block: the graph only produced MAX_BLOCK frames, and that
        // is what the capture must contain -- not the silenced tail.
        let mut out = vec![0.0_f32; MAX_BLOCK * 2 * 2];
        run_block(&mut rig.state, &mut out);
        rig.tap.shared().disarm();

        let mut captured = Vec::new();
        rig.tap.drain_into(&mut captured, usize::MAX);
        assert_eq!(captured.len(), MAX_BLOCK * 2);
    }

    #[test]
    fn the_debug_tap_is_handed_back_when_the_state_is_torn_down() {
        let mut rig = rig(2);
        assert!(rig.state.take_debug_tap().is_some());
        assert!(rig.state.take_debug_tap().is_none());

        // Still processes fine without a debug tap.
        let mut out = vec![0.0_f32; 64 * 2];
        run_block(&mut rig.state, &mut out);
        assert_eq!(rig.stats.blocks_processed(), 1);
    }

    #[test]
    fn a_full_tap_ring_drops_samples_instead_of_stalling_the_callback() {
        let gc = GcThread::spawn();
        let (graph, slot) = graph_channel(4, gc.handle());
        let (_cmd_tx, cmd_rx) = rt_channel::<RtCommand>(8);
        let (_note_tx, note_rx) = note_channel(8);
        let (_pattern_tx, pattern_rx) = pattern_channel(2, gc.handle());
        let (meter_tx, _throttle) = meter_channel(64);
        // Room for 32 stereo frames; each block pushes 128.
        let (tap_rt, tap) = debug_tap_channel(64);
        let stats = Arc::new(AudioIoStats::default());
        let mut state = RtState::new(RtStateParams {
            graph: slot,
            commands: cmd_rx,
            notes: note_rx,
            note_capacity: 8,
            pattern: pattern_rx,
            meters: Some(meter_tx),
            debug_tap: Some(tap_rt),
            transport: Arc::new(TransportClock::new(SR)),
            sample_rate: SR,
            out_channels: 2,
            max_block: MAX_BLOCK,
            stats: Arc::clone(&stats),
        });
        drop(graph);
        tap.shared().arm();

        let mut out = vec![0.0_f32; 128 * 2];
        for _ in 0..4 {
            run_block(&mut state, &mut out);
        }

        assert_eq!(stats.blocks_processed(), 4, "the callback never stalled");
        assert!(tap.shared().dropped() > 0, "the overflow must be counted");
    }

    #[test]
    fn live_notes_reach_the_graph_at_the_start_of_the_next_block() {
        let mut rig = rig(2);
        let spy = NoteSpy::default();
        rig.graph
            .install(GraphSpec::new().with(Box::new(spy.clone())))
            .unwrap();

        let mut out = vec![0.0_f32; 128 * 2];
        // Block 1: nothing queued.
        run_block(&mut rig.state, &mut out);

        rig.notes.push(LiveNote::note_on(69, 0.75)).unwrap();
        rig.notes.push(LiveNote::note_on(72, 1.0)).unwrap();
        run_block(&mut rig.state, &mut out);

        // Block 3: the queue is empty again, so the events must be gone.
        run_block(&mut rig.state, &mut out);

        let blocks = spy.blocks();
        assert_eq!(blocks.len(), 3);
        assert!(blocks[0].is_empty(), "no notes were queued for block 1");
        assert_eq!(
            blocks[1],
            vec![
                NoteEvent::note_on(0, 69, 0.75),
                NoteEvent::note_on(0, 72, 1.0)
            ],
            "live notes arrive at frame 0, in the order they were sent"
        );
        assert!(
            blocks[2].is_empty(),
            "the buffer is cleared every block; events must not linger"
        );
        assert_eq!(rig.stats.note_events_dropped(), 0);
    }

    #[test]
    fn notes_are_delivered_whether_or_not_the_transport_is_running() {
        // Playing an instrument with the sequencer stopped is the normal way to
        // audition one, so the note path must not be gated on the transport.
        let mut rig = rig(2);
        let spy = NoteSpy::default();
        rig.graph
            .install(GraphSpec::new().with(Box::new(spy.clone())))
            .unwrap();

        let mut out = vec![0.0_f32; 64 * 2];
        rig.notes.push(LiveNote::note_on(60, 1.0)).unwrap();
        run_block(&mut rig.state, &mut out);
        assert!(!rig.transport.snapshot().playing);

        rig.commands.push(RtCommand::SetPlaying(true)).unwrap();
        rig.notes.push(LiveNote::note_off(60)).unwrap();
        run_block(&mut rig.state, &mut out);

        let blocks = spy.blocks();
        assert_eq!(blocks[0], vec![NoteEvent::note_on(0, 60, 1.0)]);
        assert_eq!(blocks[1], vec![NoteEvent::note_off(0, 60)]);
    }

    #[test]
    fn a_note_burst_larger_than_the_block_buffer_is_dropped_and_counted() {
        // Queue deeper than the block buffer: only the engine's own config keeps
        // these equal, so the overflow branch has to be real and counted.
        let mut rig = rig_with_note_capacity(2, 2);
        let (mut tx, rx) = note_channel(16);
        rig.state.notes = rx;
        for key in 60..=64 {
            tx.push(LiveNote::note_on(key, 1.0)).unwrap();
        }

        let spy = NoteSpy::default();
        rig.graph
            .install(GraphSpec::new().with(Box::new(spy.clone())))
            .unwrap();
        let mut out = vec![0.0_f32; 64 * 2];
        run_block(&mut rig.state, &mut out);

        assert_eq!(spy.blocks()[0].len(), 2, "the buffer holds what it can");
        assert_eq!(rig.stats.note_events_dropped(), 3);
        assert_eq!(rig.stats.blocks_processed(), 1, "the callback carried on");
    }

    #[test]
    fn an_empty_output_buffer_is_a_no_op() {
        let mut rig = rig(2);
        let mut out: [f32; 0] = [];
        run_block(&mut rig.state, &mut out);
        assert_eq!(rig.stats.blocks_processed(), 0);
    }

    // ----------------------------------------------------------- sequencer

    /// A pattern with one row on `key` hitting `steps`.
    fn pattern_on(key: u8, steps: &[usize]) -> Pattern {
        let mut pattern = Pattern::silent();
        pattern.rows.push(PatternRow::empty(key).with_steps(steps));
        pattern
    }

    /// Install `spy` as the whole graph and start the transport.
    fn play_with_spy(rig: &mut Rig, spy: &NoteSpy) {
        rig.graph
            .install(GraphSpec::new().with(Box::new(spy.clone())))
            .unwrap();
        rig.commands.push(RtCommand::SetPlaying(true)).unwrap();
    }

    /// Every event the spy saw, as `(absolute sample, event)`.
    ///
    /// Valid only for a run that played from sample 0 in equal blocks, which is
    /// how these tests drive it.
    fn absolute(spy: &NoteSpy, frames: usize) -> Vec<(u64, NoteEvent)> {
        spy.blocks()
            .into_iter()
            .enumerate()
            .flat_map(|(block, events)| {
                events.into_iter().map(move |event| {
                    (
                        (block * frames) as u64 + u64::from(event.frame_offset),
                        event,
                    )
                })
            })
            .collect()
    }

    #[test]
    fn a_pattern_reaches_the_graph_at_the_sample_the_grid_says() {
        // The whole point of the step, through the real callback: a four-on-the
        // -floor kick lands on samples 0, 22154, 44308, 66462 at 130 BPM.
        let mut rig = rig(2);
        let spy = NoteSpy::default();
        play_with_spy(&mut rig, &spy);
        rig.pattern
            .install(pattern_on(36, &[0, 4, 8, 12]).compile())
            .unwrap();

        let frames = 256;
        let mut out = vec![0.0_f32; frames * 2];
        // Two bars at 130 BPM is 177 231 samples.
        for _ in 0..700 {
            run_block(&mut rig.state, &mut out);
        }

        let step_len = mydaw_plugin_sequencer::step_len_samples(SR, DEFAULT_BPM);
        let onsets: Vec<u64> = absolute(&spy, frames)
            .into_iter()
            .filter(|(_, e)| e.kind.is_note_on())
            .map(|(at, _)| at)
            .collect();
        let expected: Vec<u64> = (0..8)
            .map(|hit| mydaw_plugin_sequencer::step_boundary(hit * 4, step_len))
            .collect();
        assert!(onsets.len() >= 8, "two bars is eight kicks, got {onsets:?}");
        assert_eq!(&onsets[..8], &expected[..], "two bars of four-on-the-floor");
        assert_eq!(rig.stats.note_events_dropped(), 0);
    }

    #[test]
    fn a_stopped_transport_produces_no_pattern_events_and_leaves_nothing_owed() {
        let mut rig = rig(2);
        let spy = NoteSpy::default();
        rig.graph
            .install(GraphSpec::new().with(Box::new(spy.clone())))
            .unwrap();
        rig.pattern
            .install(pattern_on(36, &[0, 4, 8, 12]).compile())
            .unwrap();

        let mut out = vec![0.0_f32; 256 * 2];
        for _ in 0..20 {
            run_block(&mut rig.state, &mut out);
        }
        assert!(
            spy.blocks().iter().all(Vec::is_empty),
            "the grid must be silent until Play is pressed"
        );

        // Play, then stop: the stopping block releases what is owed.
        rig.commands.push(RtCommand::SetPlaying(true)).unwrap();
        run_block(&mut rig.state, &mut out);
        assert_eq!(rig.state.pending_note_offs(), 1, "the kick's gate is open");

        rig.commands.push(RtCommand::SetPlaying(false)).unwrap();
        run_block(&mut rig.state, &mut out);
        assert_eq!(
            rig.state.pending_note_offs(),
            0,
            "Stop must not leave a note hanging"
        );
        let last = spy.blocks().pop().unwrap();
        assert_eq!(last.len(), 1);
        assert_eq!(last[0].kind, NoteKind::NoteOff { key: 36 });
        assert_eq!(last[0].frame_offset, 0);
    }

    #[test]
    fn a_tempo_change_moves_the_grid_from_the_block_it_arrives_on() {
        let mut rig = rig(2);
        let spy = NoteSpy::default();
        play_with_spy(&mut rig, &spy);
        rig.pattern.install(pattern_on(36, &[0]).compile()).unwrap();
        // Sent with the same message as Play: both are drained before the
        // snapshot, so the very first block already runs at the new tempo.
        rig.commands.push(RtCommand::SetBpm(260.0)).unwrap();

        let frames = 256;
        let mut out = vec![0.0_f32; frames * 2];
        for _ in 0..200 {
            run_block(&mut rig.state, &mut out);
        }
        assert_eq!(rig.transport.snapshot().bpm, 260.0);

        let step_len = mydaw_plugin_sequencer::step_len_samples(SR, 260.0);
        let bar = mydaw_plugin_sequencer::step_boundary(16, step_len);
        let onsets: Vec<u64> = absolute(&spy, frames)
            .into_iter()
            .filter(|(_, e)| e.kind.is_note_on())
            .map(|(at, _)| at)
            .collect();
        assert_eq!(onsets[0], 0);
        assert_eq!(onsets[1], bar, "the bar is half as long at twice the tempo");
    }

    #[test]
    fn live_notes_and_pattern_events_share_one_buffer_with_live_input_first() {
        // Both land at frame 0 of the same block: the documented tie-break says
        // the key the user is pressing wins.
        let mut rig = rig(2);
        let spy = NoteSpy::default();
        play_with_spy(&mut rig, &spy);
        rig.pattern.install(pattern_on(36, &[0]).compile()).unwrap();
        rig.notes.push(LiveNote::note_on(69, 0.5)).unwrap();

        let mut out = vec![0.0_f32; 256 * 2];
        run_block(&mut rig.state, &mut out);

        let first = &spy.blocks()[0];
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].kind.key(), Some(69), "live input first");
        assert_eq!(first[1].kind.key(), Some(36), "then the grid");
        assert!(first.iter().all(|e| e.frame_offset == 0));
    }

    #[test]
    fn a_pattern_edited_mid_play_is_heard_from_the_next_block() {
        let mut rig = rig(2);
        let spy = NoteSpy::default();
        play_with_spy(&mut rig, &spy);
        rig.pattern.install(pattern_on(36, &[0]).compile()).unwrap();

        let frames = 256;
        let mut out = vec![0.0_f32; frames * 2];
        run_block(&mut rig.state, &mut out);
        assert_eq!(spy.blocks()[0].len(), 1, "step 0 of the first pattern");

        // Swap to a different row while the transport runs. The old pattern is
        // dropped on this thread by `poll_swap`, which under basedrop is a
        // hand-off to the collector, not a free.
        rig.pattern
            .install(pattern_on(38, &(0..16).collect::<Vec<_>>()).compile())
            .unwrap();
        for _ in 0..200 {
            run_block(&mut rig.state, &mut out);
        }

        let keys: Vec<u8> = absolute(&spy, frames)
            .into_iter()
            .filter(|(at, e)| *at > 0 && e.kind.is_note_on())
            .filter_map(|(_, e)| e.kind.key())
            .collect();
        assert!(!keys.is_empty());
        assert!(
            keys.iter().all(|key| *key == 38),
            "everything after the swap must come from the new pattern"
        );
        assert!(rig.state.has_pattern());
    }

    #[test]
    fn a_pattern_with_more_events_than_the_block_buffer_is_counted_not_grown() {
        // Sixteen rows all hitting every step, into a four-event buffer.
        let mut rig = rig_with_note_capacity(2, 4);
        let spy = NoteSpy::default();
        play_with_spy(&mut rig, &spy);

        let mut pattern = Pattern::silent();
        for row in 0..mydaw_plugin_sequencer::MAX_ROWS {
            let mut pattern_row = PatternRow::empty(36 + row as u8);
            for step in 0..mydaw_plugin_sequencer::STEPS {
                pattern_row.steps[step] = Step::accent();
            }
            pattern.rows.push(pattern_row);
        }
        rig.pattern.install(pattern.compile()).unwrap();

        let mut out = vec![0.0_f32; 256 * 2];
        run_block(&mut rig.state, &mut out);

        assert_eq!(spy.blocks()[0].len(), 4, "the buffer holds what it can");
        assert!(
            rig.stats.note_events_dropped() >= 12,
            "the rest must be counted, not silently lost"
        );
        assert_eq!(rig.stats.blocks_processed(), 1, "the callback carried on");
    }

    #[test]
    fn a_truncated_block_does_not_leave_a_gap_in_the_grid() {
        // The device asks for more than `max_block`, so only `max_block` frames
        // are produced and the transport advances by that. The scheduler's
        // window has to agree, or the steps inside the discarded tail would be
        // scheduled once and never heard.
        let mut rig = rig(2);
        let spy = NoteSpy::default();
        play_with_spy(&mut rig, &spy);
        rig.pattern
            .install(pattern_on(36, &(0..16).collect::<Vec<_>>()).compile())
            .unwrap();

        // Twice max_block, every block.
        let mut out = vec![0.0_f32; MAX_BLOCK * 2 * 2];
        for _ in 0..200 {
            run_block(&mut rig.state, &mut out);
        }
        assert!(
            rig.stats.truncated_blocks() > 0,
            "the setup really truncated"
        );

        let step_len = mydaw_plugin_sequencer::step_len_samples(SR, DEFAULT_BPM);
        let onsets: Vec<u64> = absolute(&spy, MAX_BLOCK)
            .into_iter()
            .filter(|(_, e)| e.kind.is_note_on())
            .map(|(at, _)| at)
            .collect();
        for (index, onset) in onsets.iter().enumerate() {
            assert_eq!(
                *onset,
                mydaw_plugin_sequencer::step_boundary(index as u64, step_len),
                "step {index} moved"
            );
        }
        assert!(onsets.len() > 16, "more than a bar was rendered");
    }
}
