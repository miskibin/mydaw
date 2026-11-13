//! Audio the project plays: decoded once, shared by `Arc` with the audio thread.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::content::ContentFeatures;
use crate::pitch::{F0Estimator, PitchAnalysis};
use crate::reference::{Alignment, PitchTarget};

/// Interleaved stereo at the file's own rate, padded for cubic interpolation
/// (`wave_audio_file::PAD_LEAD` frames before, `PAD_TRAIL` after).
pub struct Audio {
    pub data: Vec<f32>,
    pub frames: usize,
    pub sr: u32,
    pub channels: u16,
    /// [min,max] pairs, 100 per second, mono (max of both channels).
    pub peaks: Vec<f32>,
}

pub const PEAK_RATE: f64 = 100.0;

impl Audio {
    pub fn from_stereo(interleaved: Vec<f32>, sr: u32, channels: u16) -> Self {
        let frames = interleaved.len() / 2;
        let peaks = peaks_of(&interleaved, sr);
        let mut data = Vec::with_capacity((frames + wave_audio_file::PAD_LEAD + wave_audio_file::PAD_TRAIL) * 2);
        data.extend(std::iter::repeat(0.0).take(wave_audio_file::PAD_LEAD * 2));
        data.extend_from_slice(&interleaved);
        data.extend(std::iter::repeat(0.0).take(wave_audio_file::PAD_TRAIL * 2));
        Self { data, frames, sr, channels, peaks }
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        // cap at 30 minutes so a stray podcast cannot eat the machine
        let d = wave_audio_file::decode_stereo(path, |sr| Some((sr * 1800.0) as usize)).map_err(|e| e.to_string())?;
        let sr = d.sample_rate.round() as u32;
        let mono = d.data.chunks_exact(2).all(|f| f[0] == f[1]);
        Ok(Self::from_stereo(d.data, sr, if mono { 1 } else { 2 }))
    }

    /// Loads a project source by its stored path: a `lib:` path of the sample library (the
    /// standard library is decoded from the program itself, so it is never missing), an
    /// absolute path, or one relative to `base` (the project's folder).
    pub fn load_path(path: &str, base: &Path) -> Result<Self, String> {
        if let Some(f) = crate::factory::embedded(path) {
            let d = wave_audio_file::decode_stereo_bytes(f.bytes, Path::new(f.file), |_| None).map_err(|e| e.to_string())?;
            let sr = d.sample_rate.round() as u32;
            return Ok(Self::from_stereo(d.data, sr, f.ch));
        }
        if path.starts_with(crate::factory::SCHEME) {
            let abs = crate::factory::resolve(path).ok_or_else(|| format!("{path}: not a library path"))?;
            return Self::load(&abs).map_err(|e| format!("{e} (a sample of a library kit that is not installed on this computer)"));
        }
        let p = Path::new(path);
        if p.is_absolute() {
            Self::load(p)
        } else {
            Self::load(&base.join(p))
        }
    }

    pub fn seconds(&self) -> f64 {
        self.frames as f64 / self.sr as f64
    }

    /// Unpadded interleaved content.
    pub fn content(&self) -> &[f32] {
        let a = wave_audio_file::PAD_LEAD * 2;
        &self.data[a..a + self.frames * 2]
    }

    /// Stereo frame `pos` (fractional, in the file's frames), cubic-interpolated. RT-safe.
    #[inline]
    pub fn sample(&self, pos: f64) -> (f32, f32) {
        if pos < 0.0 || pos >= self.frames as f64 {
            return (0.0, 0.0);
        }
        let i = pos as usize;
        let x = (pos - i as f64) as f32;
        let idx = i;
        (
            wave_audio_file::cubic(wave_audio_file::taps(&self.data, idx, 0), x),
            wave_audio_file::cubic(wave_audio_file::taps(&self.data, idx, 1), x),
        )
    }

    pub fn peak(&self) -> f32 {
        self.content().iter().fold(0f32, |a, b| a.max(b.abs()))
    }

    /// Normalised (to −0.3 dBFS) and/or reversed copy.
    pub fn derived(&self, norm: bool, rev: bool) -> Audio {
        let mut c = self.content().to_vec();
        if rev {
            let frames: Vec<[f32; 2]> = c.chunks_exact(2).rev().map(|f| [f[0], f[1]]).collect();
            c = frames.into_iter().flatten().collect();
        }
        if norm {
            let pk = self.peak();
            if pk > 1e-6 {
                let g = 10f32.powf(-0.3 / 20.0) / pk;
                c.iter_mut().for_each(|x| *x *= g);
            }
        }
        Audio::from_stereo(c, self.sr, self.channels)
    }
}

fn peaks_of(interleaved: &[f32], sr: u32) -> Vec<f32> {
    let per = ((sr as f64 / PEAK_RATE) as usize).max(1);
    let frames = interleaved.len() / 2;
    let bins = frames.div_ceil(per);
    let mut out = Vec::with_capacity(bins * 2);
    for b in 0..bins {
        let (mut mn, mut mx) = (0f32, 0f32);
        for f in b * per..((b + 1) * per).min(frames) {
            for c in 0..2 {
                let v = interleaved[f * 2 + c];
                mn = mn.min(v);
                mx = mx.max(v);
            }
        }
        out.push(mn);
        out.push(mx);
    }
    out
}

type Variant = (u64, bool, bool);
/// (take variant, reference variant, first and last analysis frame of the take's region)
pub type AlignKey = (Variant, Variant, usize, usize);
/// Alignments kept (each is a few floats per 5 ms of the take).
const MAX_ALIGNMENTS: usize = 32;

/// Every loaded source, plus normalised/reversed variants built on demand.
#[derive(Default)]
pub struct SourceStore {
    pub audio: HashMap<u64, Arc<Audio>>,
    derived: HashMap<Variant, Arc<Audio>>,
    analysis: HashMap<Variant, Arc<PitchAnalysis>>,
    content: HashMap<Variant, Arc<ContentFeatures>>,
    aligned: HashMap<AlignKey, (u64, Arc<Alignment>)>,
    align_tick: u64,
    music: HashMap<Variant, Arc<crate::music::SourceMusic>>,
    /// clip id → (hash of its edits, rendered audio)
    tuned: HashMap<u64, (u64, Arc<Audio>)>,
    /// Pitch-preserving time-stretches of clips' audio.
    stretch: Stretcher,
    /// Pitch detector for analysis (`None` = built-in YIN).
    estimator: Option<Arc<dyn F0Estimator>>,
}

/// (source id, identity of the audio before stretching, render ratio bits, `Clip::pitch` bits)
pub type StretchKey = (u64, u64, u64, u32);
type StretchDone = (StretchKey, Result<Arc<Audio>, String>);
/// Stretched audio kept, in bytes (least recently played dropped first).
const STRETCH_CACHE_BYTES: usize = 1 << 30;

/// A time-stretch and/or transposition to render.
pub struct StretchJob {
    pub key: StretchKey,
    pub audio: Arc<Audio>,
    /// Output length / input length.
    pub ratio: f64,
    /// Transposition, semitones.
    pub semis: f32,
    /// The rate the clip reads the render at (`Clip::rendered_rate`), and the one it reads
    /// `audio` at when there is no render (resampled, at its own pitch).
    pub rate: f64,
    pub fallback_rate: f64,
}

impl StretchJob {
    fn of(c: &crate::model::Clip, ident: u64, audio: Arc<Audio>) -> Self {
        StretchJob {
            key: stretch_key(c, ident),
            audio,
            ratio: c.render_ratio(),
            semis: c.pitch,
            rate: c.rendered_rate(),
            fallback_rate: 1.0 / c.stretch,
        }
    }
}

fn stretch_key(c: &crate::model::Clip, ident: u64) -> StretchKey {
    (c.source, ident, c.render_ratio().to_bits(), c.pitch.to_bits())
}

/// Renders a stretch job (slow: run it off the engine lock).
pub fn render_stretch(j: &StretchJob) -> Result<Arc<Audio>, String> {
    let out = wave_stretch::stretch_shift_stereo(j.audio.content(), j.audio.sr, j.ratio, j.semis)?;
    Ok(Arc::new(Audio::from_stereo(out, j.audio.sr, j.audio.channels)))
}

/// Cache of rendered time-stretches and the background thread that renders them.
#[derive(Default)]
struct Stretcher {
    cache: HashMap<StretchKey, (u64, Arc<Audio>)>,
    bytes: usize,
    tick: u64,
    pending: std::collections::HashSet<StretchKey>,
    failed: HashMap<StretchKey, String>,
    /// clip id → the stretch it last asked for and did not have
    waiting: HashMap<u64, StretchKey>,
    jobs: Option<std::sync::mpsc::Sender<StretchJob>>,
    done: Option<std::sync::mpsc::Receiver<StretchDone>>,
}

impl Stretcher {
    fn get(&mut self, k: &StretchKey) -> Option<Arc<Audio>> {
        self.tick += 1;
        let t = self.tick;
        self.cache.get_mut(k).map(|e| {
            e.0 = t;
            e.1.clone()
        })
    }

    fn put(&mut self, k: StretchKey, a: Arc<Audio>) {
        self.tick += 1;
        self.bytes += a.data.len() * 4;
        if let Some((_, old)) = self.cache.insert(k, (self.tick, a)) {
            self.bytes -= old.data.len() * 4;
        }
        // keep the newest entry even if it alone is over the budget
        while self.bytes > STRETCH_CACHE_BYTES && self.cache.len() > 1 {
            let oldest = *self.cache.iter().min_by_key(|e| e.1 .0).map(|e| e.0).unwrap();
            if let Some((_, a)) = self.cache.remove(&oldest) {
                self.bytes -= a.data.len() * 4;
            }
        }
    }

    fn forget_source(&mut self, id: u64) {
        let gone: Vec<StretchKey> = self.cache.keys().filter(|k| k.0 == id).copied().collect();
        for k in gone {
            if let Some((_, a)) = self.cache.remove(&k) {
                self.bytes -= a.data.len() * 4;
            }
        }
        self.failed.retain(|k, _| k.0 != id);
    }

    /// Queues a render on the background thread (started on first use).
    fn submit(&mut self, job: StretchJob) {
        if self.jobs.is_none() {
            let (jtx, jrx) = std::sync::mpsc::channel::<StretchJob>();
            let (dtx, drx) = std::sync::mpsc::channel::<StretchDone>();
            let spawned = std::thread::Builder::new().name("wave-stretch".into()).spawn(move || {
                // ends when the store (holding the job sender) is dropped
                while let Ok(j) = jrx.recv() {
                    let r = render_stretch(&j);
                    if dtx.send((j.key, r)).is_err() {
                        break;
                    }
                }
            });
            if let Err(e) = spawned {
                self.failed.insert(job.key, format!("cannot start the stretch renderer: {e}"));
                return;
            }
            self.jobs = Some(jtx);
            self.done = Some(drx);
        }
        let key = job.key;
        if self.jobs.as_ref().is_some_and(|tx| tx.send(job).is_ok()) {
            self.pending.insert(key);
        } else {
            self.failed.insert(key, "the stretch renderer stopped".into());
        }
    }
}

/// Mono mix of a variant.
pub fn mono_of(audio: &Audio) -> Vec<f32> {
    audio.content().chunks_exact(2).map(|f| (f[0] + f[1]) * 0.5).collect()
}

/// The part of the audio [`SourceStore::clip_audio`] returned that clip `c` plays, in
/// seconds of that audio (read at `rate`), clamped to its `seconds`.
pub fn clip_region(c: &crate::model::Clip, bpm: f64, rate: f64, seconds: f64) -> (f64, f64) {
    let spb = 60.0 / bpm.max(1.0);
    let a = (c.offset * spb * rate).clamp(0.0, seconds);
    (a, ((c.offset + c.len) * spb * rate).clamp(a, seconds))
}

impl SourceStore {
    pub fn insert(&mut self, id: u64, a: Arc<Audio>) {
        self.derived.retain(|k, _| k.0 != id);
        self.analysis.retain(|k, _| k.0 != id);
        self.content.retain(|k, _| k.0 != id);
        self.aligned.retain(|k, _| k.0 .0 != id && k.1 .0 != id);
        self.music.retain(|k, _| k.0 != id);
        self.stretch.forget_source(id);
        self.audio.insert(id, a);
    }

    /// Switches the pitch detector; every analysis (and what depends on it) is redone.
    pub fn set_estimator(&mut self, e: Option<Arc<dyn F0Estimator>>) {
        self.estimator = e;
        self.analysis.clear();
        self.aligned.clear();
        self.tuned.clear();
    }
    pub fn estimator(&self) -> Option<Arc<dyn F0Estimator>> {
        self.estimator.clone()
    }
    pub fn backend(&self) -> &'static str {
        self.estimator.as_ref().map(|e| e.name()).unwrap_or("yin")
    }
    pub fn get(&self, id: u64) -> Option<&Arc<Audio>> {
        self.audio.get(&id)
    }
    pub fn variant(&mut self, id: u64, norm: bool, rev: bool) -> Option<Arc<Audio>> {
        let base = self.audio.get(&id)?.clone();
        if !norm && !rev {
            return Some(base);
        }
        Some(self.derived.entry((id, norm, rev)).or_insert_with(|| Arc::new(base.derived(norm, rev))).clone())
    }
}

impl SourceStore {
    /// Pitch analysis of a source variant (cached).
    pub fn analysis(&mut self, id: u64, norm: bool, rev: bool) -> Option<Arc<PitchAnalysis>> {
        if let Some(a) = self.analysis.get(&(id, norm, rev)) {
            return Some(a.clone());
        }
        let audio = self.variant(id, norm, rev)?;
        let a = Arc::new(crate::pitch::analyze_with(&mono_of(&audio), audio.sr as f32, self.estimator.as_deref()));
        self.analysis.insert((id, norm, rev), a.clone());
        Some(a)
    }
    pub fn cached_analysis(&self, id: u64, norm: bool, rev: bool) -> Option<Arc<PitchAnalysis>> {
        self.analysis.get(&(id, norm, rev)).cloned()
    }
    /// Stores an analysis made outside the store, unless the detector changed meanwhile.
    pub fn put_analysis(&mut self, id: u64, norm: bool, rev: bool, a: Arc<PitchAnalysis>) {
        if a.backend == self.backend() || (a.backend == "yin" && self.estimator.is_none()) {
            self.analysis.insert((id, norm, rev), a);
        }
    }

    /// Content features (MFCC) of a source variant (cached).
    pub fn content(&mut self, id: u64, norm: bool, rev: bool) -> Option<Arc<ContentFeatures>> {
        if let Some(c) = self.content.get(&(id, norm, rev)) {
            return Some(c.clone());
        }
        let audio = self.variant(id, norm, rev)?;
        let c = Arc::new(crate::content::mfcc(&mono_of(&audio), audio.sr as f32));
        self.content.insert((id, norm, rev), c.clone());
        Some(c)
    }
    pub fn cached_content(&self, id: u64, norm: bool, rev: bool) -> Option<Arc<ContentFeatures>> {
        self.content.get(&(id, norm, rev)).cloned()
    }
    pub fn put_content(&mut self, id: u64, norm: bool, rev: bool, c: Arc<ContentFeatures>) {
        self.content.insert((id, norm, rev), c);
    }

    pub fn cached_alignment(&mut self, k: &AlignKey) -> Option<Arc<Alignment>> {
        self.align_tick += 1;
        let t = self.align_tick;
        self.aligned.get_mut(k).map(|e| {
            e.0 = t;
            e.1.clone()
        })
    }
    /// Keeps at most `MAX_ALIGNMENTS`, dropping the least recently used.
    pub fn put_alignment(&mut self, k: AlignKey, a: Arc<Alignment>) {
        self.align_tick += 1;
        self.aligned.insert(k, (self.align_tick, a));
        while self.aligned.len() > MAX_ALIGNMENTS {
            let oldest = *self.aligned.iter().min_by_key(|e| e.1 .0).map(|e| e.0).unwrap();
            self.aligned.remove(&oldest);
        }
    }

    /// Alignment key of a clip's region against its reference, if it has one.
    pub fn align_key(&mut self, c: &crate::model::Clip, bpm: f64, reference: u64) -> Option<AlignKey> {
        let audio = self.variant(c.source, c.norm, c.rev)?;
        let (a, b) = c.source_span(bpm);
        let (a, b) = (a.clamp(0.0, audio.seconds()), b.clamp(0.0, audio.seconds()));
        let (a, b) = (a, b.max(a));
        let r = crate::pitch::ANALYSIS_RATE as f64;
        Some(((c.source, c.norm, c.rev), (reference, false, false), (a * r).round() as usize, (b * r).round() as usize))
    }

    /// Tune to Reference target of a clip with `params` (computed here if not cached).
    pub fn pitch_target(&mut self, c: &crate::model::Clip, bpm: f64, params: &crate::model::RefTune) -> Option<Arc<PitchTarget>> {
        let key = self.align_key(c, bpm, params.source)?;
        let take = self.analysis(c.source, c.norm, c.rev)?;
        if let Some(m) = &params.melody {
            // the timeline places the melody: nothing to align, nothing worth caching
            let r = crate::pitch::ANALYSIS_RATE as f64;
            let al = crate::melody::alignment(&take, (key.2 as f64 / r, key.3 as f64 / r), &note_spans(c, &take), &m.notes);
            return Some(Arc::new(crate::reference::target(&take, &note_spans(c, &take), &al, params)));
        }
        let al = match self.cached_alignment(&key) {
            Some(a) => a,
            None => {
                let reference = self.analysis(params.source, false, false)?;
                let (tc, rc) = (self.content(c.source, c.norm, c.rev)?, self.content(params.source, false, false)?);
                let r = crate::pitch::ANALYSIS_RATE as f64;
                let al = Arc::new(crate::reference::align(
                    &crate::reference::Performance { pitch: &take, content: &tc },
                    (key.2 as f64 / r, key.3 as f64 / r),
                    &crate::reference::Performance { pitch: &reference, content: &rc },
                ));
                self.put_alignment(key, al.clone());
                al
            }
        };
        Some(Arc::new(target_for(c, &take, &al, params)))
    }

    pub fn cached_music(&self, id: u64, norm: bool, rev: bool) -> Option<Arc<crate::music::SourceMusic>> {
        self.music.get(&(id, norm, rev)).cloned()
    }

    pub fn put_music(&mut self, id: u64, norm: bool, rev: bool, m: Arc<crate::music::SourceMusic>) {
        self.music.insert((id, norm, rev), m);
    }

    /// What a clip plays before stretching: source variant plus its pitch edits, in source
    /// time. While `allow_stale` (a drag is in progress) a previous render is reused rather
    /// than re-rendering the whole clip on every mouse move.
    pub fn clip_source_audio(&mut self, c: &crate::model::Clip, allow_stale: bool, bpm: f64) -> Option<Arc<Audio>> {
        self.pre_stretch(c, allow_stale, bpm).map(|x| x.0)
    }

    /// [`Self::clip_source_audio`] with an identity of that audio (for the stretch cache).
    fn pre_stretch(&mut self, c: &crate::model::Clip, allow_stale: bool, bpm: f64) -> Option<(Arc<Audio>, u64)> {
        use std::hash::{Hash, Hasher};
        let base = self.variant(c.source, c.norm, c.rev)?;
        let Some(_) = c.tune.as_ref().filter(|t| t.is_active()) else {
            return Some((base, (c.norm as u64) | (c.rev as u64) << 1));
        };
        let a = self.tuned_audio(c, base, allow_stale, bpm)?;
        // the hash of the edits the returned render was made with (a stale one in a drag)
        let edits = self.tuned.get(&c.id).map(|t| t.0).unwrap_or_default();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (c.id, edits).hash(&mut h);
        Some((a, h.finish() | 1 << 63))
    }

    /// What a clip plays, in clip time, and the rate to read it at (source seconds per clip
    /// second: 1 unless the clip is resampled, or its time-stretch is not rendered yet).
    /// Never blocks: a missing time-stretch is queued for the background renderer (unless a
    /// drag is in progress) and the clip plays resampled meanwhile.
    pub fn clip_play(&mut self, c: &crate::model::Clip, allow_stale: bool, bpm: f64) -> Option<(Arc<Audio>, f64)> {
        let (pre, ident) = self.pre_stretch(c, allow_stale, bpm)?;
        if c.kind != crate::model::ClipKind::Audio {
            self.stretch.waiting.remove(&c.id);
            return Some((pre, 1.0));
        }
        let rate = 1.0 / c.stretch;
        if !c.needs_stretch() {
            self.stretch.waiting.remove(&c.id);
            return Some((pre, rate));
        }
        let key = stretch_key(c, ident);
        if let Some(a) = self.stretch.get(&key) {
            self.stretch.waiting.remove(&c.id);
            return Some((a, c.rendered_rate()));
        }
        self.stretch.waiting.insert(c.id, key);
        if !allow_stale && !self.stretch.pending.contains(&key) && !self.stretch.failed.contains_key(&key) {
            self.stretch.submit(StretchJob::of(c, ident, pre.clone()));
        }
        Some((pre, rate))
    }

    /// [`Self::clip_play`] that renders a missing time-stretch now (offline renders,
    /// analysis). Slow for long audio. Falls back to resampling if the stretch fails.
    pub fn clip_audio(&mut self, c: &crate::model::Clip, allow_stale: bool, bpm: f64) -> Option<(Arc<Audio>, f64)> {
        match self.stretch_request(c, allow_stale, bpm)? {
            Ok(x) => Some(x),
            Err(job) => {
                let r = render_stretch(&job);
                let ok = r.is_ok();
                self.put_stretch(job.key, r);
                if ok {
                    self.clip_play(c, allow_stale, bpm)
                } else {
                    Some((job.audio, job.fallback_rate))
                }
            }
        }
    }

    /// What [`Self::clip_audio`] returns if it is ready, else the stretch to render (outside
    /// any lock) and hand back through [`Self::put_stretch`].
    pub fn stretch_request(
        &mut self,
        c: &crate::model::Clip,
        allow_stale: bool,
        bpm: f64,
    ) -> Option<Result<(Arc<Audio>, f64), StretchJob>> {
        let (pre, ident) = self.pre_stretch(c, allow_stale, bpm)?;
        if !c.needs_stretch() {
            return Some(Ok((pre, if c.kind == crate::model::ClipKind::Audio { 1.0 / c.stretch } else { 1.0 })));
        }
        let key = stretch_key(c, ident);
        if let Some(a) = self.stretch.get(&key) {
            return Some(Ok((a, c.rendered_rate())));
        }
        if self.stretch.failed.contains_key(&key) {
            return Some(Ok((pre, 1.0 / c.stretch)));
        }
        Some(Err(StretchJob::of(c, ident, pre)))
    }

    /// Stores a finished (or failed) stretch render.
    pub fn put_stretch(&mut self, key: StretchKey, r: Result<Arc<Audio>, String>) {
        self.stretch.pending.remove(&key);
        match r {
            Ok(a) => self.stretch.put(key, a),
            Err(e) => {
                log::warn!("time-stretch failed: {e}");
                self.stretch.failed.insert(key, e);
            }
        }
    }

    /// Collects stretches the background renderer finished. True if any did (the timeline
    /// should be rebuilt to play them).
    pub fn poll_stretch(&mut self) -> bool {
        let done: Vec<StretchDone> = match self.stretch.done.as_ref() {
            Some(rx) => rx.try_iter().collect(),
            None => return false,
        };
        let any = !done.is_empty();
        for (k, r) in done {
            self.put_stretch(k, r);
        }
        any
    }

    /// Clips whose time-stretch is being rendered (they play resampled meanwhile), and
    /// clips whose time-stretch failed (they play resampled) with the reason.
    pub fn stretch_status(&self, clips: &[crate::model::Clip]) -> (Vec<u64>, HashMap<u64, String>) {
        let mut busy = vec![];
        let mut failed = HashMap::new();
        for c in clips.iter().filter(|c| c.needs_stretch()) {
            let Some(k) = self.stretch.waiting.get(&c.id) else { continue };
            if k.2 != c.render_ratio().to_bits() || k.3 != c.pitch.to_bits() || k.0 != c.source {
                continue;
            }
            if let Some(e) = self.stretch.failed.get(k) {
                failed.insert(c.id, e.clone());
            } else if self.stretch.pending.contains(k) {
                busy.push(c.id);
            }
        }
        (busy, failed)
    }

    /// Source variant plus pitch edits (cached per clip).
    fn tuned_audio(&mut self, c: &crate::model::Clip, base: Arc<Audio>, allow_stale: bool, bpm: f64) -> Option<Arc<Audio>> {
        let tune = c.tune.as_ref()?;
        let region = tune.reference.as_ref().and_then(|r| self.align_key(c, bpm, r.source)).map(|k| (k.2, k.3));
        let hash = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (c.source, c.norm, c.rev, region).hash(&mut h);
            serde_json::to_string(tune).unwrap_or_default().hash(&mut h);
            h.finish()
        };
        if let Some((hh, a)) = self.tuned.get(&c.id) {
            if *hh == hash || allow_stale {
                return Some(a.clone());
            }
        }
        let an = self.analysis(c.source, c.norm, c.rev)?;
        let target = tune.reference.as_ref().and_then(|r| self.pitch_target(c, bpm, r));
        let out = Arc::new(crate::tune_render::render(&base, &an, tune, target.as_deref()));
        self.tuned.insert(c.id, (hash, out.clone()));
        Some(out)
    }
}

/// The target for clip `c` from a ready alignment. The note spans are the editor's
/// segmentation when the clip has one, else the analysis'.
pub fn target_for(c: &crate::model::Clip, take: &PitchAnalysis, al: &Alignment, params: &crate::model::RefTune) -> PitchTarget {
    crate::reference::target(take, &note_spans(c, take), al, params)
}

/// The take's notes (seconds of the source): the editor's segmentation, else the analysis'.
pub fn note_spans(c: &crate::model::Clip, take: &PitchAnalysis) -> Vec<(f64, f64)> {
    match c.tune.as_ref().filter(|t| !t.notes.is_empty()) {
        Some(t) => t.notes.iter().map(|n| (n.start, n.end)).collect(),
        None => take.notes.iter().map(|n| (n.start, n.end)).collect(),
    }
}

/// Writes interleaved stereo float to a 32-bit float WAV.
pub fn write_wav(path: &Path, interleaved: &[f32], sr: u32) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let spec = hound::WavSpec { channels: 2, sample_rate: sr, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut w = hound::WavWriter::create(path, spec).map_err(|e| e.to_string())?;
    for &x in interleaved {
        w.write_sample(x).map_err(|e| e.to_string())?;
    }
    w.finalize().map_err(|e| e.to_string())
}

/// Float stereo → 16-bit with TPDF dither (±1 LSB triangular) and hard clip protection.
pub fn to_i16_dithered(interleaved: &[f32]) -> Vec<i16> {
    let mut state: u32 = 0x9E37_79B9;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state as f32 / u32::MAX as f32
    };
    interleaved
        .iter()
        .map(|&x| {
            let x = if x.is_finite() { x.clamp(-1.0, 1.0) } else { 0.0 };
            let d = next() - next();
            (x * 32767.0 + d).round().clamp(-32768.0, 32767.0) as i16
        })
        .collect()
}

/// Encodes interleaved stereo float to a CBR MP3 (LAME) with an optional ID3v2 title.
/// `kbps` must be one of 128, 192, 256, 320; `sr` 44100 or 48000.
pub fn write_mp3(path: &Path, interleaved: &[f32], sr: u32, kbps: u32, title: Option<&str>) -> Result<(), String> {
    use mp3lame_encoder::{Bitrate, Builder, FlushNoGap, Id3Tag, InterleavedPcm, Mode, Quality};
    let brate = match kbps {
        128 => Bitrate::Kbps128,
        192 => Bitrate::Kbps192,
        256 => Bitrate::Kbps256,
        320 => Bitrate::Kbps320,
        _ => return Err("mp3 bitrate must be 128, 192, 256 or 320 kbps".into()),
    };
    if !(sr == 44_100 || sr == 48_000) {
        return Err("mp3 sample rate must be 44100 or 48000".into());
    }
    let mut b = Builder::new().ok_or("mp3 encoder unavailable")?;
    b.set_num_channels(2).map_err(|e| format!("mp3: {e:?}"))?;
    b.set_sample_rate(sr).map_err(|e| format!("mp3: {e:?}"))?;
    b.set_brate(brate).map_err(|e| format!("mp3: {e:?}"))?;
    b.set_mode(Mode::JointStereo).map_err(|e| format!("mp3: {e:?}"))?;
    b.set_quality(Quality::Best).map_err(|e| format!("mp3: {e:?}"))?;
    if let Some(t) = title.filter(|t| !t.is_empty()) {
        let t = t.as_bytes();
        b.set_id3_tag(Id3Tag { title: t, artist: b"", album: b"", album_art: b"", year: b"", comment: b"" })
            .map_err(|e| format!("mp3: {e:?}"))?;
    }
    let mut enc = b.build().map_err(|e| format!("mp3: {e:?}"))?;
    let pcm = to_i16_dithered(interleaved);
    let mut out: Vec<u8> = Vec::with_capacity(pcm.len() / 2 + 16_384);
    // The ID3v2 tag has to lead the stream; LAME emits it with the first encode call.
    for chunk in pcm.chunks(2 * 8192) {
        out.reserve(chunk.len() * 5 / 8 + 7200);
        enc.encode_to_vec(InterleavedPcm(chunk), &mut out).map_err(|e| format!("mp3: {e:?}"))?;
    }
    out.reserve(7200);
    enc.flush_to_vec::<FlushNoGap>(&mut out).map_err(|e| format!("mp3: {e:?}"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, out).map_err(|e| e.to_string())
}
