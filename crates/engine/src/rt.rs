//! The audio thread. Everything here runs inside the device callback (or the offline renderer):
//! no allocation, no blocking locks, no I/O. Structures arrive pre-built through [`Cmd`] and
//! leave through [`Garbage`] so they are freed elsewhere.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::Arc;
use std::time::Instant;

use crate::model::{Id, Inst};
use crate::processor::{ParamBlock, ProcessCtx, Processor, METERS};
use crate::sources::Audio;
use crate::synth::{VoiceKind, VoicePool};

pub const MAX_INSERTS: usize = 128;
pub const WATCH_SLOTS: usize = 8;
pub const TAP_CAPACITY: usize = 16384;

// ------------------------------------------------------------------ shared state

/// Lock-free numbers the audio thread publishes for the UI.
pub struct Shared {
    beat: AtomicU64,
    pub playing: AtomicBool,
    pub recording: AtomicBool,
    pub stopped_at_end: AtomicBool,
    in_level: AtomicU32,
    cpu: AtomicU32,
    preview_pos: AtomicU32,
    /// Per insert *index*: (L, R) peak since last read.
    meters: Vec<(AtomicU32, AtomicU32)>,
    gr: Vec<AtomicU32>,
    fx_meters: Vec<[AtomicU32; METERS]>,
    pub sample_rate: AtomicU32,
}

fn fset(a: &AtomicU32, v: f32) {
    a.store(v.to_bits(), Relaxed)
}
fn fget(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Relaxed))
}
fn fmax(a: &AtomicU32, v: f32) {
    if v > fget(a) {
        fset(a, v)
    }
}

impl Shared {
    pub fn new() -> Self {
        Self {
            beat: AtomicU64::new(0f64.to_bits()),
            playing: AtomicBool::new(false),
            recording: AtomicBool::new(false),
            stopped_at_end: AtomicBool::new(false),
            in_level: AtomicU32::new(0),
            cpu: AtomicU32::new(0),
            preview_pos: AtomicU32::new((-1f32).to_bits()),
            meters: (0..MAX_INSERTS).map(|_| (AtomicU32::new(0), AtomicU32::new(0))).collect(),
            gr: (0..WATCH_SLOTS).map(|_| AtomicU32::new(0)).collect(),
            fx_meters: (0..WATCH_SLOTS).map(|_| std::array::from_fn(|_| AtomicU32::new(0))).collect(),
            sample_rate: AtomicU32::new(48000),
        }
    }
    pub fn beat(&self) -> f64 {
        f64::from_bits(self.beat.load(Relaxed))
    }
    pub fn take_meter(&self, idx: usize) -> (f32, f32) {
        let (l, r) = &self.meters[idx];
        (f32::from_bits(l.swap(0, Relaxed)), f32::from_bits(r.swap(0, Relaxed)))
    }
    pub fn take_in_level(&self) -> f32 {
        f32::from_bits(self.in_level.swap(0, Relaxed))
    }
    pub fn cpu(&self) -> f32 {
        fget(&self.cpu)
    }
    pub fn preview_pos(&self) -> f32 {
        fget(&self.preview_pos)
    }
    pub fn fx_meters(&self, slot: usize) -> [f32; METERS] {
        std::array::from_fn(|i| fget(&self.fx_meters[slot][i]))
    }
    pub fn gr(&self, slot: usize) -> f32 {
        fget(&self.gr[slot])
    }
}

impl Default for Shared {
    fn default() -> Self {
        Self::new()
    }
}

// ------------------------------------------------------------------ graph

pub struct FxSlot {
    pub id: Id,
    pub bypass: bool,
    /// Dry/wet 0..1.
    pub mix: f32,
    /// `None` = "take the running processor with this id from the current graph".
    pub proc: Option<Box<dyn Processor>>,
    pub params: Option<ParamBlock>,
    /// Delays the dry path by the effect's latency, so dry/wet blends line up.
    dly_l: Vec<f32>,
    dly_r: Vec<f32>,
    dly_w: usize,
}

impl FxSlot {
    /// `latency` in samples (allocates; call off the audio thread).
    pub fn new(id: Id, bypass: bool, mix: f32, proc: Option<Box<dyn Processor>>, params: Option<ParamBlock>, latency: usize) -> Self {
        Self { id, bypass, mix, proc, params, dly_l: vec![0.0; latency], dly_r: vec![0.0; latency], dly_w: 0 }
    }

    fn delay_dry(&mut self, l: &mut [f32], r: &mut [f32]) {
        let n = self.dly_l.len();
        for (x, y) in l.iter_mut().zip(r.iter_mut()) {
            let w = self.dly_w;
            *x = std::mem::replace(&mut self.dly_l[w], *x);
            *y = std::mem::replace(&mut self.dly_r[w], *y);
            self.dly_w = if w + 1 == n { 0 } else { w + 1 };
        }
    }
}

pub struct InsertRt {
    pub id: Id,
    pub gain: f32,
    pub pan: f32,
    pub on: bool,
    pub fx: Vec<FxSlot>,
    g_cur: f32,
    l: Vec<f32>,
    r: Vec<f32>,
    /// Latency compensation: extra delay so this insert lines up with the slowest one.
    pdc_l: Vec<f32>,
    pdc_r: Vec<f32>,
    pdc_w: usize,
    dry_l: Vec<f32>,
    dry_r: Vec<f32>,
}

impl InsertRt {
    pub fn new(id: Id, gain: f32, pan: f32, on: bool, fx: Vec<FxSlot>, max_block: usize) -> Self {
        Self { id, gain, pan, on, fx, g_cur: if on { gain } else { 0.0 }, l: vec![0.0; max_block], r: vec![0.0; max_block], pdc_l: vec![], pdc_r: vec![], pdc_w: 0, dry_l: vec![0.0; max_block], dry_r: vec![0.0; max_block] }
    }

    /// Delays this insert's output by `samples` (allocates; call off the audio thread).
    pub fn with_delay(mut self, samples: usize) -> Self {
        if samples > 0 {
            self.pdc_l = vec![0.0; samples];
            self.pdc_r = vec![0.0; samples];
        }
        self
    }
}

#[derive(Clone, Copy)]
pub struct TrackRt {
    pub insert: usize,
    pub gain: f32,
    pub on: bool,
}

pub struct Graph {
    pub inserts: Vec<InsertRt>,
    pub tracks: Vec<TrackRt>,
    track_bufs: Vec<(Vec<f32>, Vec<f32>)>,
    /// Track index whose insert receives the live input when monitoring.
    pub monitor_track: Option<usize>,
}

impl Graph {
    pub fn new(inserts: Vec<InsertRt>, tracks: Vec<TrackRt>, monitor_track: Option<usize>, max_block: usize) -> Self {
        let track_bufs = tracks.iter().map(|_| (vec![0.0; max_block], vec![0.0; max_block])).collect();
        Self { inserts, tracks, track_bufs, monitor_track }
    }
}

/// Fader/pan/mute for everything, sent when only those changed.
pub struct MixState {
    pub inserts: Vec<(Id, f32, f32, bool)>,
    pub tracks: Vec<(f32, bool)>,
    pub monitor_track: Option<usize>,
}

// ------------------------------------------------------------------ timeline

pub enum ClipSrc {
    Pat(usize),
    Audio(Arc<Audio>),
    Auto(usize),
    /// A comp, resolved off-thread (shared by every clip that plays it).
    Comp(Arc<crate::comp::CompRt>),
}

/// A resolved automation target (indices into the graph built from the same project).
#[derive(Clone, Copy, Debug)]
pub enum TargetRt {
    Param { fx: Id, idx: usize, min: f32, max: f32, log: bool },
    /// EQ band field: 0 freq, 1 gain, 2 Q.
    Band { fx: Id, band: usize, field: u8 },
    Mix { fx: Id },
    InsVol(usize),
    InsPan(usize),
    TrackVol(usize),
}

pub struct AutoRt {
    pub target: TargetRt,
    pub env: crate::model::Automation,
}

pub struct ClipRt {
    pub track: usize,
    pub start: f64,
    pub end: f64,
    pub offset: f64,
    pub gain: f32,
    pub pan: f32,
    pub fade_in: f32,
    pub fade_out: f32,
    pub src: ClipSrc,
}

pub struct NoteRt {
    pub beat: f64,
    pub len: f64,
    pub inst: Inst,
    pub key: Option<u8>,
}

pub struct PatRt {
    pub len: f64,
    pub notes: Vec<NoteRt>,
}

pub struct Timeline {
    pub bpm: f64,
    pub clips: Vec<ClipRt>,
    pub pats: Vec<PatRt>,
    pub autos: Vec<AutoRt>,
    pub loop_range: Option<(f64, f64)>,
    pub song_end: f64,
}

pub struct Preview {
    pub audio: Arc<Audio>,
    pub pos: f64,
    pub end: f64,
    pub gain: f32,
    pub fade_in: f64,
    pub fade_out: f64,
    pub start: f64,
}

// ------------------------------------------------------------------ messages

pub enum Cmd {
    Graph(Box<Graph>),
    Timeline(Box<Timeline>),
    Mix(Box<MixState>),
    Params { fx: Id, params: Option<ParamBlock>, mix: f32 },
    /// `rec_loop`: record in passes over the loop range (one take per pass).
    Play { from: f64, count_in: f64, rec: bool, rec_loop: bool },
    /// Stops capturing input but keeps the transport running.
    PunchOut,
    Stop,
    Flags { metro: bool, monitor: bool },
    Watch { ids: [Id; WATCH_SLOTS], n: u8 },
    Preview(Option<Box<Preview>>),
}

pub enum Garbage {
    Graph(Box<Graph>),
    Timeline(Box<Timeline>),
    Mix(Box<MixState>),
    Preview(Box<Preview>),
}

pub struct Taps {
    pub input: Vec<rtrb::Producer<f32>>,
    pub output: Vec<rtrb::Producer<f32>>,
}

// ------------------------------------------------------------------ the engine

pub struct Rt {
    sr: f32,
    max_block: usize,
    cmds: rtrb::Consumer<Cmd>,
    garbage: rtrb::Producer<Garbage>,
    shared: Arc<Shared>,
    graph: Box<Graph>,
    timeline: Box<Timeline>,
    voices: VoicePool,
    click: Vec<f32>,
    tmp_l: Vec<f32>,
    tmp_r: Vec<f32>,
    comp_l: Vec<f32>,
    comp_r: Vec<f32>,
    playing: bool,
    beat: f64,
    rec: bool,
    rec_loop: bool,
    rec_start: f64,
    count_end: f64,
    metro: bool,
    monitor: bool,
    watch: [Id; WATCH_SLOTS],
    watch_n: usize,
    taps: Taps,
    record: rtrb::Producer<f32>,
    preview: Option<Box<Preview>>,
}

impl Rt {
    pub fn new(sr: f32, max_block: usize, cmds: rtrb::Consumer<Cmd>, garbage: rtrb::Producer<Garbage>, shared: Arc<Shared>, taps: Taps, record: rtrb::Producer<f32>) -> Self {
        shared.sample_rate.store(sr as u32, Relaxed);
        Self {
            sr,
            max_block,
            cmds,
            garbage,
            shared,
            graph: Box::new(Graph::new(vec![], vec![], None, max_block)),
            timeline: Box::new(Timeline { bpm: 120.0, clips: vec![], pats: vec![], autos: vec![], loop_range: None, song_end: 4.0 }),
            voices: VoicePool::new(sr),
            click: vec![0.0; max_block],
            tmp_l: vec![0.0; max_block],
            tmp_r: vec![0.0; max_block],
            comp_l: vec![0.0; max_block],
            comp_r: vec![0.0; max_block],
            playing: false,
            beat: 0.0,
            rec: false,
            rec_loop: false,
            rec_start: 0.0,
            count_end: f64::NEG_INFINITY,
            metro: false,
            monitor: false,
            watch: [u64::MAX; WATCH_SLOTS],
            watch_n: 0,
            taps,
            record,
            preview: None,
        }
    }

    pub fn sample_rate(&self) -> f32 {
        self.sr
    }
    pub fn max_block(&self) -> usize {
        self.max_block
    }

    fn trash(&mut self, g: Garbage) {
        // The ring is sized generously; if it is ever full we leak rather than free here.
        if let Err(rtrb::PushError::Full(g)) = self.garbage.push(g) {
            std::mem::forget(g);
        }
    }

    fn drain(&mut self) {
        while let Ok(cmd) = self.cmds.pop() {
            match cmd {
                Cmd::Graph(mut g) => {
                    // carry running processors (and their tails) over by effect id
                    for ins in g.inserts.iter_mut() {
                        for slot in ins.fx.iter_mut() {
                            if let Some(old) = self.graph.inserts.iter_mut().flat_map(|i| i.fx.iter_mut()).find(|s| s.id == slot.id) {
                                if slot.proc.is_none() {
                                    slot.proc = old.proc.take();
                                }
                                if old.dly_l.len() == slot.dly_l.len() {
                                    std::mem::swap(&mut slot.dly_l, &mut old.dly_l);
                                    std::mem::swap(&mut slot.dly_r, &mut old.dly_r);
                                    slot.dly_w = old.dly_w;
                                }
                            }
                            if let (Some(p), Some(pb)) = (slot.proc.as_mut(), slot.params.as_ref()) {
                                p.set_params(pb);
                            }
                        }
                        if let Some(old) = self.graph.inserts.iter().find(|i| i.id == ins.id) {
                            ins.g_cur = old.g_cur;
                        }
                    }
                    let old = std::mem::replace(&mut self.graph, g);
                    self.trash(Garbage::Graph(old));
                }
                Cmd::Timeline(t) => {
                    let old = std::mem::replace(&mut self.timeline, t);
                    self.trash(Garbage::Timeline(old));
                }
                Cmd::Mix(m) => {
                    for (id, gain, pan, on) in m.inserts.iter().copied() {
                        if let Some(i) = self.graph.inserts.iter_mut().find(|i| i.id == id) {
                            i.gain = gain;
                            i.pan = pan;
                            i.on = on;
                        }
                    }
                    for (t, &(gain, on)) in self.graph.tracks.iter_mut().zip(m.tracks.iter()) {
                        t.gain = gain;
                        t.on = on;
                    }
                    self.graph.monitor_track = m.monitor_track;
                    self.trash(Garbage::Mix(m));
                }
                Cmd::Params { fx, params, mix } => {
                    if let Some(s) = self.graph.inserts.iter_mut().flat_map(|i| i.fx.iter_mut()).find(|s| s.id == fx) {
                        s.mix = mix;
                        if let Some(params) = params {
                            s.params = Some(params);
                            if let Some(p) = s.proc.as_mut() {
                                p.set_params(&params);
                            }
                        }
                    }
                }
                Cmd::Play { from, count_in, rec, rec_loop } => {
                    self.voices.clear();
                    self.beat = from - count_in;
                    self.count_end = if count_in > 0.0 { from } else { f64::NEG_INFINITY };
                    self.rec = rec;
                    self.rec_loop = rec && rec_loop;
                    self.rec_start = from;
                    self.playing = true;
                    self.shared.playing.store(true, Relaxed);
                    self.shared.stopped_at_end.store(false, Relaxed);
                }
                Cmd::PunchOut => {
                    // `rec` stays set so the run keeps its open-ended extent instead of
                    // wrapping at a song end that the new take has not extended yet
                    self.rec_start = f64::INFINITY;
                    self.shared.recording.store(false, Relaxed);
                }
                Cmd::Stop => self.stop(false),
                Cmd::Flags { metro, monitor } => {
                    self.metro = metro;
                    self.monitor = monitor;
                }
                Cmd::Watch { ids, n } => {
                    self.watch = ids;
                    self.watch_n = n as usize;
                }
                Cmd::Preview(p) => {
                    if let Some(old) = std::mem::replace(&mut self.preview, p) {
                        self.trash(Garbage::Preview(old));
                    }
                    if self.preview.is_none() {
                        fset(&self.shared.preview_pos, -1.0);
                    }
                }
            }
        }
    }

    fn stop(&mut self, at_end: bool) {
        self.playing = false;
        self.rec = false;
        self.rec_loop = false;
        self.shared.playing.store(false, Relaxed);
        self.shared.recording.store(false, Relaxed);
        if at_end {
            self.shared.stopped_at_end.store(true, Relaxed);
        }
        // tails of instruments keep ringing; effect tails too
    }

    /// Renders `out_l.len()` frames. `input` is the device input for the same period, if any.
    pub fn process(&mut self, out_l: &mut [f32], out_r: &mut [f32], input: Option<(&[f32], &[f32])>) {
        let t0 = Instant::now();
        self.drain();
        let n = out_l.len();
        let mut pos = 0;
        while pos < n {
            let m = (n - pos).min(self.max_block);
            let inp = input.map(|(l, r)| (&l[pos.min(l.len())..(pos + m).min(l.len())], &r[pos.min(r.len())..(pos + m).min(r.len())]));
            self.block(pos, m, out_l, out_r, inp);
            pos += m;
        }
        let period = n as f32 / self.sr;
        let load = t0.elapsed().as_secs_f32() / period.max(1e-6);
        let prev = fget(&self.shared.cpu);
        fset(&self.shared.cpu, if load > prev { load } else { prev * 0.95 + load * 0.05 });
    }

    fn block(&mut self, at: usize, n: usize, out_l: &mut [f32], out_r: &mut [f32], input: Option<(&[f32], &[f32])>) {
        let sr = self.sr;
        for (l, r) in self.graph.track_bufs.iter_mut() {
            l[..n].fill(0.0);
            r[..n].fill(0.0);
        }
        for ins in self.graph.inserts.iter_mut() {
            ins.l[..n].fill(0.0);
            ins.r[..n].fill(0.0);
        }
        self.click[..n].fill(0.0);

        let bpm = self.timeline.bpm;
        let spb = 60.0 / bpm; // seconds per beat
        let bps = 1.0 / (spb * sr as f64); // beats per sample
        let block_beat = self.beat;

        // ---- schedule & render the timeline
        if self.playing {
            let mut p = 0usize;
            let mut guard = 0;
            while p < n && self.playing && guard < 8 {
                guard += 1;
                let (ls, le) = if self.rec {
                    match self.timeline.loop_range {
                        Some(lr) if self.rec_loop && lr.1 > lr.0 => lr,
                        _ => (0.0, crate::model::TOTAL_BEATS),
                    }
                } else {
                    self.timeline.loop_range.unwrap_or((0.0, self.timeline.song_end))
                };
                let left = ((le - self.beat) / bps).ceil().max(1.0) as usize;
                let seg = left.min(n - p);
                let b0 = self.beat;
                let b1 = b0 + seg as f64 * bps;
                self.schedule(b0, b1, p, seg, bps, spb);
                // record input
                if self.rec && b1 > self.rec_start {
                    self.shared.recording.store(true, Relaxed);
                    if let Some((il, ir)) = input {
                        let first = if b0 >= self.rec_start { 0 } else { (((self.rec_start - b0) / bps).ceil() as usize).min(seg) };
                        for i in p + first..(p + seg).min(il.len()) {
                            if self.record.slots() >= 2 {
                                // NaN is the pass marker (below): never let it in from the device
                                let fin = |x: f32| if x.is_finite() { x } else { 0.0 };
                                let _ = self.record.push(fin(il[i]));
                                let _ = self.record.push(fin(ir[i]));
                            }
                        }
                    }
                }
                self.beat = b1;
                p += seg;
                if self.beat >= le - 1e-9 {
                    if self.rec && !self.rec_loop {
                        self.stop(true);
                    } else {
                        self.beat = ls;
                        // loop recording: a NaN frame tells the engine a new pass (take) begins
                        if self.rec && self.rec_start.is_finite() && self.record.slots() >= 2 {
                            let _ = self.record.push(f32::NAN);
                            let _ = self.record.push(f32::NAN);
                        }
                    }
                }
            }
        }
        if self.playing {
            apply_automation(&self.timeline, &mut self.graph, block_beat);
        }
        let shared_beat = if self.playing { self.beat } else { block_beat };
        self.shared.beat.store(shared_beat.to_bits(), Relaxed);
        self.voices.render(n, &mut self.graph.track_bufs, &mut self.click[..n]);

        // ---- input level + monitoring
        if let Some((il, ir)) = input {
            let pk = il.iter().chain(ir.iter()).fold(0f32, |a, b| a.max(b.abs()));
            fmax(&self.shared.in_level, pk);
            if self.monitor {
                if let Some(t) = self.graph.monitor_track {
                    if let Some((l, r)) = self.graph.track_bufs.get_mut(t) {
                        for i in 0..n.min(il.len()) {
                            l[i] += il[i];
                            r[i] += ir[i];
                        }
                    }
                }
            }
        }

        // ---- tracks → inserts
        let g = &mut *self.graph;
        for (ti, t) in g.tracks.iter().enumerate() {
            let Some(ins) = g.inserts.get_mut(t.insert) else { continue };
            if !t.on {
                continue;
            }
            let (tl, tr) = &g.track_bufs[ti];
            for i in 0..n {
                ins.l[i] += tl[i] * t.gain;
                ins.r[i] += tr[i] * t.gain;
            }
        }

        // ---- inserts (non-master first), then master
        let ctx = ProcessCtx { sample_rate: sr, bpm, beat: block_beat, playing: self.playing };
        let count = g.inserts.len();
        for k in (1..count).chain(std::iter::once(0)) {
            if count == 0 {
                break;
            }
            let (head, tail) = g.inserts.split_at_mut(k);
            let ins = &mut tail[0];
            run_chain(ins, n, &ctx, &self.watch[..self.watch_n], &mut self.taps, &self.shared);
            // fader + pan, smoothed across the block
            let target = if ins.on { ins.gain } else { 0.0 };
            let (gl, gr) = (if ins.pan > 0.0 { 1.0 - ins.pan } else { 1.0 }, if ins.pan < 0.0 { 1.0 + ins.pan } else { 1.0 });
            let g0 = ins.g_cur;
            let step = (target - g0) / n as f32;
            let (mut pl, mut pr) = (0f32, 0f32);
            for i in 0..n {
                let gg = g0 + step * (i + 1) as f32;
                ins.l[i] *= gg * gl;
                ins.r[i] *= gg * gr;
                pl = pl.max(ins.l[i].abs());
                pr = pr.max(ins.r[i].abs());
            }
            ins.g_cur = target;
            if k < MAX_INSERTS {
                fmax(&self.shared.meters[k].0, pl);
                fmax(&self.shared.meters[k].1, pr);
            }
            if !ins.pdc_l.is_empty() {
                let d = ins.pdc_l.len();
                for i in 0..n {
                    let w = ins.pdc_w;
                    let (a, b) = (ins.pdc_l[w], ins.pdc_r[w]);
                    ins.pdc_l[w] = ins.l[i];
                    ins.pdc_r[w] = ins.r[i];
                    ins.l[i] = a;
                    ins.r[i] = b;
                    ins.pdc_w = if w + 1 == d { 0 } else { w + 1 };
                }
            }
            if k != 0 {
                let master = &mut head[0];
                for i in 0..n {
                    master.l[i] += ins.l[i];
                    master.r[i] += ins.r[i];
                }
            }
        }

        // ---- out: master + click + preview
        let (ml, mr) = match g.inserts.first() {
            Some(m) => (&m.l[..n], &m.r[..n]),
            None => (&self.tmp_l[..n], &self.tmp_r[..n]),
        };
        for i in 0..n {
            out_l[at + i] = ml[i] + self.click[i] * 0.6;
            out_r[at + i] = mr[i] + self.click[i] * 0.6;
        }
        if let Some(pv) = self.preview.as_mut() {
            let rate = pv.audio.sr as f64 / sr as f64;
            for i in 0..n {
                if pv.pos >= pv.end {
                    break;
                }
                let t = (pv.pos - pv.start) / pv.audio.sr as f64;
                let tl = (pv.end - pv.pos) / pv.audio.sr as f64;
                let mut env = pv.gain as f64;
                if pv.fade_in > 0.0 && t < pv.fade_in {
                    env *= t / pv.fade_in;
                }
                if pv.fade_out > 0.0 && tl < pv.fade_out {
                    env *= tl / pv.fade_out;
                }
                let (a, b) = pv.audio.sample(pv.pos);
                out_l[at + i] += a * env as f32;
                out_r[at + i] += b * env as f32;
                pv.pos += rate;
            }
            fset(&self.shared.preview_pos, (pv.pos / pv.audio.sr as f64) as f32);
            if pv.pos >= pv.end {
                let done = self.preview.take().unwrap();
                fset(&self.shared.preview_pos, -1.0);
                self.trash(Garbage::Preview(done));
            }
        }
    }

    fn schedule(&mut self, b0: f64, b1: f64, at: usize, n: usize, bps: f64, spb: f64) {
        let sr = self.sr as f64;
        // metronome & count-in clicks
        let mut b = b0.ceil();
        while b < b1 {
            if self.metro || b < self.count_end {
                let delay = ((b - b0) / bps) as u32 + at as u32;
                let accent = (b as i64).rem_euclid(4) == 0;
                self.voices.trigger(VoiceKind::Click { accent }, usize::MAX, delay, 0.06, 0.0);
            }
            b += 1.0;
        }
        let tl = &*self.timeline;
        for c in tl.clips.iter() {
            if c.end <= b0 || c.start >= b1 {
                continue;
            }
            match &c.src {
                ClipSrc::Pat(pi) => {
                    let Some(pat) = tl.pats.get(*pi) else { continue };
                    if pat.len <= 0.0 {
                        continue;
                    }
                    let base = c.start - c.offset;
                    let lo = b0.max(c.start);
                    let hi = b1.min(c.end);
                    let r0 = ((lo - base) / pat.len).floor() as i64 - 1;
                    let r1 = ((hi - base) / pat.len).floor() as i64;
                    for r in r0..=r1 {
                        for nt in &pat.notes {
                            let nb = base + r as f64 * pat.len + nt.beat;
                            if nb >= lo && nb < hi {
                                let delay = ((nb - b0) / bps) as u32 + at as u32;
                                let len_beats = nt.len.min(c.end - nb);
                                self.voices.trigger_inst(nt.inst, c.track, delay, (len_beats * spb) as f32, nt.key);
                            }
                        }
                    }
                }
                ClipSrc::Auto(_) => {}
                ClipSrc::Comp(cp) => {
                    let Some((tl_l, tl_r)) = self.graph.track_bufs.get_mut(c.track) else { continue };
                    // samples of this block inside the clip
                    // (same per-sample test as audio clips: start ≤ beat < end)
                    let mut i0 = (((c.start - b0) / bps).ceil().max(0.0) as usize).min(n);
                    let mut i1 = (((c.end - b0) / bps).ceil().max(0.0) as usize).min(n);
                    while i1 > i0 && b0 + (i1 - 1) as f64 * bps >= c.end {
                        i1 -= 1;
                    }
                    while i0 < i1 && b0 + i0 as f64 * bps < c.start {
                        i0 += 1;
                    }
                    if i1 <= i0 {
                        continue;
                    }
                    let m = i1 - i0;
                    let (bl, br) = (&mut self.comp_l[..m], &mut self.comp_r[..m]);
                    bl.fill(0.0);
                    br.fill(0.0);
                    let bt0 = b0 + i0 as f64 * bps;
                    cp.render_add(c.offset + bt0 - c.start, bps, bl, br, spb);
                    let dur = (c.end - c.start) * spb;
                    let g = 10f32.powf(c.gain / 20.0);
                    let (pl, pr) = (if c.pan > 0.0 { 1.0 - c.pan } else { 1.0 }, if c.pan < 0.0 { 1.0 + c.pan } else { 1.0 });
                    for k in 0..m {
                        let t = (bt0 + k as f64 * bps - c.start) * spb;
                        let mut env = g;
                        if c.fade_in > 0.0 && t < c.fade_in as f64 {
                            env *= (t / c.fade_in as f64).max(0.0) as f32;
                        }
                        let left = dur - t;
                        if c.fade_out > 0.0 && left < c.fade_out as f64 {
                            env *= (left / c.fade_out as f64).max(0.0) as f32;
                        }
                        tl_l[at + i0 + k] += bl[k] * env * pl;
                        tl_r[at + i0 + k] += br[k] * env * pr;
                    }
                }
                ClipSrc::Audio(a) => {
                    let Some((tl_l, tl_r)) = self.graph.track_bufs.get_mut(c.track) else { continue };
                    let dur = (c.end - c.start) * spb;
                    let g = 10f32.powf(c.gain / 20.0);
                    let (pl, pr) = (if c.pan > 0.0 { 1.0 - c.pan } else { 1.0 }, if c.pan < 0.0 { 1.0 + c.pan } else { 1.0 });
                    let fsr = a.sr as f64;
                    for i in 0..n {
                        let bt = b0 + i as f64 * bps;
                        if bt < c.start || bt >= c.end {
                            continue;
                        }
                        let rel = bt - c.start;
                        let t = rel * spb;
                        let mut env = g;
                        if c.fade_in > 0.0 && t < c.fade_in as f64 {
                            env *= (t / c.fade_in as f64) as f32;
                        }
                        let left = dur - t;
                        if c.fade_out > 0.0 && left < c.fade_out as f64 {
                            env *= (left / c.fade_out as f64) as f32;
                        }
                        let src = (c.offset + rel) * spb * fsr;
                        let (x, y) = a.sample(src);
                        tl_l[at + i] += x * env * pl;
                        tl_r[at + i] += y * env * pr;
                    }
                    let _ = sr;
                }
            }
        }
    }
}

fn run_chain(ins: &mut InsertRt, n: usize, ctx: &ProcessCtx, watch: &[Id], taps: &mut Taps, shared: &Shared) {
    for slot in ins.fx.iter_mut() {
        let w = watch.iter().position(|&id| id == slot.id);
        if let Some(w) = w {
            push_tap(&mut taps.input[w], &ins.l[..n], &ins.r[..n]);
        }
        let delayed = !slot.dly_l.is_empty();
        if !slot.bypass && slot.mix <= 0.0005 && delayed {
            // fully dry: still late by the latency PDC is compensating for
            slot.delay_dry(&mut ins.l[..n], &mut ins.r[..n]);
        } else if !slot.bypass && slot.mix > 0.0005 {
            if let Some(p) = slot.proc.as_mut() {
                let mix = slot.mix.min(1.0);
                if mix < 0.9995 || delayed {
                    ins.dry_l[..n].copy_from_slice(&ins.l[..n]);
                    ins.dry_r[..n].copy_from_slice(&ins.r[..n]);
                }
                p.process(&mut ins.l[..n], &mut ins.r[..n], ctx);
                if delayed {
                    // the line runs at every Mix so lowering it never plays stale audio
                    slot.delay_dry(&mut ins.dry_l[..n], &mut ins.dry_r[..n]);
                }
                if mix < 0.9995 {
                    for i in 0..n {
                        ins.l[i] = ins.dry_l[i] + (ins.l[i] - ins.dry_l[i]) * mix;
                        ins.r[i] = ins.dry_r[i] + (ins.r[i] - ins.dry_r[i]) * mix;
                    }
                }
            }
        }
        if let Some(w) = w {
            let wet = if slot.bypass { None } else { slot.proc.as_ref().and_then(|p| p.wet_tap()) };
            match wet {
                Some(wet) if !wet.is_empty() => push_mono(&mut taps.output[w], wet),
                _ => push_tap(&mut taps.output[w], &ins.l[..n], &ins.r[..n]),
            }
            let gr = if slot.bypass { 0.0 } else { slot.proc.as_ref().map(|p| p.gain_reduction_db()).unwrap_or(0.0) };
            fset(&shared.gr[w], gr);
            let mut m = [0f32; METERS];
            if let Some(p) = slot.proc.as_ref() {
                p.meters(&mut m);
            }
            for (a, v) in shared.fx_meters[w].iter().zip(m) {
                fset(a, v);
            }
        }
    }
}

fn push_tap(p: &mut rtrb::Producer<f32>, l: &[f32], r: &[f32]) {
    if p.slots() < l.len() {
        return;
    }
    for i in 0..l.len() {
        let _ = p.push((l[i] + r[i]) * 0.5);
    }
}

fn push_mono(p: &mut rtrb::Producer<f32>, x: &[f32]) {
    if p.slots() < x.len() {
        return;
    }
    for &v in x {
        let _ = p.push(v);
    }
}

fn slot(g: &mut Graph, fx: Id) -> Option<&mut FxSlot> {
    g.inserts.iter_mut().flat_map(|i| i.fx.iter_mut()).find(|s| s.id == fx)
}

/// Moves automated parameters to their value at `beat`. Allocation-free.
fn apply_automation(tl: &Timeline, g: &mut Graph, beat: f64) {
    for c in tl.clips.iter() {
        let ClipSrc::Auto(ai) = c.src else { continue };
        if beat < c.start || beat >= c.end {
            continue;
        }
        let Some(a) = tl.autos.get(ai) else { continue };
        let Some(v) = a.env.value_at(beat - c.start + c.offset) else { continue };
        match a.target {
            TargetRt::Param { fx, idx, min, max, log } => {
                if let Some(s) = slot(g, fx) {
                    let x = if log { min * (max / min).powf(v) } else { min + (max - min) * v };
                    if let Some(pb) = s.params.as_mut() {
                        if (pb.values[idx] - x).abs() > 1e-6 * x.abs().max(1.0) {
                            pb.values[idx] = x;
                            let pb = *pb;
                            if let Some(p) = s.proc.as_mut() {
                                p.set_params(&pb);
                            }
                        }
                    }
                }
            }
            TargetRt::Band { fx, band, field } => {
                if let Some(s) = slot(g, fx) {
                    if let Some(pb) = s.params.as_mut() {
                        let b = &mut pb.bands[band];
                        let x = match field {
                            0 => 20.0 * 1000f32.powf(v),
                            1 => -30.0 + 60.0 * v,
                            _ => 0.1 * 180f32.powf(v),
                        };
                        let cur = match field {
                            0 => &mut b.f,
                            1 => &mut b.g,
                            _ => &mut b.q,
                        };
                        if (*cur - x).abs() > 1e-4 {
                            *cur = x;
                            let pb = *pb;
                            if let Some(p) = s.proc.as_mut() {
                                p.set_params(&pb);
                            }
                        }
                    }
                }
            }
            TargetRt::Mix { fx } => {
                if let Some(s) = slot(g, fx) {
                    s.mix = v;
                }
            }
            TargetRt::InsVol(i) => {
                if let Some(ins) = g.inserts.get_mut(i) {
                    ins.gain = crate::model::fader_gain(v);
                }
            }
            TargetRt::InsPan(i) => {
                if let Some(ins) = g.inserts.get_mut(i) {
                    ins.pan = v * 2.0 - 1.0;
                }
            }
            TargetRt::TrackVol(i) => {
                if let Some(t) = g.tracks.get_mut(i) {
                    t.gain = crate::model::fader_gain(v);
                }
            }
        }
    }
}
