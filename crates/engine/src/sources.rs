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
    /// Pitch detector for analysis (`None` = built-in YIN).
    estimator: Option<Arc<dyn F0Estimator>>,
}

/// Mono mix of a variant.
pub fn mono_of(audio: &Audio) -> Vec<f32> {
    audio.content().chunks_exact(2).map(|f| (f[0] + f[1]) * 0.5).collect()
}

/// The part of its source a clip plays, in seconds.
pub fn clip_region(c: &crate::model::Clip, bpm: f64, source_seconds: f64) -> (f64, f64) {
    let spb = 60.0 / bpm.max(1.0);
    let a = (c.offset * spb).clamp(0.0, source_seconds);
    (a, ((c.offset + c.len) * spb).clamp(a, source_seconds))
}

impl SourceStore {
    pub fn insert(&mut self, id: u64, a: Arc<Audio>) {
        self.derived.retain(|k, _| k.0 != id);
        self.analysis.retain(|k, _| k.0 != id);
        self.content.retain(|k, _| k.0 != id);
        self.aligned.retain(|k, _| k.0 .0 != id && k.1 .0 != id);
        self.music.retain(|k, _| k.0 != id);
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
        let (a, b) = clip_region(c, bpm, audio.seconds());
        let r = crate::pitch::ANALYSIS_RATE as f64;
        Some(((c.source, c.norm, c.rev), (reference, false, false), (a * r).round() as usize, (b * r).round() as usize))
    }

    /// Tune to Reference target of a clip with `params` (computed here if not cached).
    pub fn pitch_target(&mut self, c: &crate::model::Clip, bpm: f64, params: &crate::model::RefTune) -> Option<Arc<PitchTarget>> {
        let key = self.align_key(c, bpm, params.source)?;
        let take = self.analysis(c.source, c.norm, c.rev)?;
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

    /// The audio a clip plays: source variant plus its pitch edits. While
    /// `allow_stale` (a drag is in progress) a previous render is reused rather
    /// than re-rendering the whole clip on every mouse move.
    pub fn clip_audio(&mut self, c: &crate::model::Clip, allow_stale: bool, bpm: f64) -> Option<Arc<Audio>> {
        let base = self.variant(c.source, c.norm, c.rev)?;
        let Some(tune) = c.tune.as_ref().filter(|t| t.is_active()) else { return Some(base) };
        let region = tune.reference.and_then(|r| self.align_key(c, bpm, r.source)).map(|k| (k.2, k.3));
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
        let target = tune.reference.and_then(|r| self.pitch_target(c, bpm, &r));
        let out = Arc::new(crate::tune_render::render(&base, &an, tune, target.as_deref()));
        self.tuned.insert(c.id, (hash, out.clone()));
        Some(out)
    }
}

/// The target for clip `c` from a ready alignment. The note spans are the editor's
/// segmentation when the clip has one, else the analysis'.
pub fn target_for(c: &crate::model::Clip, take: &PitchAnalysis, al: &Alignment, params: &crate::model::RefTune) -> PitchTarget {
    let spans: Vec<(f64, f64)> = match c.tune.as_ref().filter(|t| !t.notes.is_empty()) {
        Some(t) => t.notes.iter().map(|n| (n.start, n.end)).collect(),
        None => take.notes.iter().map(|n| (n.start, n.end)).collect(),
    };
    crate::reference::target(take, &spans, al, params)
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
