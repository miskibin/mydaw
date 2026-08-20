//! [`AudioIoService`] -- the non-RT face of audio output.
//!
//! Owns the cpal device and stream, and hands out the three non-RT halves the
//! rest of the app needs: the [`GraphHandle`] to publish graphs, the shared
//! [`TransportClock`] to read the playhead, and the command producer to steer the
//! transport.
//!
//! # Where the wall is
//! ```text
//!   non-RT (this type, behind mutexes)      RT (inside the cpal closure)
//!   ----------------------------------      ----------------------------
//!   GraphHandle::install  ---- rtrb ---->    RtGraphSlot::poll_swap
//!   RtCommandProducer     ---- rtrb ---->    drain_commands
//!   NoteProducer          ---- rtrb ---->    drain_notes
//!   MeterThrottle::poll   <--- rtrb -----    MeterProducer::push
//!   DebugTapService       <--- rtrb -----    TapRtSide::push_block
//!   Arc<TransportClock>   <-- atomics -->    advance / snapshot
//! ```
//! Every mutex in this file is locked from ordinary threads only: `start`,
//! `stop`, `install_graph` and `send_command` all run on whoever is driving the
//! app, never in the callback.
//!
//! # Lifecycle
//! The plugin does **not** open the device during kernel boot. `start()` is
//! explicit, for two reasons: a headless CI machine has no output device and
//! would fail the boot, and the app wants to choose *when* audio goes live
//! (after the graph is built, not before).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use mydaw_ipc_bridge::{MeterProducer, SpectrumProducer, SpectrumWatch};
use mydaw_plugin_debug_tap::TapRtSide;
use mydaw_plugin_sequencer::{
    song_channel, CompiledSong, SongHandle, SongInstallError, SONG_QUEUE_CAPACITY,
};
use mydaw_rt_core::{
    clamp_bpm, graph_channel, note_channel, rt_channel, GcThread, GraphHandle, GraphInstallError,
    GraphLimits, GraphSpec, LiveNote, NoteProducer, PlayMode, RtCommand, RtCommandProducer,
    SpectrumTapRt, TransportClock, DEFAULT_NOTE_CAPACITY,
};
use parking_lot::Mutex;

use crate::rt::{run_block, AudioIoStats, RtState, RtStateParams};

/// Everything that can go wrong on the non-RT side of audio I/O.
#[derive(Debug, thiserror::Error)]
pub enum AudioIoError {
    /// The system reports no default output device (headless CI, no sound card,
    /// or every device disabled).
    #[error("no default audio output device is available")]
    NoOutputDevice,

    /// cpal failed while querying the device or building the stream.
    #[error("audio device error: {0}")]
    Device(#[from] cpal::Error),

    /// The device's default configuration is not 32-bit float.
    ///
    /// Phase 0 does not convert sample formats: WASAPI shared mode is f32 in
    /// practice, and a conversion layer written before it is needed is a
    /// conversion layer written against guesses.
    #[error("output device uses {0} samples; Phase 0 requires f32")]
    UnsupportedSampleFormat(SampleFormat),

    /// `start()` called while a stream is already running.
    #[error("audio output is already running")]
    AlreadyRunning,

    /// An operation that needs a live stream was called while stopped.
    #[error("audio output is not running")]
    NotRunning,

    /// The graph could not be handed to the audio thread.
    #[error("could not install graph: {0}")]
    GraphInstall(#[from] GraphInstallError),

    /// The song could not be handed to the audio thread.
    #[error("could not install song: {0}")]
    SongInstall(#[from] SongInstallError),

    /// The transport command queue is full -- the audio thread has not drained
    /// it, which in practice means the stream is wedged.
    #[error("transport command queue is full")]
    CommandQueueFull,

    /// The live note queue is full. The audio thread drains it every block, so
    /// this means either a wedged stream or a note source producing more than a
    /// block's worth of events between callbacks.
    #[error("note queue is full")]
    NoteQueueFull,
}

/// Tunables for [`AudioIoService`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioIoConfig {
    /// Largest block the callback will be asked for. Buffers are pre-allocated to
    /// this; a device asking for more gets a truncated (glitching) block rather
    /// than an allocation on the audio thread. 4096 covers every sane WASAPI
    /// shared-mode period with room to spare.
    pub max_block: usize,
    /// Depth of the graph publish queue. Two is plenty -- the audio thread drains
    /// it fully every block -- but a little headroom absorbs a burst of edits.
    pub graph_queue_capacity: usize,
    /// Depth of the transport command queue.
    pub command_queue_capacity: usize,
    /// Depth of the song publish queue. The audio thread drains it fully
    /// every block; the depth only absorbs a burst of grid edits.
    pub song_queue_capacity: usize,
    /// Depth of the live note queue, and the size of the per-block note event
    /// buffer it drains into.
    ///
    /// One number for both on purpose: with the buffer at least as large as the
    /// queue, a full queue always fits into one block, so the live path can
    /// never drop an event between the two. (The overflow counter still exists
    /// for the sequencer, which will fill the same buffer from elsewhere.)
    pub note_queue_capacity: usize,
}

impl Default for AudioIoConfig {
    fn default() -> Self {
        Self {
            max_block: 4096,
            graph_queue_capacity: 8,
            command_queue_capacity: 256,
            song_queue_capacity: SONG_QUEUE_CAPACITY,
            note_queue_capacity: DEFAULT_NOTE_CAPACITY,
        }
    }
}

/// What the device negotiated when the stream came up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamInfo {
    pub device_name: String,
    pub sample_rate: u32,
    pub channels: u16,
    /// Largest block the callback can process without truncating.
    pub max_block: usize,
}

/// Live stream, plus every non-RT half that belongs to it.
struct Session {
    /// Dropping this tears the stream down and, with it, the RT state.
    stream: Stream,
    transport: Arc<TransportClock>,
    graph: GraphHandle,
    commands: RtCommandProducer,
    notes: NoteProducer,
    /// Publishes the compiled song to the sequencer's slot in the RT state.
    song: SongHandle,
    info: StreamInfo,
    /// The callback hands the meter producer back here when it is dropped, so a
    /// later `start()` can reuse it. See [`RtStateCarrier`].
    meters_returned: mpsc::Receiver<MeterProducer>,
    /// Same, for the debug capture tap.
    debug_tap_returned: mpsc::Receiver<TapRtSide>,
    /// Same, for the analysis hop producer.
    spectrum_returned: mpsc::Receiver<SpectrumProducer>,
}

/// Owns [`RtState`] inside the cpal closure and returns the borrowed RT halves
/// on teardown.
///
/// The metering and debug-tap plugins hand out their producers exactly once
/// (SPSC), so if stopping the stream simply dropped them, meters and captures
/// would be dead for the rest of the process. This carrier makes stop/start
/// cycles lossless.
struct RtStateCarrier {
    state: RtState,
    meters_returned: mpsc::SyncSender<MeterProducer>,
    debug_tap_returned: mpsc::SyncSender<TapRtSide>,
    spectrum_returned: mpsc::SyncSender<SpectrumProducer>,
}

impl Drop for RtStateCarrier {
    fn drop(&mut self) {
        // Bounded channels, never contended: a failed send just means nobody
        // is listening, which is fine at teardown.
        if let Some(producer) = self.state.take_meters() {
            let _ = self.meters_returned.try_send(producer);
        }
        if let Some(tap) = self.state.take_debug_tap() {
            let _ = self.debug_tap_returned.try_send(tap);
        }
        if let Some(tap) = self.state.take_spectrum() {
            let _ = self.spectrum_returned.try_send(tap.take_producer());
        }
    }
}

/// The audio output service.
///
/// Provided as a kernel service by [`crate::AudioIoPlugin`]; resolve it with
/// `ctx.services.get::<AudioIoService>()`.
pub struct AudioIoService {
    config: AudioIoConfig,
    /// Frees graphs retired by the audio thread. Long-lived: it outlives
    /// individual streams so a stop/start cycle cannot strand a graph.
    gc: GcThread,
    stats: Arc<AudioIoStats>,
    /// The meter producer between stream lifetimes.
    meters: Mutex<Option<MeterProducer>>,
    /// The debug capture tap between stream lifetimes.
    debug_tap: Mutex<Option<TapRtSide>>,
    /// Analysis hop producer between stream lifetimes.
    spectrum: Mutex<Option<SpectrumProducer>>,
    /// Shared watch word; cloned into each RT tap.
    spectrum_watch: Mutex<Option<Arc<SpectrumWatch>>>,
    session: Mutex<Option<Session>>,
    /// Mirrors `session.is_some()` so [`Self::is_running`] needs no lock.
    running: AtomicBool,
}

impl Default for AudioIoService {
    fn default() -> Self {
        Self::new(AudioIoConfig::default())
    }
}

impl AudioIoService {
    /// Create the service. Spawns the basedrop collector thread; opens no device.
    pub fn new(config: AudioIoConfig) -> Self {
        Self {
            config,
            gc: GcThread::spawn(),
            stats: Arc::new(AudioIoStats::default()),
            meters: Mutex::new(None),
            debug_tap: Mutex::new(None),
            spectrum: Mutex::new(None),
            spectrum_watch: Mutex::new(None),
            session: Mutex::new(None),
            running: AtomicBool::new(false),
        }
    }

    /// Give the service the meter queue's producer.
    ///
    /// Called once by the plugin during kernel start, with the producer taken from
    /// the metering plugin's `MeterTap`. Without it the stream still runs; the UI
    /// meters simply stay at zero.
    pub fn attach_meters(&self, producer: MeterProducer) {
        *self.meters.lock() = Some(producer);
    }

    /// Give the service the debug tap's RT side.
    ///
    /// Called once by the plugin during kernel start, with the RT side taken
    /// from `debug-tap`'s `DebugTapHandoff`. Without it the stream still runs;
    /// `debug_capture` simply reports that the engine never started.
    pub fn attach_debug_tap(&self, tap: TapRtSide) {
        *self.debug_tap.lock() = Some(tap);
    }

    /// Give the service the analysis hop producer and the shared watch word.
    pub fn attach_spectrum(&self, producer: SpectrumProducer, watch: Arc<SpectrumWatch>) {
        *self.spectrum.lock() = Some(producer);
        *self.spectrum_watch.lock() = Some(watch);
    }

    /// Whether a debug tap is parked here, waiting for the next stream.
    pub fn has_debug_tap(&self) -> bool {
        self.debug_tap.lock().is_some()
    }

    /// Counters maintained by the audio thread.
    pub fn stats(&self) -> &Arc<AudioIoStats> {
        &self.stats
    }

    /// Handle for allocating `basedrop::Owned` values that the audio thread may
    /// drop. Graph installation uses it internally; exposed for graph builders
    /// that need to park other RT-owned data.
    pub fn gc_handle(&self) -> mydaw_rt_core::basedrop::Handle {
        self.gc.handle()
    }

    /// Whether a stream is currently running.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// Negotiated device configuration, or `None` while stopped.
    pub fn stream_info(&self) -> Option<StreamInfo> {
        self.session.lock().as_ref().map(|s| s.info.clone())
    }

    /// Device sample rate, or `None` while stopped.
    pub fn sample_rate(&self) -> Option<f64> {
        self.session
            .lock()
            .as_ref()
            .map(|s| s.transport.sample_rate())
    }

    /// The shared transport clock, or `None` while stopped.
    ///
    /// Only exists while a stream does: the clock is built around the device's
    /// sample rate, and inventing one before the device has been asked would mean
    /// the playhead reports the wrong time on any device that is not 48 kHz.
    pub fn transport(&self) -> Option<Arc<TransportClock>> {
        self.session
            .lock()
            .as_ref()
            .map(|s| Arc::clone(&s.transport))
    }

    /// Publish a graph to the audio thread. Takes effect on the next block.
    ///
    /// The graph's per-track scratch buffers, event buffers and parameter ramps
    /// are sized from the **open stream** unless the builder pinned them with
    /// [`GraphSpec::with_limits`]. A graph builder should not have to know the
    /// device's block size to be correct, and a graph built against a stale one
    /// would truncate every block.
    pub fn install_graph(&self, mut spec: GraphSpec) -> Result<(), AudioIoError> {
        let mut guard = self.session.lock();
        let session = guard.as_mut().ok_or(AudioIoError::NotRunning)?;
        spec.apply_default_limits(GraphLimits {
            sample_rate: session.transport.sample_rate(),
            max_block: self.config.max_block,
            max_events: self.config.note_queue_capacity,
        });
        session.graph.install(spec)?;
        Ok(())
    }

    /// The graph limits a graph installed right now would inherit.
    ///
    /// Exposed so an offline render (and the shell's own project builder) can
    /// build a graph against exactly what the stream will give it. `None` while
    /// stopped.
    pub fn graph_limits(&self) -> Option<GraphLimits> {
        self.session.lock().as_ref().map(|session| GraphLimits {
            sample_rate: session.transport.sample_rate(),
            max_block: self.config.max_block,
            max_events: self.config.note_queue_capacity,
        })
    }

    /// Escape hatch for callers that need the [`GraphHandle`] itself (e.g. to
    /// read `gc_handle()` or install several graphs under one lock).
    ///
    /// Returns `None` while stopped. Do not park the closure -- the session mutex
    /// is held for its duration.
    pub fn with_graph_handle<R>(&self, f: impl FnOnce(&mut GraphHandle) -> R) -> Option<R> {
        let mut guard = self.session.lock();
        guard.as_mut().map(|session| f(&mut session.graph))
    }

    /// Send a transport command to the audio thread.
    pub fn send_command(&self, command: RtCommand) -> Result<(), AudioIoError> {
        let mut guard = self.session.lock();
        let session = guard.as_mut().ok_or(AudioIoError::NotRunning)?;
        session
            .commands
            .push(command)
            .map_err(|_| AudioIoError::CommandQueueFull)
    }

    /// Send a live note event to the audio thread.
    ///
    /// The event takes effect at the first sample of the next block; live input
    /// carries no offset within a block (see
    /// [`LiveNote`](mydaw_rt_core::LiveNote)). Deliberately *not* gated on the
    /// transport: playing an instrument with the sequencer stopped is the normal
    /// way to audition one.
    ///
    /// # Errors
    /// [`AudioIoError::NotRunning`] with no stream open, and
    /// [`AudioIoError::NoteQueueFull`] if the audio thread is not draining.
    pub fn send_note(&self, note: LiveNote) -> Result<(), AudioIoError> {
        let mut guard = self.session.lock();
        let session = guard.as_mut().ok_or(AudioIoError::NotRunning)?;
        session
            .notes
            .push(note)
            .map_err(|_| AudioIoError::NoteQueueFull)
    }

    /// Publish the compiled song to the sequencer. Takes effect on the next
    /// block.
    ///
    /// A full replacement rather than a diff: a 16x16 grid is ~1.4 kB of fixed
    /// arrays, which is cheaper to build, send and reason about than any
    /// incremental protocol would be, and it makes a lost message impossible to
    /// tell from a slow one.
    ///
    /// # Errors
    /// [`AudioIoError::NotRunning`] with no stream open, and
    /// [`AudioIoError::SongInstall`] when the audio thread is behind
    /// (retryable).
    pub fn install_song(&self, song: CompiledSong) -> Result<(), AudioIoError> {
        let mut guard = self.session.lock();
        let session = guard.as_mut().ok_or(AudioIoError::NotRunning)?;
        session.song.install(song)?;
        Ok(())
    }

    /// Start or stop the transport.
    pub fn set_playing(&self, playing: bool) -> Result<(), AudioIoError> {
        self.send_command(RtCommand::SetPlaying(playing))
    }

    /// Move the playhead to `sample_pos`.
    pub fn seek(&self, sample_pos: u64) -> Result<(), AudioIoError> {
        self.send_command(RtCommand::Seek(sample_pos))
    }

    /// Change the tempo. Returns the value that will be applied (clamped to
    /// `MIN_BPM..=MAX_BPM`(mydaw_rt_core::MIN_BPM)).
    ///
    /// Goes through the command queue rather than writing the clock directly,
    /// so a tempo change lands on a block boundary together with any transport
    /// command sent with it -- the scheduler reads one snapshot per block, and a
    /// tempo that changed halfway through one would move the grid under it.
    pub fn set_bpm(&self, bpm: f64) -> Result<f64, AudioIoError> {
        let clamped = clamp_bpm(bpm);
        self.send_command(RtCommand::SetBpm(clamped))?;
        Ok(clamped)
    }

    /// Switch between playing the active pattern and playing the arrangement.
    ///
    /// Through the command queue rather than by writing the clock directly, for
    /// the same reason the tempo is: the scheduler reads one snapshot per block,
    /// and a mode that changed halfway through one would emit half a bar of each.
    ///
    /// # Errors
    /// [`AudioIoError::NotRunning`], or a full command queue.
    pub fn set_play_mode(&self, mode: PlayMode) -> Result<(), AudioIoError> {
        self.send_command(RtCommand::SetPlayMode(mode))
    }

    /// Repeat the arrangement, or stop at the end of it.
    ///
    /// # Errors
    /// [`AudioIoError::NotRunning`] with no stream open, or a full queue.
    pub fn set_looping(&self, looping: bool) -> Result<(), AudioIoError> {
        self.send_command(RtCommand::SetLooping(looping))
    }

    /// Whether the audio thread is looping the song, or `None` while stopped.
    pub fn is_looping(&self) -> Option<bool> {
        self.session
            .lock()
            .as_ref()
            .map(|s| s.transport.is_looping())
    }

    /// The mode the audio thread is in, or `None` while stopped.
    pub fn play_mode(&self) -> Option<PlayMode> {
        self.session
            .lock()
            .as_ref()
            .map(|s| s.transport.play_mode())
    }

    /// The tempo the audio thread is running at, or `None` while stopped.
    pub fn bpm(&self) -> Option<f64> {
        self.session.lock().as_ref().map(|s| s.transport.bpm())
    }

    /// Open the default output device and start the audio callback.
    ///
    /// The device's default configuration is used as-is: negotiating buffer sizes
    /// and rates is a settings-panel feature, and Phase 0 has no settings panel.
    ///
    /// # Errors
    /// [`AudioIoError::AlreadyRunning`] if a stream is live,
    /// [`AudioIoError::NoOutputDevice`] on a machine without audio output,
    /// [`AudioIoError::UnsupportedSampleFormat`] if the device is not f32, and
    /// [`AudioIoError::Device`] for anything cpal reports.
    pub fn start(&self) -> Result<StreamInfo, AudioIoError> {
        let mut guard = self.session.lock();
        if guard.is_some() {
            return Err(AudioIoError::AlreadyRunning);
        }

        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or(AudioIoError::NoOutputDevice)?;
        let supported = device.default_output_config()?;

        if supported.sample_format() != SampleFormat::F32 {
            return Err(AudioIoError::UnsupportedSampleFormat(
                supported.sample_format(),
            ));
        }

        let config: StreamConfig = supported.config();
        let sample_rate = config.sample_rate;
        let channels = config.channels;
        let device_name = device
            .description()
            .map(|d| d.name().to_owned())
            .unwrap_or_else(|_| "<unknown output device>".to_owned());

        // --- build both sides of the wall, on this (non-RT) thread ---
        let transport = Arc::new(TransportClock::new(sample_rate as f64));
        let (graph_handle, graph_slot) =
            graph_channel(self.config.graph_queue_capacity, self.gc.handle());
        let (command_tx, command_rx) = rt_channel::<RtCommand>(self.config.command_queue_capacity);
        let (note_tx, note_rx) = note_channel(self.config.note_queue_capacity);
        let (song_handle, song_slot) =
            song_channel(self.config.song_queue_capacity, self.gc.handle());
        let (meters_return_tx, meters_returned) = mpsc::sync_channel(1);
        let (tap_return_tx, debug_tap_returned) = mpsc::sync_channel(1);
        let (spectrum_return_tx, spectrum_returned) = mpsc::sync_channel(1);

        let spectrum = match (
            self.spectrum.lock().take(),
            self.spectrum_watch.lock().as_ref().map(Arc::clone),
        ) {
            (Some(producer), Some(watch)) => {
                Some(SpectrumTapRt::new(producer, watch, sample_rate as f64))
            }
            (producer, _) => {
                if let Some(producer) = producer {
                    *self.spectrum.lock() = Some(producer);
                }
                None
            }
        };

        let state = RtState::new(RtStateParams {
            graph: graph_slot,
            commands: command_rx,
            notes: note_rx,
            note_capacity: self.config.note_queue_capacity,
            song: song_slot,
            meters: self.meters.lock().take(),
            debug_tap: self.debug_tap.lock().take(),
            spectrum,
            transport: Arc::clone(&transport),
            sample_rate: sample_rate as f64,
            out_channels: channels as usize,
            max_block: self.config.max_block,
            stats: Arc::clone(&self.stats),
        });

        // Everything RT now lives in `carrier`, which moves into the closure and
        // is never touched from this side again.
        let mut carrier = RtStateCarrier {
            state,
            meters_returned: meters_return_tx,
            debug_tap_returned: tap_return_tx,
            spectrum_returned: spectrum_return_tx,
        };
        let out_channels = channels as usize;
        let error_stats = Arc::clone(&self.stats);

        let stream = device.build_output_stream::<f32, _, _>(
            config,
            move |output: &mut [f32], _info| {
                // The entire callback body. Promotion is a no-op after block one.
                let frames = output.len().checked_div(out_channels).unwrap_or(0);
                carrier.state.ensure_realtime(frames);
                run_block(&mut carrier.state, output);
            },
            move |_err| {
                // Audio thread: count, do not log. `stats().stream_errors()` is
                // how the app finds out.
                error_stats.record_stream_error();
            },
            None,
        )?;
        stream.play()?;

        let info = StreamInfo {
            device_name,
            sample_rate,
            channels,
            max_block: self.config.max_block,
        };

        log::info!(
            "audio output started: {} @ {} Hz, {} ch",
            info.device_name,
            info.sample_rate,
            info.channels
        );

        *guard = Some(Session {
            stream,
            transport,
            graph: graph_handle,
            commands: command_tx,
            notes: note_tx,
            song: song_handle,
            info: info.clone(),
            meters_returned,
            debug_tap_returned,
            spectrum_returned,
        });
        self.running.store(true, Ordering::Release);

        Ok(info)
    }

    /// Stop the stream and release the device. Idempotent.
    ///
    /// Dropping the stream joins cpal's audio thread, which drops the callback
    /// closure and with it the RT state: the graph slot, the command consumer and
    /// the real-time promotion (reverted by
    /// [`RealtimeToken`](crate::priority::RealtimeToken) on that same thread). The
    /// meter producer is recovered so a later `start()` still has metering.
    pub fn stop(&self) {
        let Some(session) = self.session.lock().take() else {
            return;
        };
        self.running.store(false, Ordering::Release);

        // Order matters: the stream must be gone before the returned producer can
        // possibly arrive.
        drop(session.stream);

        // cpal drops the callback during `drop(stream)`, so the producer is
        // normally already waiting. The timeout covers backends that tear the
        // thread down asynchronously; losing the producer only costs metering
        // after a restart, so this must never block the caller for long.
        match session
            .meters_returned
            .recv_timeout(Duration::from_millis(250))
        {
            Ok(producer) => *self.meters.lock() = Some(producer),
            Err(_) => log::debug!("meter producer not returned by the audio callback"),
        }
        match session
            .debug_tap_returned
            .recv_timeout(Duration::from_millis(250))
        {
            Ok(tap) => *self.debug_tap.lock() = Some(tap),
            Err(_) => log::debug!("debug tap not returned by the audio callback"),
        }
        match session
            .spectrum_returned
            .recv_timeout(Duration::from_millis(250))
        {
            Ok(producer) => *self.spectrum.lock() = Some(producer),
            Err(_) => log::debug!("spectrum producer not returned by the audio callback"),
        }

        log::info!("audio output stopped: {}", session.info.device_name);
    }
}

impl Drop for AudioIoService {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These run on CI, which has no audio device. They cover everything that does
    // not need one; the callback body itself is tested in `rt.rs` via `run_block`.

    #[test]
    fn a_fresh_service_is_stopped_and_inert() {
        let service = AudioIoService::default();
        assert!(!service.is_running());
        assert!(service.transport().is_none());
        assert!(service.stream_info().is_none());
        assert!(service.sample_rate().is_none());
        assert_eq!(service.stats().blocks_processed(), 0);
    }

    #[test]
    fn operations_needing_a_stream_fail_cleanly_while_stopped() {
        let service = AudioIoService::default();
        assert!(matches!(
            service.install_graph(GraphSpec::new()),
            Err(AudioIoError::NotRunning)
        ));
        assert!(matches!(
            service.send_command(RtCommand::SetPlaying(true)),
            Err(AudioIoError::NotRunning)
        ));
        assert!(matches!(service.seek(0), Err(AudioIoError::NotRunning)));
        assert!(matches!(
            service.send_note(LiveNote::note_on(60, 1.0)),
            Err(AudioIoError::NotRunning)
        ));
        assert!(matches!(
            service.install_song(CompiledSong::empty()),
            Err(AudioIoError::NotRunning)
        ));
        assert!(matches!(
            service.set_bpm(130.0),
            Err(AudioIoError::NotRunning)
        ));
        assert!(service.bpm().is_none());
        assert!(service.with_graph_handle(|_| ()).is_none());
    }

    #[test]
    fn stop_is_idempotent_without_a_stream() {
        let service = AudioIoService::default();
        service.stop();
        service.stop();
        assert!(!service.is_running());
    }

    #[test]
    fn an_attached_meter_producer_survives_until_a_stream_takes_it() {
        let (producer, _throttle) = mydaw_ipc_bridge::meter_channel(8);
        let service = AudioIoService::default();
        service.attach_meters(producer);
        assert!(service.meters.lock().is_some());
    }

    #[test]
    fn an_attached_debug_tap_survives_until_a_stream_takes_it() {
        let (tap, consumer) = mydaw_plugin_debug_tap::debug_tap_channel(64);
        let service = AudioIoService::default();
        assert!(!service.has_debug_tap());
        service.attach_debug_tap(tap);
        assert!(service.has_debug_tap());
        assert_eq!(
            consumer.shared().sample_rate(),
            0.0,
            "the rate is only known once a stream is built"
        );
    }

    #[test]
    fn the_gc_handle_is_usable_before_any_stream_exists() {
        let service = AudioIoService::default();
        let owned = mydaw_rt_core::basedrop::Owned::new(&service.gc_handle(), vec![0_u8; 16]);
        assert_eq!(owned.len(), 16);
    }

    #[test]
    fn the_default_config_is_the_documented_one() {
        let config = AudioIoConfig::default();
        assert_eq!(config.max_block, 4096);
        assert!(config.graph_queue_capacity >= 2);
        assert!(config.command_queue_capacity >= 64);
        assert_eq!(config.song_queue_capacity, SONG_QUEUE_CAPACITY);
        assert_eq!(config.note_queue_capacity, DEFAULT_NOTE_CAPACITY);
    }
}
