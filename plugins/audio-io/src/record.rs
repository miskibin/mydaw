//! [`RecordService`] -- the non-RT face of audio input.
//!
//! Owns the cpal *capture* device, the worker that drains it, and the take that
//! worker is building. The output half of the crate ([`crate::service`]) is
//! deliberately untouched: recording must not be able to interrupt playback, so
//! the two streams are independent and share nothing but the engine's sample
//! rate.
//!
//! ```text
//!   UI / IPC (non-RT)              worker (non-RT)         capture (RT)
//!   -----------------              ---------------         ------------
//!   RecordService::arm    --------> opens the stream -----> InputRtSide
//!   RecordService::start  -- flag -> InputShared::start           |
//!                                    drain + convert  <--rtrb-----'
//!   RecordService::status <- atomics + take length
//!   RecordService::stop   --------> flush, hand the take back
//! ```
//!
//! # Three states, and why arming is one of them
//! `Idle` holds no device at all: a DAW that keeps the microphone open while you
//! are not recording is a DAW that stops your conferencing app from working and
//! lights the OS privacy indicator for nothing. `Armed` opens the device and
//! meters it so levels can be set before the take. `Recording` additionally
//! copies. Arming is what makes the level meter honest -- the number cannot come
//! from anywhere but an open device.
//!
//! # ASIO
//! Input always uses the platform's default host. An ASIO driver hands out one
//! instance for both directions and cpal opens the two halves separately, so
//! asking ASIO for a capture stream while it is already playing is how a driver
//! gets wedged. WASAPI shared mode has no such problem, and the output stream's
//! ASIO preference is left alone.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{ErrorKind, SampleFormat, Stream, StreamConfig};
use mydaw_rt_core::{disable_denormals, CHANNELS};
use parking_lot::Mutex;

use crate::input::{
    input_channel, InputConsumer, InputFault, InputRtSide, InputShared, RateConverter,
    INPUT_QUEUE_CAPACITY,
};
use crate::monitor::{monitor_channel, MonitorReader, MonitorSlot};
use crate::priority::RealtimeToken;
use crate::timeline::{duration_ns, now_ns, PlaybackTimeline};

/// How often the worker looks at the ring.
///
/// Four milliseconds is well inside one capture block (a 480-frame period at
/// 48 kHz is 10 ms) and far above the cost of a `slots()` load, so the worker
/// neither spins nor lets the ring fill.
pub const DRAIN_INTERVAL: Duration = Duration::from_millis(4);

/// Longest take the recorder will build, in seconds.
///
/// The take lives in RAM until it is written, at 8 bytes a frame: ten minutes
/// at 48 kHz is 230 MB -- a whole song with room to spare, and the longest
/// file the playlist's source registry accepts. Hitting the limit stops the
/// take and keeps it -- silently discarding a performance because a button
/// was left down would be the worst possible behaviour.
pub const MAX_RECORD_SECONDS: f64 = 600.0;

/// Rate a take is written at when no output stream has ever opened, so the
/// engine rate is unknown. Only reachable on a machine with no audio output.
pub const FALLBACK_SAMPLE_RATE: f64 = 48_000.0;

/// Largest capture block the conversion scratch is sized for.
///
/// A device asking for more is processed in several passes rather than
/// allocating on the capture thread; see [`build_typed_stream`].
const MAX_CAPTURE_BLOCK: usize = 8_192;

/// What the recorder is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RecordState {
    /// No capture device is open.
    Idle = 0,
    /// The device is open and metering, but nothing is being kept.
    Armed = 1,
    /// The device is open and the worker is building a take.
    Recording = 2,
}

impl RecordState {
    fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Armed,
            2 => Self::Recording,
            _ => Self::Idle,
        }
    }

    /// Wire name the UI reads.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Armed => "armed",
            Self::Recording => "recording",
        }
    }
}

/// Everything that can go wrong on the non-RT side of recording.
#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    /// The system reports no capture device at all: no interface, no built-in
    /// microphone, or every one of them disabled.
    #[error("no audio input device is available")]
    NoInputDevice,

    /// The chosen device is not in the host's list any more.
    #[error("input device '{0}' is not available")]
    UnknownDevice(String),

    /// The device's default configuration is a format this build cannot read.
    #[error("input device uses {0} samples, which this build cannot record")]
    UnsupportedSampleFormat(SampleFormat),

    /// cpal failed while querying the device or building the stream.
    #[error("input device error: {0}")]
    Device(#[from] cpal::Error),

    /// An operation that needs an open device was called while idle.
    #[error("no input device is armed")]
    NotArmed,

    /// Stop or cancel with no take running.
    #[error("nothing is being recorded")]
    NotRecording,

    /// Start with a take already running.
    #[error("a take is already being recorded")]
    AlreadyRecording,

    /// The device vanished (or was revoked) while the take was running.
    #[error("{0}")]
    DeviceLost(&'static str),

    /// Stopped with not one frame captured.
    #[error("nothing was recorded; the input device produced no audio")]
    Empty,

    /// A device change was asked for mid-take.
    #[error("stop the take before changing the input device")]
    BusyRecording,
}

/// One capture device, as Settings lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDeviceInfo {
    /// Host-stable identifier, or the device name when the host has no id.
    pub id: String,
    pub name: String,
    /// Channels the default configuration delivers. `0` = the host would not
    /// say, which is a device that will refuse to open.
    pub channels: u16,
    /// Rate the default configuration runs at. `0` = as above.
    pub sample_rate: u32,
    /// Whether this is the host's default input.
    pub is_default: bool,
}

/// A finished take: interleaved stereo at the engine's rate.
#[derive(Clone, PartialEq)]
pub struct RecordedTake {
    /// Interleaved `L R L R ...`. Always a whole number of frames.
    pub samples: Vec<f32>,
    /// The rate the samples are at -- the engine's, not necessarily the
    /// device's. See [`crate::input`].
    pub sample_rate: f64,
    /// Samples the capture callback had to throw away. Non-zero means holes.
    pub dropped: u64,
    /// The device the take came from, for the log and the status line.
    pub device_name: String,
    /// Whether [`MAX_RECORD_SECONDS`] ended the take rather than the user.
    pub limit_reached: bool,
    /// Transport position, in engine samples (fractional, possibly negative),
    /// that was audible when the take's first frame was captured -- where the
    /// take belongs on the song, with input and output latency compensated.
    /// `None` when the transport never played during the take.
    pub start_sample: Option<f64>,
}

/// Counts rather than a hundred megabytes of samples.
impl std::fmt::Debug for RecordedTake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordedTake")
            .field("frames", &self.frames())
            .field("sample_rate", &self.sample_rate)
            .field("dropped", &self.dropped)
            .field("device_name", &self.device_name)
            .field("limit_reached", &self.limit_reached)
            .field("start_sample", &self.start_sample)
            .finish()
    }
}

impl RecordedTake {
    /// Frames captured.
    pub fn frames(&self) -> usize {
        self.samples.len() / CHANNELS
    }

    /// Whether nothing at all was captured.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Captured length in seconds.
    pub fn duration_seconds(&self) -> f64 {
        if self.sample_rate > 0.0 {
            self.frames() as f64 / self.sample_rate
        } else {
            0.0
        }
    }

    /// Largest absolute sample. Near zero means the input was silent, which is
    /// the difference between "the cable is unplugged" and "it worked".
    pub fn peak(&self) -> f32 {
        self.samples
            .iter()
            .copied()
            .filter(|sample| sample.is_finite())
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()))
    }
}

/// Everything the UI draws about the input path, in one message.
///
/// Answers with an `Idle` shape rather than an error when nothing is open,
/// because "not armed" is an ordinary state a transport bar has to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct InputStatus {
    pub state: RecordState,
    /// The device that is open, or empty while idle.
    pub device_name: String,
    /// The device Settings selected, or `None` for "follow the system default".
    pub device_id: Option<String>,
    /// Rate the open device runs at. `0` while idle.
    pub device_sample_rate: u32,
    /// Channels the open device delivers. `0` while idle.
    pub device_channels: u16,
    /// Rate the take is being written at -- the engine's.
    pub sample_rate: f64,
    /// Peak of the last blocks, linear, with a 20 dB/s hold. `0` while idle.
    pub peak: f32,
    /// RMS of the last block, linear. `0` while idle.
    pub rms: f32,
    /// Seconds recorded so far. `0` unless recording.
    pub elapsed_seconds: f64,
    /// Frames recorded so far, at [`Self::sample_rate`].
    pub frames: usize,
    /// Samples the capture callback dropped during this take.
    pub dropped_samples: u64,
    /// Whether [`MAX_RECORD_SECONDS`] has been hit.
    pub limit_reached: bool,
    /// Longest take this build will keep.
    pub max_seconds: f64,
    /// Why the device is unhappy, or empty. Non-empty means the stream is gone.
    pub fault: String,
    /// Whether the input is heard through the mixer while the device is open.
    pub monitoring: bool,
}

impl InputStatus {
    /// The "nothing is open" answer.
    fn idle(device_id: Option<String>, sample_rate: f64, fault: String) -> Self {
        Self {
            state: RecordState::Idle,
            device_name: String::new(),
            device_id,
            device_sample_rate: 0,
            device_channels: 0,
            sample_rate,
            peak: 0.0,
            rms: 0.0,
            elapsed_seconds: 0.0,
            frames: 0,
            dropped_samples: 0,
            limit_reached: false,
            max_seconds: MAX_RECORD_SECONDS,
            fault,
            monitoring: false,
        }
    }
}

/// How long [`Session::end_take`] waits for the worker's last flush.
///
/// The worker's loop is bounded by [`DRAIN_INTERVAL`], so this is two orders of
/// magnitude of slack. Timing out costs the last few milliseconds of the take,
/// which is better than a UI that hangs on a wedged thread.
const FLUSH_TIMEOUT: Duration = Duration::from_millis(250);

/// The state one worker shares with the service.
///
/// One allocation, cloned as an `Arc` into the worker, rather than six
/// `Arc`s -- which is how the wrong two get swapped at a call site.
#[derive(Debug)]
struct TakeBuffer {
    /// Converted frames, interleaved stereo at the engine's rate.
    samples: Mutex<Vec<f32>>,
    /// Raised to ask the worker to finish and exit. Session teardown only.
    stopping: AtomicBool,
    /// Raised by the worker when [`MAX_RECORD_SECONDS`] ended the take.
    limit_reached: AtomicBool,
    /// Bumped by the worker every time it finishes flushing a take.
    ///
    /// This is the handshake that makes `stop()` deterministic: lowering the
    /// recording flag is not enough, because the last block may still be in the
    /// ring and the converter still holds three frames of tail.
    flushed: AtomicU64,
}

impl TakeBuffer {
    fn new() -> Self {
        Self {
            samples: Mutex::new(Vec::new()),
            stopping: AtomicBool::new(false),
            limit_reached: AtomicBool::new(false),
            flushed: AtomicU64::new(0),
        }
    }

    fn frames(&self) -> usize {
        self.samples.lock().len() / CHANNELS
    }
}

/// The open capture device and the worker draining it.
struct Session {
    /// Dropping this closes the device and stops the callback.
    stream: Stream,
    shared: Arc<InputShared>,
    buffer: Arc<TakeBuffer>,
    worker: Option<JoinHandle<()>>,
    device_name: String,
    device_sample_rate: u32,
    device_channels: u16,
    /// The rate the take is in.
    sample_rate: f64,
}

impl Session {
    /// Lower the recording flag, wait for the worker's flush, take the buffer.
    ///
    /// Shared by stop and cancel so the two cannot drift about what "the take is
    /// over" means.
    fn end_take(&self) -> Vec<f32> {
        let before = self.buffer.flushed.load(Ordering::Acquire);
        self.shared.stop();

        let deadline = Instant::now() + FLUSH_TIMEOUT;
        while self.buffer.flushed.load(Ordering::Acquire) == before {
            if Instant::now() >= deadline {
                log::warn!(
                    "the input drain did not flush within {FLUSH_TIMEOUT:?}; keeping what it had"
                );
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        std::mem::take(&mut *self.buffer.samples.lock())
    }

    /// Close the device and stop the worker. Consuming, so the order is not a
    /// choice: the callback goes first, then the thread that was reading from
    /// it, then the level the meter is drawing.
    fn close(mut self) {
        self.shared.stop();
        self.buffer.stopping.store(true, Ordering::Release);
        drop(self.stream);
        if let Some(worker) = self.worker.take() {
            // Bounded by DRAIN_INTERVAL, so this returns in milliseconds.
            let _ = worker.join();
        }
        self.shared.clear_level();
    }
}

/// The audio input service.
///
/// Held by the app next to [`crate::AudioIoService`]. Unlike that one it is not
/// a kernel service: nothing in the graph reads the input, so there is no RT
/// half for the kernel to wire up.
pub struct RecordService {
    /// The rate takes are converted to, as `f64` bits. `0` = no output stream
    /// has opened yet.
    engine_rate_bits: AtomicU64,
    /// The device Settings chose, or `None` for the system default.
    selected: Mutex<Option<String>>,
    session: Mutex<Option<Session>>,
    /// Mirrors the session's state so [`Self::state`] needs no lock.
    state: AtomicU8,
    /// The last fault, kept after the session that reported it is gone -- a
    /// device that vanished mid-take must still be able to say so.
    last_fault: Mutex<String>,
    /// Where the output graph reads the live input from.
    monitor: Arc<MonitorSlot>,
    /// Whether an open device is heard through the mixer.
    monitoring: AtomicBool,
    /// Manual placement offset in milliseconds, as `f64` bits.
    offset_ms_bits: AtomicU64,
}

impl Default for RecordService {
    fn default() -> Self {
        Self::new()
    }
}

impl RecordService {
    /// A stopped service. Opens no device and spawns no thread.
    pub fn new() -> Self {
        crate::timeline::init_clock();
        Self {
            engine_rate_bits: AtomicU64::new(0),
            selected: Mutex::new(None),
            session: Mutex::new(None),
            state: AtomicU8::new(RecordState::Idle as u8),
            last_fault: Mutex::new(String::new()),
            monitor: Arc::new(MonitorSlot::new()),
            monitoring: AtomicBool::new(true),
            offset_ms_bits: AtomicU64::new(0),
        }
    }

    /// The manual recording offset, in milliseconds. Positive moves takes
    /// earlier, on top of the automatic latency compensation.
    pub fn offset_ms(&self) -> f64 {
        f64::from_bits(self.offset_ms_bits.load(Ordering::Relaxed))
    }

    /// Set the manual recording offset. Non-finite values reset it.
    pub fn set_offset_ms(&self, offset_ms: f64) {
        let value = if offset_ms.is_finite() {
            offset_ms.clamp(-500.0, 500.0)
        } else {
            0.0
        };
        self.offset_ms_bits
            .store(value.to_bits(), Ordering::Relaxed);
    }

    /// The slot the output graph's monitor point renders the live input from.
    pub fn monitor_slot(&self) -> &Arc<MonitorSlot> {
        &self.monitor
    }

    /// Hear (or stop hearing) the input through the mixer while it is armed.
    pub fn set_monitoring(&self, on: bool) {
        self.monitoring.store(on, Ordering::Relaxed);
        if let Some(session) = self.session.lock().as_ref() {
            session.shared.set_monitoring(on);
        }
    }

    /// Whether the input is heard through the mixer while it is armed.
    pub fn monitoring(&self) -> bool {
        self.monitoring.load(Ordering::Relaxed)
    }

    /// Tell the recorder what rate takes should be written at.
    ///
    /// Called whenever the output stream opens. A take already running keeps the
    /// rate it started at: changing the target mid-flight would splice two rates
    /// into one file.
    pub fn set_engine_sample_rate(&self, rate: f64) {
        let usable = if rate.is_finite() && rate > 0.0 {
            rate
        } else {
            0.0
        };
        self.engine_rate_bits
            .store(usable.to_bits(), Ordering::Relaxed);
    }

    /// The rate the next take will be written at.
    pub fn engine_sample_rate(&self) -> f64 {
        let rate = f64::from_bits(self.engine_rate_bits.load(Ordering::Relaxed));
        if rate > 0.0 {
            rate
        } else {
            FALLBACK_SAMPLE_RATE
        }
    }

    /// What the recorder is doing. One relaxed load.
    pub fn state(&self) -> RecordState {
        RecordState::from_code(self.state.load(Ordering::Acquire))
    }

    fn set_state(&self, state: RecordState) {
        self.state.store(state as u8, Ordering::Release);
    }

    /// The device Settings chose, or `None` for the system default.
    pub fn selected_device(&self) -> Option<String> {
        self.selected.lock().clone()
    }

    /// Every capture device the default host can offer.
    ///
    /// Empty is a normal answer on a headless machine, an RDP session or a CI
    /// runner -- never an error. A device whose configuration the host will not
    /// report is still listed, with zeros, so Settings can show that it is there
    /// and arming can explain why it will not open.
    pub fn devices(&self) -> Vec<InputDeviceInfo> {
        let host = cpal::default_host();
        let default_key = host
            .default_input_device()
            .map(|device| device_key(&device));
        let Ok(devices) = host.input_devices() else {
            return Vec::new();
        };
        devices
            .map(|device| {
                let key = device_key(&device);
                let config = device.default_input_config().ok();
                InputDeviceInfo {
                    is_default: default_key.as_ref() == Some(&key),
                    id: key,
                    name: device_name(&device),
                    channels: config.as_ref().map_or(0, |config| config.channels()),
                    sample_rate: config.as_ref().map_or(0, |config| config.sample_rate()),
                }
            })
            .collect()
    }

    /// Choose the capture device. `None` follows the system default.
    ///
    /// Takes effect on the next [`arm`](Self::arm); an armed but idle device is
    /// re-opened immediately so the meter follows the choice. Refused mid-take,
    /// because swapping the device under a running take would splice two
    /// recordings together.
    pub fn select_device(&self, id: Option<&str>) -> Result<(), RecordError> {
        if self.state() == RecordState::Recording {
            return Err(RecordError::BusyRecording);
        }
        if let Some(id) = id {
            let known = self.devices().into_iter().any(|device| device.id == id);
            if !known {
                return Err(RecordError::UnknownDevice(id.to_owned()));
            }
        }
        let changed = {
            let mut selected = self.selected.lock();
            let next = id.map(str::to_owned);
            let changed = *selected != next;
            *selected = next;
            changed
        };
        if changed && self.state() == RecordState::Armed {
            self.disarm();
            self.arm()?;
        }
        Ok(())
    }

    /// Open the capture device and start metering. Idempotent while armed.
    ///
    /// # Errors
    /// [`RecordError::NoInputDevice`] on a machine with no capture hardware,
    /// [`RecordError::UnknownDevice`] when the chosen device has gone,
    /// [`RecordError::UnsupportedSampleFormat`] for an exotic format, and
    /// [`RecordError::Device`] for anything cpal reports.
    pub fn arm(&self) -> Result<InputStatus, RecordError> {
        // A session whose device has already died must not be re-used: it would
        // report "armed" over a stream that will never deliver another block.
        self.reap_fault();
        if self.session.lock().is_some() {
            return Ok(self.status());
        }
        let selected = self.selected_device();
        let session = self.open_session(selected.as_deref())?;
        log::info!(
            "audio input armed: {} @ {} Hz, {} ch (take rate {} Hz)",
            session.device_name,
            session.device_sample_rate,
            session.device_channels,
            session.sample_rate
        );
        *self.last_fault.lock() = String::new();
        *self.session.lock() = Some(session);
        self.set_state(RecordState::Armed);
        Ok(self.status())
    }

    /// Close the capture device. Idempotent, and discards any running take.
    ///
    /// Deliberately not an error while recording: this is also the teardown
    /// path, and a shutdown that refused to release the microphone because a
    /// take was running would leave the device held by a dead process.
    pub fn disarm(&self) {
        let Some(session) = self.session.lock().take() else {
            self.set_state(RecordState::Idle);
            return;
        };
        let name = session.device_name.clone();
        self.monitor.install(None, None);
        session.close();
        self.set_state(RecordState::Idle);
        log::info!("audio input released: {name}");
    }

    /// Start a take, arming the device first if it is not open yet.
    ///
    /// # Errors
    /// [`RecordError::AlreadyRecording`], plus everything [`arm`](Self::arm)
    /// can report.
    pub fn start(&self) -> Result<InputStatus, RecordError> {
        if self.state() == RecordState::Recording {
            return Err(RecordError::AlreadyRecording);
        }
        self.arm()?;

        {
            let guard = self.session.lock();
            let session = guard.as_ref().ok_or(RecordError::NotArmed)?;
            if session.shared.fault().is_fatal() {
                return Err(RecordError::DeviceLost(session.shared.fault().message()));
            }
            session.buffer.samples.lock().clear();
            session.buffer.limit_reached.store(false, Ordering::Relaxed);
            session.shared.reset_dropped();
            // The worker throws away whatever is in the ring before the flag
            // goes up, so a take never opens with the room noise from while you
            // were setting the level. See `run_worker`.
            session.shared.start();
        }

        self.set_state(RecordState::Recording);
        Ok(self.status())
    }

    /// End the take and hand it over. The device stays armed for a retake.
    ///
    /// # Errors
    /// [`RecordError::NotRecording`] with no take running, and
    /// [`RecordError::Empty`] when the device produced nothing at all -- which
    /// is what a muted interface or an unplugged cable looks like, and is worth
    /// saying rather than writing a zero-length file.
    pub fn stop(&self) -> Result<RecordedTake, RecordError> {
        if self.state() != RecordState::Recording {
            return Err(RecordError::NotRecording);
        }
        let (take, fault) = {
            let guard = self.session.lock();
            let session = guard.as_ref().ok_or(RecordError::NotRecording)?;
            let anchor = session.shared.anchor();
            let samples = session.end_take();
            let start_sample = anchor.and_then(|(_, captured_ns)| {
                take_start_sample(
                    PlaybackTimeline::global(),
                    captured_ns,
                    now_ns(),
                    session.sample_rate,
                )
            });
            (
                RecordedTake {
                    samples,
                    sample_rate: session.sample_rate,
                    dropped: session.shared.dropped(),
                    device_name: session.device_name.clone(),
                    limit_reached: session.buffer.limit_reached.load(Ordering::Relaxed),
                    start_sample,
                },
                session.shared.fault(),
            )
        };
        self.set_state(RecordState::Armed);

        if take.is_empty() {
            if fault.is_fatal() {
                return Err(RecordError::DeviceLost(fault.message()));
            }
            return Err(RecordError::Empty);
        }
        log::info!(
            "take recorded: {} frames @ {} Hz from {} ({} dropped)",
            take.frames(),
            take.sample_rate,
            take.device_name,
            take.dropped
        );
        Ok(take)
    }

    /// Throw the take away. The device stays armed.
    ///
    /// # Errors
    /// [`RecordError::NotRecording`] with no take running.
    pub fn cancel(&self) -> Result<InputStatus, RecordError> {
        if self.state() != RecordState::Recording {
            return Err(RecordError::NotRecording);
        }
        {
            let guard = self.session.lock();
            let session = guard.as_ref().ok_or(RecordError::NotRecording)?;
            let frames = session.end_take().len() / CHANNELS;
            log::info!("take cancelled after {frames} frames");
        }
        self.set_state(RecordState::Armed);
        Ok(self.status())
    }

    /// Everything the transport bar and Settings draw.
    pub fn status(&self) -> InputStatus {
        let selected = self.selected_device();
        let engine_rate = self.engine_sample_rate();
        let guard = self.session.lock();
        let Some(session) = guard.as_ref() else {
            let mut idle = InputStatus::idle(selected, engine_rate, self.last_fault.lock().clone());
            // The setting, not the device: Settings draws it while disarmed.
            idle.monitoring = self.monitoring();
            return idle;
        };
        let fault = session.shared.fault();
        let frames = session.buffer.frames();
        let recording = self.state() == RecordState::Recording;
        InputStatus {
            state: self.state(),
            device_name: session.device_name.clone(),
            device_id: selected,
            device_sample_rate: session.device_sample_rate,
            device_channels: session.device_channels,
            sample_rate: session.sample_rate,
            peak: session.shared.peak(),
            rms: session.shared.rms(),
            // From the buffer rather than the wall clock: what the musician is
            // watching is how much audio exists, and a stalled device must not
            // keep counting.
            elapsed_seconds: if recording && session.sample_rate > 0.0 {
                frames as f64 / session.sample_rate
            } else {
                0.0
            },
            frames: if recording { frames } else { 0 },
            dropped_samples: session.shared.dropped(),
            limit_reached: session.buffer.limit_reached.load(Ordering::Relaxed),
            max_seconds: MAX_RECORD_SECONDS,
            fault: fault.message().to_owned(),
            monitoring: self.monitoring(),
        }
    }

    /// Fold a fatal stream fault into the state machine.
    ///
    /// Called by whoever is polling [`status`](Self::status): a device that has
    /// gone has no callback left to notice its own death, so the transition out
    /// of `Recording` has to happen on a thread that is still alive. Returns the
    /// take when a running one was salvaged.
    pub fn reap_fault(&self) -> Option<RecordedTake> {
        let fault = {
            let guard = self.session.lock();
            guard.as_ref().map(|session| session.shared.fault())
        };
        if !fault.is_some_and(InputFault::is_fatal) {
            return None;
        }
        let salvaged = if self.state() == RecordState::Recording {
            self.stop().ok()
        } else {
            None
        };
        if let Some(fault) = fault {
            *self.last_fault.lock() = fault.message().to_owned();
        }
        self.disarm();
        salvaged
    }

    fn open_session(&self, wanted: Option<&str>) -> Result<Session, RecordError> {
        let host = cpal::default_host();
        let device = match wanted {
            Some(id) => host
                .input_devices()
                .ok()
                .and_then(|mut devices| devices.find(|device| device_key(device) == id))
                .ok_or_else(|| RecordError::UnknownDevice(id.to_owned()))?,
            None => host
                .default_input_device()
                .or_else(|| {
                    host.input_devices()
                        .ok()
                        .and_then(|mut devices| devices.next())
                })
                .ok_or(RecordError::NoInputDevice)?,
        };

        let supported = device.default_input_config()?;
        let format = supported.sample_format();
        let config: StreamConfig = supported.config();
        let device_rate = config.sample_rate;
        let device_channels = config.channels;
        let device_name = device_name(&device);
        let engine_rate = self.engine_sample_rate();

        let (rt, consumer) = input_channel(
            INPUT_QUEUE_CAPACITY,
            device_channels as usize,
            block_frames_of(&config),
            f64::from(device_rate),
        );
        let shared = Arc::clone(rt.shared());
        let (monitor_tx, monitor_rx) = monitor_channel();
        let rt = rt.with_monitor(monitor_tx);
        shared.set_monitoring(self.monitoring());
        self.monitor.install(
            Some(MonitorReader::new(monitor_rx, f64::from(device_rate))),
            Some(Arc::clone(&shared)),
        );
        if let Ok(id) = device.id() {
            crate::ducking::opt_out_of_ducking(id.id());
        }
        let stream = match build_stream(&device, config, format, rt, Arc::clone(&shared)) {
            Ok(stream) => stream,
            Err(err) => {
                self.monitor.install(None, None);
                return Err(err);
            }
        };
        if let Err(err) = stream.play() {
            self.monitor.install(None, None);
            return Err(err.into());
        }

        let buffer = Arc::new(TakeBuffer::new());
        let worker = spawn_worker(
            consumer,
            Arc::clone(&shared),
            Arc::clone(&buffer),
            f64::from(device_rate),
            engine_rate,
        );

        Ok(Session {
            stream,
            shared,
            buffer,
            worker: Some(worker),
            device_name,
            device_sample_rate: device_rate,
            device_channels,
            sample_rate: engine_rate,
        })
    }
}

impl Drop for RecordService {
    fn drop(&mut self) {
        self.disarm();
    }
}

fn spawn_worker(
    consumer: InputConsumer,
    shared: Arc<InputShared>,
    buffer: Arc<TakeBuffer>,
    device_rate: f64,
    engine_rate: f64,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("wave-input-drain".to_owned())
        .spawn(move || run_worker(consumer, shared, buffer, device_rate, engine_rate))
        .expect("the OS refused a thread for the input drain")
}

/// Drain the ring into the take, converting to the engine rate as it goes.
///
/// One worker per open device, not one per take. It lives from the moment the
/// device opens until it closes, and watches the recording flag itself:
///
/// - **rising edge** -- empty the ring and start a fresh converter, so a take
///   never opens with the room noise from while you were setting the level;
/// - **while recording** -- drain, convert, append, and stop at the length cap;
/// - **falling edge** -- drain what is left, flush the converter's tail, and bump
///   `flushed`, which is what [`Session::end_take`] is waiting on.
fn run_worker(
    mut consumer: InputConsumer,
    shared: Arc<InputShared>,
    buffer: Arc<TakeBuffer>,
    device_rate: f64,
    engine_rate: f64,
) {
    let mut converter = RateConverter::new(device_rate, engine_rate);
    let mut scratch: Vec<f32> = Vec::with_capacity(INPUT_QUEUE_CAPACITY);
    let max_samples = (MAX_RECORD_SECONDS * engine_rate) as usize * CHANNELS;
    let mut was_recording = false;
    // Whether the ring has been wound forward to this take's anchor frame.
    let mut aligned = false;

    loop {
        let stop = buffer.stopping.load(Ordering::Acquire);
        let recording = shared.is_recording() && !stop;

        if recording && !was_recording {
            converter = RateConverter::new(device_rate, engine_rate);
            buffer.limit_reached.store(false, Ordering::Relaxed);
            was_recording = true;
            aligned = false;
        }

        if recording || was_recording {
            // Anything in the ring before the anchor frame is the tail of an
            // earlier take (a block the callback wrote just as it stopped). It
            // is skipped by *index*, never by "drain whatever is there", which
            // would race the callback and eat the take's first block.
            if !aligned {
                if let Some((anchor_frame, _)) = shared.anchor() {
                    let behind = anchor_frame.saturating_sub(consumer.consumed_frames());
                    if behind > 0 {
                        consumer.skip_frames(behind);
                    }
                    aligned = consumer.consumed_frames() >= anchor_frame;
                }
            }
            if aligned {
                scratch.clear();
                consumer.drain_into(&mut scratch, INPUT_QUEUE_CAPACITY);
                if !scratch.is_empty() {
                    append(&buffer, &mut converter, &scratch, max_samples);
                }
            }
        }

        if !recording && was_recording {
            // Falling edge: what the callback pushed before the flag went down
            // was drained above; flush the converter's tail and hand over.
            {
                let mut samples = buffer.samples.lock();
                if aligned {
                    converter.finish(&mut samples);
                }
                samples.truncate(max_samples);
            }
            was_recording = false;
            aligned = false;
            buffer.flushed.fetch_add(1, Ordering::Release);
        }

        if stop {
            return;
        }
        thread::sleep(DRAIN_INTERVAL);
    }
}

/// Where a take captured from `captured_ns` (until `ended_ns`) belongs on the
/// song, in samples at `take_rate`, given the output's playback runs (see
/// [`crate::timeline`]).
///
/// `None` when no *song* run played during the take: a take over silence (or
/// over a pattern loop, whose positions mean nothing on the playlist) has no
/// position of its own, and the caller places it at the start marker.
pub fn take_start_sample(
    timeline: &PlaybackTimeline,
    captured_ns: u64,
    ended_ns: u64,
    take_rate: f64,
) -> Option<f64> {
    let run = timeline.run_for_take(captured_ns, ended_ns)?;
    if !run.song || run.sample_rate <= 0.0 || take_rate <= 0.0 {
        return None;
    }
    Some(run.sample_at(captured_ns) * take_rate / run.sample_rate)
}

/// Convert one drained block into the take, stopping at the length cap.
fn append(buffer: &TakeBuffer, converter: &mut RateConverter, scratch: &[f32], max_samples: usize) {
    let mut samples = buffer.samples.lock();
    if samples.len() >= max_samples {
        return;
    }
    converter.push(scratch, &mut samples);
    if samples.len() >= max_samples {
        samples.truncate(max_samples);
        // Advisory: the service turns this into a stopped take and a sentence
        // the next time it is asked for the status.
        buffer.limit_reached.store(true, Ordering::Relaxed);
    }
}

/// Build the capture stream for whatever format the device speaks.
///
/// Integer devices are converted to `f32` in the callback, which is a multiply
/// per sample over a pre-allocated scratch buffer. Refusing them would make
/// recording impossible on the many interfaces whose shared-mode default is
/// 16-bit.
fn build_stream(
    device: &cpal::Device,
    config: StreamConfig,
    format: SampleFormat,
    rt: InputRtSide,
    shared: Arc<InputShared>,
) -> Result<Stream, RecordError> {
    shared.clear_fault();
    let stream = match format {
        SampleFormat::F32 => build_typed_stream::<f32>(device, config, rt, shared)?,
        SampleFormat::I16 => build_typed_stream::<i16>(device, config, rt, shared)?,
        SampleFormat::U16 => build_typed_stream::<u16>(device, config, rt, shared)?,
        SampleFormat::I32 => build_typed_stream::<i32>(device, config, rt, shared)?,
        SampleFormat::I8 => build_typed_stream::<i8>(device, config, rt, shared)?,
        SampleFormat::U8 => build_typed_stream::<u8>(device, config, rt, shared)?,
        other => return Err(RecordError::UnsupportedSampleFormat(other)),
    };
    Ok(stream)
}

fn build_typed_stream<T>(
    device: &cpal::Device,
    config: StreamConfig,
    mut rt: InputRtSide,
    shared: Arc<InputShared>,
) -> Result<Stream, cpal::Error>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let channels = config.channels.max(1) as usize;
    let sample_rate = config.sample_rate;
    // Whole frames, so a block split across passes never swaps the channels.
    let scratch_len = (MAX_CAPTURE_BLOCK / channels).max(1) * channels;
    let mut scratch = vec![0.0_f32; scratch_len];
    let mut priority = RealtimeToken::empty();
    let mut promoted = false;
    let error_shared = Arc::clone(&shared);

    let frame_ns = if sample_rate > 0 {
        1e9 / f64::from(sample_rate)
    } else {
        0.0
    };
    device.build_input_stream::<T, _, _>(
        config,
        move |data: &[T], info: &cpal::InputCallbackInfo| {
            // When this block's first frame was captured, on the clock the
            // output stream also publishes against. See `crate::timeline`.
            let stamp = info.timestamp();
            let captured_ns =
                now_ns().saturating_sub(duration_ns(stamp.callback.duration_since(stamp.capture)));
            if !promoted {
                // Same trade the output callback makes in `RtState::ensure_realtime`:
                // one allocation and one syscall on the first block buys correct
                // scheduling for every block after it.
                promoted = true;
                disable_denormals();
                priority.promote((data.len() / channels) as u32, sample_rate);
            }
            for (index, block) in data.chunks(scratch_len).enumerate() {
                let out = &mut scratch[..block.len()];
                for (dst, src) in out.iter_mut().zip(block) {
                    *dst = <f32 as cpal::FromSample<T>>::from_sample_(*src);
                }
                let offset_frames = (index * scratch_len / channels) as f64;
                rt.push_block(out, captured_ns + (offset_frames * frame_ns) as u64);
            }
        },
        move |err| {
            // Only a fatal error is stored: a glitch report must neither close
            // the input nor clear a real fault reported before it.
            let fault = fault_for(&err);
            if fault.is_fatal() {
                error_shared.set_fault(fault);
            }
        },
        None,
    )
}

/// Which fault a cpal error is, as a code the callback can store.
fn fault_for(error: &cpal::Error) -> InputFault {
    match error.kind() {
        ErrorKind::DeviceNotAvailable | ErrorKind::StreamInvalidated => InputFault::DeviceLost,
        ErrorKind::PermissionDenied => InputFault::PermissionDenied,
        // A reroute is the host following the default device; the stream is
        // still live, so it is not a fault at all.
        ErrorKind::DeviceChanged => InputFault::None,
        // A glitch, not a failure: WASAPI flags a capture discontinuity on
        // most devices right after the stream starts, and a busy machine can
        // drop a block mid-take. The stream keeps delivering either way --
        // closing the input over it made recording impossible.
        ErrorKind::Xrun | ErrorKind::RealtimeDenied => InputFault::None,
        _ => InputFault::Backend,
    }
}

/// A device's stable identifier, falling back to its name.
///
/// Hosts that cannot produce an id (and ALSA plug devices are one) still need
/// something Settings can persist; a name is imperfect but it is what the user
/// is looking at.
fn device_key(device: &cpal::Device) -> String {
    device
        .id()
        .map(|id| id.to_string())
        .unwrap_or_else(|_| device_name(device))
}

fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|description| description.name().to_owned())
        .unwrap_or_else(|_| "<unknown input device>".to_owned())
}

/// The block the device will ask for, or `0` when it has not committed to one.
fn block_frames_of(config: &StreamConfig) -> u32 {
    match config.buffer_size {
        cpal::BufferSize::Fixed(frames) => frames,
        cpal::BufferSize::Default => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These run on CI, which has no capture device. They cover everything that
    // does not need one; the callback body itself is tested in `input.rs`.

    #[test]
    fn a_fresh_service_is_idle_and_holds_no_device() {
        let service = RecordService::new();
        assert_eq!(service.state(), RecordState::Idle);
        assert_eq!(service.selected_device(), None);
        let status = service.status();
        assert_eq!(status.state, RecordState::Idle);
        assert!(status.device_name.is_empty());
        assert_eq!(status.frames, 0);
        assert_eq!(status.peak, 0.0);
        assert!(status.fault.is_empty());
        assert_eq!(status.max_seconds, MAX_RECORD_SECONDS);
    }

    #[test]
    fn the_take_rate_falls_back_before_any_output_stream_has_opened() {
        let service = RecordService::new();
        assert_eq!(service.engine_sample_rate(), FALLBACK_SAMPLE_RATE);
        service.set_engine_sample_rate(44_100.0);
        assert_eq!(service.engine_sample_rate(), 44_100.0);
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            service.set_engine_sample_rate(bad);
            assert_eq!(
                service.engine_sample_rate(),
                FALLBACK_SAMPLE_RATE,
                "{bad} is not a rate"
            );
        }
    }

    #[test]
    fn operations_needing_a_take_fail_cleanly_while_idle() {
        let service = RecordService::new();
        assert!(matches!(service.stop(), Err(RecordError::NotRecording)));
        assert!(matches!(service.cancel(), Err(RecordError::NotRecording)));
        assert!(service.reap_fault().is_none());
    }

    #[test]
    fn disarming_is_idempotent_without_a_device() {
        let service = RecordService::new();
        service.disarm();
        service.disarm();
        assert_eq!(service.state(), RecordState::Idle);
    }

    #[test]
    fn selecting_a_device_that_is_not_there_is_refused_by_name() {
        let service = RecordService::new();
        let err = service
            .select_device(Some("no-such-device-9f2a"))
            .expect_err("an invented id cannot be selected");
        assert!(
            err.to_string().contains("no-such-device-9f2a"),
            "the message must name the device: {err}"
        );
        assert_eq!(service.selected_device(), None, "the choice must not stick");
    }

    #[test]
    fn selecting_the_system_default_is_always_allowed() {
        let service = RecordService::new();
        service
            .select_device(None)
            .expect("the default is always selectable");
        assert_eq!(service.selected_device(), None);
    }

    #[test]
    fn enumeration_never_fails_on_a_machine_without_capture_hardware() {
        // The list may legitimately be empty here; what matters is that asking
        // does not panic and every entry it does produce is self-describing.
        for device in RecordService::new().devices() {
            assert!(
                !device.id.is_empty(),
                "a device needs an id to be selectable"
            );
            assert!(!device.name.is_empty(), "a device needs a name to be shown");
        }
    }

    #[test]
    fn arming_without_hardware_says_so_rather_than_panicking() {
        let service = RecordService::new();
        match service.arm() {
            // A machine that does have a capture device: the state must follow.
            Ok(status) => {
                assert_eq!(status.state, RecordState::Armed);
                assert!(!status.device_name.is_empty());
                service.disarm();
                assert_eq!(service.state(), RecordState::Idle);
            }
            Err(err) => {
                assert!(!err.to_string().is_empty());
                assert_eq!(service.state(), RecordState::Idle);
            }
        }
    }

    #[test]
    fn a_state_has_a_stable_wire_name() {
        assert_eq!(RecordState::Idle.as_str(), "idle");
        assert_eq!(RecordState::Armed.as_str(), "armed");
        assert_eq!(RecordState::Recording.as_str(), "recording");
        assert_eq!(RecordState::from_code(2), RecordState::Recording);
        assert_eq!(RecordState::from_code(99), RecordState::Idle);
    }

    #[test]
    fn an_empty_take_reports_zero_rather_than_dividing_by_it() {
        let take = RecordedTake {
            samples: Vec::new(),
            sample_rate: 0.0,
            dropped: 0,
            device_name: "nothing".to_owned(),
            limit_reached: false,
            start_sample: None,
        };
        assert!(take.is_empty());
        assert_eq!(take.frames(), 0);
        assert_eq!(take.duration_seconds(), 0.0);
        assert_eq!(take.peak(), 0.0);
    }

    #[test]
    fn a_take_measures_its_length_and_peak_ignoring_a_stray_nan() {
        let take = RecordedTake {
            samples: vec![0.5, -0.75, f32::NAN, 0.25],
            sample_rate: 48_000.0,
            dropped: 3,
            device_name: "Interface".to_owned(),
            limit_reached: false,
            start_sample: None,
        };
        assert_eq!(take.frames(), 2);
        assert!((take.duration_seconds() - 2.0 / 48_000.0).abs() < 1e-12);
        assert!((take.peak() - 0.75).abs() < 1e-6);
        assert!(
            format!("{take:?}").contains("frames: 2"),
            "Debug must not print samples"
        );
    }

    #[test]
    fn every_cpal_error_maps_onto_a_fault_and_a_reroute_maps_onto_none() {
        assert_eq!(
            fault_for(&cpal::Error::new(ErrorKind::DeviceNotAvailable)),
            InputFault::DeviceLost
        );
        assert_eq!(
            fault_for(&cpal::Error::new(ErrorKind::StreamInvalidated)),
            InputFault::DeviceLost
        );
        assert_eq!(
            fault_for(&cpal::Error::new(ErrorKind::PermissionDenied)),
            InputFault::PermissionDenied
        );
        assert_eq!(
            fault_for(&cpal::Error::new(ErrorKind::DeviceChanged)),
            InputFault::None,
            "following the default device is not a failure"
        );
        assert_eq!(
            fault_for(&cpal::Error::new(ErrorKind::DeviceBusy)),
            InputFault::Backend
        );
        assert_eq!(
            fault_for(&cpal::Error::new(ErrorKind::Xrun)),
            InputFault::None,
            "a capture discontinuity (WASAPI reports one at start) must not close the input"
        );
        assert_eq!(
            fault_for(&cpal::Error::new(ErrorKind::RealtimeDenied)),
            InputFault::None
        );
    }

    #[test]
    fn a_default_buffer_size_reports_no_block_and_a_fixed_one_reports_it() {
        let mut config = StreamConfig {
            channels: 2,
            sample_rate: 48_000,
            buffer_size: cpal::BufferSize::Default,
        };
        assert_eq!(block_frames_of(&config), 0);
        config.buffer_size = cpal::BufferSize::Fixed(256);
        assert_eq!(block_frames_of(&config), 256);
    }

    #[test]
    fn the_length_cap_is_the_documented_one_and_fits_in_memory() {
        // Ten minutes of stereo f32 at 48 kHz (a song, and the playlist source
        // limit). Raising MAX_RECORD_SECONDS must make somebody look at this.
        let bytes = (MAX_RECORD_SECONDS * 48_000.0) as usize * CHANNELS * 4;
        assert!(
            bytes < 256 * 1024 * 1024,
            "a take must not be able to eat the machine"
        );
    }

    /// Spin until `ready` or the deadline, so the worker tests wait for a
    /// condition rather than for a guessed number of milliseconds.
    fn wait_until(mut ready: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if ready() {
                return true;
            }
            thread::sleep(Duration::from_millis(1));
        }
        ready()
    }

    /// The worker under test, wired to a ring nothing but the test writes to.
    /// This is the production worker; only the device is missing.
    struct Harness {
        rt: InputRtSide,
        shared: Arc<InputShared>,
        buffer: Arc<TakeBuffer>,
        worker: Option<JoinHandle<()>>,
    }

    impl Harness {
        fn new(device_rate: f64, engine_rate: f64) -> Self {
            let (rt, consumer) = input_channel(1 << 16, 2, 256, device_rate);
            let shared = Arc::clone(rt.shared());
            let buffer = Arc::new(TakeBuffer::new());
            let worker = spawn_worker(
                consumer,
                Arc::clone(&shared),
                Arc::clone(&buffer),
                device_rate,
                engine_rate,
            );
            Self {
                rt,
                shared,
                buffer,
                worker: Some(worker),
            }
        }

        /// Push `frames` frames of a constant value, as the callback would.
        fn play(&mut self, frames: usize, value: f32) {
            let block = vec![value; frames * CHANNELS];
            for chunk in block.chunks(512) {
                self.rt.push_interleaved(chunk);
                thread::sleep(Duration::from_millis(1));
            }
        }

        /// Lower the flag and wait for the worker's flush, as the service does.
        fn end_take(&self) -> Vec<f32> {
            let before = self.buffer.flushed.load(Ordering::Acquire);
            self.shared.stop();
            assert!(
                wait_until(|| self.buffer.flushed.load(Ordering::Acquire) != before),
                "the worker must flush the take"
            );
            std::mem::take(&mut *self.buffer.samples.lock())
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            self.buffer.stopping.store(true, Ordering::Release);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    #[test]
    fn the_worker_keeps_only_what_arrives_between_start_and_stop() {
        let mut harness = Harness::new(48_000.0, 48_000.0);

        // Armed but idle: the room noise while a level is being set must not
        // end up at the head of the take.
        harness.play(512, 1.0);
        assert!(
            wait_until(|| harness.rt.free_slots() == (1 << 16)),
            "an idle worker must keep the ring empty"
        );
        assert!(harness.buffer.samples.lock().is_empty());

        harness.shared.start();
        // Let the worker take the rising edge before the take's audio arrives.
        assert!(wait_until(|| harness.shared.is_recording()));
        thread::sleep(DRAIN_INTERVAL * 3);

        harness.play(1_024, 0.5);
        let take = harness.end_take();

        assert_eq!(
            take.len(),
            1_024 * CHANNELS,
            "every frame of the take is kept"
        );
        assert!(
            take.iter().all(|sample| (*sample - 0.5).abs() < 1e-6),
            "no audio from before the take may survive"
        );
        assert_eq!(
            harness.shared.dropped(),
            0,
            "the ring must not have overflowed"
        );
    }

    #[test]
    fn the_worker_converts_the_device_rate_to_the_engine_rate() {
        let mut harness = Harness::new(44_100.0, 48_000.0);
        harness.shared.start();
        assert!(wait_until(|| harness.shared.is_recording()));
        thread::sleep(DRAIN_INTERVAL * 3);

        harness.play(4_410, 0.25);
        let take = harness.end_take();

        let frames = take.len() / CHANNELS;
        assert!(
            frames.abs_diff(4_800) <= 8,
            "4410 frames at 44.1k should become ~4800 at 48k, got {frames}"
        );
    }

    #[test]
    fn a_second_take_starts_empty_rather_than_appending_to_the_first() {
        let mut harness = Harness::new(48_000.0, 48_000.0);

        harness.shared.start();
        assert!(wait_until(|| harness.shared.is_recording()));
        thread::sleep(DRAIN_INTERVAL * 3);
        harness.play(512, 0.5);
        let first = harness.end_take();
        assert_eq!(first.len(), 512 * CHANNELS);

        harness.shared.start();
        thread::sleep(DRAIN_INTERVAL * 3);
        harness.play(256, -0.5);
        let second = harness.end_take();
        assert_eq!(
            second.len(),
            256 * CHANNELS,
            "the second take is its own length"
        );
        assert!(second.iter().all(|sample| (*sample + 0.5).abs() < 1e-6));
    }

    #[test]
    fn the_paths_a_fault_takes_are_named_sentences() {
        let service = RecordService::new();
        // Nothing armed: reaping is a no-op and the status stays clean.
        assert!(service.reap_fault().is_none());
        assert!(service.status().fault.is_empty());
    }
}

#[cfg(test)]
mod alignment_tests {
    use super::*;
    use crate::input::input_channel;

    #[test]
    fn a_take_captured_while_playing_lands_where_the_listener_was() {
        // The beat's sample 96 000 was audible at t = 2 s; the take's first
        // frame left the microphone 250 ms later.
        let timeline = PlaybackTimeline::new();
        timeline.publish(true, true, 96_000, 480, 2_000_000_000, 48_000.0);
        let start = take_start_sample(&timeline, 2_250_000_000, 9_000_000_000, 48_000.0);
        assert_eq!(start, Some(108_000.0));
        // A take at another rate gets the same *time*, in its own samples.
        let start = take_start_sample(&timeline, 2_250_000_000, 9_000_000_000, 44_100.0);
        assert_eq!(start, Some(99_225.0));
    }

    #[test]
    fn a_take_started_before_play_reaches_back_before_the_run() {
        let timeline = PlaybackTimeline::new();
        timeline.publish(true, true, 0, 480, 1_000_000_000, 48_000.0);
        let start = take_start_sample(&timeline, 900_000_000, 5_000_000_000, 48_000.0);
        assert_eq!(start, Some(-4_800.0));
    }

    #[test]
    fn a_take_over_a_stopped_transport_or_a_pattern_loop_has_no_position() {
        let timeline = PlaybackTimeline::new();
        assert_eq!(take_start_sample(&timeline, 5_000, 9_000, 48_000.0), None);
        timeline.publish(true, true, 0, 480, 0, 48_000.0);
        timeline.publish(false, true, 480, 0, 1_000, 48_000.0);
        assert_eq!(take_start_sample(&timeline, 5_000, 9_000, 48_000.0), None);
        timeline.publish(true, false, 0, 480, 10_000, 48_000.0);
        assert_eq!(take_start_sample(&timeline, 20_000, 90_000, 48_000.0), None);
    }

    #[test]
    fn the_worker_starts_exactly_at_the_anchor_even_with_stale_frames_queued() {
        let (mut rt, consumer) = input_channel(4_096, 2, 4, 48_000.0);
        let shared = Arc::clone(rt.shared());
        // A stale block from an earlier take: the flag is up, then down.
        shared.start();
        rt.push_block(&[9.0; 8], 1);
        shared.stop();
        // The new take.
        shared.start();
        rt.push_block(&[1.0, 1.0, 2.0, 2.0, 3.0, 3.0, 4.0, 4.0], 1_000);
        let (anchor_frame, anchor_ns) = shared.anchor().expect("anchored");
        assert_eq!(anchor_frame, 4, "the stale block occupies frames 0..4");
        assert_eq!(anchor_ns, 1_000);

        let buffer = Arc::new(TakeBuffer::new());
        let worker = {
            let shared = Arc::clone(&shared);
            let buffer = Arc::clone(&buffer);
            thread::spawn(move || run_worker(consumer, shared, buffer, 48_000.0, 48_000.0))
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        while buffer.frames() < 4 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        shared.stop();
        let before = buffer.flushed.load(Ordering::Acquire);
        while buffer.flushed.load(Ordering::Acquire) == before && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        buffer.stopping.store(true, Ordering::Release);
        worker.join().unwrap();
        let samples = buffer.samples.lock().clone();
        assert_eq!(&samples[..8], &[1.0, 1.0, 2.0, 2.0, 3.0, 3.0, 4.0, 4.0]);
    }
}
