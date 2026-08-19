//! Audio clips, measured through the sound card.
//!
//! `plugins/sequencer/src/clips.rs` proves the voice pool is right as
//! arithmetic, over a matrix of tempos, rates and block sizes with no device
//! anywhere. This proves it is right *after the audio device has had it*: a real
//! WASAPI stream, a real callback at whatever block size the driver asked for, a
//! real file decoded off disk, and a capture taken from the same tap the debug
//! harness uses.
//!
//! ```text
//!   bar   0      1      2      3      4      5      6      7
//!   lane0 [ kick pattern, every beat, all eight bars .............. ]
//!   lane1                    [ hat loop .wav ]
//!   lane2                                        [ 440 Hz @ 32 kHz ]
//! ```
//!
//! The source on lane 1 is **rendered by this engine, through this device**: two
//! bars of the open hat are played and captured to a WAV first, trimmed so frame
//! zero is a transient, and that file is then loaded back as an audio source and
//! placed at bar 3. So the claims are about the clip player rather than about
//! whichever file happened to be lying around:
//!
//! 1. **Onset math.** The device's output inside the clip is the *file*, offset
//!    by exactly the sample `tick_boundary(3 bars)` names -- established by
//!    cross-correlating the two, so the answer is a lag in samples rather than a
//!    detector's opinion. An onset detector cannot settle this one: the kick
//!    lands on bar 3 as well, and its two-millisecond noise click has the same
//!    peak in the hat band as the hat does, so no threshold separates them. The
//!    band's *energy* separates them (a click does not ring for 350 ms), and
//!    that is the second half of the claim.
//! 2. **Timbre.** The high band of the clip as it comes out of the device has
//!    the same dominant partial as the file that went in -- the interpolator is
//!    not colouring it.
//! 3. **Pitch across a rate change.** Lane 2 holds a 440 Hz tone recorded at
//!    **32 000 Hz**, which is not any device rate. It has to come out at 440 Hz,
//!    which is the whole of "resample by the rate ratio" as a listener hears it.
//! 4. **Seeking into the middle.** The playhead is dropped two and a half beats
//!    into the clip and the output is cross-correlated against the source file:
//!    the lag that matches is the offset that was asked for, to within a handful
//!    of samples.
//!
//! `#[ignore]`d: it opens the audio device and plays an audible arrangement, so
//! `cargo test --workspace` stays silent and passes on a machine with no sound
//! card.
//!
//! ```sh
//! cargo test -p mydaw-app --test audio_clip_audio -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Every run leaves a WAV, an annotated PNG and a stats JSON under
//! `.debug/captures/<timestamp>/`, so a failure can be looked at rather than
//! guessed at.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::sleep;
use std::time::Duration;

use mydaw_app::{boot_kernel, capture_root, capture_to_disk, AppState};
use mydaw_plugin_debug_tap::{detect_onsets, spectrum, Band, Capture, OnsetConfig, SpectralPeak};
use mydaw_plugin_sequencer::{
    step_len_samples, tick_boundary, ChannelClip, PlayMode, StepRow, TICKS_PER_BAR,
};

/// Tempo everything here is played at. The engine's default, and a dance tempo.
const BPM: f64 = 130.0;

/// Where the audio clip is placed.
const CLIP_BAR: u64 = 3;

/// How long the audio clip is, in bars.
const CLIP_BARS: u64 = 2;

/// Where the 440 Hz tone clip is placed.
const TONE_BAR: u64 = 6;

/// The tone's own sample rate. Deliberately not a device rate: the ratio has to
/// be doing real work, or "resampling" is untested by construction.
const TONE_RATE: u32 = 32_000;

/// The tone's frequency.
const TONE_HZ: f64 = 440.0;

/// Corner of the band the kick is measured in. The kick sweeps 150 -> 50 Hz.
const KICK_BAND_HZ: f32 = 120.0;

/// Corner of the band the hat -- and therefore the audio clip -- is measured in.
/// Both hats are highpassed at 6 kHz and the kick has nothing up there.
const HAT_BAND_HZ: f32 = 5_000.0;

/// Gap between arming the capture and pressing Play.
///
/// This is what makes the capture's own origin knowable: the engine is silent
/// while stopped, so the first kick in the capture *is* a song sample the test
/// can name, and every measurement below is relative to it.
const PRE_ROLL: Duration = Duration::from_millis(300);

/// Worst onset deviation allowed, in milliseconds. A millisecond is roughly a
/// tenth of what a listener hears as a flam, and forty samples at 44.1 kHz.
const TOLERANCE_MS: f64 = 1.0;

/// Root-mean-square of `samples` above `corner`, in dBFS.
fn high_band_dbfs(samples: &[f32], sample_rate: f64, corner: f32) -> f64 {
    let banded = Band::High(corner).apply(samples, sample_rate);
    if banded.is_empty() {
        return f64::NEG_INFINITY;
    }
    let sum: f64 = banded.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
    let rms = (sum / banded.len() as f64).sqrt();
    if rms <= 0.0 {
        f64::NEG_INFINITY
    } else {
        20.0 * rms.log10()
    }
}

/// The first frame at or after `from` where `samples` rises above `threshold`.
///
/// Used on the **unfiltered** capture, to find the first kick and therefore the
/// capture's own origin. Unfiltered on purpose: a band-pass here would add its
/// own group delay -- two one-poles at 120 Hz is about 2.6 ms of it -- to a
/// number every other measurement is subtracted from. Before the clip enters,
/// the kick is the only thing in the capture, so the raw signal needs no filter
/// to be unambiguous.
fn first_above(samples: &[f32], threshold: f32, from: usize) -> Option<usize> {
    samples
        .iter()
        .enumerate()
        .skip(from)
        .find(|(_, sample)| sample.abs() > threshold)
        .map(|(frame, _)| frame)
}

/// The largest absolute value in `samples`.
fn peak_of(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()))
}

/// The loudest partial of `samples` above `corner`.
fn dominant_above(samples: &[f32], sample_rate: f64, corner: f32) -> Option<SpectralPeak> {
    let banded = Band::High(corner).apply(samples, sample_rate);
    spectrum(&banded, sample_rate).dominant()
}

/// Normalised cross-correlation of `signal` against `reference`, over lags in
/// `lags`, returning the best lag and its coefficient.
///
/// `signal[i]` is claimed to be `reference[lag + i]`. This is how "the seek
/// landed at the right place in the file" becomes a number rather than an
/// impression: the lag that matches best *is* the source position the device was
/// playing, measured from the audio rather than asserted from the code.
fn best_lag(signal: &[f32], reference: &[f32], lags: std::ops::Range<i64>) -> (i64, f64) {
    let energy: f64 = signal.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
    if energy <= 0.0 {
        return (0, 0.0);
    }
    let mut best = (0_i64, f64::NEG_INFINITY);
    for lag in lags {
        let mut dot = 0.0_f64;
        let mut reference_energy = 0.0_f64;
        for (index, sample) in signal.iter().enumerate() {
            let at = lag + index as i64;
            if at < 0 || at as usize >= reference.len() {
                continue;
            }
            let other = f64::from(reference[at as usize]);
            dot += f64::from(*sample) * other;
            reference_energy += other * other;
        }
        if reference_energy <= 0.0 {
            continue;
        }
        let coefficient = dot / (energy * reference_energy).sqrt();
        if coefficient > best.1 {
            best = (lag, coefficient);
        }
    }
    best
}

/// Play `seconds` of the arrangement from song sample `from`, capturing the
/// whole of it including the silence before Play.
///
/// Returns the capture and the frame its first kick landed on, which is the song
/// sample `first_kick` -- the capture's origin.
fn capture_from(state: &AppState, from: u64, seconds: f32) -> (Capture, PathBuf) {
    state.set_playing(false).ok();
    state.seek(from).expect("the playhead moves");

    // Play is pressed from another thread, because the capture blocks: the
    // silence it records first is what makes its own origin measurable.
    let audio = Arc::clone(state.audio());
    let starter = std::thread::spawn(move || {
        sleep(PRE_ROLL);
        audio.set_playing(true).ok();
    });

    let outcome = capture_to_disk(state.debug_tap(), seconds, &capture_root())
        .expect("the engine is running, so a capture must arrive");
    starter.join().ok();
    state.set_playing(false).ok();

    eprintln!(
        "capture from song sample {from}: {} frames | peak {:.4} ({:.2} dBFS) | dropped {} | clipped {} | non-finite {}",
        outcome.stats.frames,
        outcome.stats.per_channel[0].peak,
        outcome.stats.per_channel[0].peak_dbfs,
        outcome.capture.dropped,
        outcome.stats.per_channel[0].clipped,
        outcome.stats.per_channel[0].non_finite,
    );
    eprintln!("  wav  {}", outcome.artifacts.wav.display());
    eprintln!("  png  {}", outcome.artifacts.png.display());
    assert_eq!(outcome.capture.dropped, 0, "the capture has holes");
    assert_eq!(
        outcome.stats.per_channel[0].non_finite, 0,
        "a NaN reached the device"
    );
    (outcome.capture, outcome.artifacts.wav)
}

/// A `hz` sine at `rate`, `seconds` long, written as a 32-bit float mono WAV.
fn write_tone(path: &Path, rate: u32, hz: f64, seconds: f64) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec).expect("wav writer");
    let frames = (f64::from(rate) * seconds) as usize;
    for frame in 0..frames {
        let value = ((frame as f64 / f64::from(rate)) * hz * std::f64::consts::TAU).sin();
        writer
            .write_sample((value * 0.5) as f32)
            .expect("wav sample");
    }
    writer.finalize().expect("wav finalize");
}

#[test]
#[ignore = "opens the audio device and plays an audible arrangement"]
fn an_audio_clip_comes_out_of_the_device_on_the_song_timeline() {
    let kernel = boot_kernel().expect("kernel boots");
    let state = AppState::from_kernel(&kernel).expect("services present");

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

    let sample_rate = f64::from(info.sample_rate);
    let step_len = step_len_samples(sample_rate, BPM);
    let beat = step_len * 4.0;
    let bar = u64::from(TICKS_PER_BAR);
    let bar_samples = tick_boundary(bar, step_len);
    let bar_seconds = bar_samples as f64 / sample_rate;
    eprintln!(
        "tempo: {BPM} BPM | step {step_len:.3} samples | beat {beat:.3} | bar {bar_samples} samples ({bar_seconds:.4} s)"
    );

    assert_eq!(state.set_bpm(BPM).expect("the tempo is legal"), BPM);
    state
        .set_play_mode(PlayMode::Song)
        .expect("the mode is session state");

    // ---------------------------------------------------------------- phase 1
    // Render the source. Two bars of the open hat on every beat, played by this
    // engine through this device and captured -- so the file the clip plays is
    // one this build made, at this device's rate, rather than an asset whose
    // provenance the test would have to take on faith.
    let mut pattern = state.sequencer().active_pattern();
    pattern.channels[0].steps = Some(StepRow::empty(36).with_steps(&[]));
    *pattern.channel_mut(3).expect("the rack has an open hat") = ChannelClip {
        channel: 3,
        steps: Some(StepRow::empty(46).with_steps(&[0, 4, 8, 12])),
        notes: Vec::new(),
    };
    state
        .set_pattern(pattern)
        .expect("the hat pattern is valid");
    let hat_id = state.sequencer().patterns().active_id();
    state
        .edit_playlist(|list| list.add_pattern_clip(hat_id, 0, 0, CLIP_BARS * bar))
        .expect("two bars of hat");

    let (rendered, _) = capture_from(&state, 0, (bar_seconds * CLIP_BARS as f64) as f32);
    let rendered_left = rendered.channel(0);
    let hats = detect_onsets(&rendered_left, sample_rate, &OnsetConfig::high(HAT_BAND_HZ));
    assert!(
        hats.len() >= 4,
        "the source render must contain hats to trim to -- got {}",
        hats.len()
    );

    // Trim so frame zero *is* a transient. Without this the clip's first sample
    // would be silence and "the clip started at bar 3" would be unmeasurable.
    let first = hats[0].frame;
    let trimmed = Capture::new(
        rendered.samples[first * rendered.channels..].to_vec(),
        rendered.sample_rate,
        0,
    );
    let scratch = std::env::temp_dir().join(format!("mydaw-audio-clip-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("a scratch directory");
    let source_path = scratch.join("audio-clip-source.wav");
    trimmed
        .write_wav(&source_path)
        .expect("the source WAV must be writable");
    let source_left = trimmed.channel(0);
    let source_high = Band::High(HAT_BAND_HZ).apply(&source_left, sample_rate);
    eprintln!(
        "source: {} frames ({:.3} s) at {} Hz, trimmed {first} frames to its first transient",
        trimmed.frames(),
        trimmed.duration_seconds(),
        trimmed.sample_rate,
    );
    eprintln!("  {}", source_path.display());

    let tone_path = scratch.join("audio-clip-tone.wav");
    write_tone(&tone_path, TONE_RATE, TONE_HZ, 2.0);
    eprintln!(
        "tone:   {TONE_HZ} Hz recorded at {TONE_RATE} Hz -- {:.4}x the device rate",
        f64::from(TONE_RATE) / sample_rate
    );

    // ---------------------------------------------------------------- phase 2
    // The arrangement under test: the kick on every beat for eight bars, the
    // rendered hat loop as an audio clip at bar 3, the tone at bar 6.
    let mut pattern = state.sequencer().active_pattern();
    pattern.channels[0].steps = Some(StepRow::empty(36).with_steps(&[0, 4, 8, 12]));
    *pattern.channel_mut(3).expect("the open hat") = ChannelClip {
        channel: 3,
        steps: Some(StepRow::empty(46).with_steps(&[])),
        notes: Vec::new(),
    };
    state
        .set_pattern(pattern)
        .expect("the kick pattern is valid");
    // Clear the render's own clip and lay the real arrangement out.
    let existing: Vec<u64> = state
        .sequencer()
        .playlist()
        .clips()
        .iter()
        .map(|clip| clip.id())
        .collect();
    for id in existing {
        state
            .edit_playlist(|list| list.remove_clip(id).map(|_| ()))
            .expect("the render's clip goes");
    }
    while state.sequencer().playlist().tracks().len() < 3 {
        state
            .edit_playlist(|list| list.add_track(None).map(|_| ()))
            .expect("room for three lanes");
    }
    state
        .edit_playlist(|list| list.add_pattern_clip(hat_id, 0, 0, 8 * bar))
        .expect("the kick runs the whole song");

    let source = state
        .load_audio_source(&source_path)
        .expect("the rendered WAV must load");
    eprintln!(
        "loaded source {}: {} frames at {} Hz ({:.3} s)",
        source.id(),
        source.frames(),
        source.sample_rate(),
        source.seconds()
    );
    assert_eq!(source.sample_rate(), sample_rate, "the render's own rate");

    let tone = state
        .load_audio_source(&tone_path)
        .expect("the tone must load");
    assert_eq!(tone.sample_rate(), f64::from(TONE_RATE));

    let _ = state.ensure_audio_clip_channel(source.id());
    let _ = state.ensure_audio_clip_channel(tone.id());

    state
        .edit_playlist(|list| {
            list.add_audio_clip(source.id(), 1, CLIP_BAR * bar, CLIP_BARS * bar, 0, 0.0)
        })
        .expect("the clip is placed at bar 3");
    state
        .edit_playlist(|list| list.add_audio_clip(tone.id(), 2, TONE_BAR * bar, bar, 0, -6.0))
        .expect("the tone is placed at bar 6");

    let song = state.sequencer_state();
    eprintln!(
        "song: {} clip(s), ends at tick {} ({} bars)",
        song.playlist.clips.len(),
        song.playlist.end_ticks,
        song.playlist.end_ticks / bar
    );
    assert_eq!(song.playlist.clips.len(), 3);
    assert_eq!(
        song.playlist.end_ticks,
        8 * bar,
        "the arrangement ends at the last thing in it"
    );

    // ---------------------------------------------------------------- phase 3
    // Bars 0 to 5, from a standing start: the clip must be inaudible before bar
    // 3 and land exactly on it.
    let (captured, _) = capture_from(&state, 0, (bar_seconds * 5.0) as f32);
    let left = captured.channel(0);

    // The capture's own clock. The transport was rewound before Play and the
    // engine is silent while stopped, so the first sound in the capture *is*
    // song sample 0 -- the kick on the first beat.
    let bar_frames = bar_samples as usize;
    let opening = &left[..(2 * bar_frames).min(left.len())];
    let origin = first_above(&left, peak_of(opening) * 0.2, 0)
        .expect("the kick is the capture's clock; without it nothing can be located")
        as i64;
    let kicks = detect_onsets(&left, sample_rate, &OnsetConfig::low(KICK_BAND_HZ));
    eprintln!(
        "[origin] the first kick's attack is at capture frame {origin} = song sample 0 ({} kicks in the capture)",
        kicks.len()
    );

    let clip_start = tick_boundary(CLIP_BAR * bar, step_len) as i64;
    let clip_end = tick_boundary((CLIP_BAR + CLIP_BARS) * bar, step_len) as i64;

    // Before the clip: the high band holds only the kick's broadband click.
    let before = &left[origin as usize..(origin + clip_start) as usize];
    let before_db = high_band_dbfs(before, sample_rate, HAT_BAND_HZ);
    // Inside it: the hat loop.
    let during_from = (origin + clip_start) as usize;
    let during_to = ((origin + clip_end) as usize).min(left.len());
    let during = &left[during_from..during_to];
    let during_db = high_band_dbfs(during, sample_rate, HAT_BAND_HZ);
    eprintln!(
        "[hat band] bars 0-{CLIP_BAR}: {before_db:.2} dBFS | bars {CLIP_BAR}-{}: {during_db:.2} dBFS | {:+.2} dB",
        CLIP_BAR + CLIP_BARS,
        during_db - before_db
    );
    assert!(
        during_db - before_db > 10.0,
        "the clip enters at bar {CLIP_BAR}, so the hat band must be far louder inside it"
    );

    // Claim 1: the device's output inside the clip *is* the file, offset by
    // exactly the sample bar 3 names.
    //
    // Correlation rather than onset detection, because onset detection cannot
    // answer this: the kick is on bar 3 too, and its click has the same peak in
    // the hat band as the hat (measured -- see the note printed below), so no
    // threshold tells them apart. The lag that matches the waveform does.
    let captured_high = Band::High(HAT_BAND_HZ).apply(&left, sample_rate);
    eprintln!(
        "[bands] kick click peaks at {:.5} in the hat band; the clip at {:.5} -- which is why this is correlated, not thresholded",
        peak_of(&captured_high[origin as usize..(origin + clip_start) as usize]),
        peak_of(&captured_high[during_from..during_to]),
    );

    // A window half a bar into the clip: past the fade-in, and far from either
    // edge of the capture.
    let into = bar_samples as i64 / 2;
    let window_len = (sample_rate * 0.1) as usize;
    let window_at = (origin + clip_start + into) as usize;
    assert!(
        window_at + window_len < captured_high.len(),
        "the capture is too short to correlate"
    );
    let (lag, coefficient) = best_lag(
        &captured_high[window_at..window_at + window_len],
        &source_high,
        (into - 1_500).max(0)..(into + 1_500),
    );
    let error_samples = lag - into;
    let error_ms = error_samples as f64 / sample_rate * 1_000.0;
    eprintln!(
        "[placement] bar {CLIP_BAR} is song sample {clip_start}; the device is playing source frame {lag} where the file says {into} (r = {coefficient:.3}) -- error {error_samples:+} samples ({error_ms:+.3} ms)"
    );
    assert!(
        coefficient > 0.7,
        "the clip does not look like the file it is supposed to be playing (r = {coefficient:.3})"
    );
    assert!(
        error_ms.abs() <= TOLERANCE_MS,
        "the clip is playing {error_ms:+.3} ms away from where bar {CLIP_BAR} puts it"
    );

    // Claim 2: timbre. The dominant partial the device produced inside the clip
    // is the one the file holds, so the interpolator is not colouring it.
    let played = dominant_above(during, sample_rate, HAT_BAND_HZ).expect("the clip has a spectrum");
    let recorded =
        dominant_above(&source_left, sample_rate, HAT_BAND_HZ).expect("the file has a spectrum");
    let cents = 1_200.0 * (played.frequency_hz / recorded.frequency_hz).log2();
    eprintln!(
        "[timbre] file {:.1} Hz -> device {:.1} Hz ({cents:+.1} cents)",
        recorded.frequency_hz, played.frequency_hz
    );
    assert!(
        cents.abs() < 100.0,
        "the clip came out a semitone or more away from the file it plays"
    );

    // ---------------------------------------------------------------- phase 4
    // Claim 3: a source at another rate keeps its pitch. Bar 6 holds 440 Hz
    // recorded at 32 kHz; 440 Hz is what has to come out.
    let tone_start = tick_boundary(TONE_BAR * bar, step_len);
    let (tone_capture, _) = capture_from(&state, tone_start + beat as u64, bar_seconds as f32);
    let tone_left = tone_capture.channel(0);
    // Above the kick's fundamental and far below the hat band: the tone is the
    // only thing in this part of the spectrum.
    let tone_band = Band::High(250.0).apply(&tone_left, sample_rate);
    let heard = spectrum(&tone_band, sample_rate)
        .dominant()
        .expect("the tone has a spectrum");
    let tone_cents = 1_200.0 * (heard.frequency_hz / TONE_HZ).log2();
    eprintln!(
        "[pitch] {TONE_HZ} Hz at {TONE_RATE} Hz on a {sample_rate} Hz device -> {:.2} Hz ({tone_cents:+.1} cents)",
        heard.frequency_hz
    );
    assert!(
        tone_cents.abs() < 50.0,
        "a {TONE_RATE} Hz source played at {sample_rate} Hz came out at {:.2} Hz, not {TONE_HZ}",
        heard.frequency_hz
    );

    // ---------------------------------------------------------------- phase 5
    // Claim 4: seeking into the middle of a clip continues it from the right
    // frame. The playhead is dropped two and a half beats in, and the output is
    // cross-correlated against the file: the lag that matches best is the source
    // position the device was actually reading.
    let into_clip = (beat * 2.5) as u64;
    let seek_to = clip_start as u64 + into_clip;
    let (seeked, _) = capture_from(&state, seek_to, (bar_seconds * 1.2) as f32);
    let seeked_left = seeked.channel(0);

    // Playback resumed at `seek_to`, so the *next beat line* after it is the
    // first sound in the capture -- the same trick as the origin above, one
    // beat later.
    let first_beat_after = ((seek_to as f64 / beat).ceil() as u64) * 4 * 240;
    let first_kick_song = tick_boundary(first_beat_after, step_len) as i64;
    let opening = &seeked_left[..(2 * bar_frames).min(seeked_left.len())];
    let seek_origin = first_above(&seeked_left, peak_of(opening) * 0.2, 0)
        .expect("the kick still marks the clock") as i64
        - first_kick_song;
    eprintln!(
        "[seek] to song sample {seek_to} ({:.3} beats into the clip); the first kick after it is song sample {first_kick_song}",
        into_clip as f64 / beat,
    );

    // A window well inside the clip, away from the fade-in and both edges of
    // the capture.
    let seeked_high = Band::High(HAT_BAND_HZ).apply(&seeked_left, sample_rate);
    let window_start = (seek_origin + clip_start + into_clip as i64 + 4_000).max(0) as usize;
    assert!(
        window_start + window_len < seeked_high.len(),
        "the capture is too short to correlate: {window_start} + {window_len} vs {}",
        seeked_high.len()
    );
    // Where in the file that window should be: song sample minus the clip's
    // start, since the clip's offset is zero and the rates match exactly.
    let expected = window_start as i64 - seek_origin - clip_start;
    let (lag, coefficient) = best_lag(
        &seeked_high[window_start..window_start + window_len],
        &source_high,
        (expected - 1_500).max(0)..(expected + 1_500),
    );
    eprintln!(
        "[seek] the device is playing source frame {lag} (r = {coefficient:.3}); the seek asked for {expected} -- error {:+} samples ({:+.3} ms)",
        lag - expected,
        (lag - expected) as f64 / sample_rate * 1_000.0
    );
    assert!(
        coefficient > 0.7,
        "the seek capture does not look like the file it is supposed to be playing (r = {coefficient:.3})"
    );
    assert!(
        (lag - expected).abs() <= 64,
        "a seek {into_clip} frames into the clip continued at source frame {lag}, not {expected}"
    );

    // The captures stay under `.debug/captures/` to be looked at; the two
    // scratch WAVs were only ever inputs.
    std::fs::remove_dir_all(&scratch).ok();
}
