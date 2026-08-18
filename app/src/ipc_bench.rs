//! The IPC benchmark harness (Phase 1, spike 4): how fast is Rust -> webview?
//!
//! The shell's high-frequency data (meters now, playhead and waveforms later)
//! all crosses one boundary: a `tauri::ipc::Channel` into WebView2. This module
//! is the instrument that measures that boundary, and nothing else. It is dev
//! machinery, kept in the repo so the numbers can be re-taken after a Tauri
//! upgrade or on another machine, and it is **inert unless an environment
//! variable asks for it**:
//!
//! | Variable | Effect |
//! | --- | --- |
//! | [`DEV_PANELS_ENV`]`=1` | contributes the `ipc-bench` panel, so the matrix can be run by hand |
//! | [`IPC_BENCH_ENV`]`=1` | contributes the *auto* panel: the UI runs the full matrix on startup, writes `.debug/ipc-bench/<timestamp>.json`, prints a report between [`REPORT_BEGIN`]/[`REPORT_END`] and exits |
//!
//! With neither set nothing here runs: no thread is spawned, no panel is
//! contributed, and the UI never even downloads the bench chunk (the panel is a
//! lazy import keyed on a manifest that is not there).
//!
//! # What is measured, and how the two clocks are reconciled
//!
//! Every streamed frame carries the sender's timestamp in *Rust* time
//! ([`epoch_ms`]: milliseconds since the first call, i.e. an `Instant` epoch);
//! the receiver stamps `performance.now()` in *JavaScript* time. Those are two
//! unrelated monotonic clocks, so the offset between them is measured first, by
//! a ping handshake:
//!
//! ```text
//!   js_before = performance.now()
//!   rust_ms   = await invoke('bench_ping')      // Rust's epoch_ms()
//!   js_after  = performance.now()
//!   offset    = rust_ms - (js_before + js_after) / 2
//! ```
//!
//! [`estimate_epoch_offset`] keeps the offset from the *lowest-RTT* sample (the
//! one with the least room for path asymmetry) and reports `rtt_min / 2` as the
//! error bar: a one-way latency figure from this harness is only meaningful to
//! within that. See [`EpochSync`].
//!
//! One-way latency is then `js_receive_ms - (rust_sent_ms - offset)`, and it
//! includes everything real: serialisation, the hop to the webview's UI thread,
//! `eval`/`fetch`, JSON parse or `ArrayBuffer` handover, and the JS callback
//! being scheduled.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

/// Set to `1` and the shell contributes the hidden `ipc-bench` panel, which can
/// run one scenario or the whole matrix on demand.
pub const DEV_PANELS_ENV: &str = "MYDAW_DEV_PANELS";

/// Set to `1` and the shell runs the full matrix on startup with no clicking,
/// writes the results, prints a summary and exits. The agent/CI entry point,
/// mirroring `MYDAW_DEBUG_CAPTURE`.
pub const IPC_BENCH_ENV: &str = "MYDAW_IPC_BENCH";

/// Directory, relative to the debug root, that result files land in.
pub const RESULTS_SUBDIR: [&str; 2] = [".debug", "ipc-bench"];

/// Panel component key, mirrored in `ui/src/panels/registry.ts`.
pub const IPC_BENCH_PANEL: &str = "ipc-bench";
/// Manifest id of the manual panel.
pub const IPC_BENCH_PANEL_ID: &str = "ipc-bench";
/// Manifest id of the auto-running panel. Its *presence* is what tells the UI to
/// run the matrix unattended -- no extra command, no extra round trip.
pub const IPC_BENCH_AUTO_PANEL_ID: &str = "ipc-bench-auto";

/// Markers around the stdout report, so a caller can find it in a log.
pub const REPORT_BEGIN: &str = "===== MYDAW IPC BENCH BEGIN =====";
/// Closing marker; see [`REPORT_BEGIN`].
pub const REPORT_END: &str = "===== MYDAW IPC BENCH END =====";

/// Smallest payload the harness accepts. Below this a JSON frame is all header.
pub const MIN_PAYLOAD_BYTES: usize = 48;
/// Largest payload the harness accepts (2 MiB); guards against a typo in the UI
/// asking Rust to allocate something absurd once per frame.
pub const MAX_PAYLOAD_BYTES: usize = 2 * 1024 * 1024;
/// Fixed header of a binary frame: `seq: u32`, `sent_ms: f64`, `len: u32`, LE.
pub const BINARY_HEADER_BYTES: usize = 16;
/// Longest single scenario.
pub const MAX_DURATION_MS: u64 = 60_000;
/// Most frames one scenario may schedule.
pub const MAX_FRAMES: u32 = 100_000;

// ---------------------------------------------------------------------------
// Clock
// ---------------------------------------------------------------------------

/// Milliseconds since this process first asked for the time.
///
/// A monotonic epoch shared by every part of the harness. `f64` milliseconds
/// (not `Duration`) because the number crosses into JavaScript, where
/// `performance.now()` is exactly that.
pub fn epoch_ms() -> f64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_secs_f64() * 1_000.0
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Which way a streamed frame crosses the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BenchTransport {
    /// `Channel<T: Serialize>`: serde JSON, exactly what `subscribe_meters` uses.
    ChannelJson,
    /// `Channel<InvokeResponseBody>` carrying `Raw` bytes -- the rawest thing
    /// Tauri 2.11 offers on a channel. Small frames still reach JS through an
    /// `eval`'d `Uint8Array` literal; larger ones take the internal fetch path.
    ChannelBinary,
}

impl BenchTransport {
    /// Stable short name, used in labels and the stdout table.
    pub fn label(self) -> &'static str {
        match self {
            Self::ChannelJson => "channel-json",
            Self::ChannelBinary => "channel-binary",
        }
    }
}

impl fmt::Display for BenchTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// One scenario: send frames of `payload_bytes` at `rate_hz` for `duration_ms`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamConfig {
    pub transport: BenchTransport,
    pub payload_bytes: usize,
    pub rate_hz: f64,
    pub duration_ms: u64,
}

/// Why a [`StreamConfig`] was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("payload must be between {MIN_PAYLOAD_BYTES} and {MAX_PAYLOAD_BYTES} bytes, got {0}")]
    Payload(usize),
    #[error("rate must be a finite number of frames per second above zero, got {0}")]
    Rate(String),
    #[error("duration must be between 1 and {MAX_DURATION_MS} ms, got {0}")]
    Duration(u64),
    #[error("that configuration schedules {0} frames; the limit is {MAX_FRAMES}")]
    TooManyFrames(u64),
}

impl StreamConfig {
    /// A scenario in the shape the matrix builds them (5 s runs).
    pub fn new(transport: BenchTransport, payload_bytes: usize, rate_hz: f64) -> Self {
        Self {
            transport,
            payload_bytes,
            rate_hz,
            duration_ms: DEFAULT_DURATION_MS,
        }
    }

    /// Frames this scenario will send.
    ///
    /// Saturates rather than wrapping: validation is what rejects the silly
    /// values, this only has to be total.
    pub fn frame_count(&self) -> u32 {
        let frames = (self.duration_ms as f64 / 1_000.0) * self.rate_hz;
        if !frames.is_finite() || frames <= 0.0 {
            return 0;
        }
        frames.round().min(f64::from(MAX_FRAMES)) as u32
    }

    /// Nominal spacing between frames.
    pub fn period(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.rate_hz)
    }

    /// A label of the form `channel-json/64B@30Hz`.
    pub fn label(&self) -> String {
        format!(
            "{}/{}@{}Hz",
            self.transport.label(),
            human_bytes(self.payload_bytes),
            trim_float(self.rate_hz)
        )
    }

    /// # Errors
    /// [`ConfigError`] when a field is outside the harness's limits, or when the
    /// combination would schedule more than [`MAX_FRAMES`] frames.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !(MIN_PAYLOAD_BYTES..=MAX_PAYLOAD_BYTES).contains(&self.payload_bytes) {
            return Err(ConfigError::Payload(self.payload_bytes));
        }
        if !self.rate_hz.is_finite() || self.rate_hz <= 0.0 {
            return Err(ConfigError::Rate(trim_float(self.rate_hz)));
        }
        if self.duration_ms == 0 || self.duration_ms > MAX_DURATION_MS {
            return Err(ConfigError::Duration(self.duration_ms));
        }
        let frames = (self.duration_ms as f64 / 1_000.0) * self.rate_hz;
        if frames > f64::from(MAX_FRAMES) {
            return Err(ConfigError::TooManyFrames(frames as u64));
        }
        Ok(())
    }
}

/// Every scenario runs this long, so p99 rests on a comparable sample count at
/// every rate.
pub const DEFAULT_DURATION_MS: u64 = 5_000;

/// The standard matrix: both transports x the payload/rate grid.
///
/// Payloads stand for real things: 64 B is a meter frame, 4 KB a chunk of
/// playhead/automation state, 64 KB a waveform mipmap tile, 1 MB a full
/// pre-rendered overview image. The 1 MB row is only run at 5 and 10 Hz --
/// nothing in a DAW pushes a megabyte 240 times a second, and the question there
/// is throughput, not frame rate.
pub fn default_plan() -> Vec<StreamConfig> {
    let mut plan = Vec::new();
    for transport in [BenchTransport::ChannelJson, BenchTransport::ChannelBinary] {
        for payload in [64_usize, 4 * 1024, 64 * 1024] {
            for rate in [30.0, 60.0, 240.0] {
                plan.push(StreamConfig::new(transport, payload, rate));
            }
        }
        for rate in [5.0, 10.0] {
            plan.push(StreamConfig::new(transport, 1024 * 1024, rate));
        }
    }
    plan
}

/// Scenarios past anything a DAW needs, to find where the boundary gives out.
///
/// The standard matrix tops out at 15 MB/s and never drops a frame, which
/// answers "is this fast enough" but not "how much headroom is there". These
/// four sizes push to 30 and 60 MB/s from both directions -- more frames of the
/// same size, and bigger frames at the same rate.
pub fn stress_plan() -> Vec<StreamConfig> {
    let mut plan = Vec::new();
    for transport in [BenchTransport::ChannelJson, BenchTransport::ChannelBinary] {
        plan.push(StreamConfig::new(transport, 64 * 1024, 480.0)); // 30 MB/s
        plan.push(StreamConfig::new(transport, 256 * 1024, 240.0)); // 60 MB/s
        plan.push(StreamConfig::new(transport, 1024 * 1024, 30.0)); // 30 MB/s
        plan.push(StreamConfig::new(transport, 1024 * 1024, 60.0)); // 60 MB/s
    }
    plan
}

/// The plan a run should execute, given the value of [`IPC_BENCH_ENV`].
///
/// `stress` appends [`stress_plan`]; anything else is the standard matrix. Kept
/// pure so the gating is a test rather than a hope.
pub fn plan_for(mode: &str) -> Vec<StreamConfig> {
    let mut plan = default_plan();
    if mode.trim().eq_ignore_ascii_case("stress") {
        plan.extend(stress_plan());
    }
    plan
}

/// [`plan_for`] applied to this process's environment.
pub fn env_plan() -> Vec<StreamConfig> {
    plan_for(&std::env::var(IPC_BENCH_ENV).unwrap_or_default())
}

// ---------------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------------

/// One frame, ready to hand to the transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BenchPayload {
    /// A JSON document, already serialised. Tauri sends the string as-is.
    Json(String),
    /// Raw bytes: 16-byte little-endian header then filler.
    Binary(Vec<u8>),
}

impl BenchPayload {
    /// Bytes actually handed to Tauri.
    pub fn len(&self) -> usize {
        match self {
            Self::Json(text) => text.len(),
            Self::Binary(bytes) => bytes.len(),
        }
    }

    /// Never true in practice; present because clippy asks for it next to `len`.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Build the frame `seq` for `config`, stamped `sent_ms`.
///
/// Both encodings are *exactly* `payload_bytes` long, so a JSON scenario and a
/// binary scenario at the same payload size move the same number of bytes and
/// the comparison is about encoding, not volume.
pub fn build_frame(config: &StreamConfig, seq: u32, sent_ms: f64) -> BenchPayload {
    match config.transport {
        BenchTransport::ChannelJson => {
            BenchPayload::Json(json_frame(seq, sent_ms, config.payload_bytes))
        }
        BenchTransport::ChannelBinary => {
            BenchPayload::Binary(binary_frame(seq, sent_ms, config.payload_bytes))
        }
    }
}

/// `{"seq":0,"sentMs":12.345,"pad":"aaa..."}`, padded to exactly `payload_bytes`.
///
/// Hand-built rather than derived: the padding has to be sized against the
/// serialised length, which means knowing that length before serialising.
pub fn json_frame(seq: u32, sent_ms: f64, payload_bytes: usize) -> String {
    let head = format!("{{\"seq\":{seq},\"sentMs\":{sent_ms:.3},\"pad\":\"");
    let tail = "\"}";
    let pad = payload_bytes.saturating_sub(head.len() + tail.len());
    let mut out = String::with_capacity(head.len() + pad + tail.len());
    out.push_str(&head);
    out.extend(std::iter::repeat_n('a', pad));
    out.push_str(tail);
    out
}

/// `seq: u32 | sent_ms: f64 | len: u32` little-endian, then filler to
/// `payload_bytes`.
pub fn binary_frame(seq: u32, sent_ms: f64, payload_bytes: usize) -> Vec<u8> {
    let total = payload_bytes.max(BINARY_HEADER_BYTES);
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&sent_ms.to_le_bytes());
    out.extend_from_slice(&((total - BINARY_HEADER_BYTES) as u32).to_le_bytes());
    // A repeating ramp rather than zeros: nothing in the path compresses today,
    // but a zero-filled megabyte is the one payload a future optimisation would
    // flatter.
    out.extend((0..total - BINARY_HEADER_BYTES).map(|i| (i % 251) as u8));
    out
}

/// Send `config`'s frames, paced at its rate, until they run out or `sink`
/// returns `false` (the webview went away).
///
/// Returns how many frames were handed to `sink` successfully. Runs on a plain
/// std thread -- never the audio thread, never the main thread.
pub fn run_stream(config: StreamConfig, mut sink: impl FnMut(BenchPayload) -> bool) -> u32 {
    let frames = config.frame_count();
    let period = config.period();
    let start = Instant::now();
    for seq in 0..frames {
        sleep_until(start + period.mul_f64(f64::from(seq)));
        let payload = build_frame(&config, seq, epoch_ms());
        if !sink(payload) {
            return seq;
        }
    }
    frames
}

/// Sleep to `deadline`, then spin the last millisecond.
///
/// `thread::sleep` on Windows overshoots by a scheduler quantum (~1.5 ms with
/// the default timer resolution), which at 240 Hz is a third of the frame
/// period. Handing back the last millisecond to a spin keeps the *sender's*
/// jitter well below what is being measured; it costs one core on one dev-only
/// thread for at most a millisecond per frame.
fn sleep_until(deadline: Instant) {
    const SPIN: Duration = Duration::from_millis(1);
    let now = Instant::now();
    if deadline <= now {
        return;
    }
    let remaining = deadline - now;
    if remaining > SPIN {
        std::thread::sleep(remaining - SPIN);
    }
    while Instant::now() < deadline {
        std::hint::spin_loop();
    }
}

// ---------------------------------------------------------------------------
// Statistics
// ---------------------------------------------------------------------------

/// Latency distribution of one scenario, in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatencyStats {
    pub count: usize,
    pub min_ms: f64,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

/// Nearest-rank percentile of an already-sorted slice.
///
/// Nearest-rank (not interpolated): with a few hundred samples per scenario the
/// interpolation would invent precision the measurement does not have, and the
/// reported p99 should be a value that actually occurred.
pub fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let q = q.clamp(0.0, 1.0);
    let rank = (q * sorted.len() as f64).ceil().max(1.0) as usize;
    sorted[rank.min(sorted.len()) - 1]
}

/// Summarise raw latency samples. `None` when there are none.
pub fn summarize(samples: &[f64]) -> Option<LatencyStats> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted: Vec<f64> = samples.iter().copied().filter(|v| v.is_finite()).collect();
    if sorted.is_empty() {
        return None;
    }
    sorted.sort_by(f64::total_cmp);
    let sum: f64 = sorted.iter().sum();
    Some(LatencyStats {
        count: sorted.len(),
        min_ms: sorted[0],
        mean_ms: sum / sorted.len() as f64,
        p50_ms: percentile(&sorted, 0.50),
        p95_ms: percentile(&sorted, 0.95),
        p99_ms: percentile(&sorted, 0.99),
        max_ms: sorted[sorted.len() - 1],
    })
}

// ---------------------------------------------------------------------------
// Epoch synchronisation
// ---------------------------------------------------------------------------

/// One ping of the clock handshake, as observed by JavaScript.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EpochSample {
    /// `performance.now()` immediately before the invoke.
    pub js_before_ms: f64,
    /// What Rust answered: [`epoch_ms`] at the moment the command ran.
    pub rust_ms: f64,
    /// `performance.now()` when the promise resolved.
    pub js_after_ms: f64,
}

impl EpochSample {
    /// Round-trip time of this ping.
    pub fn rtt_ms(&self) -> f64 {
        self.js_after_ms - self.js_before_ms
    }

    /// Rust-minus-JS offset implied by this ping alone, assuming a symmetric
    /// path (the assumption the error bar exists to bound).
    pub fn offset_ms(&self) -> f64 {
        self.rust_ms - (self.js_before_ms + self.js_after_ms) / 2.0
    }
}

/// The reconciled clocks, with the honesty attached.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EpochSync {
    /// Subtract from a Rust timestamp to get the equivalent `performance.now()`.
    pub offset_ms: f64,
    pub rtt_min_ms: f64,
    pub rtt_median_ms: f64,
    /// +/- bound on every one-way latency derived from this sync: half the best
    /// round trip, which is the most the (unmeasurable) path asymmetry can hide.
    pub error_ms: f64,
    pub samples: usize,
}

/// Reconcile the two clocks from a handshake.
///
/// Uses the offset of the *lowest-RTT* ping rather than the median offset: on a
/// quiet path the fastest round trip is the one least distorted by scheduling on
/// either side, which is the same argument NTP makes. The median RTT is reported
/// alongside so a run where the two diverge is visible rather than silent.
///
/// Returns `None` for an empty handshake.
pub fn estimate_epoch_offset(samples: &[EpochSample]) -> Option<EpochSync> {
    let usable: Vec<&EpochSample> = samples
        .iter()
        .filter(|s| s.rtt_ms().is_finite() && s.rtt_ms() >= 0.0 && s.rust_ms.is_finite())
        .collect();
    let best = usable
        .iter()
        .copied()
        .min_by(|a, b| a.rtt_ms().total_cmp(&b.rtt_ms()))?;
    let mut rtts: Vec<f64> = usable.iter().map(|s| s.rtt_ms()).collect();
    rtts.sort_by(f64::total_cmp);
    Some(EpochSync {
        offset_ms: best.offset_ms(),
        rtt_min_ms: rtts[0],
        rtt_median_ms: percentile(&rtts, 0.50),
        error_ms: rtts[0] / 2.0,
        samples: usable.len(),
    })
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

/// What the UI measured for one scenario, before aggregation.
///
/// Raw samples travel back to Rust so that percentiles, achieved rate and the
/// drop count are computed in exactly one place -- the one with tests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioSamples {
    pub label: String,
    /// `None` for the live meter-path scenario, which has no configured transport.
    #[serde(default)]
    pub transport: Option<BenchTransport>,
    pub payload_bytes: usize,
    pub target_rate_hz: f64,
    pub duration_ms: u64,
    /// Frames Rust said it would send.
    pub planned: u32,
    /// One-way latencies, already offset-corrected by the UI.
    pub latencies_ms: Vec<f64>,
    /// `performance.now()` of the first and last frame received.
    pub first_recv_ms: f64,
    pub last_recv_ms: f64,
    /// `PerformanceObserver('longtask')` during this scenario.
    pub long_tasks: u32,
    pub long_task_ms: f64,
    #[serde(default)]
    pub note: Option<String>,
}

/// One row of the results table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioReport {
    pub label: String,
    pub transport: Option<BenchTransport>,
    pub payload_bytes: usize,
    pub target_rate_hz: f64,
    pub duration_ms: u64,
    pub planned: u32,
    pub received: usize,
    /// Frames scheduled but never seen by JS. Negative is impossible and clamped.
    pub dropped: u32,
    /// Frames per second actually observed at the receiver.
    pub achieved_rate_hz: f64,
    /// Bytes per second actually delivered.
    pub throughput_mb_s: f64,
    pub long_tasks: u32,
    pub long_task_ms: f64,
    pub stats: Option<LatencyStats>,
    pub note: Option<String>,
}

/// Aggregate one scenario's raw samples.
pub fn build_scenario(input: &ScenarioSamples) -> ScenarioReport {
    let received = input.latencies_ms.len();
    let span_s = (input.last_recv_ms - input.first_recv_ms) / 1_000.0;
    let achieved_rate_hz = if received > 1 && span_s > 0.0 {
        (received - 1) as f64 / span_s
    } else {
        0.0
    };
    ScenarioReport {
        label: input.label.clone(),
        transport: input.transport,
        payload_bytes: input.payload_bytes,
        target_rate_hz: input.target_rate_hz,
        duration_ms: input.duration_ms,
        planned: input.planned,
        received,
        dropped: (input.planned as usize).saturating_sub(received) as u32,
        achieved_rate_hz,
        throughput_mb_s: achieved_rate_hz * input.payload_bytes as f64 / (1024.0 * 1024.0),
        long_tasks: input.long_tasks,
        long_task_ms: input.long_task_ms,
        stats: summarize(&input.latencies_ms),
        note: input.note.clone(),
    }
}

/// Where the run happened. The JS half is what the webview knows about itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchEnvironment {
    pub user_agent: String,
    #[serde(default)]
    pub hardware_concurrency: Option<u32>,
    #[serde(default)]
    pub device_pixel_ratio: Option<f64>,
    /// Chromium build behind WebView2, parsed out of the user agent.
    #[serde(default)]
    pub webview_version: Option<String>,
    pub os: String,
    pub arch: String,
    /// `debug` or `release`: a debug shell serialises measurably slower.
    pub profile: String,
    pub app_version: String,
}

/// Everything the UI hands back at the end of a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchRunInput {
    pub user_agent: String,
    #[serde(default)]
    pub hardware_concurrency: Option<u32>,
    #[serde(default)]
    pub device_pixel_ratio: Option<f64>,
    pub epoch_samples: Vec<EpochSample>,
    /// Round-trip times of a bare `invoke`, in ms.
    pub rtt_ms: Vec<f64>,
    /// Round-trip times of an `async` command (Tauri's worker pool) in ms.
    #[serde(default)]
    pub rtt_async_ms: Vec<f64>,
    pub scenarios: Vec<ScenarioSamples>,
}

/// A finished run: the file written to `.debug/ipc-bench/`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BenchReport {
    /// UTC `YYYYMMDD-HHMMSS`, also the file name.
    pub run_id: String,
    pub environment: BenchEnvironment,
    pub epoch: Option<EpochSync>,
    /// Invoke round-trip, sync command (Tauri runs it on the main thread).
    pub rtt: Option<LatencyStats>,
    /// Invoke round-trip, async command (Tauri's worker pool).
    pub rtt_async: Option<LatencyStats>,
    pub scenarios: Vec<ScenarioReport>,
}

/// Aggregate a whole run.
pub fn build_report(input: &BenchRunInput, run_id: String) -> BenchReport {
    BenchReport {
        run_id,
        environment: BenchEnvironment {
            webview_version: webview_version_from_user_agent(&input.user_agent),
            user_agent: input.user_agent.clone(),
            hardware_concurrency: input.hardware_concurrency,
            device_pixel_ratio: input.device_pixel_ratio,
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
            .to_owned(),
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
        },
        epoch: estimate_epoch_offset(&input.epoch_samples),
        rtt: summarize(&input.rtt_ms),
        rtt_async: summarize(&input.rtt_async_ms),
        scenarios: input.scenarios.iter().map(build_scenario).collect(),
    }
}

/// The Chromium version out of a WebView2 user agent (`... Edg/141.0.3537.85`).
///
/// WebView2 reports its runtime version in the `Edg/` token; there is no other
/// way for the webview to tell us what it is, and it matches what the Evergreen
/// runtime installer reports.
pub fn webview_version_from_user_agent(user_agent: &str) -> Option<String> {
    user_agent
        .split_whitespace()
        .find_map(|token| token.strip_prefix("Edg/"))
        .map(str::to_owned)
        .filter(|v| !v.is_empty())
}

/// The report as an agent reads it: one header block, one row per scenario.
pub fn report_text(report: &BenchReport, path: Option<&Path>) -> String {
    let mut out = String::new();
    out.push_str(REPORT_BEGIN);
    out.push('\n');
    out.push_str(&format!("run              {}\n", report.run_id));
    out.push_str(&format!(
        "environment      {} {} / {} build / webview {}\n",
        report.environment.os,
        report.environment.arch,
        report.environment.profile,
        report
            .environment
            .webview_version
            .as_deref()
            .unwrap_or("unknown"),
    ));
    match &report.epoch {
        Some(sync) => out.push_str(&format!(
            "clock sync       offset {:.3} ms, rtt min {:.3} ms (error bar +/-{:.3} ms), {} pings\n",
            sync.offset_ms, sync.rtt_min_ms, sync.error_ms, sync.samples
        )),
        None => out.push_str("clock sync       NOT MEASURED (latencies are unreliable)\n"),
    }
    if let Some(rtt) = &report.rtt {
        out.push_str(&format!(
            "invoke rtt       p50 {:.3} ms, p95 {:.3} ms, max {:.3} ms ({} samples)\n",
            rtt.p50_ms, rtt.p95_ms, rtt.max_ms, rtt.count
        ));
    }
    if let Some(rtt) = &report.rtt_async {
        out.push_str(&format!(
            "invoke rtt async p50 {:.3} ms, p95 {:.3} ms, max {:.3} ms ({} samples)\n",
            rtt.p50_ms, rtt.p95_ms, rtt.max_ms, rtt.count
        ));
    }
    out.push('\n');
    out.push_str(&format!(
        "{:<28} {:>7} {:>7} {:>7} {:>7} {:>7} {:>8} {:>7} {:>6}\n",
        "scenario", "p50", "p95", "p99", "max", "rate", "MB/s", "drops", "lt"
    ));
    for scenario in &report.scenarios {
        let (p50, p95, p99, max) = match &scenario.stats {
            Some(s) => (s.p50_ms, s.p95_ms, s.p99_ms, s.max_ms),
            None => (f64::NAN, f64::NAN, f64::NAN, f64::NAN),
        };
        out.push_str(&format!(
            "{:<28} {p50:>7.2} {p95:>7.2} {p99:>7.2} {max:>7.2} {:>7.1} {:>8.2} {:>7} {:>6}\n",
            truncate(&scenario.label, 28),
            scenario.achieved_rate_hz,
            scenario.throughput_mb_s,
            scenario.dropped,
            scenario.long_tasks,
        ));
        if let Some(note) = &scenario.note {
            out.push_str(&format!("  note: {note}\n"));
        }
    }
    out.push('\n');
    out.push_str(
        "latency is one-way (Rust send -> JS receive); rate is measured at the receiver\n",
    );
    if let Some(path) = path {
        out.push_str(&format!("results          {}\n", path.display()));
    }
    out.push_str(REPORT_END);
    out.push('\n');
    out
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

/// `<root>/.debug/ipc-bench`.
pub fn results_dir(root: &Path) -> PathBuf {
    let mut path = root.to_path_buf();
    for part in RESULTS_SUBDIR {
        path.push(part);
    }
    path
}

/// Write `report` as `<root>/.debug/ipc-bench/<run id>.json`.
///
/// # Errors
/// Whatever creating the directory or writing the file reports.
pub fn write_report(report: &BenchReport, root: &Path) -> std::io::Result<PathBuf> {
    let dir = results_dir(root);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", report.run_id));
    let json = serde_json::to_string_pretty(report)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    std::fs::write(&path, json)?;
    Ok(path)
}

/// Run id for a report taken now: the same UTC stamp the capture harness uses.
pub fn run_id(now: SystemTime) -> String {
    crate::debug_capture::timestamp_utc(now)
}

// ---------------------------------------------------------------------------
// Environment gating
// ---------------------------------------------------------------------------

/// Whether an environment variable is set to something other than `0`/empty.
fn env_flag(name: &str) -> bool {
    match std::env::var(name) {
        Ok(value) => {
            let value = value.trim();
            !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
        }
        Err(_) => false,
    }
}

/// Whether the shell was started with [`IPC_BENCH_ENV`]: run the matrix, write
/// the file, exit.
pub fn auto_mode() -> bool {
    env_flag(IPC_BENCH_ENV)
}

/// Whether the hidden dev panels were asked for.
pub fn dev_panels_enabled() -> bool {
    env_flag(DEV_PANELS_ENV) || auto_mode()
}

/// The bench panels this run contributes, if any.
///
/// Pure so the gating is testable: `dev_panels` alone gives the manual panel,
/// `auto` gives the self-running one (whose *id* is the signal the UI acts on).
pub fn bench_panels_for(dev_panels: bool, auto: bool) -> Vec<(String, String, String)> {
    let mut panels = Vec::new();
    if auto {
        panels.push((
            IPC_BENCH_AUTO_PANEL_ID.to_owned(),
            "IPC Bench (auto)".to_owned(),
            IPC_BENCH_PANEL.to_owned(),
        ));
    } else if dev_panels {
        panels.push((
            IPC_BENCH_PANEL_ID.to_owned(),
            "IPC Bench".to_owned(),
            IPC_BENCH_PANEL.to_owned(),
        ));
    }
    panels
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

/// `64B`, `4KB`, `1MB` -- exact binary multiples only, otherwise plain bytes.
fn human_bytes(bytes: usize) -> String {
    if bytes >= 1024 * 1024 && bytes.is_multiple_of(1024 * 1024) {
        format!("{}MB", bytes / (1024 * 1024))
    } else if bytes >= 1024 && bytes.is_multiple_of(1024) {
        format!("{}KB", bytes / 1024)
    } else {
        format!("{bytes}B")
    }
}

/// `30` not `30.0`, `2.5` still `2.5`.
fn trim_float(value: f64) -> String {
    if value.fract() == 0.0 && value.is_finite() {
        format!("{value:.0}")
    } else {
        format!("{value}")
    }
}

/// Clip a label to `width` characters so the table stays aligned.
fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        text.to_owned()
    } else {
        text.chars().take(width - 1).chain(['~']).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(transport: BenchTransport, payload: usize, rate: f64) -> StreamConfig {
        StreamConfig::new(transport, payload, rate)
    }

    // --- configuration ---------------------------------------------------

    #[test]
    fn a_scenario_schedules_rate_times_duration_frames() {
        assert_eq!(
            cfg(BenchTransport::ChannelJson, 64, 30.0).frame_count(),
            150
        );
        assert_eq!(
            cfg(BenchTransport::ChannelJson, 64, 240.0).frame_count(),
            1_200
        );
        assert_eq!(
            cfg(BenchTransport::ChannelBinary, 1024 * 1024, 5.0).frame_count(),
            25
        );
    }

    #[test]
    fn configuration_limits_are_enforced() {
        assert_eq!(
            cfg(BenchTransport::ChannelJson, 8, 30.0).validate(),
            Err(ConfigError::Payload(8))
        );
        assert_eq!(
            cfg(BenchTransport::ChannelJson, 64, 0.0).validate(),
            Err(ConfigError::Rate("0".to_owned()))
        );
        assert!(matches!(
            cfg(BenchTransport::ChannelJson, 64, f64::NAN).validate(),
            Err(ConfigError::Rate(_))
        ));

        let mut long = cfg(BenchTransport::ChannelJson, 64, 30.0);
        long.duration_ms = MAX_DURATION_MS + 1;
        assert_eq!(
            long.validate(),
            Err(ConfigError::Duration(MAX_DURATION_MS + 1))
        );
        long.duration_ms = 0;
        assert_eq!(long.validate(), Err(ConfigError::Duration(0)));

        let mut dense = cfg(BenchTransport::ChannelJson, 64, 50_000.0);
        dense.duration_ms = 10_000;
        assert!(matches!(
            dense.validate(),
            Err(ConfigError::TooManyFrames(_))
        ));

        assert!(cfg(BenchTransport::ChannelBinary, 64, 240.0)
            .validate()
            .is_ok());
    }

    #[test]
    fn a_scenario_config_round_trips_through_the_wire_shape() {
        // The UI sends these back verbatim; camelCase and kebab-case names are
        // part of the contract with `ui/src/dev/ipcBench.ts`.
        let config = cfg(BenchTransport::ChannelBinary, 4096, 60.0);
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("\"transport\":\"channel-binary\""), "{json}");
        assert!(json.contains("\"payloadBytes\":4096"), "{json}");
        assert!(json.contains("\"rateHz\":60"), "{json}");
        assert_eq!(
            serde_json::from_str::<StreamConfig>(&json).unwrap(),
            config,
            "a config survives the round trip the UI puts it through"
        );
    }

    #[test]
    fn the_default_matrix_covers_both_transports_and_every_payload() {
        let plan = default_plan();
        assert_eq!(
            plan.len(),
            22,
            "3 payloads x 3 rates + 1MB x 2 rates, twice"
        );
        assert!(plan.iter().all(|c| c.validate().is_ok()));
        for transport in [BenchTransport::ChannelJson, BenchTransport::ChannelBinary] {
            let rows: Vec<&StreamConfig> =
                plan.iter().filter(|c| c.transport == transport).collect();
            assert_eq!(rows.len(), 11);
            assert!(rows
                .iter()
                .any(|c| c.payload_bytes == 64 && c.rate_hz == 240.0));
            assert!(rows.iter().any(|c| c.payload_bytes == 1024 * 1024));
        }
        assert!(
            !plan
                .iter()
                .any(|c| c.payload_bytes == 1024 * 1024 && c.rate_hz > 10.0),
            "a megabyte is never asked for above 10 Hz"
        );
    }

    #[test]
    fn the_stress_tail_is_opt_in_and_pushes_past_the_standard_matrix() {
        let standard = plan_for("1");
        assert_eq!(standard, default_plan(), "only 'stress' adds anything");

        let stressed = plan_for("STRESS");
        assert_eq!(stressed.len(), default_plan().len() + 8);
        assert!(stressed.iter().all(|c| c.validate().is_ok()));
        let peak = stressed
            .iter()
            .map(|c| c.payload_bytes as f64 * c.rate_hz)
            .fold(0.0, f64::max);
        assert!(
            peak >= 60.0 * 1024.0 * 1024.0,
            "the stress tail asks for at least 60 MB/s, got {peak} B/s"
        );
    }

    #[test]
    fn labels_are_readable_and_stable() {
        assert_eq!(
            cfg(BenchTransport::ChannelJson, 64, 30.0).label(),
            "channel-json/64B@30Hz"
        );
        assert_eq!(
            cfg(BenchTransport::ChannelBinary, 65_536, 240.0).label(),
            "channel-binary/64KB@240Hz"
        );
        assert_eq!(
            cfg(BenchTransport::ChannelBinary, 1024 * 1024, 5.0).label(),
            "channel-binary/1MB@5Hz"
        );
    }

    // --- frames ----------------------------------------------------------

    #[test]
    fn a_json_frame_is_exactly_the_requested_size() {
        for size in [64_usize, 4 * 1024, 64 * 1024, 1024 * 1024] {
            for seq in [0_u32, 7, 999_999] {
                let frame = json_frame(seq, 12_345.678, size);
                assert_eq!(frame.len(), size, "seq {seq} at {size} bytes");
                let parsed: serde_json::Value = serde_json::from_str(&frame).unwrap();
                assert_eq!(parsed["seq"], seq);
                assert_eq!(parsed["sentMs"], 12_345.678);
            }
        }
    }

    #[test]
    fn a_binary_frame_carries_its_header_little_endian() {
        let frame = binary_frame(9, 1.5, 64);
        assert_eq!(frame.len(), 64);
        assert_eq!(u32::from_le_bytes(frame[0..4].try_into().unwrap()), 9);
        assert_eq!(f64::from_le_bytes(frame[4..12].try_into().unwrap()), 1.5);
        assert_eq!(
            u32::from_le_bytes(frame[12..16].try_into().unwrap()),
            (64 - BINARY_HEADER_BYTES) as u32
        );
        // Never all-zero: a compressing transport must not get a free ride.
        assert!(frame[16..].iter().any(|&b| b != 0));
    }

    #[test]
    fn both_encodings_move_the_same_number_of_bytes() {
        for size in [64_usize, 4 * 1024, 64 * 1024] {
            let json = build_frame(&cfg(BenchTransport::ChannelJson, size, 30.0), 3, 1.0);
            let raw = build_frame(&cfg(BenchTransport::ChannelBinary, size, 30.0), 3, 1.0);
            assert_eq!(json.len(), size);
            assert_eq!(raw.len(), size);
        }
    }

    #[test]
    fn a_stream_sends_every_frame_in_order_and_stops_when_the_sink_dies() {
        let mut config = cfg(BenchTransport::ChannelBinary, 64, 200.0);
        config.duration_ms = 100; // 20 frames, ~100 ms
        let mut seen = Vec::new();
        let sent = run_stream(config, |payload| {
            let BenchPayload::Binary(bytes) = payload else {
                panic!("a binary scenario must produce binary frames")
            };
            seen.push(u32::from_le_bytes(bytes[0..4].try_into().unwrap()));
            true
        });
        assert_eq!(sent, 20);
        assert_eq!(seen, (0..20).collect::<Vec<u32>>());

        let mut count = 0;
        let sent = run_stream(config, |_| {
            count += 1;
            count < 3
        });
        assert_eq!(sent, 2, "the frame the sink refused is not counted as sent");
    }

    #[test]
    fn a_stream_holds_its_rate_within_a_frame_or_two() {
        let mut config = cfg(BenchTransport::ChannelJson, 64, 100.0);
        config.duration_ms = 300; // 30 frames at 10 ms
        let start = Instant::now();
        let sent = run_stream(config, |_| true);
        let elapsed = start.elapsed();
        assert_eq!(sent, 30);
        // The last frame is scheduled at 290 ms; anything past 400 ms means the
        // pacer is not pacing.
        assert!(
            (Duration::from_millis(250)..Duration::from_millis(400)).contains(&elapsed),
            "30 frames at 100 Hz took {elapsed:?}"
        );
    }

    // --- statistics ------------------------------------------------------

    #[test]
    fn percentiles_are_nearest_rank_over_the_sorted_sample() {
        let sorted: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(percentile(&sorted, 0.0), 1.0);
        assert_eq!(percentile(&sorted, 0.50), 50.0);
        assert_eq!(percentile(&sorted, 0.95), 95.0);
        assert_eq!(percentile(&sorted, 0.99), 99.0);
        assert_eq!(percentile(&sorted, 1.0), 100.0);
        assert!(percentile(&[], 0.5).is_nan());
        assert_eq!(percentile(&[7.0], 0.99), 7.0);
        // Small samples: p99 of five values is the largest one, not an
        // interpolated value that never occurred.
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0, 5.0], 0.99), 5.0);
    }

    #[test]
    fn summarize_reports_the_whole_distribution() {
        let stats = summarize(&[5.0, 1.0, 3.0, 2.0, 4.0]).unwrap();
        assert_eq!(stats.count, 5);
        assert_eq!(stats.min_ms, 1.0);
        assert_eq!(stats.max_ms, 5.0);
        assert_eq!(stats.mean_ms, 3.0);
        assert_eq!(stats.p50_ms, 3.0);
        assert_eq!(stats.p95_ms, 5.0);
    }

    #[test]
    fn summarize_survives_empty_and_non_finite_input() {
        assert!(summarize(&[]).is_none());
        assert!(summarize(&[f64::NAN, f64::INFINITY]).is_none());
        let stats = summarize(&[1.0, f64::NAN, 3.0]).unwrap();
        assert_eq!(stats.count, 2, "a NaN sample is dropped, not counted");
        assert_eq!(stats.max_ms, 3.0);
    }

    // --- epoch sync ------------------------------------------------------

    #[test]
    fn the_epoch_offset_is_recovered_from_a_symmetric_handshake() {
        // Rust's clock reads 1000 ms ahead of the webview's; each ping takes 4 ms
        // split evenly, so the true offset is exactly 1000.
        let samples: Vec<EpochSample> = (0..5)
            .map(|i| {
                let before = 100.0 + f64::from(i) * 10.0;
                EpochSample {
                    js_before_ms: before,
                    rust_ms: before + 2.0 + 1_000.0,
                    js_after_ms: before + 4.0,
                }
            })
            .collect();
        let sync = estimate_epoch_offset(&samples).unwrap();
        assert!((sync.offset_ms - 1_000.0).abs() < 1e-9);
        assert!((sync.rtt_min_ms - 4.0).abs() < 1e-9);
        assert!((sync.error_ms - 2.0).abs() < 1e-9);
        assert_eq!(sync.samples, 5);

        // And the offset is what converts a Rust stamp into webview time.
        let rust_sent = 1_500.0;
        assert!(((rust_sent - sync.offset_ms) - 500.0).abs() < 1e-9);
    }

    #[test]
    fn the_lowest_rtt_ping_wins_over_a_stalled_one() {
        let samples = vec![
            // A ping that got stuck behind a busy main thread: 100 ms round trip,
            // all of it on the way back, which would imply a 48 ms offset error.
            EpochSample {
                js_before_ms: 0.0,
                rust_ms: 2.0,
                js_after_ms: 100.0,
            },
            // A clean one.
            EpochSample {
                js_before_ms: 200.0,
                rust_ms: 202.0,
                js_after_ms: 204.0,
            },
        ];
        let sync = estimate_epoch_offset(&samples).unwrap();
        assert!((sync.offset_ms - 0.0).abs() < 1e-9, "clean ping's offset");
        assert!((sync.rtt_min_ms - 4.0).abs() < 1e-9);
        // Nearest-rank on two samples takes the lower one; the stalled ping is
        // still visible in `max`-shaped numbers, never in the offset.
        assert!((sync.rtt_median_ms - 4.0).abs() < 1e-9);
    }

    #[test]
    fn an_empty_or_impossible_handshake_yields_no_sync() {
        assert!(estimate_epoch_offset(&[]).is_none());
        assert!(estimate_epoch_offset(&[EpochSample {
            js_before_ms: 10.0,
            rust_ms: 5.0,
            js_after_ms: 0.0, // time ran backwards
        }])
        .is_none());
    }

    // --- report ----------------------------------------------------------

    fn samples(label: &str, planned: u32, latencies: Vec<f64>) -> ScenarioSamples {
        ScenarioSamples {
            label: label.to_owned(),
            transport: Some(BenchTransport::ChannelJson),
            payload_bytes: 64,
            target_rate_hz: 30.0,
            duration_ms: 1_000,
            planned,
            first_recv_ms: 0.0,
            last_recv_ms: 1_000.0,
            latencies_ms: latencies,
            long_tasks: 0,
            long_task_ms: 0.0,
            note: None,
        }
    }

    #[test]
    fn a_scenario_reports_drops_and_the_rate_it_actually_achieved() {
        let report = build_scenario(&samples("x", 30, vec![1.0; 21]));
        assert_eq!(report.received, 21);
        assert_eq!(report.dropped, 9);
        // 20 intervals across one second.
        assert!((report.achieved_rate_hz - 20.0).abs() < 1e-9);
        assert!(report.stats.is_some());
    }

    #[test]
    fn a_scenario_that_received_nothing_is_reported_not_dropped_silently() {
        let report = build_scenario(&samples("dead", 30, vec![]));
        assert_eq!(report.received, 0);
        assert_eq!(report.dropped, 30);
        assert_eq!(report.achieved_rate_hz, 0.0);
        assert!(report.stats.is_none());
    }

    #[test]
    fn extra_frames_never_produce_a_negative_drop_count() {
        let report = build_scenario(&samples("over", 10, vec![1.0; 12]));
        assert_eq!(report.dropped, 0);
    }

    #[test]
    fn throughput_follows_the_achieved_rate_not_the_target() {
        let mut input = samples("mb", 10, vec![1.0; 11]);
        input.payload_bytes = 1024 * 1024;
        let report = build_scenario(&input);
        // 10 intervals in 1 s => 10 MiB/s.
        assert!((report.throughput_mb_s - 10.0).abs() < 1e-9);
    }

    #[test]
    fn the_webview_version_comes_out_of_the_user_agent() {
        let ua =
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
                  Chrome/141.0.0.0 Safari/537.36 Edg/141.0.3537.85";
        assert_eq!(
            webview_version_from_user_agent(ua).as_deref(),
            Some("141.0.3537.85")
        );
        assert_eq!(webview_version_from_user_agent("Mozilla/5.0"), None);
        assert_eq!(webview_version_from_user_agent(""), None);
    }

    #[test]
    fn the_report_text_is_bracketed_by_markers_and_names_every_scenario() {
        let input = BenchRunInput {
            user_agent: "x Edg/141.0.0.1".to_owned(),
            hardware_concurrency: Some(16),
            device_pixel_ratio: Some(1.5),
            epoch_samples: vec![EpochSample {
                js_before_ms: 0.0,
                rust_ms: 1.0,
                js_after_ms: 2.0,
            }],
            rtt_ms: vec![1.0, 2.0, 3.0],
            rtt_async_ms: vec![2.0],
            scenarios: vec![samples("channel-json/64B@30Hz", 30, vec![3.0; 30])],
        };
        let report = build_report(&input, "20260818-120000".to_owned());
        assert_eq!(
            report.environment.webview_version.as_deref(),
            Some("141.0.0.1")
        );
        assert_eq!(report.scenarios.len(), 1);

        let text = report_text(&report, Some(Path::new("C:/x/y.json")));
        assert!(text.starts_with(REPORT_BEGIN), "{text}");
        assert!(text.trim_end().ends_with(REPORT_END), "{text}");
        assert!(text.contains("channel-json/64B@30Hz"), "{text}");
        assert!(text.contains("20260818-120000"), "{text}");
        assert!(text.contains("C:/x/y.json"), "{text}");
        assert!(text.contains("error bar"), "the error bar is always stated");
    }

    #[test]
    fn a_report_without_a_clock_sync_says_so_loudly() {
        let input = BenchRunInput {
            user_agent: String::new(),
            hardware_concurrency: None,
            device_pixel_ratio: None,
            epoch_samples: vec![],
            rtt_ms: vec![],
            rtt_async_ms: vec![],
            scenarios: vec![],
        };
        let report = build_report(&input, "run".to_owned());
        assert!(report.epoch.is_none());
        assert!(report_text(&report, None).contains("NOT MEASURED"));
    }

    #[test]
    fn a_run_round_trips_through_json_the_way_the_command_receives_it() {
        // The UI posts `BenchRunInput` as camelCase JSON; this is the contract.
        let json = r#"{
            "userAgent": "ua Edg/1.2.3",
            "hardwareConcurrency": 8,
            "devicePixelRatio": 2,
            "epochSamples": [{"jsBeforeMs": 1, "rustMs": 2, "jsAfterMs": 3}],
            "rttMs": [1.0, 2.0],
            "scenarios": [{
                "label": "meter-path",
                "payloadBytes": 96,
                "targetRateHz": 30,
                "durationMs": 3000,
                "planned": 0,
                "latenciesMs": [4.0, 5.0],
                "firstRecvMs": 0,
                "lastRecvMs": 1000,
                "longTasks": 1,
                "longTaskMs": 60,
                "note": "live meter path"
            }]
        }"#;
        let input: BenchRunInput = serde_json::from_str(json).unwrap();
        assert!(input.rtt_async_ms.is_empty(), "an absent field defaults");
        let report = build_report(&input, "id".to_owned());
        let scenario = &report.scenarios[0];
        assert_eq!(scenario.transport, None, "the meter path has no transport");
        assert_eq!(scenario.note.as_deref(), Some("live meter path"));
        assert_eq!(scenario.stats.unwrap().count, 2);
    }

    #[test]
    fn a_report_is_written_where_the_agent_looks_for_it() {
        let root = std::env::temp_dir().join(format!("mydaw-ipc-bench-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let report = build_report(
            &BenchRunInput {
                user_agent: "ua".to_owned(),
                hardware_concurrency: None,
                device_pixel_ratio: None,
                epoch_samples: vec![],
                rtt_ms: vec![],
                rtt_async_ms: vec![],
                scenarios: vec![],
            },
            "20260818-010203".to_owned(),
        );
        let path = write_report(&report, &root).unwrap();
        assert_eq!(path, results_dir(&root).join("20260818-010203.json"));

        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: BenchReport = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed, report, "the file is the report, losslessly");
        std::fs::remove_dir_all(&root).unwrap();
    }

    // --- gating ----------------------------------------------------------

    #[test]
    fn no_flag_contributes_no_panel() {
        assert!(bench_panels_for(false, false).is_empty());
    }

    #[test]
    fn the_dev_flag_contributes_the_manual_panel_and_auto_replaces_it() {
        let manual = bench_panels_for(true, false);
        assert_eq!(manual.len(), 1);
        assert_eq!(manual[0].0, IPC_BENCH_PANEL_ID);
        assert_eq!(manual[0].2, IPC_BENCH_PANEL);

        let auto = bench_panels_for(true, true);
        assert_eq!(auto.len(), 1, "auto and manual are never both contributed");
        assert_eq!(auto[0].0, IPC_BENCH_AUTO_PANEL_ID);
        assert_eq!(auto[0].2, IPC_BENCH_PANEL, "both use the same component");
    }

    #[test]
    fn the_monotonic_epoch_only_moves_forward() {
        let a = epoch_ms();
        let b = epoch_ms();
        assert!(b >= a);
        assert!(a >= 0.0);
    }
}
