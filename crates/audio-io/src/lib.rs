//! Audio devices for WAVE.
//!
//! One thread owns the cpal streams (they are not `Send` on every platform);
//! the app talks to it through [`AudioIo`]. The output callback owns the
//! engine's [`wave_engine::Rt`] outright — no locks on the audio thread. Input
//! (for recording and monitoring) arrives from a second stream through a
//! lock-free ring.
//!
//! **ASIO**: compiled with the `asio` feature on Windows. Input and output are
//! opened on the *same* driver, which is the only way ASIO works. If ASIO was
//! asked for and cannot be opened we fall back to the system host and say why
//! in [`AudioStatus::note`], so there is always sound.

use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

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
}

/// Handle to the audio thread. Cheap to clone.
#[derive(Clone)]
pub struct AudioIo {
    tx: mpsc::Sender<Req>,
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
                let mut want_input = false;
                while let Ok(req) = rx.recv() {
                    let (cfg, reply) = match req {
                        Req::Start(cfg, reply) => (cfg, reply),
                        Req::Input(on, reply) => {
                            if on == want_input && cur.is_some() {
                                let _ = reply.send(status.clone());
                                continue;
                            }
                            want_input = on;
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
                                if let Some(e) = r.error.lock().ok().and_then(|g| g.clone()) {
                                    status.error = Some(e);
                                    status.running = false;
                                }
                            }
                            let _ = reply.send(status.clone());
                            continue;
                        }
                    };
                    cfg_now = cfg.clone();
                    {
                        {
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
                    }
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

    /// Opens or closes the input (microphone) without changing the device choice.
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
}

struct Running {
    _out: cpal::Stream,
    _inp: Option<cpal::Stream>,
    error: Arc<std::sync::Mutex<Option<String>>>,
}

const MAX_CALLBACK: usize = 8192;

fn find(devs: Result<impl Iterator<Item = cpal::Device>, cpal::Error>, name: &str) -> Option<cpal::Device> {
    devs.ok()?.find(|d| dev_name(d).map(|n| n == name).unwrap_or(false))
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

    // ---- input (same host; on ASIO the same driver)
    let is_asio = host.id().name().eq_ignore_ascii_case("asio");
    // ASIO inputs live on the same driver and cost nothing; elsewhere only open the mic on demand
    let in_dev = if is_asio {
        find(host.input_devices(), &out_name)
    } else if !want_input {
        None
    } else {
        match cfg.input.as_deref() {
            Some("") => None,
            Some(n) => find(host.input_devices(), n),
            None => host.default_input_device(),
        }
    };
    let (in_tx, in_rx) = rtrb::RingBuffer::<f32>::new(rate as usize * 2);
    let error = Arc::new(std::sync::Mutex::new(None::<String>));
    // input latency (capture → input callback) in frames, measured by the input callback
    let in_lat = Arc::new(AtomicU32::new(0));
    let mut in_name = None;
    let mut in_ch = 0u16;
    let mut inp = None;
    if let Some(d) = in_dev {
        match d.default_input_config() {
            Ok(ic) => {
                let ich = ic.channels().max(1);
                let isc = cpal::StreamConfig { channels: ich, sample_rate: rate, buffer_size: buffer };
                let err = error.clone();
                let pick = cfg.input_channel.unwrap_or(0).min(ich - 1) as usize;
                match build_input(&d, &isc, ic.sample_format(), in_tx, err, pick, in_lat.clone()) {
                    Ok(s) => {
                        in_name = dev_name(&d);
                        in_ch = ich;
                        inp = Some(s);
                    }
                    Err(e) => note = Some(format!("Recording input could not open ({e}). Playback works.")),
                }
            }
            Err(e) => note = Some(format!("Recording input could not open ({e}). Playback works.")),
        }
    }

    let rt = engine.create_rt(rate as f32, (nominal as usize).clamp(64, 2048));
    let shared = engine.shared();
    let err = error.clone();
    let out = build_output(&out_dev, &sc, fmt, rt, in_rx, err, Latency { shared, input: in_lat }).map_err(|e| format!("{out_name}: {e}"))?;
    out.play().map_err(|e| format!("{out_name}: {e}"))?;
    if let Some(s) = &inp {
        if let Err(e) = s.play() {
            note = Some(format!("Recording input could not start ({e})."));
        }
    }
    let status = AudioStatus {
        config: cfg.clone(),
        running: true,
        actual_host: host.id().name().to_string(),
        actual_output: out_name,
        actual_input: in_name,
        actual_rate: rate,
        actual_buffer: nominal,
        latency_ms: nominal as f32 / rate as f32 * 1000.0,
        input_channels: in_ch,
        error: None,
        note,
        asio_compiled: asio_compiled(),
    };
    Ok((Running { _out: out, _inp: inp, error }, status))
}

/// Where the output callback publishes the measured round trip.
struct Latency {
    shared: Arc<wave_engine::rt::Shared>,
    /// Input latency in frames from the input callback (0 = not measured yet).
    input: Arc<AtomicU32>,
}

/// Round trip in frames for one output callback: what the callback renders now is heard
/// `out` frames later, and the oldest input it consumes was captured `backlog + input`
/// frames ago. Hosts that report no timestamps fall back to one buffer each way.
fn round_trip(out: u32, backlog: u32, input: u32, buffer: u32) -> u32 {
    let out = if out == 0 { buffer } else { out };
    let input = if input == 0 { buffer } else { input };
    out + backlog + input
}

fn frames_of(d: std::time::Duration, rate: u32) -> u32 {
    (d.as_secs_f64() * rate as f64).round().min(u32::MAX as f64) as u32
}

fn build_output(dev: &cpal::Device, sc: &cpal::StreamConfig, fmt: cpal::SampleFormat, mut rt: wave_engine::Rt, mut input: rtrb::Consumer<f32>, err: Arc<std::sync::Mutex<Option<String>>>, lat: Latency) -> Result<cpal::Stream, String> {
    let ch = sc.channels as usize;
    let rate = sc.sample_rate;
    // smoothed round trip (frames); the backlog of the input ring jitters by a callback
    let mut smooth = -1f64;
    let mut l = vec![0f32; MAX_CALLBACK];
    let mut r = vec![0f32; MAX_CALLBACK];
    let mut il = vec![0f32; MAX_CALLBACK];
    let mut ir = vec![0f32; MAX_CALLBACK];
    let mut render = move |frames: usize, out_lat: u32, write: &mut dyn FnMut(usize, f32, f32)| {
        let backlog = (input.slots() / 2) as u32;
        let rtt = round_trip(out_lat, backlog, lat.input.load(Relaxed), frames as u32) as f64;
        smooth = if smooth < 0.0 { rtt } else { smooth * 0.95 + rtt * 0.05 };
        lat.shared.set_io_latency(smooth.round() as u32);
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(MAX_CALLBACK);
            // input: as many frames as are ready, zero-padded
            let avail = (input.slots() / 2).min(n);
            for i in 0..avail {
                il[i] = input.pop().unwrap_or(0.0);
                ir[i] = input.pop().unwrap_or(0.0);
            }
            il[avail..n].fill(0.0);
            ir[avail..n].fill(0.0);
            // don't let input latency creep up: drop what piles up beyond ~2 callbacks
            while input.slots() > n * 2 * 2 {
                let _ = input.pop();
            }
            rt.process(&mut l[..n], &mut r[..n], Some((&il[..n], &ir[..n])));
            for i in 0..n {
                write(done + i, l[i], r[i]);
            }
            done += n;
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
                    render(frames, out_lat, &mut |i, a, b| {
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

fn build_input(dev: &cpal::Device, sc: &cpal::StreamConfig, fmt: cpal::SampleFormat, mut tx: rtrb::Producer<f32>, err: Arc<std::sync::Mutex<Option<String>>>, pick: usize, lat: Arc<AtomicU32>) -> Result<cpal::Stream, String> {
    let ch = sc.channels as usize;
    let pick = pick.min(ch - 1);
    let rate = sc.sample_rate;
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
                    lat.store(frames_of(ts.callback.duration_since(ts.capture), rate), Relaxed);
                    // one channel (a mic on input 1 or 2), mono in both sides: a vocal recorded
                    // from a stereo interface must not end up in the left ear only
                    for f in data.chunks_exact(ch) {
                        if tx.slots() < 2 {
                            break;
                        }
                        let a = $conv(f[pick]);
                        let _ = tx.push(a);
                        let _ = tx.push(a);
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

    #[test]
    fn settings_saved_before_input_channels_still_load() {
        let c: AudioConfig = serde_json::from_str(r#"{"host":"","output":null,"input":null,"sampleRate":null,"bufferSize":null}"#).unwrap();
        assert_eq!(c.input_channel, None);
    }
}
