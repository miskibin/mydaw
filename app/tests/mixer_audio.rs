//! The mixer, measured through the sound card.
//!
//! `app/tests/mixer_graph.rs` proves the mixer is right in an offline render.
//! This proves it is right *after the audio device has had it*: a real WASAPI
//! stream, a real callback at whatever block size the driver asked for, two
//! tracks summed by the RT graph, and a capture taken from the same tap the
//! debug harness uses.
//!
//! The claim is the one a mixer exists to make, in three parts:
//!
//! 1. a beat on track 0 and a held chord on track 1 are **both** in the output,
//!    each identifiable by its own band of the spectrum;
//! 2. **muting** track 1 removes the chord and leaves the beat exactly where it
//!    was -- not "quieter", not "mostly gone";
//! 3. **panning** track 1 hard left removes the chord from the right channel and
//!    leaves it in the left, while the centred beat stays in both.
//!
//! Bands rather than levels, because a level tells you something got quieter and
//! a band tells you *which track* did. The kit's kick sweeps 150 -> 50 Hz and
//! nothing else in this test has energy down there; the chord's three partials
//! sit at 261.6 / 329.6 / 392.0 Hz and nothing else in this test has energy
//! there either. The pattern is kick-only for exactly that reason -- a hi-hat is
//! broadband and would put energy in the chord's band.
//!
//! `#[ignore]`d: it opens the audio device and is audible, so
//! `cargo test --workspace` must stay silent and pass on a machine with no
//! sound card.
//!
//! ```sh
//! cargo test -p mydaw-app --test mixer_audio -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Every capture leaves a WAV, an annotated PNG and a stats JSON under
//! `.debug/captures/<timestamp>/`, and the paths are printed -- so a failure can
//! be looked at rather than guessed at.

use std::thread::sleep;
use std::time::Duration;

use mydaw_app::{boot_kernel, capture_root, capture_to_disk, AppState, CaptureOutcome};
use mydaw_plugin_debug_tap::{spectrum, Spectrum};
use mydaw_plugin_sequencer::{Pattern, StepRow};

/// Tempo the beat runs at. The engine's default, and a dance tempo.
const BPM: f64 = 130.0;

/// Seconds each measured capture runs for. Long enough for two bars at 130 BPM
/// (~3.7 s is four; 1.5 s is a bar and a half) and for an FFT with ~1.5 Hz bins.
const CAPTURE_SECONDS: f32 = 1.5;

/// Blocks of settling before a capture, so it starts inside steady-state
/// playback rather than on the device's first block after a change.
const SETTLE: Duration = Duration::from_millis(500);

/// The kick's band. The kit's kick sweeps 150 -> 50 Hz.
const KICK_BAND: (f64, f64) = (30.0, 140.0);

/// The chord's band: C4 261.63, E4 329.63, G4 392.00 Hz.
const CHORD_BAND: (f64, f64) = (240.0, 420.0);

/// The chord the synth holds, as MIDI keys.
const CHORD: [u8; 3] = [60, 64, 67];

/// Fundamentals the chord must show, in Hz.
const CHORD_PARTIALS: [f64; 3] = [261.63, 329.63, 392.00];

/// Tolerance on a measured partial, in Hz. The FFT's bin spacing at these
/// lengths is a couple of Hz and the peaks are parabolically interpolated.
const HZ_TOLERANCE: f64 = 4.0;

/// How far above the beat-only floor the chord has to sit to count as present,
/// in dB.
///
/// Measured against a *reference capture of the beat alone* rather than against
/// an absolute number, because a kick drum's attack is broadband: it puts a
/// floor of its own into the chord's band, and "the chord is gone" can only mean
/// "the band is back down to that floor". On this machine the chord sits ~21 dB
/// above it, so 15 is a comfortable bar that is still far more than a ghost.
const PRESENT_DB: f32 = 15.0;

/// How much a band that must *not* have changed is allowed to move, in dB.
///
/// The kick is a transient measured over a window that does not align to the
/// grid, so the same beat measured twice differs by a fraction of a dB. Three is
/// generous for that and far tighter than any real change would be.
const UNCHANGED_DB: f32 = 3.0;

/// Energy in a frequency band, as dBFS.
///
/// The root of the summed squared magnitudes, which is the band's contribution
/// to the signal's RMS -- so two captures of the same source at the same level
/// give the same number regardless of how many bins the band happens to span.
fn band_dbfs(spectrum: &Spectrum, band: (f64, f64)) -> f32 {
    if spectrum.is_empty() || spectrum.bin_hz <= 0.0 {
        return -200.0;
    }
    let low = (band.0 / spectrum.bin_hz).floor().max(1.0) as usize;
    let high = ((band.1 / spectrum.bin_hz).ceil() as usize).min(spectrum.magnitudes.len() - 1);
    if low > high {
        return -200.0;
    }
    let energy: f64 = spectrum.magnitudes[low..=high]
        .iter()
        .map(|m| f64::from(*m) * f64::from(*m))
        .sum();
    if energy <= 0.0 {
        return -200.0;
    }
    20.0 * energy.sqrt().log10() as f32
}

/// Whether the spectrum has a local maximum within [`HZ_TOLERANCE`] of `hz`.
fn has_partial(spectrum: &Spectrum, hz: f64) -> bool {
    spectrum
        .top_peaks(24)
        .iter()
        .any(|peak| (peak.frequency_hz - hz).abs() <= HZ_TOLERANCE)
}

/// One measured capture: both channels' spectra, plus the paths on disk.
struct Measured {
    outcome: CaptureOutcome,
    left: Spectrum,
    right: Spectrum,
}

impl Measured {
    fn take(state: &AppState, label: &str) -> Self {
        let outcome = capture_to_disk(state.debug_tap(), CAPTURE_SECONDS, &capture_root())
            .expect("the engine is running, so a capture must arrive");
        let rate = outcome.stats.sample_rate;
        let left = spectrum(&outcome.capture.channel(0), rate);
        let right = spectrum(&outcome.capture.channel(1), rate);

        let measured = Self {
            outcome,
            left,
            right,
        };
        measured.report(label);
        measured
    }

    /// One line an agent can read, per channel.
    fn report(&self, label: &str) {
        let stats = &self.outcome.stats;
        eprintln!(
            "[{label}] {} frames @ {} Hz ({:.3} s) | dropped {} | fft {} ({:.2} Hz/bin)",
            stats.frames,
            stats.sample_rate,
            stats.duration_seconds,
            self.outcome.capture.dropped,
            stats.fft_size,
            stats.frequency_resolution_hz,
        );
        for (spectrum, channel) in [&self.left, &self.right]
            .into_iter()
            .zip(&stats.per_channel)
        {
            eprintln!(
                "[{label}]   {}  peak {:.4} ({:.2} dBFS)  rms {:.5}  kick {:.2} dB  chord {:.2} dB",
                channel.label,
                channel.peak,
                channel.peak_dbfs,
                channel.rms,
                band_dbfs(spectrum, KICK_BAND),
                band_dbfs(spectrum, CHORD_BAND),
            );
        }
        eprintln!("[{label}]   wav  {}", self.outcome.artifacts.wav.display());
        eprintln!("[{label}]   png  {}", self.outcome.artifacts.png.display());
        eprintln!(
            "[{label}]   json {}",
            self.outcome.artifacts.stats_json.display()
        );
    }

    fn kick(&self, channel: usize) -> f32 {
        band_dbfs(self.spectrum(channel), KICK_BAND)
    }

    fn chord(&self, channel: usize) -> f32 {
        band_dbfs(self.spectrum(channel), CHORD_BAND)
    }

    fn spectrum(&self, channel: usize) -> &Spectrum {
        if channel == 0 {
            &self.left
        } else {
            &self.right
        }
    }
}

/// Kick on every beat, and nothing else. See the module docs for why the hats
/// are not in it.
fn kick_only() -> Pattern {
    let mut pattern = Pattern::with_channels(1, "Pattern 1", &[0, 1, 2, 3, 4], |channel| {
        mydaw_plugin_sequencer::DEFAULT_ROW_KEYS
            .get(channel as usize)
            .copied()
            .unwrap_or(mydaw_plugin_sequencer::DEFAULT_STEP_KEY)
    });
    let kick = pattern.channels[0].steps.as_ref().unwrap().key;
    pattern.channels[0].steps = Some(StepRow::empty(kick).with_steps(&[0, 4, 8, 12]));
    pattern
}

#[test]
#[ignore = "opens the audio device and plays an audible beat and chord"]
fn two_tracks_sum_mute_and_pan_through_the_device() {
    let kernel = boot_kernel().expect("kernel boots");
    let state = AppState::from_kernel(&kernel).expect("services present");

    // The shell's own project: drums on track 0, a synth on track 1. Exactly
    // what the window opens on.
    let info = match state.start_audio_with_project() {
        Ok(info) => info,
        Err(err) => {
            eprintln!("no audio device ({err}); nothing to verify on this machine");
            return;
        }
    };
    eprintln!(
        "device: {} @ {} Hz, {} ch (max block {})",
        info.device_name, info.sample_rate, info.channels, info.max_block
    );
    let mixer = state.mixer_state();
    assert_eq!(mixer.tracks.len(), 2, "the default project is two tracks");
    assert_eq!(mixer.tracks[0].source_id, "drum-kit");
    assert_eq!(mixer.tracks[1].source_id, "poly-synth");

    // --- programme the beat on track 0 --------------------------------------
    assert_eq!(state.set_bpm(BPM).expect("the tempo is legal"), BPM);
    state.set_pattern(kick_only()).expect("the beat is valid");
    // A sequencer row's channel *is* its track: the four default rows are on
    // channel 0, so they play track 0's kit.
    assert!(state
        .sequencer()
        .active_pattern()
        .channels
        .iter()
        .all(|row| row.channel == 0));

    // --- hold the chord on track 1 ------------------------------------------
    // Live notes go to the selected track, so selecting it is how the keyboard
    // is pointed at the synth.
    state.set_selected_track(1).expect("track 1 exists");
    assert_eq!(state.current_source().id(), "poly-synth");

    // ============================================== 0. the beat-only reference
    // A kick's attack is broadband, so it puts a floor into the chord's band.
    // Everything below is measured against *this* rather than against zero:
    // "the chord is gone" means "that band is back down to the beat's own
    // floor", which is the only statement the physics allows.
    state
        .set_track_muted(1, true)
        .expect("track 1 can be muted");
    state.set_playing(true).expect("the transport starts");
    for key in CHORD {
        state.note_on(key, 0.9).expect("the note reaches the queue");
    }
    sleep(SETTLE);
    let beat_only = Measured::take(&state, "beat-only");
    let kick_floor = beat_only.kick(0);
    let chord_floor = beat_only.chord(0);
    for partial in CHORD_PARTIALS {
        assert!(
            !has_partial(&beat_only.left, partial),
            "the muted synth is audible at {partial} Hz before it was ever unmuted"
        );
    }

    // ================================================================ 1. both
    state
        .set_track_muted(1, false)
        .expect("track 1 can be unmuted");
    sleep(SETTLE);
    let both = Measured::take(&state, "both");
    assert_eq!(
        both.outcome.capture.dropped, 0,
        "the capture has holes; its bands cannot be trusted"
    );
    assert_eq!(
        both.outcome.stats.per_channel[0].non_finite, 0,
        "a NaN reached the device"
    );
    assert!(
        !both.outcome.stats.has_clipping(),
        "a kick and a three-note chord must not clip the sum"
    );

    for partial in CHORD_PARTIALS {
        assert!(
            has_partial(&both.left, partial),
            "the chord's {partial} Hz partial is missing; peaks were {:?}",
            both.left
                .top_peaks(8)
                .iter()
                .map(|p| p.frequency_hz.round())
                .collect::<Vec<_>>()
        );
    }
    let kick_both = both.kick(0);
    let chord_both = both.chord(0);
    assert!(
        chord_both - chord_floor >= PRESENT_DB,
        "the chord should stand {PRESENT_DB} dB clear of the beat's own floor, but it is \
         only {:.2} dB up ({chord_floor:.2} -> {chord_both:.2})",
        chord_both - chord_floor
    );
    assert!(
        (kick_both - kick_floor).abs() <= UNCHANGED_DB,
        "adding the synth moved the kick by {:.2} dB ({kick_floor:.2} -> {kick_both:.2})",
        (kick_both - kick_floor).abs()
    );

    // ================================================================ 2. mute
    state
        .set_track_muted(1, true)
        .expect("track 1 can be muted");
    sleep(SETTLE);
    let muted = Measured::take(&state, "muted");

    let chord_residual = muted.chord(0) - chord_floor;
    assert!(
        chord_residual.abs() <= UNCHANGED_DB,
        "muting the synth should put the chord's band back on the beat's floor, but it \
         sits {chord_residual:+.2} dB off it ({chord_floor:.2} -> {:.2})",
        muted.chord(0)
    );
    let kick_change = (muted.kick(0) - kick_both).abs();
    assert!(
        kick_change <= UNCHANGED_DB,
        "muting the synth moved the kick by {kick_change:.2} dB ({kick_both:.2} -> {:.2}); \
         a mute must touch one track only",
        muted.kick(0)
    );
    for partial in CHORD_PARTIALS {
        assert!(
            !has_partial(&muted.left, partial),
            "the chord's {partial} Hz partial survived the mute"
        );
    }

    // ================================================================= 3. pan
    state
        .set_track_muted(1, false)
        .expect("track 1 can be unmuted");
    state
        .set_track_pan(1, -1.0)
        .expect("hard left is a legal pan");
    sleep(SETTLE);
    let panned = Measured::take(&state, "panned-left");

    let chord_left = panned.chord(0);
    let chord_right = panned.chord(1);
    assert!(
        chord_right - chord_floor <= UNCHANGED_DB,
        "hard left should leave the right channel with nothing but the beat's own floor, \
         but its chord band is {:+.2} dB above it (floor {chord_floor:.2}, R {chord_right:.2})",
        chord_right - chord_floor
    );
    assert!(
        chord_left - chord_floor >= PRESENT_DB,
        "the left channel should keep the chord (floor {chord_floor:.2}, L {chord_left:.2})"
    );
    for partial in CHORD_PARTIALS {
        assert!(
            has_partial(&panned.left, partial),
            "the chord's {partial} Hz partial left the left channel too"
        );
        assert!(
            !has_partial(&panned.right, partial),
            "the chord's {partial} Hz partial is still in the right channel"
        );
    }

    // The kit is still centred, so the beat must be in both channels.
    let kick_spread = (panned.kick(0) - panned.kick(1)).abs();
    assert!(
        kick_spread <= UNCHANGED_DB,
        "panning the synth moved the kick's stereo balance by {kick_spread:.2} dB"
    );

    // --- tidy up ------------------------------------------------------------
    for key in CHORD {
        state.note_off(key).ok();
    }
    state.set_playing(false).ok();
    state.set_track_pan(1, 0.0).ok();

    eprintln!(
        "summary: kick  floor {kick_floor:.2} | both {kick_both:.2} | muted {:.2} | \
         panned L {:.2} R {:.2}",
        muted.kick(0),
        panned.kick(0),
        panned.kick(1)
    );
    eprintln!(
        "summary: chord floor {chord_floor:.2} | both {chord_both:.2} (+{:.2}) | muted {:.2} | \
         panned L {chord_left:.2} R {chord_right:.2} (L-R {:.2})",
        chord_both - chord_floor,
        muted.chord(0),
        chord_left - chord_right
    );
}
