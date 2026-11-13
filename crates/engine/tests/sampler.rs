//! The sampler channel and the standard library: what the settings do to the sound and how
//! library samples are stored in a project.

use std::path::PathBuf;
use std::sync::Arc;

use wave_engine::*;

const SR: u32 = 48000;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("wave-smp-{}-{}", std::process::id(), name));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// One second, 440 Hz: loud (0.8) for the first half, quiet (0.1) for the second.
fn two_level_wav(name: &str) -> PathBuf {
    // one file per call: tests run in parallel and several share a `name`, so one could read the
    // file while another was still writing it ("no suitable format reader found")
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let path = tmp(name).join(format!("two-level-{}.wav", N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    let spec = hound::WavSpec { channels: 1, sample_rate: SR, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut w = hound::WavWriter::create(&path, spec).unwrap();
    for i in 0..SR {
        let amp = if i < SR / 2 { 0.8 } else { 0.1 };
        w.write_sample((amp * (std::f64::consts::TAU * 440.0 * i as f64 / SR as f64).sin()) as f32).unwrap();
    }
    w.finalize().unwrap();
    path
}

/// An engine at 120 BPM whose only channel plays the test file with `set` applied; `notes`
/// are (step, length in steps, key) in a two-bar pattern on track 0. No master limiter.
fn engine(name: &str, set: impl Fn(&mut SamplerState), notes: &[(f32, f32, Option<u8>)]) -> (Arc<Engine>, Id) {
    let mut p = Project::empty();
    p.bpm = 120.0;
    p.inserts[0].fx.clear();
    let e = Engine::new(p, Arc::new(NoPlugins));
    let (srcs, _) = e.import_audio(&[two_level_wav(name)], None).unwrap();
    let mut q = e.project();
    let ch = q.alloc_id();
    let mut sampler = SamplerState::default();
    set(&mut sampler);
    let channel = Channel::new(ch, "Test", "#4f8ef7", ChannelSrc::Sample { source: srcs[0], sampler });
    let pattern = Pattern {
        id: 1,
        name: "P".into(),
        color: "#4f8ef7".into(),
        len: 8.0,
        min_len: 8.0,
        notes: notes.iter().map(|&(s, l, k)| Note::new(ch, s, l, k)).collect(),
        swing: 0.0,
    };
    let clip = Clip {
        id: 2,
        track: 0,
        start: 0.0,
        len: 8.0,
        offset: 0.0,
        kind: ClipKind::Pat,
        source: 1,
        muted: false,
        gain: 0.0,
        fade_in: 0.0,
        fade_out: 0.0,
        pan: 0.0,
        norm: false,
        rev: false,
        tune: None,
        stretch: 1.0,
        resample: false,
        pitch: 0.0,
    };
    e.apply(&[Op::PutChannel { channel, index: None }, Op::PutPattern { pattern }, Op::PutClips { clips: vec![clip] }], false, None)
        .unwrap();
    (e, ch)
}

/// Left channel of the first `beats` beats.
fn render(e: &Engine, beats: f64) -> Vec<f32> {
    e.render(0.0, beats, 0.0, SR).unwrap().chunks_exact(2).map(|f| f[0]).collect()
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0f32, |m, v| m.max(v.abs()))
}

/// Peak of `x` between two times in seconds.
fn peak_in(x: &[f32], a: f64, b: f64) -> f32 {
    peak(&x[(a * SR as f64) as usize..((b * SR as f64) as usize).min(x.len())])
}

fn one(set: impl Fn(&mut SamplerState), len_steps: f32) -> Vec<f32> {
    let (e, _) = engine("one", set, &[(0.0, len_steps, None)]);
    render(&e, 4.0)
}

#[test]
fn a_one_shot_plays_the_whole_file_whatever_the_note_length() {
    let out = one(|_| {}, 1.0);
    assert!((peak_in(&out, 0.0, 0.45) - 0.8).abs() < 0.01);
    // the note was a sixteenth (0.125 s) but the file's quiet second half still plays
    assert!((peak_in(&out, 0.55, 0.95) - 0.1).abs() < 0.01);
    assert_eq!(peak_in(&out, 1.01, 2.0), 0.0);
    // and it starts as recorded: no fade softens the first cycle
    assert!(peak_in(&out, 0.0, 0.002) > 0.5);
}

#[test]
fn gate_stops_with_the_note_and_fades_over_the_release() {
    let gate = |release: f32| {
        move |s: &mut SamplerState| {
            s.gate = true;
            s.release = release;
        }
    };
    // at the test tempo (120 BPM) a sixteenth is 0.125 s
    let short = one(gate(10.0), 1.0);
    assert!((peak_in(&short, 0.0, 0.12) - 0.8).abs() < 0.01);
    assert_eq!(peak_in(&short, 0.125 + 0.02, 2.0), 0.0, "10 ms after the note the sample is gone");
    let long = one(gate(200.0), 1.0);
    let after = peak_in(&long, 0.125 + 0.05, 0.125 + 0.07);
    assert!(after > 0.05 && after < 0.5, "a 200 ms release is still fading 50 ms in: {after}");
    assert_eq!(peak_in(&long, 0.125 + 0.3, 2.0), 0.0);
    // a note longer than the file just ends with the file
    let whole = one(gate(10.0), 16.0);
    assert!((peak_in(&whole, 0.55, 0.95) - 0.1).abs() < 0.01);
}

#[test]
fn start_and_end_pick_the_part_that_plays() {
    // the second half only: quiet from the first sample
    let tail = one(|s| s.start = 0.5, 1.0);
    assert!((peak_in(&tail, 0.01, 0.45) - 0.1).abs() < 0.01);
    assert_eq!(peak_in(&tail, 0.51, 2.0), 0.0);
    // the first quarter only
    let head = one(|s| s.end = 0.25, 1.0);
    assert!((peak_in(&head, 0.0, 0.24) - 0.8).abs() < 0.01);
    assert_eq!(peak_in(&head, 0.2501, 2.0), 0.0);
    // a cut inside the waveform is faded, not clicked: the last half millisecond is quiet
    assert!(peak_in(&head, 0.2496, 0.25) < 0.3);
}

#[test]
fn reverse_plays_the_part_backwards() {
    let out = one(|s| s.reverse = true, 1.0);
    assert!((peak_in(&out, 0.05, 0.45) - 0.1).abs() < 0.01, "the quiet end comes first");
    assert!((peak_in(&out, 0.55, 0.95) - 0.8).abs() < 0.01);
    assert_eq!(peak_in(&out, 1.01, 2.0), 0.0);
    // reversed and trimmed: the loud first half, backwards
    let half = one(
        |s| {
            s.reverse = true;
            s.end = 0.5;
        },
        1.0,
    );
    assert!((peak_in(&half, 0.05, 0.45) - 0.8).abs() < 0.01);
    assert_eq!(peak_in(&half, 0.51, 2.0), 0.0);
}

#[test]
fn the_envelope_shapes_the_level() {
    // attack: silent at first, full after it
    let slow = one(|s| s.attack = 200.0, 1.0);
    assert!(peak_in(&slow, 0.0, 0.01) < 0.05);
    let mid = peak_in(&slow, 0.099, 0.102);
    assert!((mid - 0.4).abs() < 0.03, "half way up the attack: {mid}");
    assert!((peak_in(&slow, 0.21, 0.45) - 0.8).abs() < 0.01);
    // decay to a sustain level
    let held = one(
        |s| {
            s.decay = 100.0;
            s.sustain = 0.5;
        },
        1.0,
    );
    assert!((peak_in(&held, 0.11, 0.45) - 0.4).abs() < 0.01);
    // sustain 0: the decay is the whole sound, however long the file
    let pluck = one(
        |s| {
            s.decay = 100.0;
            s.sustain = 0.0;
        },
        1.0,
    );
    assert!(peak_in(&pluck, 0.0, 0.01) > 0.7);
    assert_eq!(peak_in(&pluck, 0.101, 2.0), 0.0);
}

#[test]
fn transpose_and_fine_tune_repitch_on_top_of_the_note() {
    let zero_crossings = |x: &[f32]| x.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count() as f64;
    let hz = |set: fn(&mut SamplerState), key: Option<u8>| {
        let (e, _) = engine("tune", set, &[(0.0, 1.0, key)]);
        let out = render(&e, 1.0);
        zero_crossings(&out[SR as usize / 100..SR as usize / 5]) / (0.2 - 0.01)
    };
    assert!((hz(|_| {}, None) - 440.0).abs() < 6.0);
    assert!((hz(|s| s.semi = 12, None) - 880.0).abs() < 8.0);
    assert!((hz(|s| s.semi = -12, None) - 220.0).abs() < 6.0);
    // +100 cents is a semitone; on top of a note a semitone down it cancels
    assert!((hz(|s| s.fine = 100.0, Some(59)) - 440.0).abs() < 6.0);
    // an octave up is also twice as fast: the file is over in half the time
    let (e, _) = engine("tune", |s| s.semi = 12, &[(0.0, 1.0, None)]);
    assert_eq!(peak_in(&render(&e, 4.0), 0.51, 2.0), 0.0);
}

#[test]
fn normalise_brings_the_peak_up() {
    assert!((peak(&one(|_| {}, 1.0)) - 0.8).abs() < 0.01);
    let want = 10f32.powf(-0.3 / 20.0);
    let got = peak(&one(|s| s.norm = true, 1.0));
    assert!((got - want).abs() < 0.01, "normalised peak {got}, expected {want}");
}

#[test]
fn a_cut_group_silences_its_sounding_notes() {
    // two notes of the same channel a beat apart; the file lasts two beats
    let notes = [(0.0, 1.0, None), (4.0, 1.0, None)];
    let free = render(&engine("cut", |_| {}, &notes).0, 4.0);
    // without a group they overlap: the first note's quiet half under the second's loud half
    assert!(peak_in(&free, 0.52, 0.95) > 0.8);
    let cut = render(&engine("cut", |s| s.cut = 1, &notes).0, 4.0);
    let (a, b) = (peak_in(&cut, 0.52, 0.95), peak_in(&cut, 1.02, 1.45));
    assert!((a - 0.8).abs() < 0.01 && (b - 0.1).abs() < 0.01, "only the second note sounds: {a}, {b}");
    // the cut is at the new note, not earlier: the first note is whole until then
    assert!((peak_in(&cut, 0.40, 0.495) - 0.8).abs() < 0.01);

    // across channels: a second channel in the group cuts the first; another group does not
    let across = |group: u8| {
        let (e, ch) = engine("cut2", |s| s.cut = 1, &[(0.0, 1.0, None)]);
        let mut p = e.project();
        let src = p.channel(ch).unwrap().sample_source().unwrap();
        let other = p.alloc_id();
        // the other channel plays only the quiet second half, so the two can be told apart
        let sampler = SamplerState { cut: group, start: 0.5, ..Default::default() };
        let mut pat = p.patterns[0].clone();
        pat.notes.push(Note::new(other, 4.0, 1.0, None));
        e.apply(
            &[
                Op::PutChannel { channel: Channel::new(other, "Other", "#fff", ChannelSrc::Sample { source: src, sampler }), index: None },
                Op::PutPattern { pattern: pat },
            ],
            false,
            None,
        )
        .unwrap();
        peak_in(&render(&e, 4.0), 0.55, 0.95)
    };
    assert!((across(1) - 0.1).abs() < 0.02, "same group: the first channel is cut");
    assert!(across(2) > 0.1 + 0.05, "another group: both sound");
}

#[test]
fn settings_out_of_range_are_refused() {
    let (e, ch) = engine("check", |_| {}, &[]);
    let base = e.project().channel(ch).unwrap().clone();
    let with = |f: &dyn Fn(&mut SamplerState)| {
        let mut c = base.clone();
        if let ChannelSrc::Sample { sampler, .. } = &mut c.src {
            f(sampler);
        }
        e.apply(&[Op::PutChannel { channel: c, index: None }], false, None)
    };
    assert!(with(&|s| s.start = 0.6).is_ok());
    assert!(with(&|s| (s.start, s.end) = (0.6, 0.6)).is_err(), "an empty part");
    assert!(with(&|s| s.end = 1.5).is_err());
    assert!(with(&|s| s.start = f32::NAN).is_err());
    assert!(with(&|s| s.semi = 25).is_err());
    assert!(with(&|s| s.fine = 101.0).is_err());
    assert!(with(&|s| s.attack = -1.0).is_err());
    assert!(with(&|s| s.sustain = 1.1).is_err());
    assert!(with(&|s| s.release = 0.0).is_err());
    assert!(with(&|s| s.cut = 9).is_err());
    assert!(with(&|s| s.cut = 8).is_ok());
    // a refused edit changes nothing, and an accepted one is one undo step
    let ChannelSrc::Sample { sampler, .. } = e.project().channel(ch).unwrap().src else { panic!() };
    assert_eq!((sampler.start, sampler.cut), (0.0, 8));
    e.undo();
    let ChannelSrc::Sample { sampler, .. } = e.project().channel(ch).unwrap().src else { panic!() };
    assert_eq!((sampler.start, sampler.cut), (0.6, 0));
}

#[test]
fn settings_survive_a_save_and_files_without_them_get_the_defaults() {
    let (e, ch) = engine(
        "save",
        |s| {
            s.start = 0.25;
            s.reverse = true;
            s.semi = -5;
            s.gate = true;
            s.decay = 80.0;
            s.sustain = 0.4;
            s.cut = 3;
        },
        &[(0.0, 2.0, None)],
    );
    let path = tmp("save").join("song.wave");
    e.save(&path).unwrap();
    let before = render(&e, 2.0);
    let f = Engine::new(Project::empty(), Arc::new(NoPlugins));
    assert!(f.open(&path).unwrap().is_empty());
    assert_eq!(f.project().channel(ch).unwrap().src, e.project().channel(ch).unwrap().src);
    assert!(render(&f, 2.0) == before);
    // a channel written before the sampler had settings
    let old: Channel =
        serde_json::from_value(serde_json::json!({ "id": 7, "name": "Stab", "color": "#fff", "kind": "sample", "source": 3 })).unwrap();
    assert_eq!(old.src, ChannelSrc::Sample { source: 3, sampler: SamplerState::default() });
}

#[test]
fn library_samples_are_stored_by_a_portable_path_and_shared() {
    let e = Engine::new(Project::empty(), Arc::new(NoPlugins));
    let n = e.project().sources.len();
    // the browser hands over the file where the library is installed on this computer
    let dir = factory::install().unwrap();
    let file = dir.join("triangle.flac");
    assert!(file.is_file());
    let (a, clips) = e.import_audio(std::slice::from_ref(&file), Some((2, 4.0))).unwrap();
    let p = e.project();
    let src = p.sources.iter().find(|s| s.id == a[0]).unwrap();
    assert_eq!(src.path, "lib:Standard/triangle.flac");
    assert_eq!(src.name, "Triangle");
    assert_eq!(clips.len(), 1);
    // again (a sampler channel, another clip): the same source, not a copy
    let (b, _) = e.import_audio(&[file], None).unwrap();
    assert_eq!(b, a);
    assert_eq!(e.project().sources.len(), n + 1);
    // the saved file names no folder of this computer, and opens with its audio
    let path = tmp("lib").join("song.wave");
    e.save(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("lib:Standard/triangle.flac"));
    assert!(!text.contains(&factory::library_dir().display().to_string()));
    let f = Engine::new(Project::empty(), Arc::new(NoPlugins));
    assert!(f.open(&path).unwrap().is_empty());
    assert!(peak(&f.render(4.0, 6.0, 0.0, SR).unwrap()) > 0.05);
    // a sample of a kit that is not installed is reported, not fatal
    let broken = text.replace("lib:Standard/triangle.flac", "lib:No Such Kit/triangle.flac");
    let path2 = tmp("lib").join("broken.wave");
    std::fs::write(&path2, broken).unwrap();
    let warnings = f.open(&path2).unwrap();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("not installed"), "{}", warnings[0]);
}

#[test]
fn a_new_project_does_not_inherit_audio_from_the_previous_one() {
    // ids start over: the audio of source N of the old project must not play as source N of the new
    let (e, _) = engine("fresh", |_| {}, &[(0.0, 1.0, None)]);
    e.new_project(Project::empty());
    let p = e.project();
    assert!(p.sources.iter().all(|s| s.path.starts_with("lib:")));
    let kick = p.channels[0].id;
    let pattern = Pattern {
        id: 1,
        name: "P".into(),
        color: "#fff".into(),
        len: 4.0,
        min_len: 4.0,
        notes: vec![Note::new(kick, 0.0, 1.0, None)],
        swing: 0.0,
    };
    let clip = Clip {
        id: 2,
        track: 0,
        start: 0.0,
        len: 4.0,
        offset: 0.0,
        kind: ClipKind::Pat,
        source: 1,
        muted: false,
        gain: 0.0,
        fade_in: 0.0,
        fade_out: 0.0,
        pan: 0.0,
        norm: false,
        rev: false,
        tune: None,
        stretch: 1.0,
        resample: false,
        pitch: 0.0,
    };
    e.apply(&[Op::PutPattern { pattern }, Op::PutClips { clips: vec![clip] }], false, None).unwrap();
    let out = e.render(0.0, 4.0, 0.0, SR).unwrap();
    // the kick is 0.38 s long; the test file would ring for a second
    assert!(peak(&out) > 0.2);
    assert_eq!(peak(&out[2 * SR as usize * 6 / 10..]), 0.0);
}

/// The in-browser mock (`ui/src/api/mock.ts`) must start a new project like the engine and list
/// the same standard library, so it reads both from a file generated here. After a change to
/// the starter kit or the library, regenerate it:
/// `WAVE_UPDATE_FIXTURES=1 cargo test -p wave-engine --test sampler mock_fixture`.
#[test]
fn the_mock_fixture_matches_the_engine() {
    let files: Vec<serde_json::Value> = factory::files()
        .iter()
        .map(|f| serde_json::json!({ "file": f.file, "name": f.name, "group": f.group, "root": f.root, "pitched": f.pitched, "dur": f.dur, "ch": f.ch, "bytes": f.bytes.len(), "color": factory::color(f) }))
        .collect();
    let want = serde_json::json!({ "kit": factory::KIT, "files": files, "empty": Project::empty() });
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../ui/src/api/factory.json");
    if std::env::var_os("WAVE_UPDATE_FIXTURES").is_some() {
        std::fs::write(path, serde_json::to_string(&want).unwrap() + "\n").unwrap();
    }
    let have: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).expect("ui/src/api/factory.json")).unwrap();
    // compare through JSON text: floats written and read back are not bit-identical to f32
    let norm = |v: &serde_json::Value| serde_json::from_str::<serde_json::Value>(&serde_json::to_string(v).unwrap()).unwrap();
    assert!(norm(&have) == norm(&want), "ui/src/api/factory.json is out of date: see this test's comment");
}
