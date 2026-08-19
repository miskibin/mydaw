//! Bit-determinism: the same notes always produce the same samples.
//!
//! This is the property everything else in the crate leans on. The spectral
//! tests assert numbers rather than ranges because the numbers cannot move; a
//! future offline render will be reproducible for the same reason; and a bug
//! report can be replayed instead of described. It is also the property most
//! easily lost by accident -- one uninitialised field, one un-reseeded noise
//! generator, one `SystemTime`-derived anything -- so it is tested directly and
//! with `assert_eq` on the raw bits, not on a tolerance.
//!
//! Three separate claims live here:
//!
//! 1. two fresh nodes fed the same events agree, sample for sample;
//! 2. a node re-fed the same events after its voices have died agrees with
//!    itself (nothing accumulates across a note);
//! 3. the output does not depend on how the frames were divided into blocks,
//!    which is the thing a real device varies from run to run.

use mydaw_plugin_instruments::drum_kit::{CLOSED_HAT_KEY, KICK_KEY, OPEN_HAT_KEY, SNARE_KEY};
use mydaw_plugin_instruments::test_support::{peak, render, render_at};
use mydaw_plugin_instruments::{DrumKitNode, PolySynthNode};
use mydaw_rt_core::{NoteEvent, RtNode};

const SR: f64 = 48_000.0;

/// A busy, deliberately awkward drum pattern: every drum, a choke, a retrigger,
/// two events on the same frame, and a key the kit does not have.
fn drum_events() -> Vec<NoteEvent> {
    vec![
        NoteEvent::note_on(0, KICK_KEY, 1.0),
        NoteEvent::note_on(0, OPEN_HAT_KEY, 0.7),
        NoteEvent::note_on(64, SNARE_KEY, 0.9),
        NoteEvent::note_on(64, KICK_KEY, 0.4),
        NoteEvent::note_on(128, CLOSED_HAT_KEY, 0.55),
        NoteEvent::note_on(200, 60, 1.0),
        NoteEvent::note_off(200, KICK_KEY),
        NoteEvent::note_on(255, SNARE_KEY, 0.3),
    ]
}

/// A chord with a retrigger, a release, and a ninth note that steals a voice.
fn synth_events() -> Vec<NoteEvent> {
    vec![
        NoteEvent::note_on(0, 60, 1.0),
        NoteEvent::note_on(0, 64, 0.8),
        NoteEvent::note_on(32, 67, 0.6),
        NoteEvent::note_on(64, 71, 0.5),
        NoteEvent::note_on(96, 74, 0.5),
        NoteEvent::note_on(128, 77, 0.5),
        NoteEvent::note_on(160, 81, 0.5),
        NoteEvent::note_on(192, 84, 0.5),
        // All eight busy: this one steals the oldest.
        NoteEvent::note_on(224, 88, 1.0),
        NoteEvent::note_on(240, 64, 1.0), // retrigger a held key
        NoteEvent::note_off(255, 67),
    ]
}

/// Assert two renders are identical to the bit, reporting the first divergence.
fn assert_identical(a: &[f32], b: &[f32], what: &str) {
    assert_eq!(a.len(), b.len(), "{what}: different lengths");
    for (index, (left, right)) in a.iter().zip(b.iter()).enumerate() {
        assert_eq!(
            left.to_bits(),
            right.to_bits(),
            "{what}: diverged at sample {index} ({left} vs {right})"
        );
    }
}

#[test]
fn two_drum_kits_given_the_same_events_produce_the_same_samples() {
    let events = drum_events();
    let (first_l, first_r) = render(&mut DrumKitNode::new(SR), 24_000, 256, &events);
    let (second_l, second_r) = render(&mut DrumKitNode::new(SR), 24_000, 256, &events);

    assert!(
        peak(&first_l) > 0.1,
        "the pattern must actually make a sound"
    );
    assert_identical(&first_l, &second_l, "drum kit, left");
    assert_identical(&first_r, &second_r, "drum kit, right");
    assert_identical(&first_l, &first_r, "drum kit is mono, fanned out");
}

#[test]
fn two_poly_synths_given_the_same_events_produce_the_same_samples() {
    let events = synth_events();
    let (first_l, first_r) = render(&mut PolySynthNode::new(SR), 24_000, 256, &events);
    let (second_l, second_r) = render(&mut PolySynthNode::new(SR), 24_000, 256, &events);

    assert!(peak(&first_l) > 0.1, "the chord must actually make a sound");
    assert_identical(&first_l, &second_l, "poly synth, left");
    assert_identical(&first_r, &second_r, "poly synth, right");
    assert_identical(&first_l, &first_r, "poly synth is mono, fanned out");
}

#[test]
fn a_kit_that_has_gone_quiet_replays_a_hit_identically() {
    // Nothing may survive a voice: not a noise state, not a filter, not a
    // phase. Two seconds is well past every decay in the kit.
    let mut node = DrumKitNode::new(SR);
    let events = [NoteEvent::note_on(0, SNARE_KEY, 0.8)];

    let (first, _) = render(&mut node, 96_000, 512, &events);
    assert_eq!(node.active_voices(), 0, "the kit must have gone quiet");
    let (second, _) = render(&mut node, 96_000, 512, &events);

    assert_identical(&first, &second, "a replayed snare");
}

#[test]
fn a_synth_that_has_released_replays_a_note_identically() {
    let mut node = PolySynthNode::new(SR);
    let events = [
        NoteEvent::note_on(0, 69, 0.9),
        NoteEvent::note_off(4_800, 69),
    ];

    let (first, _) = render(&mut node, 48_000, 4_800, &events);
    assert_eq!(node.active_voices(), 0, "the voice must have released");
    let (second, _) = render(&mut node, 48_000, 4_800, &events);

    assert_identical(&first, &second, "a replayed note");
}

/// A real device hands the engine whatever block size it feels like, and it can
/// change between runs. An instrument whose output depended on that would be
/// unreproducible in exactly the situation where reproducibility matters.
#[test]
fn the_output_does_not_depend_on_the_block_size() {
    // Offset 0 only: an event at offset 256 genuinely means something different
    // in a 64-frame block, and the engine clamps it to the block's end.
    let drums = [
        NoteEvent::note_on(0, KICK_KEY, 1.0),
        NoteEvent::note_on(0, CLOSED_HAT_KEY, 0.8),
    ];
    let synth = [
        NoteEvent::note_on(0, 60, 1.0),
        NoteEvent::note_on(0, 67, 0.7),
    ];

    for (label, node_a, node_b, events) in [
        (
            "drum kit",
            Box::new(DrumKitNode::new(SR)) as Box<dyn RtNode>,
            Box::new(DrumKitNode::new(SR)) as Box<dyn RtNode>,
            &drums[..],
        ),
        (
            "poly synth",
            Box::new(PolySynthNode::new(SR)) as Box<dyn RtNode>,
            Box::new(PolySynthNode::new(SR)) as Box<dyn RtNode>,
            &synth[..],
        ),
    ] {
        let mut node_a = node_a;
        let mut node_b = node_b;
        let (small, _) = render(node_a.as_mut(), 12_288, 64, events);
        let (large, _) = render(node_b.as_mut(), 12_288, 1_024, events);
        assert_identical(&small, &large, label);
    }
}

/// Determinism is a property of a *configuration*, not a constant: two nodes at
/// different sample rates should not agree, or one of them is ignoring the rate.
#[test]
fn the_sample_rate_actually_reaches_the_dsp() {
    let events = [NoteEvent::note_on(0, 69, 1.0)];
    let (at_48k, _) = render_at(
        &mut PolySynthNode::new(48_000.0),
        4_800,
        480,
        &events,
        48_000.0,
    );
    let (at_44k, _) = render_at(
        &mut PolySynthNode::new(44_100.0),
        4_800,
        480,
        &events,
        44_100.0,
    );
    assert_ne!(
        at_48k, at_44k,
        "a synth that renders the same samples at two rates is not tuning itself"
    );

    let hits = [NoteEvent::note_on(0, KICK_KEY, 1.0)];
    let (at_48k, _) = render_at(&mut DrumKitNode::new(48_000.0), 4_800, 480, &hits, 48_000.0);
    let (at_44k, _) = render_at(&mut DrumKitNode::new(44_100.0), 4_800, 480, &hits, 44_100.0);
    assert_ne!(at_48k, at_44k);
}
