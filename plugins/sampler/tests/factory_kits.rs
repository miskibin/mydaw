//! What "WAVE Factory Vol. 1" has to be true of, checked against the committed
//! manifest and -- when the content is installed -- against the files.
//!
//! Two halves, and the split matters:
//!
//! * The **manifest** half runs everywhere, including a fresh clone with no
//!   `assets/` directory. It is where the curation rules live: a drum pad plays
//!   at its recorded pitch, no two entries answer for the same key and
//!   velocity, every file is pinned to the commit its source names, every kit
//!   says where it came from and under what licence. These are the mistakes a
//!   two-hundred-row table in `scripts/gen-factory-kits.mjs` invites, and they
//!   are all silent at runtime -- a shadowed entry is not an error, it is a
//!   sample that never sounds.
//!
//! * The **installed** half skips itself when the files are not there, which is
//!   the same rule every other asset-dependent test in this crate follows. It
//!   decodes each kit through the real loader and, for `808-bass`, re-derives
//!   the root pitch with [`mydaw_pitch_analysis`] and fails if the manifest has
//!   drifted from what the recordings actually sound.
//!
//! ```sh
//! node scripts/gen-factory-kits.mjs        # or pwsh -File scripts/fetch-assets.ps1
//! cargo test -p mydaw-plugin-sampler --test factory_kits -- --nocapture
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use mydaw_pitch_analysis::{detect_root_pitch, PitchOutcome};
use mydaw_plugin_sampler::{
    load_instrument, AssetCatalog, CatalogEntry, InstrumentManifest, ASSET_MANIFEST_FILE,
};

/// The kits this file is about. The piano predates them and is checked by
/// `download.rs`'s own tests.
const FACTORY_KITS: &[&str] = &[
    "808-kit",
    "808-bass",
    "percussion-fx",
    "studio-drums",
    "marimba",
    "glockenspiel",
];

/// Kits whose entries are drum pads: one file, one key, recorded pitch.
const DRUM_KITS: &[&str] = &["808-kit", "percussion-fx", "studio-drums"];

fn catalog() -> AssetCatalog {
    AssetCatalog::embedded().expect("assets-manifest.json must parse")
}

fn instrument(entry: &CatalogEntry) -> InstrumentManifest {
    InstrumentManifest::parse(entry.json.as_bytes(), Path::new(ASSET_MANIFEST_FILE))
        .unwrap_or_else(|err| panic!("'{}' must be a runtime manifest: {err}", entry.manifest.id))
}

// ------------------------------------------------------------- the manifest

#[test]
fn every_factory_kit_is_in_the_catalogue() {
    let catalog = catalog();
    for id in FACTORY_KITS {
        assert!(
            catalog.get(id).is_some(),
            "'{id}' is missing from {ASSET_MANIFEST_FILE}"
        );
    }
}

/// Two entries covering the same key *and* velocity is a silent bug:
/// `entry_for` takes the first match and the second never sounds, so a
/// mis-tiled velocity split shows up as a layer that is simply absent.
#[test]
fn no_two_entries_shadow_each_other() {
    for entry in catalog().entries() {
        let manifest = instrument(entry);
        let mut owner: HashMap<(u8, u8), &str> = HashMap::new();
        for sample in &manifest.samples {
            for key in sample.key_lo..=sample.key_hi {
                for velocity in sample.vel_lo..=sample.vel_hi {
                    if let Some(first) = owner.insert((key, velocity), &sample.file) {
                        panic!(
                            "{}: '{}' can never sound -- key {key} velocity {velocity} \
                             is already answered by '{first}'",
                            manifest.id, sample.file
                        );
                    }
                }
            }
        }
    }
}

/// A drum map is a table of unrelated sounds. Resampling one is not a
/// transposition, it is a different instrument, so every pad has to play at the
/// speed it was recorded at.
#[test]
fn drum_pads_play_at_their_recorded_pitch() {
    let catalog = catalog();
    for id in DRUM_KITS {
        let manifest = instrument(catalog.get(id).unwrap_or_else(|| panic!("'{id}' is catalogued")));
        for sample in &manifest.samples {
            assert_eq!(
                (sample.key_lo, sample.key_hi),
                (sample.root_key, sample.root_key),
                "{id}: '{}' spans {}..={} but is rooted at {}; a pad must be one key",
                sample.file,
                sample.key_lo,
                sample.key_hi,
                sample.root_key,
            );
            assert_eq!(sample.tune_cents, 0.0, "{id}: '{}' is detuned", sample.file);
        }
    }
}

/// Velocity layers on one pad have to tile `1..=127` with no hole: a gap is a
/// velocity at which the pad is silent, which reads as a broken sample rather
/// than as a mapping mistake.
#[test]
fn every_key_that_sounds_at_all_sounds_at_every_velocity() {
    for entry in catalog().entries() {
        let manifest = instrument(entry);
        let mut keys: HashMap<u8, Vec<(u8, u8)>> = HashMap::new();
        for sample in &manifest.samples {
            for key in sample.key_lo..=sample.key_hi {
                keys.entry(key).or_default().push((sample.vel_lo, sample.vel_hi));
            }
        }
        for (key, mut spans) in keys {
            spans.sort_unstable();
            let mut reach = 0_u8;
            for (lo, hi) in &spans {
                assert!(
                    *lo <= reach.saturating_add(1),
                    "{}: key {key} is silent at velocity {}",
                    manifest.id,
                    reach + 1
                );
                reach = reach.max(*hi);
            }
            assert_eq!(
                reach, 127,
                "{}: key {key} is silent above velocity {reach}",
                manifest.id
            );
        }
    }
}

/// The pins have to be real pins. A URL that does not carry its commit is a URL
/// whose bytes can change under the checksum -- which turns a supply-chain
/// change into a failed install rather than into a silent substitution, but
/// only because the checksum is there; the pin is what makes it stable.
#[test]
fn every_factory_file_names_its_source_and_licence() {
    let catalog = catalog();
    for id in FACTORY_KITS {
        let entry = catalog.get(id).unwrap_or_else(|| panic!("'{id}' is catalogued"));
        let source = &entry.manifest.source;
        assert_eq!(
            source.license, "CC0-1.0",
            "{id}: the factory kits are CC0; anything else needs a credit line in \
             the installer's ATTRIBUTION.md and a note in docs/factory-library.md"
        );
        assert!(!source.license_url.is_empty(), "{id}: no licence URL");
        assert!(!source.author.is_empty(), "{id}: no author");
        assert!(
            !entry.manifest.attribution.trim().is_empty(),
            "{id}: CC0 asks for no credit; we give one anyway"
        );
        assert_eq!(source.commit.len(), 40, "{id}: not a full commit id");
        for file in &entry.manifest.samples {
            assert!(
                file.url.starts_with("https://raw.githubusercontent.com/"),
                "{id}: '{}' is not a raw GitHub URL",
                file.file
            );
            assert!(
                file.url.contains(&source.commit),
                "{id}: '{}' is not pinned to {}",
                file.file,
                source.commit
            );
            assert!(file.bytes > 0, "{id}: '{}' is empty", file.file);
        }
    }
}

/// A pad is triggered and a note is played, and the manifest has to say which.
///
/// Without `one_shot` a sixteenth-note gate cuts a 3 s 808 tail in half, which
/// is why the kits used to carry a 30-second release instead -- a workaround
/// this asserts is gone as much as it asserts the flag is there.
#[test]
fn every_drum_kit_declares_itself_a_one_shot_and_no_kit_fakes_one() {
    let catalog = catalog();
    for id in FACTORY_KITS {
        let manifest = instrument(catalog.get(id).unwrap_or_else(|| panic!("'{id}' is catalogued")));
        assert_eq!(
            manifest.one_shot,
            DRUM_KITS.contains(id),
            "{id}: a drum map is triggered and everything else is played"
        );
        assert!(
            manifest.release_ms <= mydaw_plugin_sampler::node::MAX_ENVELOPE_MS,
            "{id}: a {} ms release is a one-shot workaround, not a release",
            manifest.release_ms
        );
    }
}

/// 808 Bass is the kit whose *instrument* the sampler's knobs make: mono, with
/// a glide, is the difference between a trap 808 and a kick played
/// chromatically.
#[test]
fn the_808_bass_ships_the_settings_that_make_it_an_808() {
    let catalog = catalog();
    let manifest = instrument(catalog.get("808-bass").expect("'808-bass' is catalogued"));
    assert_eq!(manifest.default_params.get("mono"), Some(&1.0));
    let glide = *manifest
        .default_params
        .get("glide")
        .expect("808 Bass slides");
    assert!(
        (20.0..=150.0).contains(&glide),
        "a {glide} ms glide is not a slide"
    );
    assert!(
        manifest.release_ms <= 300.0,
        "a bass note stops when the key does"
    );
}

/// A default nothing reads is a default that silently does not apply.
#[test]
fn every_declared_default_names_a_knob_the_sampler_has() {
    for entry in catalog().entries() {
        let manifest = instrument(entry);
        for (id, value) in &manifest.default_params {
            let def = mydaw_plugin_sampler::PARAMS
                .iter()
                .find(|def| def.id == id)
                .unwrap_or_else(|| panic!("{}: '{id}' is not a sampler knob", manifest.id));
            assert_eq!(
                def.sanitise(*value),
                *value,
                "{}: '{id}' = {value} is outside what the knob can hold",
                manifest.id
            );
        }
    }
}

/// A kit's name is what a user reads in the source picker. "808" is a size of
/// sound; the manufacturer's name and model number are provenance, and belong
/// in the credits rather than on the label.
#[test]
fn kit_names_carry_no_trade_dress() {
    let catalog = catalog();
    for id in FACTORY_KITS {
        let entry = catalog.get(id).unwrap();
        let manifest = instrument(entry);
        let visible = format!("{} {} {}", manifest.id, manifest.name, manifest.description);
        for forbidden in ["roland", "tr-808", "tr808"] {
            assert!(
                !visible.to_ascii_lowercase().contains(forbidden),
                "{id}: '{forbidden}' must not appear in a user-visible name; \
                 provenance goes in `attribution`"
            );
        }
    }
}

// -------------------------------------------------------------- the content

fn installed(id: &str) -> Option<PathBuf> {
    let dir = mydaw_plugin_sampler::instruments_root().join(id);
    dir.join("manifest.json").is_file().then_some(dir)
}

/// The end-to-end check: the manifest the installer wrote parses, the files
/// beside it decode, and the bank answers for the keys the manifest claims.
#[test]
fn an_installed_kit_loads_and_maps_every_key_it_claims() {
    let mut checked = 0;
    for id in FACTORY_KITS {
        let Some(dir) = installed(id) else { continue };
        checked += 1;

        let manifest = InstrumentManifest::read(&dir.join("manifest.json"))
            .unwrap_or_else(|err| panic!("'{id}' installed a manifest that does not parse: {err}"));
        assert_eq!(manifest.id, *id, "the directory and the manifest disagree");

        let bank = load_instrument(&dir).unwrap_or_else(|err| panic!("'{id}' must load: {err}"));
        assert!(
            bank.skipped().is_empty(),
            "'{id}' is missing {:?}",
            bank.skipped()
        );
        assert_eq!(bank.len(), manifest.samples.len());
        assert!(bank.bytes() > 0);

        // Probe inside each entry's own zone rather than at its root: in a
        // chromatic kit several entries share a root and differ only in the
        // keys they answer for.
        for sample in &manifest.samples {
            for key in [sample.key_lo, sample.key_hi] {
                for velocity in [sample.vel_lo, sample.vel_hi] {
                    let entry = manifest.entry_for(key, velocity).unwrap_or_else(|| {
                        panic!("'{id}': nothing answers for key {key} velocity {velocity}")
                    });
                    assert_eq!(
                        entry.file, sample.file,
                        "'{id}': key {key} velocity {velocity} plays '{}' rather than '{}'",
                        entry.file, sample.file
                    );
                }
            }
        }
        eprintln!(
            "{id}: {} samples, {:.1} MB decoded",
            bank.len(),
            bank.bytes() as f64 / 1e6
        );
    }
    if checked == 0 {
        eprintln!("no factory kits installed; run node scripts/gen-factory-kits.mjs");
    }
}

/// The part of an 808 kick that has a pitch at all.
///
/// The first third is the click and the downward sweep -- what makes the sound
/// a *kick*. The last quarter is below the noise floor. What a bass line is
/// played in is the frequency the middle settles on.
const BASS_TAIL: std::ops::Range<f64> = 0.35..0.75;

/// The band the oscillator can be in: two octaves around a low E.
const BASS_SEARCH_HZ: std::ops::Range<f64> = 40.0..90.0;

/// The strongest partial of a windowed segment, in Hz, by scanning the band
/// coarsely and then refining around the winner.
///
/// # Why this and not `mydaw_pitch_analysis::detect_root_pitch`
/// The repository's analyser is asked first, in the test below, and it declines
/// this material: it reports `Polyphonic` for all ten recordings. It is not
/// wrong to. It works at a 12 kHz analysis rate with a 2048-point frame, which
/// puts a 50 Hz fundamental in bin eight and its first few harmonics in bins
/// that are tens of cents off where a harmonic series says they should be, and
/// "the partials do not line up" is exactly what its polyphony guard is for.
/// A sound designed to sweep is not the held note it was written for.
///
/// So this measures the one thing the assertion needs -- where the settled tail
/// of a decaying sine sits -- with a method whose only assumption is that the
/// segment has one dominant partial in `BASS_SEARCH_HZ`. It is test
/// instrumentation, deliberately narrow, and has no business anywhere else.
fn dominant_partial_hz(mono: &[f32], sample_rate: f64) -> f64 {
    // Hann, so the peak is a smooth function of frequency and the refinement
    // pass has something to climb.
    let windowed: Vec<f64> = mono
        .iter()
        .enumerate()
        .map(|(index, sample)| {
            let phase = std::f64::consts::TAU * index as f64 / (mono.len() - 1).max(1) as f64;
            f64::from(*sample) * (0.5 - 0.5 * phase.cos())
        })
        .collect();

    // One Goertzel-style projection at `frequency`, as a magnitude.
    let power_at = |frequency: f64| {
        let step = std::f64::consts::TAU * frequency / sample_rate;
        let (mut real, mut imaginary) = (0.0_f64, 0.0_f64);
        for (index, sample) in windowed.iter().enumerate() {
            let phase = step * index as f64;
            real += sample * phase.cos();
            imaginary -= sample * phase.sin();
        }
        real * real + imaginary * imaginary
    };

    let mut best = (0.0_f64, BASS_SEARCH_HZ.start);
    for coarse in 0.. {
        let frequency = BASS_SEARCH_HZ.start + f64::from(coarse) * 0.25;
        if frequency > BASS_SEARCH_HZ.end {
            break;
        }
        let power = power_at(frequency);
        if power > best.0 {
            best = (power, frequency);
        }
    }
    for fine in -50..=50 {
        let frequency = best.1 + f64::from(fine) * 0.005;
        let power = power_at(frequency);
        if power > best.0 {
            best = (power, frequency);
        }
    }
    best.1
}

/// Decimate to about 1.5 kHz with a box average, which is both the anti-alias
/// filter and the speed-up: nothing above 700 Hz matters to a 50 Hz tone, and
/// the projection above is O(candidates x samples).
fn decimated_mono(stereo: &[f32], range: std::ops::Range<usize>, sample_rate: f64) -> (Vec<f32>, f64) {
    let factor = (sample_rate / 1_500.0).round().max(1.0) as usize;
    let mut out = Vec::with_capacity(range.len() / factor + 1);
    let mut frame = range.start;
    while frame + factor <= range.end {
        let mut sum = 0.0_f32;
        for offset in 0..factor {
            let index = (frame + offset) * 2;
            sum += (stereo[index] + stereo[index + 1]) * 0.5;
        }
        out.push(sum / factor as f32);
        frame += factor;
    }
    let mean = out.iter().sum::<f32>() / out.len().max(1) as f32;
    for sample in &mut out {
        *sample -= mean;
    }
    (out, sample_rate / factor as f64)
}

/// 808 Bass is the one kit whose `root_key` is a claim about a recording rather
/// than a slot in a drum map: it plays a kick chromatically, so a wrong root is
/// a bass line in the wrong key.
///
/// The claim the manifest makes is that playing `root_key` sounds `root_key`.
/// The sampler plays `key - root_key + tune_cents/100` semitones away from the
/// recording, so that is true exactly when the recording's own pitch is
/// `root_key * 100 - tune_cents` cents -- which is what this measures.
///
/// The tolerance is a third of a semitone. These are an analogue oscillator
/// settling, measured over a windowed second of tail; anything tighter would
/// assert more precision than either the source or the method has.
#[test]
fn the_808_bass_roots_match_what_the_recordings_actually_sound() {
    let Some(dir) = installed("808-bass") else {
        eprintln!("808-bass is not installed; run node scripts/gen-factory-kits.mjs");
        return;
    };
    let manifest = InstrumentManifest::read(&dir.join("manifest.json")).expect("parses");

    let mut errors = Vec::new();
    for sample in &manifest.samples {
        let path = dir.join(&sample.file);
        let decoded = mydaw_audio_file::decode_stereo(&path, |_| None)
            .unwrap_or_else(|err| panic!("{} must decode: {err}", sample.file));

        // Asked first, and expected to decline: see `dominant_partial_hz`. If
        // this ever starts detecting, the number it gives should be checked
        // against the measurement below rather than trusted over it.
        let analyser = detect_root_pitch(
            &decoded.data,
            decoded.frames,
            decoded.sample_rate,
            0..decoded.frames,
        );

        let from = (decoded.frames as f64 * BASS_TAIL.start) as usize;
        let to = (decoded.frames as f64 * BASS_TAIL.end) as usize;
        let (mono, rate) = decimated_mono(&decoded.data, from..to, decoded.sample_rate);
        assert!(
            mono.len() > 64,
            "{}: the tail is too short to measure",
            sample.file
        );
        let hertz = dominant_partial_hz(&mono, rate);
        let measured = 6_900.0 + 1_200.0 * (hertz / 440.0).log2();
        let claimed = f64::from(sample.root_key) * 100.0 - f64::from(sample.tune_cents);

        eprintln!(
            "  {:26} {:6.2} Hz -> {:7.1} cents   manifest root {} {:+.1}c -> {:7.1}   \
             error {:+5.1}c   [analyser: {}]",
            sample.file,
            hertz,
            measured,
            sample.root_key,
            sample.tune_cents,
            claimed,
            measured - claimed,
            match analyser {
                PitchOutcome::Detected(pitch) => format!("{} {:+.1}c", pitch.midi_note, pitch.cents),
                PitchOutcome::NoStablePitch(reason) => format!("{reason:?}"),
            }
        );
        errors.push((sample.file.clone(), measured - claimed));
    }

    for (file, error) in errors {
        assert!(
            error.abs() <= 33.0,
            "{file}: the manifest's root is {error:+.1} cents from the measured pitch; \
             update BASS_808_ROOT_KEY / BASS_808_TUNE_CENTS in \
             scripts/gen-factory-kits.mjs and re-run it"
        );
    }
}
