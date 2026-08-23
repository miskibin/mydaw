//! The instrument half of the sampler: the amplifier, the one-shot rule, and
//! mono glide.
//!
//! Every fixture here is a *constant* recording -- every frame the same value,
//! played at its root key so the step is exactly 1.0 and the interpolator is
//! bit-exact. That makes the output the envelope, multiplied by a number this
//! file knows ([`VOICE_GAIN`]), which is what lets an envelope's shape be
//! asserted in dB rather than inspected by eye.
//!
//! These tests need no assets and run everywhere.

use std::sync::Arc;

use mydaw_plugin_sampler::test_support::{peak, render, InstrumentBuilder};
use mydaw_plugin_sampler::{param, SamplerNode, PARAMS};
use mydaw_rt_core::{NoteEvent, ParamBlock};

const SR: f64 = 48_000.0;
const C4: u8 = 60;
const C5: u8 = 72;

/// Amplitude of one voice at the top of its layer, as `node.rs` sets it.
const VOICE_GAIN: f32 = 0.15;

/// Frames in `ms` milliseconds at [`SR`].
fn frames(ms: f64) -> usize {
    (ms / 1000.0 * SR).round() as usize
}

/// An instrument whose every sample is 1.0, `seconds` long, on every key.
///
/// Rooted at C4 and covering the keyboard, so C4 plays at step 1.0 and C5 at
/// exactly 2.0 -- the two ends of the glide tests.
fn flat(seconds: f64) -> (mydaw_plugin_sampler::test_support::TempDir, Arc<mydaw_plugin_sampler::SampleBank>) {
    flat_with(seconds, false)
}

fn flat_with(
    seconds: f64,
    one_shot: bool,
) -> (mydaw_plugin_sampler::test_support::TempDir, Arc<mydaw_plugin_sampler::SampleBank>) {
    let count = (seconds * SR) as usize;
    InstrumentBuilder::new("flat")
        .one_shot(one_shot)
        .release_ms(300.0)
        .full_range("flat.wav", C4, 48_000, vec![1.0_f32; count])
        .build()
}

/// A parameter block at the sampler's defaults, with `values` applied.
fn knobs(values: &[(&str, f32)]) -> Arc<ParamBlock> {
    let block = ParamBlock::new(PARAMS);
    for (id, value) in values {
        block
            .set_by_id(id, *value)
            .unwrap_or_else(|| panic!("'{id}' must be a parameter"));
    }
    Arc::new(block)
}

/// The output level at `ms` into `out`, averaged over a millisecond so a
/// single frame cannot decide the answer.
fn level_at(out: &[f32], ms: f64) -> f32 {
    let from = frames(ms).min(out.len().saturating_sub(1));
    let to = (from + frames(1.0)).min(out.len());
    peak(&out[from..to.max(from + 1)])
}

// ------------------------------------------------------------------- the ADSR

/// Attack, decay, sustain and release, each measured where it should be.
///
/// A 100 ms attack is half-way up at 50 ms; a 100 ms decay from 1.0 to 0.5 is
/// half-way down 50 ms after that; and a 200 ms release from 0.5 is half-way
/// down 100 ms after the key goes up. Tolerances are 8% of full scale, which is
/// wider than the ramp's own arithmetic and narrower than any wrong shape.
#[test]
fn the_amplifier_draws_the_shape_its_knobs_describe() {
    let (_dir, bank) = flat(4.0);
    let params = knobs(&[
        (param::ATTACK, 100.0),
        (param::DECAY, 100.0),
        (param::SUSTAIN, 0.5),
        (param::RELEASE, 200.0),
    ]);
    let mut node = SamplerNode::with_params(Arc::clone(&bank), params, SR);

    let (out, _) = render(&mut node, frames(400.0), 64, &[NoteEvent::note_on(0, C4, 1.0)]);
    let expect = |ms: f64, level: f32| {
        let measured = level_at(&out, ms);
        let want = level * VOICE_GAIN;
        assert!(
            (measured - want).abs() < 0.08 * VOICE_GAIN,
            "at {ms} ms the envelope should be {level}, measured {:.3}",
            measured / VOICE_GAIN
        );
    };
    expect(50.0, 0.5);
    expect(99.0, 1.0);
    expect(150.0, 0.75);
    expect(200.0, 0.5);
    expect(350.0, 0.5);
    assert_eq!(node.held_voices(), 1, "the key is still down");

    // Release: half of the sustain level after half the release time, silence
    // and a freed voice after all of it.
    let (tail, _) = render(&mut node, frames(300.0), 64, &[NoteEvent::note_off(0, C4)]);
    let half = level_at(&tail, 100.0);
    assert!(
        (half - 0.25 * VOICE_GAIN).abs() < 0.08 * VOICE_GAIN,
        "half-way through a 200 ms release the level should be 0.25, measured {:.3}",
        half / VOICE_GAIN
    );
    assert_eq!(node.active_voices(), 0, "the voice must free itself");
    assert!(
        tail[frames(220.0)..].iter().all(|sample| *sample == 0.0),
        "a finished release has to be digital silence"
    );
}

/// The default envelope is the sampler that came before it: full level from the
/// first frame, and a release the manifest's `release_ms` long.
#[test]
fn the_default_envelope_leaves_a_recording_alone() {
    let (_dir, bank) = flat(2.0);
    let mut node = SamplerNode::new(Arc::clone(&bank), SR);
    let (out, _) = render(&mut node, frames(100.0), 64, &[NoteEvent::note_on(0, C4, 1.0)]);

    assert!(
        (out[0] - VOICE_GAIN).abs() < 1e-6,
        "the very first frame must already be at full level, was {}",
        out[0]
    );
    assert!((level_at(&out, 90.0) - VOICE_GAIN).abs() < 1e-6);

    // 300 ms of release, from the manifest.
    let (tail, _) = render(&mut node, frames(400.0), 64, &[NoteEvent::note_off(0, C4)]);
    let half = level_at(&tail, 150.0);
    assert!(
        (half - 0.5 * VOICE_GAIN).abs() < 0.05 * VOICE_GAIN,
        "half-way through the manifest's 300 ms release, measured {:.3}",
        half / VOICE_GAIN
    );
    assert_eq!(node.active_voices(), 0);
}

// ---------------------------------------------------------------- the one-shot

/// A drum pad is triggered, not held: a sixteenth-note gate must not cut a tail
/// in half.
#[test]
fn a_one_shot_ignores_note_off_and_plays_its_recording_out() {
    let (_dir, bank) = flat_with(0.5, true);
    let mut node = SamplerNode::new(Arc::clone(&bank), SR);

    render(&mut node, frames(10.0), 64, &[NoteEvent::note_on(0, C4, 1.0)]);
    let (after, _) = render(&mut node, frames(200.0), 64, &[NoteEvent::note_off(0, C4)]);
    assert_eq!(
        node.held_voices(),
        1,
        "a one-shot's note-off must not release the voice"
    );
    assert!(
        (peak(&after) - VOICE_GAIN).abs() < 1e-6,
        "and must not duck it either"
    );

    // It ends when the recording does -- 500 ms -- and not before.
    render(&mut node, frames(400.0), 64, &[]);
    assert_eq!(node.active_voices(), 0, "the sample ran out");
}

/// The same instrument without the flag: note-off is a release, which is what
/// makes a played instrument playable.
#[test]
fn an_instrument_that_is_not_a_one_shot_still_answers_its_key() {
    let (_dir, bank) = flat(2.0);
    let mut node = SamplerNode::new(Arc::clone(&bank), SR);
    render(&mut node, frames(10.0), 64, &[NoteEvent::note_on(0, C4, 1.0)]);
    render(&mut node, frames(50.0), 64, &[NoteEvent::note_off(0, C4)]);
    assert_eq!(node.held_voices(), 0, "the key came up");
    assert_eq!(node.active_voices(), 1, "and the release is running");
}

/// A one-shot is stealable: a pad that could not be taken back would be 32
/// voices of decaying cymbal and no kick.
#[test]
fn a_one_shot_voice_is_still_stealable() {
    let (_dir, bank) = flat_with(4.0, true);
    let mut node = SamplerNode::new(Arc::clone(&bank), SR);
    // Different pads, so what runs out is voices rather than the same key
    // choking itself.
    for pad in 0..mydaw_plugin_sampler::VOICES + 4 {
        let key = C4 + u8::try_from(pad).expect("fewer than 128 pads");
        render(&mut node, 64, 64, &[NoteEvent::note_on(0, key, 1.0)]);
    }
    assert!(node.active_voices() <= mydaw_plugin_sampler::VOICES);
    assert!(node.stolen_voices() > 0, "the pad has to give a slot back");
}

// ------------------------------------------------------------ mono and glide

/// The glide arrives when the knob says it does.
///
/// C4 to C5 is an octave, so the playback step travels from 1.0 to exactly 2.0.
/// The pitch is a straight line in *semitones*, updated at a control rate, so
/// the arrival is measured to within one poll -- far inside the 5% this asserts.
#[test]
fn a_mono_glide_reaches_the_new_key_in_the_time_it_advertises() {
    for glide_ms in [40.0_f64, 100.0, 250.0] {
        let (_dir, bank) = flat(6.0);
        let params = knobs(&[(param::MONO, 1.0), (param::GLIDE, glide_ms as f32)]);
        let mut node = SamplerNode::with_params(Arc::clone(&bank), params, SR);

        render(&mut node, frames(20.0), 64, &[NoteEvent::note_on(0, C4, 1.0)]);
        assert_eq!(node.sounding_steps(), vec![1.0], "the first note is C4");

        // The second note starts the glide; from there it is time passing.
        // Poll in 64-frame blocks -- 1.3 ms -- until the step lands on the
        // octave, then compare with what the knob promised.
        let mut elapsed = 0_usize;
        let limit = frames(glide_ms * 3.0);
        let mut pending = vec![NoteEvent::note_on(0, C5, 1.0)];
        let arrived = loop {
            render(&mut node, 64, 64, &pending);
            pending.clear();
            elapsed += 64;
            if node
                .sounding_steps()
                .first()
                .is_some_and(|step| (step - 2.0).abs() < 1e-9)
            {
                break elapsed;
            }
            assert!(elapsed < limit, "a {glide_ms} ms glide never arrived");
        };

        let want = frames(glide_ms) as f64;
        let error = (arrived as f64 - want).abs() / want;
        assert!(
            error <= 0.05,
            "a {glide_ms} ms glide took {:.1} ms",
            arrived as f64 / SR * 1000.0
        );
        assert_eq!(node.active_voices(), 1, "mono is one voice");
        assert_eq!(node.sounding_keys(), vec![C5]);
    }
}

/// A slide is a bend, not a second note: the envelope has to keep running, or
/// what the ear hears is two notes with a bend between them.
#[test]
fn a_legato_note_bends_the_voice_without_retriggering_it() {
    let (_dir, bank) = flat(6.0);
    let params = knobs(&[
        (param::MONO, 1.0),
        (param::GLIDE, 60.0),
        // A slow attack makes a retrigger unmissable: a restarted envelope
        // would be back at zero and take 500 ms to climb out.
        (param::ATTACK, 500.0),
    ]);
    let mut node = SamplerNode::with_params(Arc::clone(&bank), params, SR);

    render(&mut node, frames(500.0), 64, &[NoteEvent::note_on(0, C4, 1.0)]);
    let before = node.envelopes();
    assert_eq!(before.len(), 1);
    assert!(before[0] > 0.95, "the attack has finished: {:?}", before);

    let (slide, _) = render(&mut node, frames(120.0), 64, &[NoteEvent::note_on(0, C5, 1.0)]);
    assert_eq!(node.active_voices(), 1, "the slide reuses the voice");
    assert!(
        node.envelopes()[0] > 0.95,
        "the envelope must not have restarted: {:?}",
        node.envelopes()
    );
    assert!(
        peak(&slide[..frames(5.0)]) > 0.9 * VOICE_GAIN,
        "and the level must not dip at the join"
    );

    // The first key's note-off belongs to a key nothing is playing any more.
    render(&mut node, frames(20.0), 64, &[NoteEvent::note_off(0, C4)]);
    assert_eq!(
        node.held_voices(),
        1,
        "letting go of the key that was slid *from* must not stop the note"
    );
    render(&mut node, frames(20.0), 64, &[NoteEvent::note_off(0, C5)]);
    assert_eq!(node.held_voices(), 0, "letting go of the key that sounds does");
}

/// Mono off is the sampler it has always been: two keys, two voices, no glide.
#[test]
fn without_mono_two_keys_are_two_voices_at_their_own_pitches() {
    let (_dir, bank) = flat(6.0);
    let params = knobs(&[(param::MONO, 0.0), (param::GLIDE, 100.0)]);
    let mut node = SamplerNode::with_params(Arc::clone(&bank), params, SR);

    render(&mut node, frames(20.0), 64, &[NoteEvent::note_on(0, C4, 1.0)]);
    render(&mut node, frames(20.0), 64, &[NoteEvent::note_on(0, C5, 1.0)]);
    assert_eq!(node.held_voices(), 2);

    let mut steps = node.sounding_steps();
    steps.sort_by(f64::total_cmp);
    assert!((steps[0] - 1.0).abs() < 1e-9, "{steps:?}");
    assert!(
        (steps[1] - 2.0).abs() < 1e-9,
        "a polyphonic note starts at its own pitch rather than gliding: {steps:?}"
    );
}

/// A mono note that arrives when nothing is held is a new note, not a bend --
/// and whatever was still fading goes with it.
#[test]
fn a_mono_note_after_a_release_starts_again_rather_than_bending() {
    let (_dir, bank) = flat(6.0);
    let params = knobs(&[
        (param::MONO, 1.0),
        (param::GLIDE, 200.0),
        (param::RELEASE, 2_000.0),
    ]);
    let mut node = SamplerNode::with_params(Arc::clone(&bank), params, SR);

    render(&mut node, frames(20.0), 64, &[NoteEvent::note_on(0, C4, 1.0)]);
    render(&mut node, frames(20.0), 64, &[NoteEvent::note_off(0, C4)]);
    assert_eq!(node.held_voices(), 0, "the long release is still fading");

    render(&mut node, frames(5.0), 64, &[NoteEvent::note_on(0, C5, 1.0)]);
    assert_eq!(node.held_voices(), 1);
    let steps = node.sounding_steps();
    assert!(
        steps.iter().any(|step| (step - 2.0).abs() < 1e-9),
        "the new note starts at its own key: {steps:?}"
    );

    // The old tail is choked rather than left to ring under the new note: mono
    // means one voice, not one voice plus its history.
    render(&mut node, frames(40.0), 64, &[]);
    assert_eq!(node.active_voices(), 1, "the old tail must be gone");
}

/// The knobs are live: a value written between blocks is heard in the next one.
#[test]
fn a_knob_moved_between_blocks_is_read_on_the_next_one() {
    let (_dir, bank) = flat(6.0);
    let params = knobs(&[(param::MONO, 0.0)]);
    let mut node = SamplerNode::with_params(Arc::clone(&bank), Arc::clone(&params), SR);

    render(&mut node, frames(20.0), 64, &[NoteEvent::note_on(0, C4, 1.0)]);
    params.set_by_id(param::MONO, 1.0).expect("mono exists");
    render(&mut node, frames(20.0), 64, &[NoteEvent::note_on(0, C5, 1.0)]);
    assert_eq!(
        node.held_voices(),
        1,
        "turning mono on has to take effect on the next note"
    );
}
