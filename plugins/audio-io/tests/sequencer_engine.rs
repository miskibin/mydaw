//! The whole step, offline: a programmed beat rendered through the real audio
//! callback, then *measured*.
//!
//! Every other test in the workspace checks one link of the chain. This one
//! closes it: a [`Pattern`] is compiled and published exactly as the shell
//! publishes it, [`run_block`] runs exactly as the cpal callback runs it, the
//! drum kit turns the scheduled events into audio, and the resulting samples are
//! analysed with `debug-tap`'s onset detector -- the same one the hardware
//! verification uses.
//!
//! ```text
//!   Pattern -> compile -> PatternHandle ==> PatternSlot -> Scheduler
//!                                                             |
//!                                              NoteEventBuffer v
//!   samples <- run_block <- DrumKitNode <- ProcessCtx::events
//!      |
//!      '-> detect_onsets -> align_grid -> "every hit within N ms of the grid"
//! ```
//!
//! No device is opened, so this runs on a headless CI machine -- which means the
//! grid assertion is a *gate*, not a thing somebody remembers to check.

use std::sync::Arc;

use mydaw_plugin_audio_io::{run_block, AudioIoStats, RtState, RtStateParams};
use mydaw_plugin_debug_tap::{align_grid, detect_onsets, GridReport, Onset, OnsetConfig};
use mydaw_plugin_instruments::DrumKitNode;
use mydaw_plugin_sequencer::{
    song_channel, step_boundary, step_len_samples, ChannelClip, CompiledSong, MelodicNote, Pattern,
    StepRow, DEFAULT_STEPS, SONG_QUEUE_CAPACITY, STEP_GATE_TICKS, TICKS_PER_STEP,
};
use mydaw_rt_core::{
    graph_channel, note_channel, rt_channel, GcThread, GraphSpec, RtCommand, TransportClock,
    DEFAULT_BPM,
};

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;
const MAX_BLOCK: usize = 512;
const NOTE_CAPACITY: usize = 128;

/// General MIDI drum keys, as the default pattern addresses them.
const KICK: u8 = 36;
const SNARE: u8 = 38;
const CLOSED_HAT: u8 = 42;

/// The demo beat: four-on-the-floor, snare on 2 and 4, eighth-note hats.
///
/// Fourteen clicks in the grid -- four kicks, two snares, eight hats.
fn demo_beat() -> Pattern {
    // Everything on **channel 0**, as notes rather than as three step rows.
    //
    // Two reasons, both about what this file is testing. It renders a single
    // `DrumKitNode` inside one track chain, and a track chain only sees its own
    // channel -- three channels would mean two thirds of the beat never
    // arriving. And a pattern holds one clip per channel, so one channel cannot
    // have three step rows at three keys; the kit picks its drum from the key,
    // which is exactly the arrangement the per-drum instruments replaced.
    //
    // Steps compile to notes anyway (see `mydaw_plugin_sequencer::pattern`), so
    // writing them as notes here changes nothing about the timing claim under
    // test: a note at `n * TICKS_PER_STEP` is a step at `n`, to the sample.
    let mut pattern = Pattern::silent(1, "Demo");
    let mut clip = ChannelClip::melodic(0);
    let mut hit = |key: u8, step: usize| {
        clip.notes.push(MelodicNote::new(
            key,
            step as u32 * TICKS_PER_STEP,
            STEP_GATE_TICKS,
        ));
    };
    for step in [0, 4, 8, 12] {
        hit(KICK, step);
    }
    for step in [4, 12] {
        hit(SNARE, step);
    }
    for step in (0..DEFAULT_STEPS).step_by(2) {
        hit(CLOSED_HAT, step);
    }
    pattern.channels.push(clip);
    pattern
}

/// Render `frames` frames of `pattern` through the real callback, returning the
/// left channel.
fn render(pattern: &Pattern, frames: usize, bpm: f64) -> (Vec<f32>, Arc<AudioIoStats>) {
    let gc = GcThread::spawn();
    let (mut graph, graph_slot) = graph_channel(4, gc.handle());
    let (mut commands, command_rx) = rt_channel::<RtCommand>(16);
    let (_notes_tx, notes) = note_channel(NOTE_CAPACITY);
    let (mut song_tx, song_slot) = song_channel(SONG_QUEUE_CAPACITY, gc.handle());
    let stats = Arc::new(AudioIoStats::default());

    graph
        .install(GraphSpec::new().with(Box::new(DrumKitNode::new(SR))))
        .expect("the graph should install");
    song_tx
        .install(CompiledSong::new(vec![pattern.compile()], 0, Vec::new()))
        .expect("the song should install");
    commands.push(RtCommand::SetBpm(bpm)).unwrap();
    commands.push(RtCommand::SetPlaying(true)).unwrap();

    let mut state = RtState::new(RtStateParams {
        graph: graph_slot,
        commands: command_rx,
        notes,
        note_capacity: NOTE_CAPACITY,
        song: song_slot,
        meters: None,
        debug_tap: None,
        spectrum: None,
        transport: Arc::new(TransportClock::new(SR)),
        sample_rate: SR,
        out_channels: 2,
        max_block: MAX_BLOCK,
        stats: Arc::clone(&stats),
    });

    let mut interleaved = vec![0.0_f32; BLOCK * 2];
    let mut left = Vec::with_capacity(frames);
    while left.len() < frames {
        run_block(&mut state, &mut interleaved);
        left.extend(interleaved.chunks_exact(2).map(|pair| pair[0]));
    }
    left.truncate(frames);
    (left, stats)
}

/// Samples in two bars at `bpm`.
fn two_bars(bpm: f64) -> usize {
    (step_len_samples(SR, bpm) * (DEFAULT_STEPS * 2) as f64).ceil() as usize
}

/// A one-line summary an agent (or a reader of a failure) can act on.
fn describe(label: &str, onsets: &[Onset], report: &GridReport) -> String {
    format!(
        "{label}: {} onsets, worst {:.3} ms / {:.1} samples, mean {:.3} ms, contiguous {}",
        onsets.len(),
        report.max_abs_error_ms,
        report.max_abs_error_samples,
        report.mean_abs_error_ms,
        report.is_contiguous()
    )
}

#[test]
fn a_programmed_beat_reaches_the_output_with_every_hit_on_the_grid() {
    let bpm = DEFAULT_BPM;
    let step_len = step_len_samples(SR, bpm);
    let (left, stats) = render(&demo_beat(), two_bars(bpm), bpm);

    assert_eq!(stats.note_events_dropped(), 0, "no event may be lost");
    assert!(
        left.iter().any(|s| s.abs() > 0.05),
        "the beat has to be audible"
    );
    assert!(left.iter().all(|s| s.is_finite()));

    // The kick lives below 120 Hz, where nothing else in this kit does, so
    // counting kicks is counting onsets in that band. Two bars of 4/4 with a
    // four-on-the-floor is eight.
    let kicks = detect_onsets(&left, SR, &OnsetConfig::low(120.0));
    let beat = step_len * 4.0;
    let kick_grid = align_grid(&kicks, beat, SR);
    let kick_report = describe("kick", &kicks, &kick_grid);
    println!("{kick_report}");
    assert_eq!(kicks.len(), 8, "{kick_report}");
    assert!(kick_grid.is_contiguous(), "{kick_report}");
    assert!(
        kick_grid.within(1.0),
        "the kick must sit on the beat grid: {kick_report}"
    );

    // The hats (and the snare's noise, which lands on hat steps) live above
    // 4 kHz. Eighth notes over two bars is sixteen, with none missing.
    let hats = detect_onsets(&left, SR, &OnsetConfig::high(4_000.0));
    let eighth = step_len * 2.0;
    let hat_grid = align_grid(&hats, eighth, SR);
    let hat_report = describe("hat", &hats, &hat_grid);
    println!("{hat_report}");
    assert_eq!(hats.len(), 16, "{hat_report}");
    assert!(hat_grid.is_contiguous(), "{hat_report}");
    assert!(
        hat_grid.within(2.0),
        "the hats must sit on the eighth-note grid: {hat_report}"
    );

    // Tightened on the off-beat hats alone. The detector reports the point at
    // which a transient reaches a fraction of its own level, so its latency
    // depends on the attack it is looking at -- and steps 4 and 12 carry a
    // snare *and* a hat, which is a different attack from a hat alone. That is
    // a property of the measurement, not of the engine, so the sharp assertion
    // is made where the source is single: steps 2, 6, 10 and 14.
    let offbeat: Vec<Onset> = hats
        .iter()
        .enumerate()
        .filter(|(index, _)| index % 2 == 1)
        .map(|(_, onset)| *onset)
        .collect();
    let offbeat_grid = align_grid(&offbeat, step_len * 4.0, SR);
    let offbeat_report = describe("off-beat hat", &offbeat, &offbeat_grid);
    println!("{offbeat_report}");
    assert_eq!(offbeat.len(), 8, "{offbeat_report}");
    assert!(offbeat_grid.is_contiguous(), "{offbeat_report}");
    assert!(
        offbeat_grid.within(0.5),
        "one source, one attack, one grid: {offbeat_report}"
    );
}

#[test]
fn the_beat_stays_on_the_grid_at_a_different_tempo() {
    // 174.3 BPM: a step length with a long fractional part, which is where an
    // engine that accumulates instead of multiplying starts to drift.
    let bpm = 174.3;
    let step_len = step_len_samples(SR, bpm);
    let (left, _) = render(&demo_beat(), two_bars(bpm), bpm);

    let kicks = detect_onsets(&left, SR, &OnsetConfig::low(120.0));
    let grid = align_grid(&kicks, step_len * 4.0, SR);
    let report = describe("kick @ 174.3", &kicks, &grid);
    println!("{report}");
    assert_eq!(kicks.len(), 8, "{report}");
    assert!(grid.is_contiguous(), "{report}");
    assert!(grid.within(1.0), "{report}");
}

#[test]
fn an_empty_pattern_renders_digital_silence() {
    // The other half of the claim: the grid is what makes sound, and an empty
    // grid makes none. A sequencer that leaks a hit from somewhere would show
    // up here and nowhere else.
    let (left, stats) = render(
        &Pattern::silent(1, "Empty"),
        two_bars(DEFAULT_BPM),
        DEFAULT_BPM,
    );
    assert!(
        left.iter().all(|s| *s == 0.0),
        "an empty pattern must be silent, not quiet"
    );
    assert_eq!(stats.note_events_dropped(), 0);
    assert!(detect_onsets(&left, SR, &OnsetConfig::default()).is_empty());
}

#[test]
fn the_hits_land_on_the_samples_the_scheduler_computed() {
    // The strongest form of the claim, without any detector in the way: the
    // first non-zero sample of a capture whose only hit is step 0 of bar 1 has
    // to be exactly `step_boundary(16, step_len)`.
    let bpm = DEFAULT_BPM;
    let step_len = step_len_samples(SR, bpm);
    let mut pattern = Pattern::silent(1, "One kick");
    let mut clip = ChannelClip::stepped(0, KICK);
    clip.steps = Some(StepRow::empty(KICK).with_steps(&[0]));
    pattern.channels.push(clip);

    let (left, _) = render(&pattern, two_bars(bpm), bpm);
    let bar = step_boundary(DEFAULT_STEPS as u64, step_len) as usize;

    // The kick's first sample is its noise click, so the very first non-zero
    // sample of each hit is the hit.
    assert_ne!(left[0], 0.0, "step 0 of bar 0 starts at sample 0");
    let second = left[1..]
        .iter()
        .enumerate()
        .skip_while(|(index, _)| *index + 1 < bar / 2)
        .find(|(_, sample)| **sample != 0.0)
        .map(|(index, _)| index + 1)
        .expect("bar 1 should have fired");
    assert_eq!(
        second, bar,
        "the second bar starts exactly where the grid says"
    );
}
