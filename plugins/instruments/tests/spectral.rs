//! What the instruments actually *sound* like, measured.
//!
//! "The kick sounds like a kick" is not a test result. These are, and they are
//! the claims a listener would make, expressed as numbers a machine can check:
//!
//! | Claim | Measurement |
//! | --- | --- |
//! | the kick is low | dominant partial below 100 Hz, and most of the energy with it |
//! | the kick decays | RMS strictly falling across successive windows |
//! | the snare is broadband | high spectral flatness, and real energy above 1 kHz |
//! | the hats are metal | spectral centroid above 4 kHz |
//! | a closed hat chokes an open one | the open hat's energy is gone 10 ms later |
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
use mydaw_plugin_instruments::drum_kit::{
    CLAP_BAND_HIGH_HZ, CLAP_BAND_LOW_HZ, CLAP_BURSTS, CLAP_SPACING_MS, CLOSED_HAT_KEY, KICK_END_HZ,
    KICK_KEY, OPEN_HAT_KEY, SNARE_KEY, SNARE_TONE_HZ,
};
use mydaw_plugin_instruments::poly_synth::SUB_MIX;
use mydaw_plugin_instruments::test_support::{peak, render, rms};
use mydaw_plugin_instruments::{key_to_hz, DrumKitNode, PolySynthNode, SingleDrum, SingleDrumNode};
use mydaw_rt_core::NoteEvent;

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

/// Render one drum hit and return the left channel.
fn hit(key: u8, frames: usize) -> Vec<f32> {
    let mut node = DrumKitNode::new(SR);
    render(&mut node, frames, BLOCK, &[NoteEvent::note_on(0, key, 1.0)]).0
}

/// Render one hit of a per-drum instrument, at its own base key.
fn single(drum: SingleDrum, frames: usize) -> Vec<f32> {
    let mut node = SingleDrumNode::new(drum, SR);
    render(
        &mut node,
        frames,
        BLOCK,
        &[NoteEvent::note_on(0, drum.base_key(), 1.0)],
    )
    .0
}

/// Sign changes per second: a frequency estimate that needs no window.
///
/// The FFT cannot answer "what pitch is the first eight milliseconds" -- eight
/// milliseconds is two bins wide -- and the kick's attack is precisely that
/// short. Counting zero crossings can.
fn crossing_hz(samples: &[f32]) -> f64 {
    let crossings = samples
        .windows(2)
        .filter(|pair| (pair[0] < 0.0) != (pair[1] < 0.0))
        .count();
    crossings as f64 / 2.0 / (samples.len() as f64 / SR)
}

/// Sum of squared magnitudes in `[low, high)` Hz, excluding DC.
fn band_energy(spectrum: &Spectrum, low: f64, high: f64) -> f64 {
    spectrum
        .magnitudes
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(bin, _)| {
            let hz = spectrum.frequency_of(*bin);
            hz >= low && hz < high
        })
        .map(|(_, magnitude)| f64::from(*magnitude) * f64::from(*magnitude))
        .sum()
}

/// Total energy above DC.
fn total_energy(spectrum: &Spectrum) -> f64 {
    band_energy(spectrum, 0.0, f64::INFINITY)
}

/// Power-weighted mean frequency: where the spectrum's centre of mass sits.
///
/// The standard "brightness" descriptor, and the one number that separates a
/// hi-hat from a kick without knowing anything else about either.
fn spectral_centroid(spectrum: &Spectrum) -> f64 {
    let mut weighted = 0.0_f64;
    let mut total = 0.0_f64;
    for (bin, magnitude) in spectrum.magnitudes.iter().enumerate().skip(1) {
        let power = f64::from(*magnitude) * f64::from(*magnitude);
        weighted += spectrum.frequency_of(bin) * power;
        total += power;
    }
    if total > 0.0 {
        weighted / total
    } else {
        0.0
    }
}

/// Whether a spectral peak sits within `tolerance` Hz of `hz`.
fn has_peak_near(spectrum: &Spectrum, hz: f64, tolerance: f64, count: usize) -> bool {
    spectrum
        .top_peaks(count)
        .iter()
        .any(|peak| (peak.frequency_hz - hz).abs() <= tolerance)
}

// ---------------------------------------------------------------------------
// Drum kit
// ---------------------------------------------------------------------------

#[test]
fn the_kick_puts_its_energy_below_a_hundred_hertz() {
    // Half a second: the whole body of the kick, sweep and all.
    let samples = hit(KICK_KEY, 24_000);
    let spectrum = spectrum(&samples, SR);

    let dominant = spectrum.dominant().expect("a kick has a fundamental");
    let low = band_energy(&spectrum, 0.0, 100.0);
    let share = low / total_energy(&spectrum);
    eprintln!(
        "[kick] dominant {:.1} Hz ({:.1} dBFS), {:.1}% of the energy below 100 Hz, peak {:.3}",
        dominant.frequency_hz,
        dominant.magnitude_dbfs,
        share * 100.0,
        peak(&samples)
    );

    assert!(
        dominant.frequency_hz < 100.0,
        "the kick's loudest partial is at {:.1} Hz; it swept to 50 Hz and should \
         have stayed there",
        dominant.frequency_hz
    );
    assert!(
        share > 0.8,
        "only {:.1}% of the kick's energy is below 100 Hz",
        share * 100.0
    );
}

#[test]
fn the_kick_decays_instead_of_sustaining() {
    let samples = hit(KICK_KEY, 24_000);
    // 50 ms windows across the first 300 ms: past the pitch sweep, well inside
    // the amplitude envelope.
    let window = (SR * 0.05) as usize;
    let levels: Vec<f32> = (0..6)
        .map(|i| rms(&samples[i * window..(i + 1) * window]))
        .collect();
    eprintln!("[kick] 50 ms RMS windows: {levels:?}");

    for pair in levels.windows(2) {
        assert!(
            pair[1] < pair[0],
            "the kick's envelope must fall monotonically, got {levels:?}"
        );
    }
    assert!(
        levels[5] < levels[0] * 0.1,
        "after 300 ms the kick should be at least 20 dB down, got {levels:?}"
    );
}

#[test]
fn the_kick_starts_far_above_where_it_lands() {
    // The punch. One sweep can be the beater or the body and not both, so the
    // attack is measured on its own: the first eight milliseconds have to be
    // several times the frequency of the tail, or there is no attack there.
    let samples = hit(KICK_KEY, 24_000);
    let attack = crossing_hz(&samples[..(SR * 0.008) as usize]);
    let body = crossing_hz(&samples[(SR * 0.15) as usize..(SR * 0.30) as usize]);
    eprintln!("[kick] attack {attack:.0} Hz, body {body:.0} Hz");

    assert!(
        attack > 200.0,
        "the kick's attack is at {attack:.0} Hz; it should start near \
         {KICK_PUNCH_HZ_DOC} Hz and fall from there"
    );
    assert!(
        attack > body * 3.0,
        "the attack ({attack:.0} Hz) is barely above the body ({body:.0} Hz); \
         the punch sweep is missing"
    );
    assert!(
        (body - f64::from(KICK_END_HZ)).abs() < 15.0,
        "the kick should land on {KICK_END_HZ} Hz, not {body:.0} Hz"
    );
}

/// Named for the message above; the constant itself is the DSP's.
const KICK_PUNCH_HZ_DOC: f64 = 420.0;

// The kick's soft clipper is measured as arithmetic rather than as a spectrum:
// see `drum_kit::tests::the_soft_clipper_bends_without_moving_the_ends`.
// Saturation is amplitude-dependent, so on a decaying drum every spectral
// measurement of it is really a measurement of where the window was put.

#[test]
fn a_clap_is_several_bursts_and_not_one() {
    // What makes a clap a clap. Three impacts a few milliseconds apart, then
    // the room: an envelope with one peak is a snare with the wires off.
    let samples = single(SingleDrum::Clap, 24_000);
    // 2 ms windows across the first 45 ms -- fine enough to separate bursts
    // 10 ms apart, coarse enough not to count individual noise samples.
    let window = (SR * 0.002) as usize;
    let levels: Vec<f32> = (0..22)
        .map(|i| rms(&samples[i * window..(i + 1) * window]))
        .collect();
    let rises = levels
        .windows(3)
        .filter(|w| w[1] > w[0] * 1.3 && w[1] >= w[2])
        .count();
    eprintln!("[clap] 2 ms RMS windows: {levels:?} -> {rises} attacks");

    assert!(
        rises >= CLAP_BURSTS - 1,
        "a clap of {CLAP_BURSTS} bursts {CLAP_SPACING_MS} ms apart should show \
         at least {} attacks after the first; found {rises} in {levels:?}",
        CLAP_BURSTS - 1
    );
    // And it has to outlive them: the tail is the room the hands are in.
    let tail = rms(&samples[(SR * 0.08) as usize..(SR * 0.15) as usize]);
    assert!(tail > 1e-4, "the clap has no tail at all: {tail:.3e}");
}

#[test]
fn a_clap_sits_in_the_band_hands_do() {
    let samples = single(SingleDrum::Clap, 24_000);
    let spectrum = spectrum(&samples, SR);
    let inside = band_energy(
        &spectrum,
        f64::from(CLAP_BAND_LOW_HZ) * 0.5,
        f64::from(CLAP_BAND_HIGH_HZ) * 1.5,
    ) / total_energy(&spectrum);
    let centroid = spectral_centroid(&spectrum);
    eprintln!(
        "[clap] centroid {centroid:.0} Hz, {:.1}% inside the band, peak {:.3}",
        inside * 100.0,
        peak(&samples)
    );

    assert!(
        inside > 0.7,
        "only {:.1}% of the clap is in its band; the filters are not working",
        inside * 100.0
    );
    assert!(
        (800.0..6_000.0).contains(&centroid),
        "a centroid of {centroid:.0} Hz is a kick or a hi-hat, not a clap"
    );
}

#[test]
fn the_snare_is_broadband_with_real_energy_above_a_kilohertz() {
    // 150 ms: the noise burst itself, before the tail runs out of signal to
    // measure.
    let samples = hit(SNARE_KEY, 7_200);
    let spectrum = spectrum(&samples, SR);

    let above = band_energy(&spectrum, 1_000.0, f64::INFINITY) / total_energy(&spectrum);
    let flatness = spectrum.flatness();
    eprintln!(
        "[snare] flatness {flatness:.3}, centroid {:.0} Hz, {:.1}% of the energy above 1 kHz, peak {:.3}",
        spectral_centroid(&spectrum),
        above * 100.0,
        peak(&samples)
    );

    assert!(
        above > 0.2,
        "only {:.1}% of the snare is above 1 kHz; the wires are missing",
        above * 100.0
    );
    assert!(
        flatness > 0.1,
        "a snare must read as broadband, got a flatness of {flatness}"
    );
    // And the body tone is in there too, or it is just a noise burst.
    assert!(
        has_peak_near(&spectrum, f64::from(SNARE_TONE_HZ), 40.0, 8),
        "the {SNARE_TONE_HZ} Hz body tone is missing from the spectrum: {:?}",
        spectrum
            .top_peaks(8)
            .iter()
            .map(|p| p.frequency_hz.round())
            .collect::<Vec<_>>()
    );
}

#[test]
fn both_hats_sit_high_in_the_spectrum() {
    for (label, key, frames) in [
        ("closed hat", CLOSED_HAT_KEY, 4_800),
        ("open hat", OPEN_HAT_KEY, 24_000),
    ] {
        let samples = hit(key, frames);
        let spectrum = spectrum(&samples, SR);
        let centroid = spectral_centroid(&spectrum);
        let below = band_energy(&spectrum, 0.0, 1_000.0) / total_energy(&spectrum);
        eprintln!(
            "[{label}] centroid {centroid:.0} Hz, flatness {:.3}, {:.2}% below 1 kHz, peak {:.3}",
            spectrum.flatness(),
            below * 100.0,
            peak(&samples)
        );

        assert!(
            centroid > 4_000.0,
            "{label}: a centroid of {centroid:.0} Hz is not a hi-hat"
        );
        assert!(
            below < 0.05,
            "{label}: {:.1}% of the energy is below 1 kHz; the highpass is not working",
            below * 100.0
        );
    }
}

#[test]
fn the_open_hat_is_gone_ten_milliseconds_after_a_closed_hat() {
    // A choke has to be measured against the alternative: the same hat, at the
    // same point in its decay, with nothing closing it.
    let ring = |choke: bool| {
        let mut node = DrumKitNode::new(SR);
        render(
            &mut node,
            2_400,
            BLOCK,
            &[NoteEvent::note_on(0, OPEN_HAT_KEY, 1.0)],
        );
        let events: &[NoteEvent] = if choke {
            // Velocity 0, so the closed hat itself contributes nothing to the
            // measurement and only its choke is visible.
            &[NoteEvent::note_on(0, CLOSED_HAT_KEY, 0.0)]
        } else {
            &[]
        };
        render(&mut node, 480, 480, events).0
    };

    let choked = ring(true);
    let ringing = ring(false);
    // The last millisecond of the 10 ms window.
    let (choked_tail, ringing_tail) = (rms(&choked[432..]), rms(&ringing[432..]));
    eprintln!("[choke] tail RMS choked {choked_tail:.3e} vs ringing {ringing_tail:.3e}");

    assert!(
        ringing_tail > 1e-3,
        "the unchoked hat should still be ringing, got {ringing_tail:.3e}"
    );
    assert!(
        choked_tail < ringing_tail * 0.01,
        "40 dB is the least a choke should take off; got {choked_tail:.3e} against \
         {ringing_tail:.3e}"
    );
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
