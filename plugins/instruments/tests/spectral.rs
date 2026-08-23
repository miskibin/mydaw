//! What the instruments actually *sound* like, measured.
//!
//! "It sounds right" is not a test result. These are, and they are
//! the claims a listener would make, expressed as numbers a machine can check:
//!
//! | Claim | Measurement |
//! | --- | --- |
//! | A4 is A4 | dominant at 440 Hz, with harmonics at 880 and 1320 |
//! | the saw does not alias | energy off the harmonic grid is 30 dB down at the top of the keyboard |
//!
//! The analysis comes from `debug-tap`, which is the same FFT the capture
//! harness and the hardware verification use -- so a number in this file and a
//! number in `.debug/captures/*/stats.json` mean the same thing.
//!
//! Every test prints what it measured, because the point of a measurement is to
//! be read as well as asserted.

use mydaw_plugin_debug_tap::{spectrum, Spectrum};
use mydaw_plugin_instruments::poly_synth::SUB_MIX;
use mydaw_plugin_instruments::test_support::{peak, render};
use mydaw_plugin_instruments::{key_to_hz, PolySynthNode};
use mydaw_rt_core::NoteEvent;

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

/// Whether a spectral peak sits within `tolerance` Hz of `hz`.
fn has_peak_near(spectrum: &Spectrum, hz: f64, tolerance: f64, count: usize) -> bool {
    spectrum
        .top_peaks(count)
        .iter()
        .any(|peak| (peak.frequency_hz - hz).abs() <= tolerance)
}

// ---------------------------------------------------------------------------
// Poly synth
// ---------------------------------------------------------------------------

/// The sustained part of a held note: past the 5 ms attack and the 80 ms decay.
fn sustained(key: u8, frames: usize) -> Vec<f32> {
    let mut node = PolySynthNode::new(SR);
    let (left, _) = render(
        &mut node,
        frames + 9_600,
        BLOCK,
        &[NoteEvent::note_on(0, key, 1.0)],
    );
    left[9_600..].to_vec()
}

#[test]
fn a4_sounds_at_440_hz_with_the_harmonics_a_saw_should_have() {
    let samples = sustained(69, 24_000);
    let spectrum = spectrum(&samples, SR);

    let dominant = spectrum.dominant().expect("a held note has a fundamental");
    let peaks = spectrum.top_peaks(8);
    eprintln!(
        "[A4] dominant {:.2} Hz, peaks {:?}",
        dominant.frequency_hz,
        peaks
            .iter()
            .map(|p| (p.frequency_hz.round(), p.magnitude_dbfs.round()))
            .collect::<Vec<_>>()
    );

    assert!(
        (dominant.frequency_hz - 440.0).abs() < 3.0,
        "key 69 should sound at 440 Hz, measured {:.2} Hz",
        dominant.frequency_hz
    );
    // A saw has every harmonic; these two are the ones a 3 kHz lowpass leaves
    // unmistakable.
    for harmonic in [880.0, 1_320.0] {
        assert!(
            has_peak_near(&spectrum, harmonic, 4.0, 8),
            "the {harmonic} Hz harmonic is missing; this is not a sawtooth"
        );
    }
    // And the sub-oscillator, an octave down.
    assert!(
        has_peak_near(&spectrum, 220.0, 4.0, 8),
        "the sub-oscillator is missing at 220 Hz"
    );
}

#[test]
fn the_sub_oscillator_sits_six_decibels_below_the_saw() {
    let samples = sustained(69, 24_000);
    let spectrum = spectrum(&samples, SR);
    let peaks = spectrum.top_peaks(8);

    let level = |hz: f64| {
        peaks
            .iter()
            .find(|p| (p.frequency_hz - hz).abs() < 4.0)
            .unwrap_or_else(|| panic!("no peak near {hz} Hz"))
            .magnitude_dbfs
    };

    // The saw's fundamental is 2/pi of its peak-to-peak; the sub is a sine at
    // SUB_MIX. Both pass the 3 kHz lowpass essentially untouched at these
    // frequencies, so the measured difference should be the arithmetic one.
    let expected = 20.0 * (SUB_MIX / (2.0 / std::f32::consts::PI)).log10();
    let measured = level(220.0) - level(440.0);
    eprintln!("[A4] sub/saw {measured:.2} dB (arithmetic: {expected:.2} dB)");
    assert!(
        (measured - expected).abs() < 1.0,
        "the sub should sit {expected:.2} dB below the fundamental, measured {measured:.2} dB"
    );
}

/// The reason polyBLEP is in this crate at all.
///
/// # Method
/// A voice's only legitimate partials are integer multiples of the
/// sub-oscillator's frequency (`f0 / 2`): the sub contributes odd and even
/// multiples of `f0/2`, the saw contributes multiples of `f0` -- which are a
/// subset. Everything else in the spectrum is either aliasing or leakage. So
/// this sums the energy in every bin more than 60 Hz from any multiple of
/// `f0/2` (60 Hz is ~20 bins here, far outside a Hann window's main lobe and
/// well down its sidelobes) and compares it with the energy on the grid.
///
/// Key 108 is the top of a 128-key keyboard: 4186 Hz, whose 6th harmonic is
/// already past Nyquist at 48 kHz, so a naive saw has nowhere to put it but
/// back down inside the audible band.
#[test]
fn the_top_of_the_keyboard_does_not_alias() {
    const KEY: u8 = 108;
    let fundamental = f64::from(key_to_hz(KEY));
    let grid = fundamental / 2.0;

    let samples = sustained(KEY, 32_768);
    let spectrum = spectrum(&samples, SR);

    let mut on_grid = 0.0_f64;
    let mut off_grid = 0.0_f64;
    for (bin, magnitude) in spectrum.magnitudes.iter().enumerate().skip(1) {
        let hz = spectrum.frequency_of(bin);
        // Skip the very bottom, where DC leakage and the envelope's own
        // spectrum live rather than anything the oscillator made.
        if hz < 100.0 {
            continue;
        }
        let power = f64::from(*magnitude) * f64::from(*magnitude);
        let distance = (hz / grid).round().max(1.0).mul_add(-grid, hz).abs();
        if distance <= 60.0 {
            on_grid += power;
        } else {
            off_grid += power;
        }
    }

    let ratio_db = 10.0 * (off_grid / on_grid).log10();
    eprintln!(
        "[key 108] fundamental {fundamental:.1} Hz, off-grid energy {ratio_db:.1} dB below on-grid"
    );
    assert!(
        ratio_db < -30.0,
        "aliasing at the top of the keyboard is only {ratio_db:.1} dB down; \
         polyBLEP is not doing its job"
    );
}

#[test]
fn a_chord_puts_every_one_of_its_notes_into_the_spectrum() {
    const KEYS: [u8; 3] = [60, 64, 67];
    let mut node = PolySynthNode::new(SR);
    let events: Vec<NoteEvent> = KEYS
        .iter()
        .map(|k| NoteEvent::note_on(0, *k, 1.0))
        .collect();
    let (left, _) = render(&mut node, 33_600, BLOCK, &events);
    let spectrum = spectrum(&left[9_600..], SR);

    eprintln!(
        "[C major] peaks {:?}",
        spectrum
            .top_peaks(8)
            .iter()
            .map(|p| (p.frequency_hz.round(), p.magnitude_dbfs.round()))
            .collect::<Vec<_>>()
    );
    for key in KEYS {
        let expected = f64::from(key_to_hz(key));
        assert!(
            has_peak_near(&spectrum, expected, 4.0, 12),
            "key {key} ({expected:.1} Hz) is missing from the chord's spectrum"
        );
    }
    assert!(peak(&left) < 1.0, "a triad must not clip");
}

#[test]
fn a_released_note_leaves_no_spectrum_at_all() {
    let mut node = PolySynthNode::new(SR);
    render(&mut node, 9_600, BLOCK, &[NoteEvent::note_on(0, 69, 1.0)]);
    render(&mut node, 24_000, BLOCK, &[NoteEvent::note_off(0, 69)]);
    let (tail, _) = render(&mut node, 24_000, BLOCK, &[]);

    let spectrum = spectrum(&tail, SR);
    assert!(
        spectrum.dominant().is_none(),
        "a released synth must have no spectral content at all, found {:?}",
        spectrum.dominant()
    );
    assert_eq!(peak(&tail), 0.0);
}
