//! Audio the project plays: decoded once, shared by `Arc` with the audio thread.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

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

/// Every loaded source, plus normalised/reversed variants built on demand.
#[derive(Default)]
pub struct SourceStore {
    pub audio: HashMap<u64, Arc<Audio>>,
    derived: HashMap<(u64, bool, bool), Arc<Audio>>,
}

impl SourceStore {
    pub fn insert(&mut self, id: u64, a: Arc<Audio>) {
        self.derived.retain(|k, _| k.0 != id);
        self.audio.insert(id, a);
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
