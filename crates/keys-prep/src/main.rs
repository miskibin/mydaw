//! `keys-prep <originals> <out>`: prepares the WAVE Keys piano for an installer.
//!
//! The Salamander recordings are 24-bit FLAC and up to 25 s long, but the engine keeps only
//! the first [`keys::MAX_SECONDS`] of each (with a fade, [`keys::trimmed`]). This step applies
//! that same cut ahead of time and stores the result as 16-bit FLAC with TPDF dither, so
//! the installer carries what actually plays: about 23 MB instead of 138 MB. When the app
//! loads a prepared file, `trimmed` finds it already short enough and leaves it alone.
//!
//! Every file is decoded again after encoding and compared with the cut recording; a file
//! that differs by more than the dither allows stops the build. The output is deterministic:
//! the dither is seeded from the file name.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use flacenc::component::BitRepr;
use flacenc::error::Verify;
use wave_engine::keys;
use wave_engine::sources::Audio;

/// The largest difference allowed between a decoded output sample and the cut recording,
/// in 16-bit steps: half a step of rounding plus at most one step of dither, with room for
/// the decoder's float conversion.
const MAX_ERROR_LSB: f32 = 1.6;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [src, out] = args.as_slice() else {
        eprintln!("usage: keys-prep <folder of original recordings> <output folder>");
        std::process::exit(2);
    };
    match run(Path::new(src), Path::new(out)) {
        Ok(s) => println!(
            "keys-prep: {} files, {:.1} MB -> {:.1} MB, largest difference {:.1} dBFS",
            s.files,
            s.bytes_in as f64 / 1e6,
            s.bytes_out as f64 / 1e6,
            20.0 * s.max_error.max(1e-12).log10()
        ),
        Err(e) => {
            eprintln!("keys-prep: {e}");
            std::process::exit(1);
        }
    }
}

#[derive(Debug, Default)]
struct Summary {
    files: usize,
    bytes_in: u64,
    bytes_out: u64,
    /// Largest |decoded output − cut recording| over every sample, full scale = 1.
    max_error: f32,
}

/// One recording written to the output folder.
struct Prepared {
    bytes_in: u64,
    bytes_out: u64,
    max_error: f32,
}

fn run(src: &Path, out: &Path) -> Result<Summary, String> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(src)
        .map_err(|e| format!("{}: {e}", src.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("flac"))
        })
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(format!("{}: no FLAC recordings", src.display()));
    }
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;

    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .clamp(1, 8);
    let next = AtomicUsize::new(0);
    let results: Vec<Mutex<Option<Result<Prepared, String>>>> =
        files.iter().map(|_| Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(path) = files.get(i) else { break };
                let r = prepare_file(path, out).map_err(|e| format!("{}: {e}", path.display()));
                *results[i].lock().unwrap() = Some(r);
            });
        }
    });

    let mut summary = Summary::default();
    for slot in results {
        let p = slot
            .into_inner()
            .unwrap()
            .unwrap_or_else(|| Err("not processed".into()))?;
        summary.files += 1;
        summary.bytes_in += p.bytes_in;
        summary.bytes_out += p.bytes_out;
        summary.max_error = summary.max_error.max(p.max_error);
    }

    // The whole folder goes into the installer: a recording removed from the manifest, or a
    // `.part` left by an interrupted run, must not linger there.
    let keep: Vec<_> = files.iter().filter_map(|p| p.file_name()).collect();
    for entry in std::fs::read_dir(out)
        .map_err(|e| format!("{}: {e}", out.display()))?
        .flatten()
    {
        let name = entry.file_name();
        let path = entry.path();
        let ext = path.extension().map(|x| x.to_ascii_lowercase());
        let stale = match ext.as_ref().and_then(|x| x.to_str()) {
            Some("flac") => !keep.contains(&name.as_os_str()),
            Some("part") => true,
            _ => false,
        };
        if stale {
            std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    Ok(summary)
}

/// Cuts, encodes, checks and writes one recording.
fn prepare_file(path: &Path, out: &Path) -> Result<Prepared, String> {
    let name = path.file_name().ok_or("no file name")?;
    let bytes_in = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    let audio = keys::trimmed(Audio::load(path)?);
    let flac = encode(&audio, seed_of(&name.to_string_lossy()))?;

    // written aside and checked there, so a bad file never takes the real name
    let dst = out.join(name);
    let part = out.join(format!("{}.part", name.to_string_lossy()));
    std::fs::write(&part, &flac).map_err(|e| format!("{}: {e}", part.display()))?;
    let max_error = match check(&part, &audio) {
        Ok(e) => e,
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
    };
    std::fs::rename(&part, &dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    Ok(Prepared {
        bytes_in,
        bytes_out: flac.len() as u64,
        max_error,
    })
}

/// 16-bit FLAC of `audio` (mono when the recording is mono), TPDF-dithered.
fn encode(audio: &Audio, seed: u64) -> Result<Vec<u8>, String> {
    let channels = usize::from(audio.channels.clamp(1, 2));
    let content = audio.content();
    let mut rng = Rng(seed | 1);
    let mut samples = Vec::with_capacity(audio.frames * channels);
    for frame in content.chunks_exact(2) {
        for &v in &frame[..channels] {
            samples.push(quantize(v, rng.tpdf()));
        }
    }
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|e| format!("encoder settings: {e:?}"))?;
    let source =
        flacenc::source::MemSource::from_samples(&samples, channels, 16, audio.sr as usize);
    let mut stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|e| format!("FLAC encoding failed: {e:?}"))?;
    // flacenc counts the short last block in the minimum block size. The format excludes it,
    // and symphonia (the app's decoder) refuses such a file, so this fixed-block stream says
    // min = max as it should.
    let max_block = stream.stream_info().max_block_size();
    stream
        .stream_info_mut()
        .set_block_sizes(max_block, max_block)
        .map_err(|e| format!("FLAC header: {e:?}"))?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|e| format!("FLAC writing failed: {e:?}"))?;
    Ok(sink.as_slice().to_vec())
}

/// A sample in 16-bit steps, with `dither` (in steps) added before rounding.
fn quantize(v: f32, dither: f32) -> i32 {
    ((v * 32768.0 + dither).round() as i32).clamp(-32768, 32767)
}

/// Decodes the FLAC file at `path` the way the app will and compares it with `audio`.
/// Returns the largest error.
fn check(path: &Path, audio: &Audio) -> Result<f32, String> {
    let decoded = wave_audio_file::decode_stereo(path, |_| None)
        .map_err(|e| format!("the encoded file does not decode: {e}"))?;
    if decoded.frames != audio.frames || decoded.sample_rate.round() as u32 != audio.sr {
        return Err(format!(
            "the encoded file has {} frames at {} Hz, expected {} at {}",
            decoded.frames, decoded.sample_rate, audio.frames, audio.sr
        ));
    }
    let max_error = decoded
        .data
        .iter()
        .zip(audio.content())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f32, f32::max);
    if max_error > MAX_ERROR_LSB / 32768.0 {
        return Err(format!(
            "the encoded file differs from the recording by {:.2} steps",
            max_error * 32768.0
        ));
    }
    Ok(max_error)
}

/// FNV-1a: a stable seed per file name, so a rebuild writes the same bytes.
fn seed_of(name: &str) -> u64 {
    name.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// xorshift64*: enough for dither, and the same on every machine.
struct Rng(u64);

impl Rng {
    fn unit(&mut self) -> f32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Triangular dither in (−1, 1) steps.
    fn tpdf(&mut self) -> f32 {
        self.unit() + self.unit() - 1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(seconds: f64, sr: u32, stereo: bool) -> Audio {
        let frames = (seconds * sr as f64) as usize;
        let mut data = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let t = i as f32 / sr as f32;
            let l = 0.5 * (std::f32::consts::TAU * 220.0 * t).sin() * (-t * 0.3).exp();
            let r = if stereo {
                0.4 * (std::f32::consts::TAU * 330.0 * t).sin()
            } else {
                l
            };
            data.extend([l, r]);
        }
        Audio::from_stereo(data, sr, if stereo { 2 } else { 1 })
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wave-keys-prep-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn long_recording_is_cut_like_the_engine_cuts_it_and_survives_a_round_trip() {
        let sr = 48_000;
        let cut = keys::trimmed(tone(12.0, sr, true));
        assert_eq!(cut.frames, (keys::MAX_SECONDS * sr as f64) as usize);
        let flac = encode(&cut, 7).unwrap();
        let dir = scratch("cut");
        let path = dir.join("C4v1.flac");
        std::fs::write(&path, &flac).unwrap();
        let err = check(&path, &cut).unwrap();
        let _ = std::fs::remove_dir_all(dir);
        assert!(err <= MAX_ERROR_LSB / 32768.0);
        // 16-bit FLAC of a decaying tone is far smaller than the float data it came from
        assert!(flac.len() < cut.frames * 4, "{} bytes", flac.len());
    }

    #[test]
    fn a_prepared_file_is_left_alone_when_the_app_loads_it() {
        let sr = 44_100;
        let cut = keys::trimmed(tone(10.0, sr, true));
        let dir = scratch("reload");
        let path = dir.join("A0v1.flac");
        std::fs::write(&path, encode(&cut, 1).unwrap()).unwrap();
        let loaded = Audio::load(&path).unwrap();
        let again = keys::trimmed(Audio::load(&path).unwrap());
        assert_eq!(again.frames, loaded.frames);
        assert_eq!(
            again.content(),
            loaded.content(),
            "loading must not fade the tail a second time"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn mono_stays_mono_and_short_recordings_keep_their_length() {
        let a = tone(2.0, 48_000, false);
        let dir = scratch("mono");
        let path = dir.join("C8v1.flac");
        std::fs::write(&path, encode(&a, 3).unwrap()).unwrap();
        check(&path, &a).unwrap();
        let back = Audio::load(&path).unwrap();
        assert_eq!(back.channels, 1);
        assert_eq!(back.frames, a.frames);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_file_that_does_not_match_is_rejected() {
        let a = tone(1.0, 48_000, true);
        let louder = Audio::from_stereo(a.content().iter().map(|v| v * 1.01).collect(), a.sr, 2);
        let dir = scratch("mismatch");
        let path = dir.join("C4v1.flac");
        std::fs::write(&path, encode(&louder, 3).unwrap()).unwrap();
        assert!(check(&path, &a).unwrap_err().contains("differs"));
        let shorter = Audio::from_stereo(a.content()[..2000].to_vec(), a.sr, 2);
        assert!(check(&path, &shorter).unwrap_err().contains("frames"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn lengths_that_are_not_a_whole_number_of_blocks_decode() {
        // 4096-sample blocks: one short frame, an exact multiple, and a short last frame
        let dir = scratch("blocks");
        for frames in [1000_usize, 8192, 8192 + 2944] {
            let data: Vec<f32> = (0..frames)
                .flat_map(|i| [0.1 * (i as f32 * 0.01).sin(); 2])
                .collect();
            let a = Audio::from_stereo(data, 48_000, 2);
            let path = dir.join(format!("C4v{frames}.flac"));
            std::fs::write(&path, encode(&a, 9).unwrap()).unwrap();
            check(&path, &a).unwrap_or_else(|e| panic!("{frames} frames: {e}"));
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn output_is_deterministic() {
        let a = tone(1.0, 48_000, true);
        assert_eq!(
            encode(&a, seed_of("D1v4.flac")).unwrap(),
            encode(&a, seed_of("D1v4.flac")).unwrap()
        );
    }

    #[test]
    fn dither_stays_within_one_step_and_quantizing_clamps() {
        let mut rng = Rng(seed_of("x") | 1);
        let (mut lo, mut hi, mut sum) = (f32::MAX, f32::MIN, 0.0_f64);
        for _ in 0..100_000 {
            let d = rng.tpdf();
            lo = lo.min(d);
            hi = hi.max(d);
            sum += f64::from(d);
        }
        assert!(lo > -1.0 && hi < 1.0, "{lo} {hi}");
        assert!(
            (sum / 100_000.0).abs() < 0.01,
            "dither must have no DC: {}",
            sum / 100_000.0
        );
        assert_eq!(quantize(1.5, 0.0), 32767);
        assert_eq!(quantize(-1.5, 0.0), -32768);
        assert_eq!(quantize(0.0, 0.4), 0);
    }

    #[test]
    fn run_writes_every_recording_and_removes_stale_ones() {
        let src = scratch("src");
        let out = scratch("out");
        for name in ["C4v1.flac", "C4v2.flac"] {
            std::fs::write(
                src.join(name),
                encode(&tone(11.0, 48_000, true), 5).unwrap(),
            )
            .unwrap();
        }
        std::fs::write(out.join("Old1v1.flac"), b"stale").unwrap();
        std::fs::write(out.join("C4v1.flac.part"), b"interrupted").unwrap();
        std::fs::write(out.join("ATTRIBUTION.txt"), b"kept").unwrap();
        let s = run(&src, &out).unwrap();
        assert_eq!(s.files, 2);
        assert!(s.bytes_out < s.bytes_in, "{s:?}");
        let mut names: Vec<_> = std::fs::read_dir(&out)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["ATTRIBUTION.txt", "C4v1.flac", "C4v2.flac"]);
        assert_eq!(keys::KeysBank::load(&out).map(|_| ()), Ok(()));
        let _ = std::fs::remove_dir_all(src);
        let _ = std::fs::remove_dir_all(out);
    }

    #[test]
    fn missing_or_empty_folder_is_an_error() {
        let empty = scratch("empty");
        assert!(run(&empty, &empty.join("out"))
            .unwrap_err()
            .contains("no FLAC"));
        assert!(run(&empty.join("nope"), &empty.join("out")).is_err());
        let _ = std::fs::remove_dir_all(empty);
    }
}
