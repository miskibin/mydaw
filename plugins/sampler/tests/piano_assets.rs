//! What the *real* piano does, when it is installed.
//!
//! Every test here **skips itself** when `assets/instruments/salamander-piano`
//! is absent, which is the established pattern in this workspace for anything
//! that needs something the repository does not carry. CI has no assets and
//! must stay green; a developer who has run `scripts/fetch-assets.ps1` gets the
//! measurements.
//!
//! The assertions are deliberately weaker than `tests/voices.rs`. A recording
//! of a real Yamaha C5 is not a sine: it has inharmonic partials, a hammer
//! transient, sympathetic string resonance and a noise floor. What can honestly
//! be asserted about it is that the fundamental lands where equal temperament
//! says, that it decays, that it does not clip, and that harder notes are
//! louder -- so that is what is asserted. The exact numbers are printed, so a
//! change in the content shows up as a number moving rather than as a test that
//! silently still passes.
//!
//! ```sh
//! cargo test -p mydaw-plugin-sampler --test piano_assets -- --nocapture
//! ```

use std::sync::Arc;

use mydaw_plugin_debug_tap::spectrum;
use mydaw_plugin_sampler::test_support::{peak, render, rms};
use mydaw_plugin_sampler::{load_instrument, SampleBank, SamplerNode};
use mydaw_rt_core::NoteEvent;

const SR: f64 = 48_000.0;
const INSTRUMENT_ID: &str = "salamander-piano";

/// Samples the manifest names: 30 notes x 3 velocity layers.
const EXPECTED_SAMPLES: usize = 90;
/// The budget from the spec. Exceeding it is a regression worth failing on.
const MEMORY_BUDGET_BYTES: usize = 400 * 1_000_000;

const A0: u8 = 21;
const C4: u8 = 60;
const C8: u8 = 108;

/// Equal temperament, A4 = 440 Hz.
fn key_hz(key: u8) -> f64 {
    440.0 * 2.0_f64.powf((f64::from(key) - 69.0) / 12.0)
}

/// The installed piano, or `None` when the assets are not there.
///
/// Loaded once for the whole file: decoding 222 MB takes about nine seconds,
/// and eight tests each doing it would make this the slowest thing in the
/// workspace by an order of magnitude.
fn piano() -> Option<Arc<SampleBank>> {
    static PIANO: std::sync::OnceLock<Option<Arc<SampleBank>>> = std::sync::OnceLock::new();
    PIANO
        .get_or_init(|| {
            // The same resolution the plugin uses, rather than a path relative
            // to this crate: it honours `MYDAW_ASSETS_DIR`, so the skip path is
            // itself testable, and it cannot drift from where the app looks.
            let dir = mydaw_plugin_sampler::instruments_root().join(INSTRUMENT_ID);
            if !dir.join("manifest.json").is_file() {
                eprintln!(
                    "skipping: no assets at {} -- run scripts/fetch-assets.ps1",
                    dir.display()
                );
                return None;
            }
            Some(load_instrument(&dir).expect("the installed piano must load"))
        })
        .clone()
}

/// Render one note and return the whole tail.
fn play(bank: &Arc<SampleBank>, key: u8, velocity: f32, frames: usize) -> Vec<f32> {
    let mut node = SamplerNode::new(Arc::clone(bank), SR);
    let (left, _) = render(
        &mut node,
        frames,
        512,
        &[NoteEvent::note_on(0, key, velocity)],
    );
    left
}

#[test]
fn the_installed_piano_loads_whole_and_inside_its_memory_budget() {
    let Some(bank) = piano() else { return };

    eprintln!("--- salamander-piano ---");
    eprintln!("  samples loaded : {}", bank.len());
    eprintln!("  samples skipped: {}", bank.skipped().len());
    eprintln!(
        "  decoded memory : {:.1} MB",
        bank.bytes() as f64 / 1_000_000.0
    );
    eprintln!("  key range      : {:?}", bank.key_range());
    eprintln!("  release        : {} ms", bank.release_ms());
    eprintln!("  attribution    : {}", bank.attribution());

    assert_eq!(
        bank.len(),
        EXPECTED_SAMPLES,
        "every sample must have loaded"
    );
    assert!(
        bank.skipped().is_empty(),
        "nothing may be missing: {:?}",
        bank.skipped()
    );
    assert!(
        bank.bytes() < MEMORY_BUDGET_BYTES,
        "the piano is {} bytes, over the {MEMORY_BUDGET_BYTES} budget",
        bank.bytes()
    );
    assert_eq!(
        bank.key_range(),
        Some((A0, C8)),
        "an 88-key piano spans A0 to C8"
    );
    assert!(
        !bank.attribution().is_empty(),
        "CC-BY content must carry its attribution into the runtime"
    );
}

/// Every key of the 88 must produce a sample, at every velocity. A hole here is
/// a dead key on the keyboard.
#[test]
fn every_key_of_the_eighty_eight_answers_at_every_velocity() {
    let Some(bank) = piano() else { return };
    for key in A0..=C8 {
        for velocity in [0_u8, 1, 54, 55, 88, 89, 127] {
            assert!(
                bank.sample_for(key, velocity).is_some(),
                "no sample for key {key} at velocity {velocity}"
            );
        }
    }
    // And nothing outside the piano's range, rather than a stretched neighbour.
    assert!(bank.sample_for(A0 - 1, 64).is_none());
    assert!(bank.sample_for(C8 + 1, 64).is_none());
}

/// The measurement that says the pitch mapping is right on real content:
/// middle C must come out at 261.6 Hz.
#[test]
fn middle_c_comes_out_at_middle_c() {
    let Some(bank) = piano() else { return };

    eprintln!("--- pitch ---");
    eprintln!(
        "{:>5}  {:>10}  {:>10}  {:>8}",
        "key", "expected", "measured", "cents"
    );
    // A root-key sample (C4 is a recorded note) and two that are pitch-shifted
    // from a neighbour, so the shifting path is measured too.
    for key in [C4, C4 + 1, C4 - 1, 69, 48, 72] {
        let expected = key_hz(key);
        let left = play(&bank, key, 0.8, 32_768);
        let peak = spectrum(&left, SR)
            .dominant()
            .expect("a piano note must have energy");
        // The loudest partial of a piano is usually the fundamental, but in the
        // bass it can be the second harmonic; fold that in rather than pretend.
        let measured = if peak.frequency_hz > expected * 1.5 {
            peak.frequency_hz / 2.0
        } else {
            peak.frequency_hz
        };
        let cents = 1200.0 * (measured / expected).log2();
        eprintln!("{key:>5}  {expected:>10.2}  {measured:>10.2}  {cents:>+8.1}");
        assert!(
            cents.abs() < 25.0,
            "key {key}: {measured:.2} Hz is {cents:+.1} cents from {expected:.2} Hz"
        );
    }
}

/// A piano note starts loud and decays. Anything else is not a piano.
#[test]
fn a_piano_note_decays_and_does_not_clip() {
    let Some(bank) = piano() else { return };

    // Three seconds, held: no note-off, so this is the recording's own decay.
    let left = play(&bank, C4, 1.0, (SR * 3.0) as usize);
    let windows: Vec<f32> = left.chunks((SR * 0.25) as usize).map(rms).collect();

    eprintln!("--- decay (250 ms RMS windows, C4 at velocity 127) ---");
    for (index, level) in windows.iter().enumerate() {
        eprintln!("  {:>4.2}s  {:.5}", index as f32 * 0.25, level);
    }

    let loudest = peak(&left);
    eprintln!("  peak: {loudest:.4}");
    assert!(loudest < 1.0, "a single note clipped at {loudest}");
    assert!(
        loudest > 0.05,
        "a single note should be audible, got {loudest}"
    );

    assert!(
        windows[0] > windows[2],
        "the note must decay after its attack"
    );
    assert!(
        windows.last().unwrap() < &(windows[0] * 0.5),
        "after 3 s it should be well down from the attack: {:?}",
        windows
    );
    assert!(
        left.iter().all(|s| s.is_finite()),
        "no sample may be non-finite"
    );
}

/// Velocity has to do something audible, and in the right direction.
#[test]
fn velocity_changes_the_level_and_the_layer() {
    let Some(bank) = piano() else { return };

    eprintln!("--- velocity ---");
    let mut levels = Vec::new();
    for velocity in [0.3_f32, 0.6, 1.0] {
        let left = play(&bank, C4, velocity, (SR * 1.0) as usize);
        let level = rms(&left);
        let db = 20.0 * level.max(1e-9).log10();
        eprintln!("  velocity {velocity:.1}  rms {level:.5}  ({db:>6.1} dBFS)");
        levels.push(level);
    }

    assert!(
        levels[1] > levels[0] && levels[2] > levels[1],
        "level must rise with velocity: {levels:?}"
    );
    let contrast_db = 20.0 * (levels[2] / levels[0]).log10();
    eprintln!("  contrast 0.3 -> 1.0: {contrast_db:.1} dB");
    assert!(
        contrast_db > 6.0,
        "a piano needs real dynamics; 0.3 to 1.0 gave only {contrast_db:.1} dB"
    );

    // And the two ends must be different *recordings*, not one scaled.
    let soft = bank.sample_for(C4, 38).expect("a soft layer");
    let loud = bank.sample_for(C4, 127).expect("a loud layer");
    assert!(
        !std::ptr::eq(soft, loud),
        "velocity 0.3 and 1.0 must select different velocity layers"
    );
}

/// A released key must go to silence, and the voice must be freed.
#[test]
fn releasing_a_piano_key_reaches_digital_silence() {
    let Some(bank) = piano() else { return };
    let mut node = SamplerNode::new(bank, SR);

    render(&mut node, 24_000, 512, &[NoteEvent::note_on(0, C4, 1.0)]);
    assert_eq!(node.active_voices(), 1);

    // The manifest's release is 400 ms; render a second.
    render(&mut node, 48_000, 512, &[NoteEvent::note_off(0, C4)]);
    assert_eq!(node.active_voices(), 0, "the voice must free itself");

    let (silence, _) = render(&mut node, 4_800, 512, &[]);
    assert!(
        silence.iter().all(|s| *s == 0.0),
        "a released piano must be digitally silent, peaked at {}",
        peak(&silence)
    );
}

/// The truncation the memory budget buys must not be audible as a click: the
/// baked fade means the sample ends in silence, not in a step.
#[test]
fn the_truncated_tail_fades_out_rather_than_stopping_dead() {
    let Some(bank) = piano() else { return };
    // `max_seconds` is 7.0 with a 1 s fade, so the last second must decline.
    let left = play(&bank, C4, 1.0, (SR * 7.5) as usize);

    let windows: Vec<f32> = left.chunks((SR * 0.25) as usize).map(rms).collect();
    let tail = &windows[24..28]; // 6.0 s to 7.0 s
    eprintln!("--- truncation fade (250 ms windows, 6.0-7.0 s) ---");
    eprintln!("  {tail:?}");
    assert!(
        tail.windows(2).all(|pair| pair[1] <= pair[0]),
        "the baked fade must decline monotonically: {tail:?}"
    );

    // Past 7 s there is nothing at all.
    let after: f32 = peak(&left[(SR * 7.05) as usize..]);
    assert_eq!(after, 0.0, "nothing may sound past the truncation point");
}

/// A chord of real piano notes: three fundamentals present, no clipping.
#[test]
fn a_c_major_chord_puts_all_three_notes_into_the_spectrum() {
    let Some(bank) = piano() else { return };
    let mut node = SamplerNode::new(bank, SR);

    let events: Vec<NoteEvent> = [60_u8, 64, 67]
        .iter()
        .map(|key| NoteEvent::note_on(0, *key, 0.8))
        .collect();
    let (left, _) = render(&mut node, 32_768, 512, &events);

    let spectrum = spectrum(&left, SR);
    let peaks = spectrum.top_peaks(24);
    eprintln!("--- C major chord ---");
    for key in [60_u8, 64, 67] {
        let expected = key_hz(key);
        // Within a semitone: partials of the other notes crowd the spectrum,
        // and a wider window would not distinguish them.
        let found = peaks
            .iter()
            .find(|peak| (peak.frequency_hz / expected).log2().abs() < 1.0 / 24.0);
        eprintln!(
            "  key {key:>3} expected {expected:>7.2} Hz -> {}",
            found.map_or("MISSING".to_owned(), |p| format!(
                "{:.2} Hz at {:.1} dBFS",
                p.frequency_hz, p.magnitude_dbfs
            ))
        );
        assert!(
            found.is_some(),
            "key {key} ({expected:.2} Hz) is not in the chord"
        );
    }
    let loudest = peak(&left);
    eprintln!("  peak: {loudest:.4}");
    assert!(loudest < 1.0, "the chord clipped at {loudest}");
}
