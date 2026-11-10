//! WAVE Keys: the sampled grand piano behind Channel Rack channels of kind `keys`.
//!
//! A [`KeysBank`] is a set of recordings of single notes (every minor third, a few
//! dynamics each). A note plays the recording nearest to its key, resampled by at most
//! two semitones, from the dynamic layer its velocity falls in. Banks are loaded off the
//! audio thread and shared as an `Arc`; voices live in [`crate::synth::VoicePool`].

use std::path::Path;
use std::sync::Arc;

use crate::sources::Audio;

/// Folder name of the piano in the sample library and in the app's bundled resources.
pub const KIT_ID: &str = "salamander-piano";
/// Recordings are cut to this length (with a fade): the longest a note can ring.
pub const MAX_SECONDS: f64 = 9.0;
const FADE_SECONDS: f64 = 1.5;

pub const RELEASE_MS: (f32, f32, f32) = (40.0, 3000.0, 350.0);
pub const DYNAMICS: (f32, f32, f32) = (0.0, 100.0, 100.0);

struct Zone {
    root: u8,
    /// Index of the zone's first layer in `audio`; layers are ordered soft to loud.
    first: u16,
    layers: u16,
}

pub struct KeysBank {
    zones: Vec<Zone>,
    audio: Vec<Arc<Audio>>,
}

/// `C4` → 60, `D#1` → 27. `None` when `s` is not a note name.
pub fn parse_note(s: &str) -> Option<u8> {
    let mut it = s.chars();
    let base = match it.next()?.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let rest = it.as_str();
    let (acc, oct) = match rest.strip_prefix('#') {
        Some(r) => (1, r),
        None => (0, rest),
    };
    let oct: i32 = oct.parse().ok()?;
    u8::try_from((oct + 1) * 12 + base + acc).ok().filter(|k| *k <= 127)
}

/// `F#3v9.flac` → (key 54, layer 9).
fn parse_file(name: &str) -> Option<(u8, u32)> {
    let stem = name.rsplit_once('.')?.0;
    let (note, layer) = stem.rsplit_once('v')?;
    Some((parse_note(note)?, layer.parse().ok()?))
}

/// The recording, cut to [`MAX_SECONDS`] with a fade so the cut is not heard.
fn trimmed(a: Audio) -> Audio {
    let max = (MAX_SECONDS * a.sr as f64) as usize;
    if a.frames <= max {
        return a;
    }
    let mut data = a.content()[..max * 2].to_vec();
    let fade = ((FADE_SECONDS * a.sr as f64) as usize).min(max);
    for i in 0..fade {
        let g = 1.0 - (i as f32 + 1.0) / fade as f32;
        let f = max - fade + i;
        data[f * 2] *= g * g;
        data[f * 2 + 1] *= g * g;
    }
    Audio::from_stereo(data, a.sr, a.channels)
}

impl KeysBank {
    /// Builds a bank from `(root key, layers soft → loud)`.
    pub fn from_layers(mut notes: Vec<(u8, Vec<Arc<Audio>>)>) -> Result<Self, String> {
        notes.retain(|(_, l)| !l.is_empty());
        if notes.is_empty() {
            return Err("the piano has no samples".into());
        }
        notes.sort_by_key(|(k, _)| *k);
        let mut zones = vec![];
        let mut audio = vec![];
        for (root, layers) in notes {
            zones.push(Zone { root, first: audio.len() as u16, layers: layers.len() as u16 });
            audio.extend(layers);
        }
        Ok(Self { zones, audio })
    }

    /// True when `dir` holds every file a bank needs (at least one note; no half-written files).
    pub fn present(dir: &Path) -> bool {
        std::fs::read_dir(dir).map(|rd| rd.flatten().filter(|e| parse_file(&e.file_name().to_string_lossy()).is_some()).count() >= 30).unwrap_or(false)
    }

    /// Loads every `<note>v<layer>.<ext>` recording in `dir` (decoded on a few threads).
    pub fn load(dir: &Path) -> Result<Self, String> {
        let mut files: Vec<(u8, u32, std::path::PathBuf)> = std::fs::read_dir(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .flatten()
            .filter_map(|e| parse_file(&e.file_name().to_string_lossy()).map(|(k, l)| (k, l, e.path())))
            .collect();
        files.sort();
        if files.is_empty() {
            return Err(format!("{}: no piano samples", dir.display()));
        }
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(1, 6);
        let next = std::sync::atomic::AtomicUsize::new(0);
        let out: Vec<std::sync::Mutex<Option<Result<Audio, String>>>> = files.iter().map(|_| std::sync::Mutex::new(None)).collect();
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some((_, _, path)) = files.get(i) else { break };
                    *out[i].lock().unwrap() = Some(Audio::load(path).map(trimmed));
                });
            }
        });
        let mut notes: Vec<(u8, Vec<Arc<Audio>>)> = vec![];
        for ((key, _, path), slot) in files.iter().zip(out) {
            let a = slot.into_inner().unwrap().unwrap_or_else(|| Err("not decoded".into())).map_err(|e| format!("{}: {e}", path.display()))?;
            match notes.last_mut() {
                Some((k, l)) if k == key => l.push(Arc::new(a)),
                _ => notes.push((*key, vec![Arc::new(a)])),
            }
        }
        Self::from_layers(notes)
    }

    /// The recording a note plays: its index (for [`Self::audio`]), the pitch shift in
    /// semitones, and the gain that carries the velocity inside the layer.
    /// `dynamics` 0..1: how much the velocity changes the level (the timbre always follows it).
    pub fn pick(&self, key: u8, vel: f32, dynamics: f32) -> (u16, f32, f32) {
        let z = self.zones.iter().min_by_key(|z| (z.root as i32 - key as i32).abs()).expect("a bank has zones");
        let n = z.layers.max(1) as f32;
        let vel = vel.clamp(0.0, 1.0);
        let layer = ((vel * n) as u16).min(z.layers.saturating_sub(1));
        // the layer was played at about the middle of its velocity band
        let centre = (layer as f32 + 0.5) / n;
        let gain = (vel.max(0.02) / centre).powf(1.2 * dynamics.clamp(0.0, 1.0));
        (z.first + layer, key as f32 - z.root as f32, gain)
    }

    pub fn audio(&self, idx: u16) -> Option<&Audio> {
        self.audio.get(idx as usize).map(|a| &**a)
    }

    pub fn notes(&self) -> usize {
        self.zones.len()
    }

    pub fn samples(&self) -> usize {
        self.audio.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(v: f32) -> Arc<Audio> {
        Arc::new(Audio::from_stereo(vec![v; 2000], 48000, 2))
    }

    #[test]
    fn note_names() {
        assert_eq!(parse_note("C4"), Some(60));
        assert_eq!(parse_note("A0"), Some(21));
        assert_eq!(parse_note("D#1"), Some(27));
        assert_eq!(parse_note("C8"), Some(108));
        assert_eq!(parse_note("H2"), None);
        assert_eq!(parse_file("F#3v9.flac"), Some((54, 9)));
        assert_eq!(parse_file("manifest.json"), None);
    }

    #[test]
    fn picks_the_nearest_note_and_the_velocity_layer() {
        let b = KeysBank::from_layers(vec![(60, vec![tone(0.1), tone(0.5), tone(0.9)]), (63, vec![tone(0.1), tone(0.5), tone(0.9)])]).unwrap();
        let (i, st, _) = b.pick(61, 0.1, 1.0);
        assert_eq!((i, st), (0, 1.0));
        let (i, st, _) = b.pick(62, 0.5, 1.0);
        assert_eq!((i, st), (4, -1.0));
        assert_eq!(b.pick(60, 1.0, 1.0).0, 2);
        // inside a layer a harder hit is louder; with dynamics off it is not
        assert!(b.pick(60, 0.6, 1.0).2 > b.pick(60, 0.4, 1.0).2);
        assert_eq!(b.pick(60, 0.6, 0.0).2, 1.0);
        // far outside the sampled range still plays the outermost note
        assert_eq!(b.pick(0, 0.5, 1.0).1, -60.0);
    }

    #[test]
    fn long_recordings_are_cut_with_a_fade() {
        let sr = 1000u32;
        let a = trimmed(Audio::from_stereo(vec![1.0; (sr as usize) * 2 * 20], sr, 2));
        assert_eq!(a.frames, (MAX_SECONDS * sr as f64) as usize);
        assert!(a.content()[a.frames * 2 - 1].abs() < 1e-3);
        assert_eq!(a.content()[0], 1.0);
    }
}
