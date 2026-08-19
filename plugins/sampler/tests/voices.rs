//! What the sampler actually *does* to a signal, measured rather than asserted.
//!
//! Every instrument here is synthetic and built on disk through the shipping
//! loader (see [`InstrumentBuilder`]), for two reasons. It exercises the real
//! path -- manifest, decode, lookup, voice -- rather than a parallel in-memory
//! constructor that could drift. And it makes the assertions *exact*: a sine
//! has one partial and a ramp has a known value at every fractional position,
//! neither of which is true of a piano. The piano's own verification lives in
//! `tests/piano_assets.rs` and asserts far weaker things, because that is all a
//! recording of a real instrument supports.
//!
//! These tests need no assets and run everywhere.

use mydaw_plugin_debug_tap::spectrum;
use mydaw_plugin_sampler::test_support::{peak, ramp, render, rms, sine, InstrumentBuilder};
use mydaw_plugin_sampler::{SamplerNode, VOICES};
use mydaw_rt_core::NoteEvent;

const SR: f64 = 48_000.0;
const C4: u8 = 60;
const A4: u8 = 69;

/// Velocity as the engine sends it, for a MIDI velocity.
fn velocity(midi: u8) -> f32 {
    f32::from(midi) / 127.0
}

/// The loudest partial of `samples`.
fn dominant_hz(samples: &[f32]) -> f64 {
    spectrum(samples, SR)
        .dominant()
        .expect("there must be energy to measure")
        .frequency_hz
}

// ---------------------------------------------------------------- pitch

/// The claim the whole instrument rests on. A 440 Hz recording rooted at C4,
/// played at A4, must come out nine semitones up -- 739.99 Hz, not 440.
#[test]
fn a_sample_rooted_at_c4_and_played_at_a4_comes_out_nine_semitones_up() {
    let (_dir, bank) = InstrumentBuilder::new("pitch")
        .full_range("tone.wav", C4, 48_000, sine(440.0, SR, 96_000))
        .build();

    for (key, expected) in [
        (C4, 440.0_f64),
        (A4, 440.0 * 2.0_f64.powf(9.0 / 12.0)),
        (C4 + 12, 880.0),
        (C4 - 12, 220.0),
    ] {
        let mut node = SamplerNode::new(std::sync::Arc::clone(&bank), SR);
        let (left, _) = render(&mut node, 32_768, 512, &[NoteEvent::note_on(0, key, 1.0)]);
        let measured = dominant_hz(&left);
        // One FFT bin at 32768 points is 1.46 Hz; allow two.
        assert!(
            (measured - expected).abs() < 3.0,
            "key {key}: expected {expected:.2} Hz, measured {measured:.2} Hz"
        );
    }
}

/// A sample recorded at a rate other than the device's must still play in tune.
#[test]
fn a_sample_recorded_at_a_different_rate_still_plays_in_tune() {
    // 440 Hz recorded at 24 kHz, played out at 48 kHz. Without the rate term
    // in the step this would sound an octave low.
    let (_dir, bank) = InstrumentBuilder::new("rate")
        .full_range("tone.wav", C4, 24_000, sine(440.0, 24_000.0, 48_000))
        .build();
    let mut node = SamplerNode::new(bank, SR);
    let (left, _) = render(&mut node, 32_768, 512, &[NoteEvent::note_on(0, C4, 1.0)]);
    let measured = dominant_hz(&left);
    assert!(
        (measured - 440.0).abs() < 3.0,
        "expected 440 Hz, measured {measured:.2} Hz"
    );
}

// -------------------------------------------------------- interpolation

/// At its root key the step is exactly 1.0, and Catmull-Rom passes through the
/// sample points -- so playback must be the recording, bit for bit, not an
/// approximation of it.
#[test]
fn playing_at_the_root_key_reproduces_the_recording_exactly() {
    let content: Vec<f32> = (0..512)
        .map(|i| ((i * 37) % 101) as f32 / 101.0 - 0.5)
        .collect();
    let (_dir, bank) = InstrumentBuilder::new("exact")
        .full_range("noise.wav", C4, 48_000, content.clone())
        .build();

    let mut node = SamplerNode::new(bank, SR);
    let (left, right) = render(&mut node, 512, 512, &[NoteEvent::note_on(0, C4, 1.0)]);

    // The only scaling between the file and the bus is the voice gain, which
    // is constant, so the ratio must be constant too.
    let gain = left[1] / content[1];
    for (index, expected) in content.iter().enumerate() {
        let got = left[index];
        assert!(
            (got - expected * gain).abs() < 1e-5,
            "frame {index}: {got} is not {} scaled by {gain}",
            expected
        );
    }
    assert_eq!(left, right, "a mono sample must land on both channels");
}

/// Interpolation must not invent discontinuities. A ramp resampled at any ratio
/// is still a ramp; a jump between adjacent output samples that is much larger
/// than the step size would be a click.
#[test]
fn interpolating_a_ramp_stays_continuous_at_every_ratio() {
    let frames = 4_096;
    let (_dir, bank) = InstrumentBuilder::new("ramp")
        .full_range("ramp.wav", C4, 48_000, ramp(frames))
        .build();

    // A spread of ratios: below 1, above 1, and deliberately irrational-ish so
    // the fractional part never repeats.
    for key in [C4 - 7, C4 - 1, C4, C4 + 1, C4 + 5, C4 + 11] {
        let mut node = SamplerNode::new(std::sync::Arc::clone(&bank), SR);
        let step = 2.0_f64.powf(f64::from(key as i16 - C4 as i16) / 12.0);
        // Stop well before the sample runs out, so the end-of-sample step to
        // silence is not what is being measured.
        let render_frames = ((frames as f64 / step) as usize).saturating_sub(64);
        let (left, _) = render(
            &mut node,
            render_frames,
            256,
            &[NoteEvent::note_on(0, key, 1.0)],
        );

        // One input frame is 1/frames of full scale; one output frame advances
        // `step` input frames. Four times that is generous for a C1-continuous
        // interpolator and still far below anything audible as a click.
        let bound = (step as f32 / frames as f32) * 4.0;
        let worst = left
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0_f32, f32::max);
        assert!(
            worst < bound,
            "key {key} (step {step:.4}): largest jump {worst:.3e} exceeds {bound:.3e}"
        );
    }
}

// ------------------------------------------------------------- velocity

/// The layer boundary is exactly where the manifest puts it, on both sides.
#[test]
fn velocity_selects_the_layer_the_manifest_says_and_changes_on_the_boundary() {
    // Two layers that are trivially distinguishable by ear and by FFT.
    let (_dir, bank) = InstrumentBuilder::new("layers")
        .sample(
            "soft.wav",
            C4,
            0,
            127,
            1,
            63,
            48_000,
            sine(440.0, SR, 96_000),
        )
        .sample(
            "loud.wav",
            C4,
            0,
            127,
            64,
            127,
            48_000,
            sine(1_000.0, SR, 96_000),
        )
        .build();

    for (midi, expected) in [(1_u8, 440.0), (63, 440.0), (64, 1_000.0), (127, 1_000.0)] {
        let mut node = SamplerNode::new(std::sync::Arc::clone(&bank), SR);
        let (left, _) = render(
            &mut node,
            32_768,
            512,
            &[NoteEvent::note_on(0, C4, velocity(midi))],
        );
        let measured = dominant_hz(&left);
        assert!(
            (measured - expected).abs() < 5.0,
            "velocity {midi}: expected the {expected} Hz layer, measured {measured:.1} Hz"
        );
    }
}

/// Louder must be louder. Within one layer the gain ramps from the floor to
/// unity, so a soft note and a hard one differ by a measurable amount.
#[test]
fn a_harder_note_is_louder_than_a_soft_one_within_the_same_layer() {
    let (_dir, bank) = InstrumentBuilder::new("dynamics")
        .full_range("tone.wav", C4, 48_000, sine(440.0, SR, 48_000))
        .build();

    let level = |midi: u8| {
        let mut node = SamplerNode::new(std::sync::Arc::clone(&bank), SR);
        let (left, _) = render(
            &mut node,
            12_000,
            512,
            &[NoteEvent::note_on(0, C4, velocity(midi))],
        );
        rms(&left)
    };

    let soft = level(1);
    let hard = level(127);
    assert!(
        hard > soft * 1.5,
        "velocity 127 ({hard:.4}) must be clearly above velocity 1 ({soft:.4})"
    );
    // The floor is -6 dB, so the span is a factor of two and not more.
    let ratio = hard / soft;
    assert!(
        (1.9..=2.1).contains(&ratio),
        "the within-layer span should be a factor of two, got {ratio:.3}"
    );
}

/// A note-on at velocity 0 is legal in CLAP. It must play, softly.
#[test]
fn a_zero_velocity_note_on_still_sounds() {
    let (_dir, bank) = InstrumentBuilder::new("zero-vel")
        .full_range("tone.wav", C4, 48_000, sine(440.0, SR, 48_000))
        .build();
    let mut node = SamplerNode::new(bank, SR);
    let (left, _) = render(&mut node, 4_800, 512, &[NoteEvent::note_on(0, C4, 0.0)]);
    assert!(
        peak(&left) > 0.0,
        "a zero-velocity note-on is not a note-off"
    );
    assert_eq!(node.active_voices(), 1);
}

/// Keys outside the instrument's range are counted, not played, and never
/// silently substituted with a neighbour.
#[test]
fn a_key_the_instrument_does_not_cover_is_counted_rather_than_faked() {
    let (_dir, bank) = InstrumentBuilder::new("narrow")
        .sample(
            "tone.wav",
            C4,
            59,
            61,
            1,
            127,
            48_000,
            sine(440.0, SR, 48_000),
        )
        .build();
    let mut node = SamplerNode::new(bank, SR);

    let (left, _) = render(&mut node, 2_400, 512, &[NoteEvent::note_on(0, 20, 1.0)]);
    assert!(left.iter().all(|s| *s == 0.0), "nothing may sound");
    assert_eq!(node.active_voices(), 0);
    assert_eq!(node.unmapped_notes(), 1);

    // And a key it does cover still works.
    render(&mut node, 2_400, 512, &[NoteEvent::note_on(0, 61, 1.0)]);
    assert_eq!(node.active_voices(), 1);
    assert_eq!(node.unmapped_notes(), 1);
}

// -------------------------------------------------------------- release

/// "Released" has to mean digital silence, not "quiet enough".
#[test]
fn a_release_completes_to_exactly_zero_and_frees_the_voice() {
    let (_dir, bank) = InstrumentBuilder::new("release")
        .release_ms(100.0)
        .full_range("tone.wav", C4, 48_000, sine(440.0, SR, 480_000))
        .build();

    let mut node = SamplerNode::new(bank, SR);
    render(&mut node, 4_800, 512, &[NoteEvent::note_on(0, C4, 1.0)]);
    assert_eq!(node.active_voices(), 1);

    // Release, then wait past the 100 ms ramp.
    let (fading, _) = render(&mut node, 2_400, 512, &[NoteEvent::note_off(0, C4)]);
    assert!(peak(&fading) > 0.0, "the tail must actually be heard");

    render(&mut node, 9_600, 512, &[]);
    assert_eq!(node.active_voices(), 0, "the voice must free itself");

    let (silence, _) = render(&mut node, 4_800, 512, &[]);
    assert!(
        silence.iter().all(|s| *s == 0.0),
        "after the release the sampler must be digitally silent, peaked at {}",
        peak(&silence)
    );
}

/// The release must fall, and fall over roughly the advertised time.
#[test]
fn the_release_ramp_declines_over_the_time_the_manifest_states() {
    let (_dir, bank) = InstrumentBuilder::new("release-shape")
        .release_ms(200.0)
        // A constant, so the only thing shaping the output is the envelope.
        .full_range("dc.wav", C4, 48_000, vec![1.0; 480_000])
        .build();

    let mut node = SamplerNode::new(bank, SR);
    render(&mut node, 4_800, 480, &[NoteEvent::note_on(0, C4, 1.0)]);
    // 200 ms is 9600 frames; render 300 ms so the tail is included.
    let (tail, _) = render(&mut node, 14_400, 480, &[NoteEvent::note_off(0, C4)]);

    // Sampled in 10 ms windows, the level must never rise.
    let windows: Vec<f32> = tail.chunks(480).map(rms).collect();
    assert!(
        windows.windows(2).all(|pair| pair[1] <= pair[0] + 1e-6),
        "the release must not rise: {windows:?}"
    );
    assert!(windows[0] > 0.1, "it must start audible: {}", windows[0]);
    // 9600 frames is window 20; by then it is over.
    assert_eq!(windows[21], 0.0, "the ramp must be finished by 210 ms");
    assert!(
        windows[15] > 0.0,
        "and must not have finished early, at 150 ms it was {}",
        windows[15]
    );
}

/// A note-off for a key that was never pressed must not release another one.
#[test]
fn a_note_off_for_the_wrong_key_or_channel_is_harmless() {
    let (_dir, bank) = InstrumentBuilder::new("note-off")
        .full_range("tone.wav", C4, 48_000, sine(440.0, SR, 480_000))
        .build();
    let mut node = SamplerNode::new(bank, SR);

    render(&mut node, 2_400, 512, &[NoteEvent::note_on(0, C4, 1.0)]);
    render(&mut node, 2_400, 512, &[NoteEvent::note_off(0, A4)]);
    assert_eq!(node.held_voices(), 1, "the wrong key must not release C4");
    render(
        &mut node,
        2_400,
        512,
        &[NoteEvent::note_off(0, C4).on_channel(3)],
    );
    assert_eq!(node.held_voices(), 1, "nor the wrong channel");
    render(&mut node, 2_400, 512, &[NoteEvent::note_off(0, C4)]);
    assert_eq!(node.held_voices(), 0);
}

// ------------------------------------------------------- voice handling

#[test]
fn the_voice_after_the_last_one_steals_the_oldest() {
    let (_dir, bank) = InstrumentBuilder::new("stealing")
        .sample(
            "tone.wav",
            C4,
            0,
            127,
            1,
            127,
            48_000,
            sine(440.0, SR, 480_000),
        )
        .build();
    let mut node = SamplerNode::new(bank, SR);

    // One block apart, so their ages differ.
    for step in 0..VOICES {
        let key = 40 + step as u8;
        render(&mut node, 128, 128, &[NoteEvent::note_on(0, key, 1.0)]);
    }
    assert_eq!(node.active_voices(), VOICES);
    assert_eq!(node.stolen_voices(), 0);

    let mut before = node.sounding_keys();
    before.sort_unstable();
    assert_eq!(before[0], 40, "key 40 is the oldest");

    render(&mut node, 128, 128, &[NoteEvent::note_on(0, 100, 1.0)]);
    assert_eq!(node.active_voices(), VOICES, "polyphony is still {VOICES}");
    assert_eq!(node.stolen_voices(), 1);

    let mut after = node.sounding_keys();
    after.sort_unstable();
    assert!(
        !after.contains(&40),
        "the oldest voice is the one that goes"
    );
    assert!(after.contains(&100), "the new note is playing");
    assert_eq!(after.len(), VOICES);
}

/// A repeated note is a fresh strike, and the ringing one is faded rather than
/// cut -- so briefly there are two voices, then one.
#[test]
fn a_retriggered_note_takes_a_new_voice_and_fades_the_old_one() {
    let (_dir, bank) = InstrumentBuilder::new("retrigger")
        .full_range("dc.wav", C4, 48_000, vec![1.0; 480_000])
        .build();
    let mut node = SamplerNode::new(bank, SR);

    render(&mut node, 4_800, 480, &[NoteEvent::note_on(0, C4, 1.0)]);
    assert_eq!(node.active_voices(), 1);

    render(&mut node, 240, 240, &[NoteEvent::note_on(0, C4, 1.0)]);
    assert_eq!(node.active_voices(), 2, "the old string is still ringing");
    assert_eq!(node.held_voices(), 1, "but only one key is down");

    // The choke is 20 ms; 40 ms later only the new voice is left.
    render(&mut node, 1_920, 480, &[]);
    assert_eq!(node.active_voices(), 1, "the choked voice must have gone");
    assert_eq!(node.held_voices(), 1);
    assert_eq!(node.stolen_voices(), 0, "a retrigger is not a steal");
}

/// Notes on different channels are different voices, as they are for the synth.
#[test]
fn notes_on_different_channels_are_different_voices() {
    let (_dir, bank) = InstrumentBuilder::new("channels")
        .full_range("tone.wav", C4, 48_000, sine(440.0, SR, 480_000))
        .build();
    let mut node = SamplerNode::new(bank, SR);

    render(
        &mut node,
        480,
        480,
        &[
            NoteEvent::note_on(0, C4, 1.0),
            NoteEvent::note_on(0, C4, 1.0).on_channel(1),
        ],
    );
    assert_eq!(
        node.held_voices(),
        2,
        "channel is part of a note's identity"
    );
}

#[test]
fn a_note_starts_at_its_frame_offset_and_not_at_the_top_of_the_block() {
    let (_dir, bank) = InstrumentBuilder::new("offset")
        .full_range("dc.wav", C4, 48_000, vec![1.0; 48_000])
        .build();
    let mut node = SamplerNode::new(bank, SR);

    let (left, _) = render(&mut node, 512, 512, &[NoteEvent::note_on(256, C4, 1.0)]);
    assert!(
        left[..256].iter().all(|s| *s == 0.0),
        "nothing may sound before the event's frame"
    );
    assert!(
        peak(&left[256..]) > 0.0,
        "the note is missing after its frame"
    );
}

/// A voice must free itself when the recording runs out, without reading past
/// the end and without a click at the join.
#[test]
fn a_voice_ends_when_its_sample_does() {
    let (_dir, bank) = InstrumentBuilder::new("short")
        // 100 ms of constant, so running out is a visible edge.
        .full_range("dc.wav", C4, 48_000, vec![1.0; 4_800])
        .build();
    let mut node = SamplerNode::new(bank, SR);

    let (left, _) = render(&mut node, 9_600, 480, &[NoteEvent::note_on(0, C4, 1.0)]);
    assert_eq!(node.active_voices(), 0, "the voice must free itself");
    assert!(peak(&left[..4_000]) > 0.0);
    assert!(
        left[5_000..].iter().all(|s| *s == 0.0),
        "nothing may sound past the end of the recording"
    );
}

/// An unplayed sampler is digitally silent, and stays that way.
#[test]
fn an_unplayed_sampler_is_digitally_silent() {
    let (_dir, bank) = InstrumentBuilder::new("silent")
        .full_range("tone.wav", C4, 48_000, sine(440.0, SR, 48_000))
        .build();
    let mut node = SamplerNode::new(bank, SR);
    let (left, right) = render(&mut node, 4_800, 128, &[]);
    assert!(left.iter().all(|s| *s == 0.0));
    assert!(right.iter().all(|s| *s == 0.0));
    assert_eq!(node.active_voices(), 0);
}

/// A dense chord must not clip. Headroom is not optional in a mixer.
#[test]
fn a_six_note_chord_at_full_velocity_stays_below_full_scale() {
    let (_dir, bank) = InstrumentBuilder::new("chord")
        .full_range("tone.wav", C4, 48_000, sine(220.0, SR, 480_000))
        .build();
    let mut node = SamplerNode::new(std::sync::Arc::clone(&bank), SR);

    let events: Vec<NoteEvent> = [48_u8, 55, 60, 64, 67, 72]
        .iter()
        .map(|key| NoteEvent::note_on(0, *key, 1.0))
        .collect();
    let (left, _) = render(&mut node, 24_000, 512, &events);
    let loudest = peak(&left);
    assert!(loudest < 1.0, "a six-note chord clipped at {loudest}");
    assert!(loudest > 0.2, "it should be clearly audible, got {loudest}");

    // And one note alone is nowhere near the ceiling, so the headroom above is
    // spent on polyphony rather than thrown away.
    let mut solo = SamplerNode::new(std::sync::Arc::clone(&bank), SR);
    let (single, _) = render(&mut solo, 24_000, 512, &events[..1]);
    assert!(
        peak(&single) < 0.2,
        "one note should sit well below full scale, got {}",
        peak(&single)
    );
}
