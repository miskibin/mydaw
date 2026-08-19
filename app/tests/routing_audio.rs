//! Buses, sends and insert chains, measured through the sound card.
//!
//! `app/tests/mixer_graph.rs` proves the routing is right in an offline render
//! and `crates/rt-core/src/graph.rs` proves the arithmetic under it. This proves
//! the **wiring**: that a bus the project describes is the bus the device plays,
//! that a send taken before the fader really is taken before the fader, and that
//! a chain of three effects on a bus survives the real callback at the real
//! block size.
//!
//! Four claims, one capture pair each:
//!
//! 1. **A send reaches a bus, and the bus's chain colours it.** A hi-hat is
//!    tapped into a bus carrying a +12 dB high shelf; the wet+dry sum has far
//!    more energy above the shelf corner than the dry reference, and no more
//!    below it than the send itself accounts for.
//! 2. **Pre-fader and post-fader are different things, with the fader down.**
//!    The same tap, the same bus, the source's fader at the bottom of its scale:
//!    pre still feeds the bus, post is silence. Two captures, two numbers.
//! 3. **A compressor on a drum bus works and says so.** Gain reduction is
//!    reported and the crest factor comes down.
//! 4. **EQ + compressor + delay on one bus runs clean.** No dropped samples, no
//!    non-finite sample, no clipping, at the device's own maximum block.
//!
//! Bands rather than levels, for the reason `mixer_audio.rs` gives: a level says
//! something changed, a band says *what*.
//!
//! `#[ignore]`d: it opens the audio device and is audible.
//!
//! ```sh
//! cargo test -p mydaw-app --test routing_audio -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Every capture leaves a WAV, an annotated PNG and a stats JSON under
//! `.debug/captures/<timestamp>/`, and the paths are printed.

use std::thread::sleep;
use std::time::Duration;

use mydaw_app::effects::InsertTarget;
use mydaw_app::{boot_kernel, capture_root, capture_to_disk, AppState, CaptureOutcome};
use mydaw_plugin_debug_tap::{spectrum, Spectrum};
use mydaw_plugin_sequencer::{Pattern, StepRow};
use mydaw_rt_core::{StripId, SILENCE_DB};

/// Tempo the beat runs at. The engine's default, and a dance tempo.
const BPM: f64 = 130.0;

/// Seconds each measured capture runs for -- a bar and a half at 130 BPM, and
/// long enough for an FFT with ~1.5 Hz bins.
const CAPTURE_SECONDS: f32 = 1.5;

/// Settling before a capture, so it starts inside steady-state playback rather
/// than on the device's first block after a change.
const SETTLE: Duration = Duration::from_millis(600);

/// Above the high shelf's corner. The shelf sits at 4 kHz, so this is entirely
/// inside the shelf's flat region and none of it is in the transition.
const SHELF_BAND: (f64, f64) = (6_000.0, 16_000.0);

/// Below it, by two and a half octaves. The shelf leaves this alone.
const BELOW_BAND: (f64, f64) = (200.0, 1_000.0);

/// The kick's band. The single kick sweeps 150 -> 50 Hz.
const KICK_BAND: (f64, f64) = (30.0, 140.0);

/// Track indices in the starter rack. A track index is also its note channel.
const KICK: usize = 0;
const SNARE: usize = 1;
const HAT: usize = 2;
const OPEN_HAT: usize = 3;

/// Energy in a frequency band, as dBFS. See `mixer_audio.rs` for why this is
/// the root of the summed squared magnitudes.
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

/// One measured capture and its spectra.
struct Measured {
    outcome: CaptureOutcome,
    left: Spectrum,
}

impl Measured {
    fn take(state: &AppState, label: &str) -> Self {
        let outcome = capture_to_disk(state.debug_tap(), CAPTURE_SECONDS, &capture_root())
            .expect("the engine is running, so a capture must arrive");
        let left = spectrum(&outcome.capture.channel(0), outcome.stats.sample_rate);
        let measured = Self { outcome, left };
        measured.report(label);
        measured
    }

    /// One line an agent can read.
    fn report(&self, label: &str) {
        let stats = &self.outcome.stats;
        let channel = &stats.per_channel[0];
        eprintln!(
            "[{label}] {} frames @ {} Hz ({:.3} s) | dropped {} | peak {:.2} dBFS | rms {:.2} dBFS \
             | crest {:.2} dB | kick {:.2} dB | mid {:.2} dB | shelf {:.2} dB",
            stats.frames,
            stats.sample_rate,
            stats.duration_seconds,
            self.outcome.capture.dropped,
            channel.peak_dbfs,
            channel.rms_dbfs,
            self.crest(),
            self.band(KICK_BAND),
            self.band(BELOW_BAND),
            self.band(SHELF_BAND),
        );
        eprintln!("[{label}]   wav  {}", self.outcome.artifacts.wav.display());
        eprintln!("[{label}]   png  {}", self.outcome.artifacts.png.display());
        eprintln!(
            "[{label}]   json {}",
            self.outcome.artifacts.stats_json.display()
        );
    }

    fn band(&self, band: (f64, f64)) -> f32 {
        band_dbfs(&self.left, band)
    }

    /// Peak-to-RMS, in dB. What a compressor is supposed to reduce.
    fn crest(&self) -> f32 {
        let channel = &self.outcome.stats.per_channel[0];
        channel.peak_dbfs - channel.rms_dbfs
    }

    fn peak_dbfs(&self) -> f32 {
        self.outcome.stats.per_channel[0].peak_dbfs
    }

    /// The checks every capture in this file has to pass before its numbers mean
    /// anything.
    fn assert_clean(&self, label: &str) {
        assert_eq!(
            self.outcome.capture.dropped, 0,
            "[{label}] the capture has holes; its bands cannot be trusted"
        );
        assert!(
            !self.outcome.stats.has_non_finite(),
            "[{label}] a NaN or an infinity reached the device"
        );
    }
}

/// The rack playing `steps` on `channel` and nothing else.
///
/// One instrument at a time, because every claim below is "this band came from
/// that strip" and a second source in the same band would make that unprovable.
fn only(channel: u8, steps: &[usize]) -> Pattern {
    let mut pattern = Pattern::with_channels(1, "Pattern 1", &[0, 1, 2, 3, 4], |channel| {
        mydaw_plugin_sequencer::DEFAULT_ROW_KEYS
            .get(channel as usize)
            .copied()
            .unwrap_or(mydaw_plugin_sequencer::DEFAULT_STEP_KEY)
    });
    for clip in &mut pattern.channels {
        let key = clip.steps.as_ref().map_or(60, |row| row.key);
        clip.steps = Some(StepRow::empty(key));
    }
    let clip = pattern
        .channel_mut(channel)
        .expect("the starter rack has this channel");
    let key = clip.steps.as_ref().expect("a grid").key;
    clip.steps = Some(StepRow::empty(key).with_steps(steps));
    pattern
}

/// A drum bus's worth of pattern: kick on the beat, snare on two and four,
/// closed hats on every sixteenth and an open hat on the off-beats.
///
/// Dense on purpose. The crest factor only says anything about a compressor
/// when there is a *body* under the transients for it to leave alone; a lone
/// kick is nine parts silence, and squashing it lowers the peak and the RMS
/// together.
fn drum_beat() -> Pattern {
    let mut pattern = only(KICK as u8, &[0, 4, 8, 12]);
    for (channel, steps) in [
        (SNARE as u8, vec![4, 12]),
        (HAT as u8, (0..16).collect::<Vec<_>>()),
        (OPEN_HAT as u8, vec![2, 6, 10, 14]),
    ] {
        let clip = pattern.channel_mut(channel).expect("a rack channel");
        let key = clip.steps.as_ref().expect("a grid").key;
        clip.steps = Some(StepRow::empty(key).with_steps(&steps));
    }
    pattern
}

/// Poll the compressor's meters for `window`, keeping the worst reduction seen.
///
/// A meter panel does exactly this at frame rate; a single sample of a per-block
/// readout on a pattern with gaps in it is a coin toss.
fn poll_reduction(state: &AppState, window: Duration) -> (f32, f32) {
    let deadline = std::time::Instant::now() + window;
    let (mut worst_gr, mut worst_max) = (0.0_f32, 0.0_f32);
    while std::time::Instant::now() < deadline {
        let readouts = state
            .insert_readouts(InsertTarget::Bus(0), 0)
            .expect("the compressor is there");
        for value in readouts {
            match value.id.as_str() {
                "gr" => worst_gr = worst_gr.max(value.value),
                "gr_max" => worst_max = worst_max.max(value.value),
                _ => {}
            }
        }
        sleep(Duration::from_millis(5));
    }
    (worst_gr, worst_max)
}

/// Boot, open the device and start the transport. `None` on a machine with no
/// sound card, which is not a failure.
fn running() -> Option<(mydaw_kernel::Kernel, AppState)> {
    let kernel = boot_kernel().expect("kernel boots");
    let state = AppState::from_kernel(&kernel).expect("services present");
    match state.start_audio_with_project() {
        Ok(info) => {
            eprintln!(
                "device: {} @ {} Hz, {} ch (max block {})",
                info.device_name, info.sample_rate, info.channels, info.max_block
            );
            Some((kernel, state))
        }
        Err(err) => {
            eprintln!("no audio device ({err}); nothing to verify on this machine");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// 1. A send reaches a bus, and the bus's chain colours it
// ---------------------------------------------------------------------------

#[test]
#[ignore = "opens the audio device and plays an audible hi-hat pattern"]
fn a_send_into_an_eq_bus_boosts_its_band_in_the_wet_dry_sum() {
    let Some((_kernel, state)) = running() else {
        return;
    };
    assert_eq!(state.set_bpm(BPM).unwrap(), BPM);
    state
        .set_pattern(only(HAT as u8, &[0, 2, 4, 6, 8, 10, 12, 14]))
        .expect("the beat is valid");

    // A bus with a +12 dB shelf from 4 kHz up, and a pre-fader tap into it from
    // the hat. The hat's own main output is untouched, so what the device plays
    // is dry + wet -- which is what a send *is*.
    assert_eq!(state.add_bus(Some("Air".to_owned())).unwrap(), 0);
    state
        .add_insert(InsertTarget::Bus(0), "effects.eq", None)
        .expect("the EQ exists");
    for (id, value) in [
        ("high.enabled", 1.0),
        ("high.freq", 4_000.0),
        ("high.gain", 12.0),
    ] {
        state
            .set_insert_param(InsertTarget::Bus(0), 0, id, value)
            .unwrap_or_else(|err| panic!("setting {id}: {err}"));
    }
    let send = state.add_send(StripId::Track(HAT), 0).expect("a legal tap");
    state.set_send_pre(send, true).unwrap();
    state.set_send_level(send, 1.0).unwrap();

    state.set_playing(true).expect("the transport starts");

    // The dry reference: the bus is there and muted, so the only difference
    // between the two captures is the bus itself.
    state.set_bus_muted(0, true).unwrap();
    sleep(SETTLE);
    let dry = Measured::take(&state, "dry");
    dry.assert_clean("dry");

    state.set_bus_muted(0, false).unwrap();
    sleep(SETTLE);
    let wet = Measured::take(&state, "wet+dry");
    wet.assert_clean("wet+dry");

    let shelf_gain = wet.band(SHELF_BAND) - dry.band(SHELF_BAND);
    let below_gain = wet.band(BELOW_BAND) - dry.band(BELOW_BAND);
    eprintln!(
        "[result] above the shelf +{shelf_gain:.2} dB, below it +{below_gain:.2} dB \
         (difference {:.2} dB)",
        shelf_gain - below_gain
    );

    // Dry + wet with a +12 dB shelf is 1 + 3.98 = +14.0 dB above the corner and
    // 1 + 1 = +6.0 dB below it. Both are measured on a noisy source through a
    // real converter, so the bar is the *relationship*: the shelf's band has to
    // rise substantially more than the band the shelf does not touch.
    assert!(
        shelf_gain > 8.0,
        "the bus's shelf did not reach the output: +{shelf_gain:.2} dB above the corner"
    );
    assert!(
        below_gain > 2.0,
        "the send itself did not reach the output: +{below_gain:.2} dB below the corner"
    );
    assert!(
        shelf_gain - below_gain > 4.0,
        "the boost is not the shelf's -- above {shelf_gain:.2} dB, below {below_gain:.2} dB"
    );
    assert!(
        !wet.outcome.stats.has_clipping(),
        "a hat through a +12 dB shelf must not clip the sum"
    );
}

// ---------------------------------------------------------------------------
// 2. Pre-fader and post-fader, with the fader down
// ---------------------------------------------------------------------------

#[test]
#[ignore = "opens the audio device and plays an audible hi-hat pattern"]
fn with_the_fader_down_a_pre_fader_send_is_audible_and_a_post_fader_one_is_not() {
    let Some((_kernel, state)) = running() else {
        return;
    };
    assert_eq!(state.set_bpm(BPM).unwrap(), BPM);
    state
        .set_pattern(only(HAT as u8, &[0, 2, 4, 6, 8, 10, 12, 14]))
        .expect("the beat is valid");

    state.add_bus(Some("Aux".to_owned())).unwrap();
    let send = state.add_send(StripId::Track(HAT), 0).expect("a legal tap");
    state.set_send_level(send, 1.0).unwrap();
    // The fader at the bottom of its scale: `db_to_gain(SILENCE_DB)` is exactly
    // zero, so the track's own path to the master is not "quiet", it is off.
    state
        .set_track_gain_db(HAT, SILENCE_DB)
        .expect("the fader takes it");
    state.set_playing(true).expect("the transport starts");

    state.set_send_pre(send, true).unwrap();
    sleep(SETTLE);
    let pre = Measured::take(&state, "pre-fader");
    pre.assert_clean("pre-fader");

    state.set_send_pre(send, false).unwrap();
    sleep(SETTLE);
    let post = Measured::take(&state, "post-fader");
    post.assert_clean("post-fader");

    let difference = pre.band(SHELF_BAND) - post.band(SHELF_BAND);
    eprintln!(
        "[result] pre {:.2} dB, post {:.2} dB in the hat's band (difference {difference:.2} dB); \
         peaks {:.2} / {:.2} dBFS",
        pre.band(SHELF_BAND),
        post.band(SHELF_BAND),
        pre.peak_dbfs(),
        post.peak_dbfs(),
    );

    assert!(
        pre.band(SHELF_BAND) > -60.0,
        "a pre-fader send must ignore the fader: {:.2} dB",
        pre.band(SHELF_BAND)
    );
    assert!(
        difference > 30.0,
        "a post-fader send must follow the fader all the way down: only {difference:.2} dB \
         between them"
    );
    assert!(
        post.peak_dbfs() < pre.peak_dbfs() - 30.0,
        "post-fader peak {:.2} dBFS is not below pre-fader {:.2} dBFS",
        post.peak_dbfs(),
        pre.peak_dbfs()
    );
}

// ---------------------------------------------------------------------------
// 3. A compressor on a drum bus
// ---------------------------------------------------------------------------

#[test]
#[ignore = "opens the audio device and plays an audible kick pattern"]
fn a_compressor_on_a_drum_bus_reports_gain_reduction_and_lowers_the_crest_factor() {
    let Some((_kernel, state)) = running() else {
        return;
    };
    assert_eq!(state.set_bpm(BPM).unwrap(), BPM);
    // Kick on the beat, hats on every sixteenth. A **drum bus**, not one drum:
    // the crest factor only means anything when there is a body under the
    // transients for the compressor to leave alone. A kick on its own is nine
    // parts silence, and squashing it lowers the peak and the RMS together.
    state.set_pattern(drum_beat()).expect("the beat is valid");

    // The drums go to the bus and *only* to the bus: faders down, pre-fader
    // taps. So what the device plays is the compressor's output, and the crest
    // factor of the capture is the crest factor of the compressed signal.
    state.add_bus(Some("Drums".to_owned())).unwrap();
    for track in [KICK, SNARE, HAT, OPEN_HAT] {
        let send = state
            .add_send(StripId::Track(track), 0)
            .expect("a legal tap");
        state.set_send_pre(send, true).unwrap();
        state.set_send_level(send, 1.0).unwrap();
        state.set_track_gain_db(track, SILENCE_DB).unwrap();
    }

    state
        .add_insert(InsertTarget::Bus(0), "effects.compressor", None)
        .expect("the compressor exists");
    // The threshold sits between the hats (~ -20 dBFS) and the kick (~ -9), so
    // the kick is crushed and the hats are barely touched -- which is what
    // "reduce the crest factor" means on a drum bus, and what a slow-release
    // leveller across the whole pattern would *not* do.
    for (id, value) in [
        ("threshold", -20.0),
        ("ratio", 8.0),
        ("attack", 1.0),
        ("release", 60.0),
        ("knee", 3.0),
    ] {
        state
            .set_insert_param(InsertTarget::Bus(0), 0, id, value)
            .unwrap_or_else(|err| panic!("setting {id}: {err}"));
    }
    // Bypassed first, so the reference is the *same* signal through the *same*
    // chain rather than through no chain at all.
    state
        .set_insert_bypass(InsertTarget::Bus(0), 0, true)
        .unwrap();

    state.set_playing(true).expect("the transport starts");
    sleep(SETTLE);
    let uncompressed = Measured::take(&state, "bypassed");
    uncompressed.assert_clean("bypassed");

    state
        .set_insert_bypass(InsertTarget::Bus(0), 0, false)
        .unwrap();
    sleep(SETTLE);

    // The meter, read the way a panel reads it: **polled**, separately from the
    // knob positions, and repeatedly. `gr_max` is the worst reduction *in one
    // block*, so a single poll that lands between two kicks reads nothing at
    // all -- which is exactly why the readout exists per block and why a meter
    // holds its own peak. This is the hold.
    let (gr, gr_max) = poll_reduction(&state, Duration::from_millis(1_500));

    let compressed = Measured::take(&state, "compressed");
    compressed.assert_clean("compressed");
    eprintln!(
        "[result] crest {:.2} dB -> {:.2} dB ({:+.2} dB); gr {gr:.2} dB, gr_max {gr_max:.2} dB",
        uncompressed.crest(),
        compressed.crest(),
        compressed.crest() - uncompressed.crest(),
    );

    assert!(
        gr_max > 3.0 && gr_max < 60.0,
        "a kick 30 dB over the threshold at 10:1 should show real reduction: gr_max {gr_max:.2} dB"
    );
    assert!(
        gr >= 0.0 && gr <= gr_max,
        "gr {gr} must be a non-negative reduction no worse than the block's worst {gr_max}"
    );
    assert!(
        compressed.crest() < uncompressed.crest() - 1.0,
        "the compressor did not reduce the crest factor: {:.2} dB -> {:.2} dB",
        uncompressed.crest(),
        compressed.crest()
    );
}

// ---------------------------------------------------------------------------
// 4. Three effects on one bus, at the device's own block size
// ---------------------------------------------------------------------------

#[test]
#[ignore = "opens the audio device and plays an audible beat through a full chain"]
fn eq_compressor_and_delay_on_one_bus_run_clean_at_the_devices_block_size() {
    let Some((_kernel, state)) = running() else {
        return;
    };
    let info = state
        .audio()
        .stream_info()
        .expect("the stream is open, so it has an info");
    assert_eq!(state.set_bpm(BPM).unwrap(), BPM);
    state
        .set_pattern(only(KICK as u8, &[0, 4, 8, 12]))
        .expect("the beat is valid");

    state.add_bus(Some("Everything".to_owned())).unwrap();
    for type_id in ["effects.eq", "effects.compressor", "effects.delay"] {
        state
            .add_insert(InsertTarget::Bus(0), type_id, None)
            .unwrap_or_else(|err| panic!("adding {type_id}: {err}"));
    }
    // Each one doing real work: a shelf, actual reduction, and repeats.
    for (index, id, value) in [
        (0_usize, "high.gain", 6.0_f32),
        (0, "low.gain", 4.0),
        (1, "threshold", -30.0),
        (1, "ratio", 6.0),
        (2, "feedback", 45.0),
        (2, "mix", 40.0),
    ] {
        state
            .set_insert_param(InsertTarget::Bus(0), index, id, value)
            .unwrap_or_else(|err| panic!("setting insert {index}'s {id}: {err}"));
    }
    let send = state
        .add_send(StripId::Track(KICK), 0)
        .expect("a legal tap");
    state.set_send_pre(send, true).unwrap();
    state.set_send_level(send, 0.8).unwrap();

    state.set_playing(true).expect("the transport starts");
    sleep(SETTLE);
    let chain = Measured::take(&state, "eq+comp+delay");
    chain.assert_clean("eq+comp+delay");

    eprintln!(
        "[result] max block {} | dropped {} | non-finite {} | clipped {} | peak {:.2} dBFS",
        info.max_block,
        chain.outcome.capture.dropped,
        chain.outcome.stats.per_channel[0].non_finite,
        chain.outcome.stats.per_channel[0].clipped,
        chain.peak_dbfs(),
    );

    assert_eq!(
        chain.outcome.capture.dropped, 0,
        "three effects on a bus dropped samples at a block of {}",
        info.max_block
    );
    assert!(
        !chain.outcome.stats.has_clipping(),
        "the chain clipped the sum"
    );
    assert!(
        chain.peak_dbfs() > -60.0,
        "the chain produced nothing at all: {:.2} dBFS",
        chain.peak_dbfs()
    );

    // ...and the mix survives the structural edit of taking one of them out
    // again, which is the same rebuild every add went through.
    state
        .remove_insert(InsertTarget::Bus(0), 1)
        .expect("the compressor comes out");
    sleep(SETTLE);
    let after = Measured::take(&state, "compressor removed");
    after.assert_clean("compressor removed");
    assert_eq!(
        after.outcome.capture.dropped, 0,
        "removing an insert mid-play dropped samples"
    );
    assert_eq!(
        state.mixer_state().buses[0].inserts.len(),
        2,
        "the chain is two effects now"
    );
}
