//! Audio devices for WAVE.
//!
//! One thread owns the cpal streams (they are not `Send` on every platform);
//! the app talks to it through [`AudioIo`]. The output callback owns the
//! engine's [`wave_engine::Rt`] outright — no locks on the audio thread. Input
//! (for recording and monitoring) arrives from a second stream through a
//! lock-free ring. The input opens and closes on its own (the output keeps playing), and
//! the round trip of every take is measured from the stream timestamps while it records;
//! `recOffsetMs` and [`AudioIo::measure_latency`] cover what the driver does not report.
//!
//! **ASIO**: compiled with the `asio` feature on Windows. Input and output are
//! opened on the *same* driver, which is the only way ASIO works. If ASIO was
//! asked for and cannot be opened we fall back to the system host and say why
//! in [`AudioStatus::note`], so there is always sound.

mod jitter;
mod probe;

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

pub use probe::LatencyReport;
use jitter::{IoStats, JitterBuf};
use probe::Probe;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use serde::{Deserialize, Serialize};
use wave_engine::Engine;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioConfig {
    /// Host id: "wasapi", "asio", "coreaudio", "alsa", "jack" … `""` = system default.
    #[serde(default)]
    pub host: String,
    pub output: Option<String>,
    pub input: Option<String>,
    pub sample_rate: Option<u32>,
    pub buffer_size: Option<u32>,
    /// Input channel recorded and monitored (0-based), as mono in both sides.
    /// `None` = the first one (where an interface's mic input 1 is).
    #[serde(default)]
    pub input_channel: Option<u16>,
    /// Recording calibration (ms, signed), added to the measured round trip: positive moves
    /// takes earlier. `None` when sent = keep the saved value (the app fills it in).
    #[serde(default)]
    pub rec_offset_ms: Option<f32>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioStatus {
    #[serde(flatten)]
    pub config: AudioConfig,
    pub running: bool,
    pub actual_host: String,
    pub actual_output: String,
    pub actual_input: Option<String>,
    pub actual_rate: u32,
    pub actual_buffer: u32,
    pub latency_ms: f32,
    pub input_channels: u16,
    pub error: Option<String>,
    pub note: Option<String>,
    pub asio_compiled: bool,
    /// Times the input stopped delivering mid-stream since it opened (each one a short
    /// faded gap in a recording); the UI shows them as "przerwy w wejściu".
    pub input_underruns: u32,
    /// Times input piled up beyond the jitter buffer (the output stalled) and was skipped.
    pub input_overruns: u32,
    /// Clock difference of the input against the output in ppm, as followed by the jitter
    /// buffer (0 for one device; up to a few hundred for separate ones).
    pub input_drift_ppm: f32,
}

#[derive(Clone, Debug, Serialize)]
pub struct HostInfo {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct DeviceList {
    pub outputs: Vec<String>,
    pub inputs: Vec<String>,
}

pub fn asio_compiled() -> bool {
    cfg!(all(windows, feature = "asio"))
}

fn dev_name(d: &cpal::Device) -> Option<String> {
    d.description().ok().map(|x| x.name().to_string())
}

fn host_key(id: cpal::HostId) -> String {
    id.name().to_ascii_lowercase()
}

pub fn hosts() -> Vec<HostInfo> {
    cpal::available_hosts().into_iter().map(|h| HostInfo { id: host_key(h), name: h.name().to_string() }).collect()
}

fn pick_host(want: &str) -> (cpal::Host, Option<String>) {
    if !want.is_empty() {
        for id in cpal::available_hosts() {
            if host_key(id) == want {
                match cpal::host_from_id(id) {
                    Ok(h) => return (h, None),
                    Err(e) => return (cpal::default_host(), Some(format!("{want} is unavailable ({e}); using {} instead.", cpal::default_host().id().name()))),
                }
            }
        }
        let note = if want == "asio" && !asio_compiled() {
            "This WAVE build has no ASIO host. Using the system audio instead — install a release build with ASIO.".to_string()
        } else {
            format!("Audio host \"{want}\" is not available here; using {}.", cpal::default_host().id().name())
        };
        return (cpal::default_host(), Some(note));
    }
    (cpal::default_host(), None)
}

pub fn devices(host: &str) -> DeviceList {
    let (h, _) = pick_host(host);
    let name = |d: cpal::Device| dev_name(&d);
    DeviceList {
        outputs: h.output_devices().map(|it| it.filter_map(name).collect()).unwrap_or_default(),
        inputs: h.input_devices().map(|it| it.filter_map(name).collect()).unwrap_or_default(),
    }
}

enum Req {
    Start(AudioConfig, mpsc::Sender<AudioStatus>),
    Input(bool, mpsc::Sender<AudioStatus>),
    Stop(mpsc::Sender<()>),
    Status(mpsc::Sender<AudioStatus>),
    Probe(Box<Probe>, mpsc::Sender<Result<(), String>>),
    ProbeResult(u64, mpsc::Sender<Option<Box<Probe>>>),
}

/// Handle to the audio thread. Cheap to clone.
#[derive(Clone)]
pub struct AudioIo {
    tx: mpsc::Sender<Req>,
}

/// The same devices and stream settings (everything but the calibration offset).
fn same_streams(a: &AudioConfig, b: &AudioConfig) -> bool {
    AudioConfig { rec_offset_ms: None, ..a.clone() } == AudioConfig { rec_offset_ms: None, ..b.clone() }
}

impl AudioIo {
    pub fn spawn(engine: Arc<Engine>) -> Self {
        let (tx, rx) = mpsc::channel::<Req>();
        thread::Builder::new()
            .name("wave-audio-io".into())
            .spawn(move || {
                let mut cur: Option<Running> = None;
                let mut status = AudioStatus { asio_compiled: asio_compiled(), ..Default::default() };
                let mut cfg_now = AudioConfig::default();
                // The microphone is opened only when it is needed (recording / monitoring):
                // on Bluetooth headphones an open mic forces the low-quality "headset" profile.
                // It opens and closes on its own, without restarting the output.
                let mut want_input = false;
                while let Ok(req) = rx.recv() {
                    if let Some(r) = cur.as_mut() {
                        r.collect_garbage();
                    }
                    let (cfg, reply) = match req {
                        Req::Start(cfg, reply) => {
                            // only the calibration offset changed: no reason to restart the streams
                            if let Some(r) = cur.as_ref().filter(|_| status.running && same_streams(&cfg, &cfg_now)) {
                                r.offset_ms.store(cfg.rec_offset_ms.unwrap_or(0.0).to_bits(), Relaxed);
                                cfg_now = cfg.clone();
                                status.config = cfg;
                                let _ = reply.send(status.clone());
                                continue;
                            }
                            (cfg, reply)
                        }
                        Req::Input(on, reply) => {
                            want_input = on;
                            if let Some(r) = cur.as_mut() {
                                r.set_input(on, &cfg_now, &mut status);
                                r.read_stats(&mut status);
                                let _ = reply.send(status.clone());
                                continue;
                            }
                            (cfg_now.clone(), reply)
                        }
                        Req::Stop(reply) => {
                            drop(cur.take());
                            status.running = false;
                            let _ = reply.send(());
                            continue;
                        }
                        Req::Status(reply) => {
                            if let Some(r) = cur.as_ref() {
                                r.read_stats(&mut status);
                                if let Some(e) = r.error.lock().ok().and_then(|g| g.clone()) {
                                    status.error = Some(e);
                                    status.running = false;
                                }
                            }
                            let _ = reply.send(status.clone());
                            continue;
                        }
                        Req::Probe(job, reply) => {
                            let res = match cur.as_mut() {
                                None => Err("Audio is not running.".to_string()),
                                Some(r) if !r.input_open => Err("No recording input is open: choose an input in Settings.".to_string()),
                                Some(r) => r.probe_tx.push(job).map_err(|_| "A measurement is already running.".to_string()),
                            };
                            let _ = reply.send(res);
                            continue;
                        }
                        Req::ProbeResult(id, reply) => {
                            let mut got = None;
                            if let Some(r) = cur.as_mut() {
                                // results of an earlier, abandoned measurement are dropped here
                                while let Ok(p) = r.probe_back.pop() {
                                    if p.id == id {
                                        got = Some(p);
                                        break;
                                    }
                                }
                            }
                            let _ = reply.send(got);
                            continue;
                        }
                    };
                    cfg_now = cfg.clone();
                    drop(cur.take()); // stop the old streams before opening new ones (ASIO is exclusive)
                    match open(&engine, &cfg, want_input) {
                        Ok((run, st)) => {
                            cur = Some(run);
                            status = st;
                        }
                        Err(e) => {
                            status = AudioStatus { config: cfg, error: Some(e), asio_compiled: asio_compiled(), ..Default::default() };
                        }
                    }
                    let _ = reply.send(status.clone());
                }
            })
            .expect("spawn audio thread");
        Self { tx }
    }

    pub fn start(&self, cfg: AudioConfig) -> AudioStatus {
        let (tx, rx) = mpsc::channel();
        let _ = self.tx.send(Req::Start(cfg, tx));
        rx.recv().unwrap_or_default()
    }

    /// Opens or closes the input (microphone) without changing the device choice. The
    /// output keeps running: a punch-in while playing does not interrupt the song.
    pub fn set_input(&self, on: bool) -> AudioStatus {
        let (tx, rx) = mpsc::channel();
        let _ = self.tx.send(Req::Input(on, tx));
        rx.recv().unwrap_or_default()
    }

    pub fn stop(&self) {
        let (tx, rx) = mpsc::channel();
        let _ = self.tx.send(Req::Stop(tx));
        let _ = rx.recv();
    }

    pub fn status(&self) -> AudioStatus {
        let (tx, rx) = mpsc::channel();
        let _ = self.tx.send(Req::Status(tx));
        rx.recv().unwrap_or_default()
    }

    /// Measures the real round trip: plays a few clicks on the output (instead of the mix)
    /// while recording the input, then finds them by cross-correlation. The input must be
    /// open ([`AudioIo::set_input`]) and the transport stopped. Blocks for about 6 s; the
    /// analysis runs on the calling thread, never on the audio thread.
    pub fn measure_latency(&self) -> Result<LatencyReport, String> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let st = self.status();
        if !st.running {
            return Err(st.error.unwrap_or_else(|| "Audio is not running.".into()));
        }
        let rate = st.actual_rate.max(1);
        let plan = probe::click_plan(rate);
        let id = NEXT.fetch_add(1, Relaxed);
        let (tx, rx) = mpsc::channel();
        let _ = self.tx.send(Req::Probe(Box::new(Probe::new(id, plan.signal.clone())), tx));
        rx.recv().map_err(|_| "The audio thread is gone.".to_string())??;
        let deadline = Instant::now() + Duration::from_secs_f64(plan.signal.len() as f64 / rate as f64 + 3.0);
        loop {
            thread::sleep(Duration::from_millis(50));
            let (tx, rx) = mpsc::channel();
            let _ = self.tx.send(Req::ProbeResult(id, tx));
            if let Ok(Some(p)) = rx.recv() {
                return probe::report(&plan, &p, rate);
            }
            if Instant::now() > deadline {
                return Err("The measurement did not finish (the audio device stopped or restarted).".into());
            }
        }
    }
}

/// Field order is drop order: the output stops before the input it reads from.
struct Running {
    _out: cpal::Stream,
    inp: Option<cpal::Stream>,
    /// An input is feeding the output (an on-demand stream, or ASIO's own inputs).
    input_open: bool,
    asio: bool,
    host: cpal::Host,
    rate: u32,
    buffer: cpal::BufferSize,
    error: Arc<std::sync::Mutex<Option<String>>>,
    /// Hands a new input (or none) to the output callback.
    swap: rtrb::Producer<Option<InputSide>>,
    /// Inputs the output callback let go of, freed here and not on the audio thread.
    trash: rtrb::Consumer<InputSide>,
    probe_tx: rtrb::Producer<Box<Probe>>,
    probe_back: rtrb::Consumer<Box<Probe>>,
    /// `recOffsetMs` as f32 bits, read by the output callback.
    offset_ms: Arc<AtomicU32>,
    stats: Arc<IoStats>,
}

impl Running {
    fn read_stats(&self, st: &mut AudioStatus) {
        st.input_underruns = self.stats.underruns.load(Relaxed);
        st.input_overruns = self.stats.overruns.load(Relaxed);
        st.input_drift_ppm = f32::from_bits(self.stats.drift_ppm.load(Relaxed));
    }

    fn collect_garbage(&mut self) {
        while self.trash.pop().is_ok() {}
    }

    fn set_input(&mut self, on: bool, cfg: &AudioConfig, st: &mut AudioStatus) {
        if self.asio || on == self.inp.is_some() {
            return; // ASIO inputs live on the output's driver and are always open
        }
        if !on {
            let _ = self.swap.push(None);
            self.inp = None;
            self.input_open = false;
            st.actual_input = None;
            st.input_channels = 0;
            return;
        }
        match open_input(&self.host, cfg.input.as_deref(), cfg, self.rate, self.buffer, self.error.clone()) {
            Ok(Some(o)) => {
                if self.swap.push(Some(o.side)).is_err() {
                    st.note = Some("Recording input could not start (the output is not running).".into());
                    return;
                }
                if let Err(e) = o.stream.play() {
                    st.note = Some(format!("Recording input could not start ({e})."));
                }
                self.stats.reset();
                self.inp = Some(o.stream);
                self.input_open = true;
                st.actual_input = o.name;
                st.input_channels = o.channels;
            }
            Ok(None) => {}
            Err(e) => st.note = Some(format!("Recording input could not open ({e}). Playback works.")),
        }
    }
}

const MAX_CALLBACK: usize = 8192;

fn find(devs: Result<impl Iterator<Item = cpal::Device>, cpal::Error>, name: &str) -> Option<cpal::Device> {
    devs.ok()?.find(|d| dev_name(d).map(|n| n == name).unwrap_or(false))
}

struct OpenedInput {
    stream: cpal::Stream,
    side: InputSide,
    name: Option<String>,
    channels: u16,
}

/// Builds (does not start) the input stream. `name`: `None` = the system default,
/// `Some("")` = no input.
fn open_input(host: &cpal::Host, name: Option<&str>, cfg: &AudioConfig, rate: u32, buffer: cpal::BufferSize, err: Arc<std::sync::Mutex<Option<String>>>) -> Result<Option<OpenedInput>, String> {
    let dev = match name {
        Some("") => return Ok(None),
        Some(n) => find(host.input_devices(), n),
        None => host.default_input_device(),
    };
    let Some(d) = dev else { return Ok(None) };
    let ic = d.default_input_config().map_err(|e| e.to_string())?;
    let ich = ic.channels().max(1);
    let isc = cpal::StreamConfig { channels: ich, sample_rate: rate, buffer_size: buffer };
    let pick = cfg.input_channel.unwrap_or(0).min(ich - 1) as usize;
    let (tx, rx) = rtrb::RingBuffer::<f32>::new(rate as usize * 2);
    let clock = Arc::new(InClock::default());
    let stream = build_input(&d, &isc, ic.sample_format(), tx, err, pick, clock.clone())?;
    // ASIO delivers each input buffer just before the output callback that uses it; other
    // hosts run the two streams on their own schedules and need two periods in reserve
    let periods = if host.id().name().eq_ignore_ascii_case("asio") { 1 } else { 2 };
    Ok(Some(OpenedInput { stream, side: InputSide { ring: rx, clock, jb: JitterBuf::new(rate, periods) }, name: dev_name(&d), channels: ich }))
}

fn open(engine: &Arc<Engine>, cfg: &AudioConfig, want_input: bool) -> Result<(Running, AudioStatus), String> {
    let (host, mut note) = pick_host(&cfg.host);
    let out_dev = match cfg.output.as_deref().filter(|s| !s.is_empty()) {
        Some(n) => match find(host.output_devices(), n) {
            Some(d) => d,
            None => {
                note = Some(format!("Output \"{n}\" is gone; using the default device."));
                host.default_output_device().ok_or("no output device")?
            }
        },
        None => host.default_output_device().ok_or("no output device")?,
    };
    let out_name = dev_name(&out_dev).unwrap_or_default();
    let def = out_dev.default_output_config().map_err(|e| format!("{out_name}: {e}"))?;
    let rate = cfg.sample_rate.unwrap_or(def.sample_rate());
    let channels = def.channels().max(1);
    let fmt = def.sample_format();
    let buffer = match (cfg.buffer_size, def.buffer_size()) {
        (Some(b), cpal::SupportedBufferSize::Range { min, max }) => cpal::BufferSize::Fixed(b.clamp(*min, *max)),
        (Some(b), _) => cpal::BufferSize::Fixed(b),
        _ => cpal::BufferSize::Default,
    };
    let sc = cpal::StreamConfig { channels, sample_rate: rate, buffer_size: buffer };
    let nominal = match buffer {
        cpal::BufferSize::Fixed(b) => b,
        _ => 512,
    };
    let host_name = host.id().name().to_string();
    let is_asio = host_name.eq_ignore_ascii_case("asio");
    // input and output timestamps share the system clock here (QPC, mach time), so the
    // round trip is measured from them; elsewhere per-stream clocks are not comparable
    let shared_clock = ["wasapi", "coreaudio"].iter().any(|h| host_name.eq_ignore_ascii_case(h));

    let error = Arc::new(std::sync::Mutex::new(None::<String>));
    let (mut swap_tx, swap_rx) = rtrb::RingBuffer::<Option<InputSide>>::new(16);
    let (trash_tx, trash_rx) = rtrb::RingBuffer::<InputSide>::new(16);
    let (probe_tx, probe_rx) = rtrb::RingBuffer::<Box<Probe>>::new(1);
    let (back_tx, back_rx) = rtrb::RingBuffer::<Box<Probe>>::new(4);
    let offset_ms = Arc::new(AtomicU32::new(cfg.rec_offset_ms.unwrap_or(0.0).to_bits()));
    let stats = Arc::new(IoStats::default());

    // ---- input (same host). ASIO inputs live on the same driver and cost nothing; elsewhere
    // the mic is opened on demand (and can be opened later without touching the output)
    let in_want = if is_asio {
        Some(Some(out_name.as_str()))
    } else if want_input {
        Some(cfg.input.as_deref())
    } else {
        None
    };
    let mut in_name = None;
    let mut in_ch = 0u16;
    let mut inp = None;
    if let Some(want) = in_want {
        match open_input(&host, want, cfg, rate, buffer, error.clone()) {
            Ok(Some(o)) => {
                in_name = o.name;
                in_ch = o.channels;
                let _ = swap_tx.push(Some(o.side));
                inp = Some(o.stream);
            }
            Ok(None) => {}
            Err(e) => note = Some(format!("Recording input could not open ({e}). Playback works.")),
        }
    }

    let mut rt = engine.create_rt(rate as f32, (nominal as usize).clamp(64, 2048));
    // after Play the music is heard at once, not after the plugin delay compensation
    rt.set_instant_start(Some(0.7));
    let wires = OutWires { shared: engine.shared(), swap: swap_rx, trash: trash_tx, probe: probe_rx, back: back_tx, offset_ms: offset_ms.clone(), stats: stats.clone(), shared_clock, buffer: nominal };
    let out = build_output(&out_dev, &sc, fmt, rt, error.clone(), wires).map_err(|e| format!("{out_name}: {e}"))?;
    out.play().map_err(|e| format!("{out_name}: {e}"))?;
    if let Some(s) = &inp {
        if let Err(e) = s.play() {
            note = Some(format!("Recording input could not start ({e})."));
        }
    }
    let input_open = inp.is_some();
    let status = AudioStatus {
        config: cfg.clone(),
        running: true,
        actual_host: host_name,
        actual_output: out_name,
        actual_input: in_name,
        actual_rate: rate,
        actual_buffer: nominal,
        latency_ms: nominal as f32 / rate as f32 * 1000.0,
        input_channels: in_ch,
        error: None,
        note,
        asio_compiled: asio_compiled(),
        ..Default::default()
    };
    let run = Running { _out: out, inp, input_open, asio: is_asio, host, rate, buffer, error, swap: swap_tx, trash: trash_rx, probe_tx, probe_back: back_rx, offset_ms, stats };
    Ok((run, status))
}

// ------------------------------------------------------------------ round trip

/// What an input callback publishes for the output callback (lock-free).
#[derive(Debug)]
struct InClock {
    /// Capture instant (ns on the stream clock, f64 bits) that input frame #0 of this stream
    /// would have had: the newest packet's capture instant minus the frames pushed before it.
    /// Frame `i` of the ring was captured at `origin + i / rate`. NaN until the first packet.
    origin: AtomicU64,
    /// Capture → callback delay of the latest packet in frames (hosts without a shared clock).
    lat: AtomicU32,
    /// Frames in the latest packet (the ring must hold one whole packet plus a callback).
    packet: AtomicU32,
}

impl Default for InClock {
    fn default() -> Self {
        Self { origin: AtomicU64::new(f64::NAN.to_bits()), lat: AtomicU32::new(0), packet: AtomicU32::new(0) }
    }
}

impl InClock {
    /// Called by the input callback after pushing a packet whose first frame was captured
    /// at `capture_ns`, with `pushed_before` frames pushed by earlier packets.
    fn publish(&self, capture_ns: f64, pushed_before: u64, frames: u64, rate: u32) {
        self.packet.store(frames.min(u32::MAX as u64) as u32, Relaxed);
        self.origin.store((capture_ns - pushed_before as f64 * 1e9 / rate as f64).to_bits(), Relaxed);
    }
}

/// An input stream as the output callback sees it.
struct InputSide {
    ring: rtrb::Consumer<f32>,
    clock: Arc<InClock>,
    jb: JitterBuf,
}

/// Round trip in frames for one output callback by the fallback rule: what the callback
/// renders now is heard `out` frames later, and the oldest input it consumes was captured
/// `backlog + input` frames ago. Hosts that report no timestamps get one buffer each way.
fn round_trip(out: u32, backlog: u32, input: u32, buffer: u32) -> u32 {
    let out = if out == 0 { buffer } else { out };
    let input = if input == 0 { buffer } else { input };
    out + backlog + input
}

fn frames_of(d: std::time::Duration, rate: u32) -> u32 {
    (d.as_secs_f64() * rate as f64).round().min(u32::MAX as f64) as u32
}

/// One output callback's measurement.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Measure {
    /// Smoothed round trip for display and as a take's provisional alignment.
    shown: u32,
    /// The round trip of the input consumed in this callback, when real input is flowing.
    take: Option<u32>,
}

/// Turns per-callback timing into round trips (frames, no calibration offset).
struct Estimator {
    rate: u32,
    smooth: f64,
    /// Real input has been measured since the input (re)started.
    live: bool,
}

impl Estimator {
    fn new(rate: u32) -> Self {
        Self { rate, smooth: -1.0, live: false }
    }

    /// A new input stream: what was measured before says nothing about it.
    fn restart(&mut self) {
        self.smooth = -1.0;
        self.live = false;
    }

    /// `out_lat`: callback → playback (0 = unknown); `now_ns`: the output callback instant on
    /// the clock the input shares (NaN = not comparable); `pos`: the input frame (fractional)
    /// the jitter buffer plays at this callback's first frame (`None` while it is not
    /// playing real input); `backlog`: frames buffered ahead of it.
    fn step(&mut self, out_lat: u32, frames: u32, now_ns: f64, clock: Option<&InClock>, pos: Option<f64>, backlog: u32) -> Measure {
        let out = if out_lat == 0 { frames } else { out_lat };
        let origin = clock.map_or(f64::NAN, |c| f64::from_bits(c.origin.load(Relaxed)));
        let in_lat = clock.map_or(0, |c| c.lat.load(Relaxed));
        let fallback = round_trip(out, backlog, in_lat, frames);
        let take = match pos {
            None => None, // nothing captured yet, or the jitter buffer is refilling
            Some(_) if origin.is_nan() => None,
            Some(x) => {
            // age at this callback of the input frame played first in it
            let age = (now_ns - origin) * self.rate as f64 / 1e9 - x;
            let ok = age.is_finite() && age > -(self.rate as f64) * 0.002 && age < self.rate as f64;
            Some(if ok { out + age.max(0.0).round() as u32 } else { fallback })
            }
        };
        // the first real measurement replaces whatever was guessed before the input ran
        if take.is_some() && !self.live {
            self.live = true;
            self.smooth = -1.0;
        }
        if take.is_some() || !self.live {
            let raw = take.unwrap_or(fallback) as f64;
            self.smooth = if self.smooth < 0.0 { raw } else { self.smooth * 0.95 + raw * 0.05 };
        }
        Measure { shown: self.smooth.max(0.0).round() as u32, take }
    }
}

/// Round trip plus the calibration offset, never negative.
fn with_offset(frames: u32, offset_ms: f32, rate: u32) -> u32 {
    (frames as f64 + offset_ms as f64 * rate as f64 / 1000.0).round().max(0.0) as u32
}

/// Everything the output callback talks to besides the engine.
struct OutWires {
    shared: Arc<wave_engine::rt::Shared>,
    swap: rtrb::Consumer<Option<InputSide>>,
    trash: rtrb::Producer<InputSide>,
    probe: rtrb::Consumer<Box<Probe>>,
    back: rtrb::Producer<Box<Probe>>,
    offset_ms: Arc<AtomicU32>,
    stats: Arc<IoStats>,
    shared_clock: bool,
    buffer: u32,
}

fn build_output(dev: &cpal::Device, sc: &cpal::StreamConfig, fmt: cpal::SampleFormat, mut rt: wave_engine::Rt, err: Arc<std::sync::Mutex<Option<String>>>, mut w: OutWires) -> Result<cpal::Stream, String> {
    let ch = sc.channels as usize;
    let rate = sc.sample_rate;
    let mut est = Estimator::new(rate);
    let mut input: Option<InputSide> = None;
    let mut probe: Option<Box<Probe>> = None;
    let mut l = vec![0f32; MAX_CALLBACK];
    let mut r = vec![0f32; MAX_CALLBACK];
    let mut il = vec![0f32; MAX_CALLBACK];
    let mut ir = vec![0f32; MAX_CALLBACK];
    let shared_clock = w.shared_clock;
    let mut render = move |frames: usize, out_lat: u32, now_ns: f64, write: &mut dyn FnMut(usize, f32, f32)| {
        while let Ok(next) = w.swap.pop() {
            if let Some(old) = std::mem::replace(&mut input, next) {
                // freed by the control thread (if its trash were ever full, it drops here)
                let _ = w.trash.push(old);
            }
            est.restart();
        }
        if probe.is_none() {
            probe = w.probe.pop().ok();
        }
        let (pos, backlog) = input.as_ref().map_or((None, 0), |i| (i.jb.pos(), i.jb.fill(i.ring.slots() / 2).max(0.0) as u32));
        let now = if shared_clock { now_ns } else { f64::NAN };
        let m = est.step(out_lat, frames as u32, now, input.as_ref().map(|i| &*i.clock), pos, backlog);
        let off = f32::from_bits(w.offset_ms.load(Relaxed));
        w.shared.set_io_latency(with_offset(m.shown, off, rate));
        if let Some(raw) = m.take {
            w.shared.add_take_latency(with_offset(raw, off, rate));
            if let Some(p) = probe.as_mut() {
                p.reported_sum += raw as f64;
                p.reported_n += 1;
            }
        }
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(MAX_CALLBACK);
            // input: as many frames as are ready, zero-padded
            // input through the jitter buffer: gap-free, never zero-padded mid-stream
            match input.as_mut() {
                Some(inp) => {
                    let packet = inp.clock.packet.load(Relaxed) as usize;
                    inp.jb.pull(&mut inp.ring, &mut il[..n], w.buffer as usize, packet, &w.stats);
                    ir[..n].copy_from_slice(&il[..n]);
                }
                None => {
                    il[..n].fill(0.0);
                    ir[..n].fill(0.0);
                }
            }
            rt.process(&mut l[..n], &mut r[..n], Some((&il[..n], &ir[..n])));
            // a latency measurement plays its clicks instead of the mix and keeps the input
            if let Some(p) = probe.as_mut() {
                for i in 0..n {
                    let s = p.signal.get(p.pos).copied().unwrap_or(0.0);
                    if let Some(c) = p.cap.get_mut(p.pos) {
                        *c = il[i];
                    }
                    p.pos += 1;
                    l[i] = s;
                    r[i] = s;
                }
            }
            for i in 0..n {
                write(done + i, l[i], r[i]);
            }
            done += n;
        }
        if probe.as_ref().is_some_and(|p| p.done()) {
            if let Some(p) = probe.take() {
                let _ = w.back.push(p);
            }
        }
    };
    let on_err = move |e: cpal::Error| {
        if let Ok(mut g) = err.lock() {
            *g = Some(e.to_string());
        }
    };
    macro_rules! stream {
        ($t:ty, $conv:expr) => {
            dev.build_output_stream(
                *sc,
                move |data: &mut [$t], info: &cpal::OutputCallbackInfo| {
                    let frames = data.len() / ch;
                    let ts = info.timestamp();
                    let out_lat = frames_of(ts.playback.duration_since(ts.callback), rate);
                    render(frames, out_lat, ts.callback.as_nanos() as f64, &mut |i, a, b| {
                        let base = i * ch;
                        data[base] = $conv(a);
                        if ch > 1 {
                            data[base + 1] = $conv(b);
                            for k in 2..ch {
                                data[base + k] = $conv(0.0);
                            }
                        }
                    });
                },
                on_err,
                None,
            )
        };
    }
    let s = match fmt {
        cpal::SampleFormat::F32 => stream!(f32, |x: f32| x),
        cpal::SampleFormat::I16 => stream!(i16, |x: f32| (x.clamp(-1.0, 1.0) * 32767.0) as i16),
        cpal::SampleFormat::I32 => stream!(i32, |x: f32| (x.clamp(-1.0, 1.0) as f64 * 2147483647.0) as i32),
        cpal::SampleFormat::U16 => stream!(u16, |x: f32| ((x.clamp(-1.0, 1.0) * 32767.0) as i32 + 32768) as u16),
        f => return Err(format!("unsupported sample format {f:?}")),
    };
    s.map_err(|e| e.to_string())
}

fn build_input(dev: &cpal::Device, sc: &cpal::StreamConfig, fmt: cpal::SampleFormat, mut tx: rtrb::Producer<f32>, err: Arc<std::sync::Mutex<Option<String>>>, pick: usize, clock: Arc<InClock>) -> Result<cpal::Stream, String> {
    let ch = sc.channels as usize;
    let pick = pick.min(ch - 1);
    let rate = sc.sample_rate;
    let mut pushed = 0u64;
    let on_err = move |e: cpal::Error| {
        if let Ok(mut g) = err.lock() {
            *g = Some(format!("input: {e}"));
        }
    };
    macro_rules! stream {
        ($t:ty, $conv:expr) => {
            dev.build_input_stream(
                *sc,
                move |data: &[$t], info: &cpal::InputCallbackInfo| {
                    let ts = info.timestamp();
                    clock.lat.store(frames_of(ts.callback.duration_since(ts.capture), rate), Relaxed);
                    // one channel (a mic on input 1 or 2), mono in both sides: a vocal recorded
                    // from a stereo interface must not end up in the left ear only
                    let mut k = 0u64;
                    for f in data.chunks_exact(ch) {
                        if tx.slots() < 2 {
                            break;
                        }
                        let a = $conv(f[pick]);
                        let _ = tx.push(a);
                        let _ = tx.push(a);
                        k += 1;
                    }
                    if k > 0 {
                        clock.publish(ts.capture.as_nanos() as f64, pushed, k, rate);
                        pushed += k;
                    }
                },
                on_err,
                None,
            )
        };
    }
    let s = match fmt {
        cpal::SampleFormat::F32 => stream!(f32, |x: f32| x),
        cpal::SampleFormat::I16 => stream!(i16, |x: i16| x as f32 / 32768.0),
        cpal::SampleFormat::I32 => stream!(i32, |x: i32| x as f32 / 2147483648.0),
        cpal::SampleFormat::U16 => stream!(u16, |x: u16| (x as f32 - 32768.0) / 32768.0),
        f => return Err(format!("unsupported sample format {f:?}")),
    };
    s.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_adds_both_directions_and_the_backlog() {
        // 10 ms out + 256 frames waiting + 5 ms in at 48 kHz
        assert_eq!(round_trip(480, 256, 240, 512), 976);
        // a host without timestamps: one buffer each way
        assert_eq!(round_trip(0, 0, 0, 512), 1024);
        assert_eq!(frames_of(std::time::Duration::from_millis(10), 48000), 480);
    }

    /// A duplex device in continuous time: the output calls back every `out_period` frames
    /// and is heard `out_lat` frames after each callback; the input (its own clock, `ppm`
    /// off) captures packets of `in_packet` frames and delivers each `in_delay_ms` (+ up to
    /// `jitter_ms` at random, in order) after its last frame, from `in_start_s` on (negative
    /// = already running). The input plays `signal(frame)`, or, with `signal` = None, a click
    /// exactly when output frame `click` is heard.
    struct Sim {
        out_period: usize,
        out_lat: u32,
        in_packet: usize,
        in_delay_ms: f64,
        jitter_ms: f64,
        in_start_s: f64,
        ppm: f64,
        click: usize,
        secs: f64,
        signal: Option<fn(f64) -> f32>,
    }

    impl Default for Sim {
        fn default() -> Self {
            Self { out_period: 512, out_lat: 1100, in_packet: 480, in_delay_ms: 1.0, jitter_ms: 0.0, in_start_s: -2.0, ppm: 0.0, click: 48000, secs: 3.0, signal: None }
        }
    }

    struct Run {
        /// Round trips the estimator reported for real input.
        takes: Vec<u32>,
        /// The one reported in the callback that consumed the click, and the true one.
        at_click: Option<u32>,
        truth: Option<u32>,
        /// Everything the output consumed from the input (what a take records).
        input: Vec<f32>,
        stats: IoStats,
    }

    const RATE: u32 = 48000;
    const MS1: f64 = RATE as f64 / 1000.0;

    fn simulate(s: &Sim) -> Run {
        let base = 1.0e12; // a QPC-like instant, far from zero
        let rate = RATE as f64;
        let in_rate = rate * (1.0 + s.ppm * 1e-6);
        let t_out = |j: usize| base + (j * s.out_period) as f64 * 1e9 / rate;
        let in_start = base + s.in_start_s * 1e9;
        // when the click (output frame `click`) is heard, and which input frame captures it
        let jc = s.click / s.out_period;
        let heard = t_out(jc) + (s.out_lat as f64 + (s.click - jc * s.out_period) as f64) * 1e9 / rate;
        let q_click = ((heard - in_start) * in_rate / 1e9).round() as i64;
        let (mut tx, rx) = rtrb::RingBuffer::<f32>::new(RATE as usize * 2);
        let mut side = InputSide { ring: rx, clock: Arc::new(InClock::default()), jb: JitterBuf::new(RATE, 2) };
        let mut est = Estimator::new(RATE);
        let stats = IoStats::default();
        let mut pushed = 0u64;
        // a running input: the stream holds only what arrives after the output starts
        let mut packet = if s.in_start_s < 0.0 { ((base - in_start) * in_rate / 1e9) as usize / s.in_packet } else { 0 };
        let mut seed = 0x1234_5678u32;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as f64 / u32::MAX as f64
        };
        let mut last_delivery = 0.0f64;
        let mut next_delivery = f64::NAN;
        let mut il = vec![0f32; s.out_period];
        let mut run = Run { takes: vec![], at_click: None, truth: None, input: vec![], stats: IoStats::default() };
        for j in 0..(s.secs * rate) as usize / s.out_period {
            let now = t_out(j);
            // deliver every packet whose input callback came before this output callback
            loop {
                let cap = in_start + (packet * s.in_packet) as f64 * 1e9 / in_rate;
                if next_delivery.is_nan() {
                    let d = cap + s.in_packet as f64 * 1e9 / in_rate + s.in_delay_ms * 1e6 + s.jitter_ms * 1e6 * rnd();
                    next_delivery = d.max(last_delivery);
                }
                if next_delivery > now {
                    break;
                }
                last_delivery = next_delivery;
                next_delivery = f64::NAN;
                let before = pushed;
                for i in 0..s.in_packet {
                    let q = (packet * s.in_packet + i) as i64;
                    let v = match s.signal {
                        Some(f) => f(q as f64 / in_rate),
                        None if q == q_click => 1.0,
                        None => 0.0,
                    };
                    let _ = tx.push(v);
                    let _ = tx.push(v);
                    pushed += 1;
                }
                side.clock.publish(cap, before, s.in_packet as u64, RATE);
                packet += 1;
            }
            let (pos, backlog) = (side.jb.pos(), side.jb.fill(side.ring.slots() / 2) as u32);
            let m = est.step(s.out_lat, s.out_period as u32, now, Some(&side.clock), pos, backlog);
            if let Some(t) = m.take {
                run.takes.push(t);
            }
            side.jb.pull(&mut side.ring, &mut il, s.out_period, s.in_packet, &stats);
            if s.signal.is_none() {
                if let Some((i, v)) = il.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())) {
                    if v.abs() > 0.5 {
                        run.truth = Some((j * s.out_period + i - s.click) as u32);
                        run.at_click = m.take;
                    }
                }
            }
            run.input.extend_from_slice(&il);
        }
        run.stats = stats;
        run
    }

    fn mean(v: &[u32]) -> f64 {
        v.iter().map(|x| *x as f64).sum::<f64>() / v.len() as f64
    }

    fn assert_aligned(r: &Run, what: &str) {
        let (at, truth) = (r.at_click.expect("a round trip was measured"), r.truth.expect("the click came back"));
        assert!((at as f64 - truth as f64).abs() <= MS1, "{what}: measured {at}, true {truth}");
    }

    #[test]
    fn round_trip_of_a_running_input_matches_where_the_click_comes_back() {
        let r = simulate(&Sim::default());
        assert_aligned(&r, "running");
        let truth = r.truth.unwrap() as f64;
        assert!((mean(&r.takes) - truth).abs() <= MS1, "take mean {} vs {truth}", mean(&r.takes));
    }

    #[test]
    fn an_input_opened_at_the_take_start_measures_right_from_its_first_frame() {
        // the output runs; the input opens 0.4 s later (punch-in / arming with monitoring
        // off). Only real input is measured, so nothing from before it ran skews the take.
        let r = simulate(&Sim { in_start_s: 0.4, click: 40000, ..Sim::default() });
        assert_aligned(&r, "opened late");
        let truth = r.truth.unwrap() as f64;
        let worst = r.takes.iter().map(|t| (*t as f64 - truth).abs()).fold(0.0, f64::max);
        assert!(worst <= MS1, "a sample is {worst} frames off");
    }

    #[test]
    fn different_input_and_output_devices_are_measured_per_callback() {
        // a separate input device: its own clock (+-200 ppm), other packets, its own delay
        for (ppm, packet) in [(200.0, 128), (-200.0, 441), (0.0, 1024)] {
            let r = simulate(&Sim { out_lat: 2400, in_packet: packet, in_delay_ms: 4.0, jitter_ms: 2.0, in_start_s: -1.0, ppm, click: 100_000, ..Sim::default() });
            assert_aligned(&r, &format!("ppm {ppm} packet {packet}"));
        }
    }

    fn sine(t: f64) -> f32 {
        (0.5 * (std::f64::consts::TAU * 440.0 * t).sin()) as f32
    }

    /// Longest run of exact zeros and largest sample-to-sample jump after the jitter
    /// buffer first started playing.
    fn damage(x: &[f32]) -> (usize, f32) {
        let start = x.iter().position(|v| *v != 0.0).unwrap_or(x.len());
        let x = &x[start..];
        let (mut run, mut longest) = (0, 0);
        for v in x {
            run = if *v == 0.0 { run + 1 } else { 0 };
            longest = longest.max(run);
        }
        let jump = x.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
        (longest, jump)
    }

    /// 0.5 * 2 pi * 440 / 48000: the largest step of the clean sine.
    const CLEAN_STEP: f32 = 0.0288;

    #[test]
    fn late_480_frame_input_periods_against_512_frame_callbacks_leave_no_zeros_in_the_take() {
        // the owner's take: WASAPI input periods of 480 frames arriving irregularly (up to
        // 6 ms late) against 512-frame output callbacks used to put ~477 zeros into the take
        // several times a second
        let r = simulate(&Sim { jitter_ms: 6.0, secs: 8.0, signal: Some(sine), ..Sim::default() });
        let (zeros, jump) = damage(&r.input);
        assert!(zeros < 2, "a run of {zeros} zeros in the take");
        assert!(jump <= CLEAN_STEP * 1.02, "a jump of {jump} (clean sine: {CLEAN_STEP})");
        assert_eq!(r.stats.underruns.load(Relaxed), 0);
        assert_eq!(r.stats.overruns.load(Relaxed), 0);
    }

    #[test]
    fn a_separate_input_clock_200_ppm_off_is_followed_without_discontinuities() {
        for ppm in [200.0, -200.0] {
            let r = simulate(&Sim { jitter_ms: 4.0, ppm, secs: 40.0, signal: Some(sine), ..Sim::default() });
            let (zeros, jump) = damage(&r.input);
            assert!(zeros < 2, "ppm {ppm}: a run of {zeros} zeros");
            assert!(jump <= CLEAN_STEP * 1.02, "ppm {ppm}: a jump of {jump} (clean sine: {CLEAN_STEP})");
            assert_eq!(r.stats.underruns.load(Relaxed), 0, "ppm {ppm}: underruns");
            assert_eq!(r.stats.overruns.load(Relaxed), 0, "ppm {ppm}: overruns");
            let seen = f32::from_bits(r.stats.drift_ppm.load(Relaxed)) as f64;
            assert!((seen - ppm).abs() < 40.0, "drift measured {seen} ppm, really {ppm}");
        }
    }

    #[test]
    fn an_input_that_stalls_fades_out_and_back_in_and_is_counted() {
        // packets stop for 100 ms: a counted gap, faded, never a hard jump
        let mut s = Sim { secs: 3.0, signal: Some(sine), ..Sim::default() };
        s.jitter_ms = 0.0;
        let (mut tx, rx) = rtrb::RingBuffer::<f32>::new(RATE as usize * 2);
        let mut jb = JitterBuf::new(RATE, 2);
        let stats = IoStats::default();
        let mut ring = rx;
        let (mut out, mut o, mut q) = (vec![], vec![0f32; 512], 0usize);
        for j in 0..250 {
            let due = (j + 1) * 512 + 1000;
            let stalled = (100..110).contains(&j);
            while !stalled && q + 480 <= due {
                for i in 0..480 {
                    let v = sine((q + i) as f64 / RATE as f64);
                    let _ = tx.push(v);
                    let _ = tx.push(v);
                }
                q += 480;
            }
            if stalled {
                q = due; // the frames captured meanwhile never arrive
            }
            jb.pull(&mut ring, &mut o, 512, 480, &stats);
            out.extend_from_slice(&o);
        }
        assert!(stats.underruns.load(Relaxed) >= 1, "the stall is counted");
        let start = out.iter().position(|v| *v != 0.0).unwrap();
        let jump = out[start..].windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
        // fades are 2 ms (96 frames): at most 0.5/96 per sample on top of the sine
        assert!(jump <= CLEAN_STEP + 0.5 / 96.0 + 1e-3, "a hard jump of {jump}");
        let _ = s;
    }

    #[test]
    fn the_offset_moves_the_alignment_and_never_goes_negative() {
        assert_eq!(with_offset(1000, 2.5, 48000), 1120);
        assert_eq!(with_offset(1000, -2.5, 48000), 880);
        assert_eq!(with_offset(100, -50.0, 48000), 0);
    }

    #[test]
    fn only_the_offset_changing_does_not_restart_the_streams() {
        let a = AudioConfig { output: Some("Speakers".into()), ..Default::default() };
        let b = AudioConfig { rec_offset_ms: Some(3.0), ..a.clone() };
        assert!(same_streams(&a, &b));
        assert!(!same_streams(&a, &AudioConfig { buffer_size: Some(256), ..b }));
    }

    #[test]
    fn settings_saved_before_input_channels_still_load() {
        let c: AudioConfig = serde_json::from_str(r#"{"host":"","output":null,"input":null,"sampleRate":null,"bufferSize":null}"#).unwrap();
        assert_eq!(c.input_channel, None);
        assert_eq!(c.rec_offset_ms, None);
    }
}
