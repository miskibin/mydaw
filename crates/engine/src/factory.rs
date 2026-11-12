//! The standard library: the one-shots that ship inside the program, so a new project has
//! real, visible samples in its Channel Rack without a download.
//!
//! The files are embedded (`crates/engine/library`, built by `scripts/make-factory-library.py` from
//! CC0 recordings, see `THIRD_PARTY_NOTICES.md`). A project names them, and any other file of
//! the sample library, by a portable path: `lib:<kit>/<file>` instead of where the library
//! happens to live on this computer, so a song opens the same everywhere. The engine decodes
//! the embedded bytes directly; [`install`] also writes them into the library folder so the
//! browser can list, preview and drag them like any other file.

use std::path::{Path, PathBuf};

use crate::model::{Id, Project, Source};

#[path = "factory_files.rs"]
mod files;

/// One embedded sample.
pub struct FactoryFile {
    pub file: &'static str,
    pub name: &'static str,
    /// Kicks, Snares, Hats …: how the browser groups them.
    pub group: &'static str,
    /// MIDI key at which the recording sounds at its own pitch (60 for unpitched drums).
    pub root: u8,
    /// It plays a note (the browser shows its key); false for drums, noises and swells.
    pub pitched: bool,
    /// Seconds.
    pub dur: f64,
    pub ch: u16,
    pub bytes: &'static [u8],
}

/// Folder of the standard library inside the sample library, and its `lib:` kit name.
pub const KIT: &str = "Standard";
/// Prefix of a portable library path.
pub const SCHEME: &str = "lib:";
/// Every file is 44.1 kHz and peaks at −1 dBFS (the build script normalises them).
const SR: u32 = 44_100;
const PEAK_DB: f32 = -1.0;

pub fn files() -> &'static [FactoryFile] {
    files::FILES
}

pub fn file(name: &str) -> Option<&'static FactoryFile> {
    files().iter().find(|f| f.file == name)
}

/// The sample library on this computer (factory kits and downloads).
pub fn library_dir() -> PathBuf {
    crate::engine::scratch_dir().parent().unwrap_or(Path::new(".")).join("Library")
}

/// `lib:Standard/<file>`.
pub fn lib_path(file: &str) -> String {
    format!("{SCHEME}{KIT}/{file}")
}

/// The embedded file a `lib:` path names, if it is one of the standard library's.
pub fn embedded(path: &str) -> Option<&'static FactoryFile> {
    let rel = path.strip_prefix(SCHEME)?;
    let (kit, name) = rel.split_once('/')?;
    (kit == KIT).then(|| file(name)).flatten()
}

/// Where a `lib:` path is on this computer (whether or not the file is there).
pub fn resolve(path: &str) -> Option<PathBuf> {
    let rel = path.strip_prefix(SCHEME)?;
    // a library path never leaves the library
    if rel.split('/').any(|part| part.is_empty() || part == "." || part == ".." || part.contains('\\')) {
        return None;
    }
    Some(rel.split('/').fold(library_dir(), |p, part| p.join(part)))
}

/// The portable form of a path inside the sample library; any other path is returned as is.
pub fn portable(path: &Path) -> String {
    match path.strip_prefix(library_dir()) {
        Ok(rel) if rel.components().count() >= 2 => {
            let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
            format!("{SCHEME}{}", parts.join("/"))
        }
        _ => path.display().to_string(),
    }
}

/// Writes the standard library into the library folder (files that are missing or differ in
/// size), so it can be browsed like a downloaded kit. Returns its folder.
pub fn install() -> std::io::Result<PathBuf> {
    let dir = library_dir().join(KIT);
    std::fs::create_dir_all(&dir)?;
    for f in files() {
        let dst = dir.join(f.file);
        if std::fs::metadata(&dst).map(|m| m.len()).ok() == Some(f.bytes.len() as u64) {
            continue;
        }
        let tmp = dst.with_extension("part");
        std::fs::write(&tmp, f.bytes)?;
        std::fs::rename(&tmp, &dst)?;
    }
    Ok(dir)
}

const COLORS: [(&str, &str); 10] = [
    ("Kicks", "#4f8ef7"),
    ("Snares", "#d9678f"),
    ("Claps", "#d9678f"),
    ("Hats", "#e0a44a"),
    ("Cymbals", "#e0a44a"),
    ("Toms", "#5aa9e6"),
    ("Percussion", "#5fbf8a"),
    ("Tonal", "#7c8cf8"),
    ("Orchestra", "#c98a5a"),
    ("FX", "#b07cf0"),
];

pub fn color(f: &FactoryFile) -> &'static str {
    COLORS.iter().find(|(g, _)| *g == f.group).map(|(_, c)| *c).unwrap_or("#69c3d6")
}

/// The project source of a standard-library file: the one the project already has, or a
/// new one. `None` when there is no such file.
pub fn source(p: &mut Project, name: &str) -> Option<Id> {
    let f = file(name)?;
    let path = lib_path(f.file);
    if let Some(s) = p.sources.iter().find(|s| s.path == path) {
        return Some(s.id);
    }
    let id = p.alloc_id();
    p.sources.push(Source { id, name: f.name.into(), color: color(f).into(), path, dur: f.dur, sr: SR, ch: f.ch, peak_db: PEAK_DB });
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_file_decodes_and_matches_its_table_entry() {
        assert!(files().len() >= 140);
        let mut names = std::collections::HashSet::new();
        for f in files() {
            assert!(names.insert(f.file) && names.insert(f.name), "{}: file or name used twice", f.file);
            assert!(COLORS.iter().any(|(g, _)| *g == f.group), "{}: group {} has no colour", f.file, f.group);
            let a = crate::sources::Audio::load_path(&lib_path(f.file), Path::new(".")).unwrap_or_else(|e| panic!("{}: {e}", f.file));
            assert_eq!(a.sr, SR, "{}", f.file);
            assert_eq!(a.channels, f.ch, "{}", f.file);
            assert!((a.seconds() - f.dur).abs() < 0.001, "{}: {} s, table says {}", f.file, a.seconds(), f.dur);
            let peak = crate::dsp::gain_to_db(a.peak());
            assert!((peak - PEAK_DB).abs() < 0.1, "{}: peak {peak} dB", f.file);
        }
    }

    /// Key (fractional MIDI) at the centre of the energy within 0.7 semitones of `near`, by a
    /// Goertzel scan of the first second.
    fn key_near(a: &crate::sources::Audio, near: u8) -> f64 {
        let n = a.frames.min(SR as usize);
        let mono: Vec<f64> = (0..n)
            .map(|i| {
                let (l, r) = a.sample(i as f64);
                (l + r) as f64
            })
            .collect();
        let power = |key: f64| {
            let w = std::f64::consts::TAU * 440.0 * 2f64.powf((key - 69.0) / 12.0) / SR as f64;
            let (mut re, mut im) = (0.0, 0.0);
            for (i, x) in mono.iter().enumerate() {
                let h = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos();
                re += x * h * (w * i as f64).cos();
                im += x * h * (w * i as f64).sin();
            }
            re * re + im * im
        };
        // sections are several players a few cents apart: take the centre of what sounds
        let (mut sum, mut weight) = (0.0, 0.0);
        for step in -70..=70 {
            let key = near as f64 + step as f64 / 100.0;
            let p = power(key);
            sum += key * p;
            weight += p;
        }
        sum / weight
    }

    /// The pitched one-shots are tuned by the build script: played at their root key they
    /// sound that key, so a melody on the piano roll is in tune with the synths.
    #[test]
    fn pitched_samples_sound_their_root_key() {
        let pitched = [
            "vibraphone.flac", "vibraphone-high.flac", "xylophone.flac", "balafon.flac", "kalimba.flac", "hand-chime.flac",
            "harpsichord.flac", "upright-piano.flac", "fm-piano.flac", "pipe-organ.flac", "tenor-sax.flac", "strings-tremolo.flac",
            "cello-spiccato.flac", "contrabass-pizz.flac", "trumpet-staccato.flac", "horn-staccato.flac", "flute-staccato.flac",
            "harp.flac", "harp-high.flac",
        ];
        for name in pitched {
            let f = file(name).unwrap_or_else(|| panic!("{name} is not in the library"));
            let a = crate::sources::Audio::load_path(&lib_path(f.file), Path::new(".")).unwrap();
            let key = key_near(&a, f.root);
            assert!((key - f.root as f64).abs() <= 0.15, "{name}: sounds key {key:.2}, table says {}", f.root);
        }
        // the hits and stabs are layers on C: every octave of C is loud in them
        for name in ["orch-hit.flac", "orch-hit-minor.flac", "orch-hit-major.flac", "strings-stab.flac", "brass-stab.flac", "pizzicato.flac"] {
            let f = file(name).unwrap();
            assert_eq!(f.root % 12, 0, "{name}");
            let a = crate::sources::Audio::load_path(&lib_path(f.file), Path::new(".")).unwrap();
            let key = key_near(&a, f.root);
            assert!((key - f.root as f64).abs() <= 0.15, "{name}: its C sounds key {key:.2}");
        }
    }

    #[test]
    fn library_paths_are_portable_and_cannot_escape() {
        let abs = library_dir().join(KIT).join("kick-808.flac");
        assert_eq!(portable(&abs), "lib:Standard/kick-808.flac");
        assert_eq!(resolve("lib:Standard/kick-808.flac"), Some(abs));
        assert_eq!(portable(Path::new("/music/kick.wav")), "/music/kick.wav");
        assert_eq!(resolve("lib:../secret.wav"), None);
        assert_eq!(resolve("lib:Standard//x.wav"), None);
        assert!(embedded("lib:Standard/kick-808.flac").is_some());
        assert!(embedded("lib:808-kit/kick-808.flac").is_none());
    }

    #[test]
    fn a_file_is_one_source_however_often_it_is_used() {
        let mut p = Project::empty();
        let n = p.sources.len();
        let a = source(&mut p, "triangle.flac").unwrap();
        let b = source(&mut p, "triangle.flac").unwrap();
        assert_eq!(a, b);
        assert_eq!(p.sources.len(), n + 1);
        assert!(source(&mut p, "nope.flac").is_none());
    }
}
