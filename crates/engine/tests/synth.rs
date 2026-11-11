//! The built-in synth: channel model and ops, playback from patterns and the MIDI keyboard,
//! catalog, and the DSP acceptance measurements (aliasing, clicks, loudness, determinism).

use std::sync::Arc;

use wave_engine::midi::MidiMsg;
use wave_engine::poly::params::{self, ix};
use wave_engine::poly::{lab, presets, tables, voice, SynthInst};
use wave_engine::synth::Dest;
use wave_engine::*;

const SR: u32 = 48000;
const BLOCK: usize = 256;

fn synth_channel(id: Id, preset: &str) -> Channel {
    Channel {
        id,
        name: preset.into(),
        color: "#9b7cf8".into(),
        src: ChannelSrc::Synth { synth: poly::preset_state(preset).unwrap() },
        vol: VOL_UNITY,
        pan: 0.0,
        mute: false,
        solo: false,
        insert: None,
        root: 60,
    }
}

/// An empty project (no master limiter) with one synth channel and one pattern clip.
fn song(preset: &str, notes: &[(f32, f32, u8)], beats: f64) -> (Project, Id) {
    let mut p = Project::empty();
    p.inserts[0].fx.clear();
    let id = p.alloc_id();
    p.channels.push(synth_channel(id, preset));
    let pid = p.alloc_id();
    p.patterns = vec![Pattern { id: pid, name: "P".into(), color: "#4f8ef7".into(), len: beats, min_len: beats, notes: notes.iter().map(|&(s, l, k)| Note::new(id, s, l, Some(k))).collect(), swing: 0.0 }];
    let cid = p.alloc_id();
    p.clips = vec![Clip { id: cid, track: 0, start: 0.0, len: beats, offset: 0.0, kind: ClipKind::Pat, source: pid, muted: false, gain: 0.0, fade_in: 0.0, fade_out: 0.0, pan: 0.0, norm: false, rev: false, tune: None }];
    (p, id)
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0f32, |m, v| m.max(v.abs()))
}

#[test]
fn synth_channels_round_trip_and_old_projects_still_load() {
    let (p, id) = song("Pad Ciepły", &[], 4.0);
    let json = serde_json::to_value(&p).unwrap();
    let ch = json["channels"].as_array().unwrap().iter().find(|c| c["id"] == id).unwrap();
    assert_eq!(ch["kind"], "synth");
    assert_eq!(ch["synth"]["preset"], "Pad Ciepły");
    assert_eq!(ch["synth"]["params"].as_object().unwrap().len(), params::COUNT);
    let back: Project = serde_json::from_value(json).unwrap();
    assert_eq!(back, p);
    // a synth channel written without its `synth` object plays the default patch
    let c: Channel = serde_json::from_str(r##"{"id":7,"name":"S","color":"#fff","kind":"synth"}"##).unwrap();
    assert_eq!(c.src, ChannelSrc::Synth { synth: SynthState::default() });
    assert!(c.pitched());
}

#[test]
fn put_channel_validates_synth_params_and_takes_the_fast_path() {
    let (mut p, id) = song("Pad Ciepły", &[], 4.0);
    let mut c = p.channel(id).unwrap().clone();
    if let ChannelSrc::Synth { synth } = &mut c.src {
        synth.params.insert("flt_cutoff".into(), 900.0);
        synth.preset = None;
    }
    let d = ops::apply(&mut p, &Op::PutChannel { channel: c.clone(), index: None }).unwrap();
    assert!(d.params && !d.timeline, "a knob on a synth must not rebuild the timeline: {d:?}");
    let mut bad = c.clone();
    if let ChannelSrc::Synth { synth } = &mut bad.src {
        synth.params.insert("nope".into(), 1.0);
    }
    assert!(ops::apply(&mut p, &Op::PutChannel { channel: bad, index: None }).is_err());
    let mut nan = c;
    if let ChannelSrc::Synth { synth } = &mut nan.src {
        synth.params.insert("flt_res".into(), f64::NAN);
    }
    assert!(ops::apply(&mut p, &Op::PutChannel { channel: nan, index: None }).is_err());
}

#[test]
fn patterns_play_synth_channels_deterministically() {
    let (p, _) = song("Supersaw Lead", &[(0.0, 4.0, 60), (4.0, 4.0, 64), (8.0, 8.0, 67)], 4.0);
    let e = Engine::new(p, Arc::new(NoPlugins));
    let a = e.render(0.0, 4.0, 0.5, SR).unwrap();
    let b = e.render(0.0, 4.0, 0.5, SR).unwrap();
    assert!(peak(&a) > 0.05, "synth pattern is silent");
    assert_eq!(a, b, "offline renders must be bit-identical");
    // the first note starts on the downbeat (within the drive's oversampler latency + attack)
    let first = a.iter().step_by(2).position(|v| v.abs() > 1e-4).unwrap();
    assert!(first < 200, "onset at {first}");
}

#[test]
fn muting_a_synth_channel_silences_it() {
    let (mut p, id) = song("Supersaw Lead", &[(0.0, 4.0, 60)], 2.0);
    p.channels.iter_mut().find(|c| c.id == id).unwrap().mute = true;
    let x = Engine::new(p, Arc::new(NoPlugins)).render(0.0, 2.0, 0.2, SR).unwrap();
    assert_eq!(peak(&x), 0.0);
}

/// Renders `frames` of output and returns the peak of the master bus.
fn run(rt: &mut Rt, frames: usize) -> f32 {
    let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let mut pk = 0f32;
    let mut done = 0;
    while done < frames {
        let n = BLOCK.min(frames - done);
        rt.process(&mut l[..n], &mut r[..n], None);
        pk = l[..n].iter().chain(r[..n].iter()).fold(pk, |a, x| a.max(x.abs()));
        done += n;
    }
    pk
}

#[test]
fn the_midi_keyboard_plays_a_synth_channel_and_parameter_edits_reach_it() {
    let (p, id) = song("Pad Ciepły", &[], 4.0);
    let e = Engine::new(p, Arc::new(NoPlugins));
    let mut rt = e.create_rt(SR as f32, BLOCK);
    e.midi_configure(Some(id), true);
    assert_eq!(run(&mut rt, SR as usize / 10), 0.0);
    e.midi_event(MidiMsg::NoteOn { ch: 0, key: 60, vel: 100 });
    e.midi_event(MidiMsg::ModWheel { ch: 0, value: 127 });
    let held = run(&mut rt, SR as usize);
    assert!(held > 0.01, "held key is silent ({held})");
    // a knob: the output trim down 30 dB, through PutChannel on the running instance
    let mut c = e.project().channel(id).unwrap().clone();
    if let ChannelSrc::Synth { synth } = &mut c.src {
        synth.params.insert("fx_out".into(), -30.0);
    }
    e.apply(&[Op::PutChannel { channel: c, index: None }], false, None).unwrap();
    run(&mut rt, SR as usize / 10);
    let quiet = run(&mut rt, SR as usize / 4);
    assert!(quiet < held * 0.1, "trim did not reach the instance: {quiet} vs {held}");
    e.midi_event(MidiMsg::NoteOff { ch: 0, key: 60 });
    run(&mut rt, SR as usize * 9); // 2.4 s release + 3.5 s reverb tail
    assert!(run(&mut rt, SR as usize / 10) < 1e-6);
    // removing the channel drops its instance
    e.apply(&[Op::RemoveChannel { id }], false, None).unwrap();
    e.midi_event(MidiMsg::NoteOn { ch: 0, key: 60, vel: 100 });
    run(&mut rt, SR as usize / 10);
}

#[test]
fn the_catalog_lists_sections_and_complete_presets() {
    let cat = catalog::catalog();
    let ids: Vec<&str> = cat.synth.sections.iter().map(|s| s.id).collect();
    assert_eq!(ids, ["osc1", "osc2", "subnoise", "unison", "filter", "ampenv", "modenv", "lfo1", "lfo2", "matrix", "voice", "fx"]);
    assert!(cat.synth.presets.len() >= 30);
    let mut names: Vec<&str> = cat.synth.presets.iter().map(|p| p.name).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), cat.synth.presets.len(), "preset names must be unique");
    for p in &cat.synth.presets {
        assert_eq!(p.params.len(), params::COUNT, "{}", p.name);
        assert!(cat.synth.categories.contains(&p.category), "{}", p.name);
        for (k, v) in &p.params {
            let d = &params::PARAMS[params::index_of(k).unwrap()];
            assert!(*v >= d.min && *v <= d.max, "{}: {k} = {v} out of range", p.name);
        }
    }
    for c in ["808 Czysty", "808 Brudny"] {
        assert!(names.contains(&c));
    }
    // the JSON the UI reads
    let j = serde_json::to_value(&cat).unwrap();
    assert_eq!(j["synth"]["sections"][0]["params"][0]["id"], "o1_wave");
    assert!(j["synth"]["sections"][0]["params"][0]["options"].is_array());
}

#[test]
fn saw_and_square_stay_alias_free_up_to_c7() {
    for k in [24, 48, 72, 96, 108] {
        let f = 440.0 * 2f64.powf((k as f64 - 69.0) / 12.0);
        for (w, sh) in [(tables::SAW, 0.0), (tables::SQUARE, 0.0), (tables::SQUARE, 0.6)] {
            let x = voice::render_osc(w, sh, f as f32, SR as f32, SR as usize);
            let snr = lab::alias_snr(&x, f, SR as f64, SR as f64 / 2.0);
            assert!(snr > 60.0, "wave {w} shape {sh} key {k}: alias SNR {snr:.1} dB");
        }
    }
}

#[test]
fn the_click_detector_catches_cuts_and_steps() {
    let sine: Vec<f32> = (0..48000).map(|i| (i as f32 * 0.0137).sin() * 0.5).collect();
    assert!(lab::click_flags(&sine, SR as f32, 3.0).is_empty());
    let mut cut = sine.clone();
    cut[24000..].fill(0.0);
    assert_eq!(lab::click_flags(&cut, SR as f32, 3.0).len(), 1);
    let mut step = sine;
    step[30000..].iter_mut().for_each(|v| *v += 0.05);
    assert_eq!(lab::click_flags(&step, SR as f32, 3.0).len(), 1);
}

fn clicks(l: &[f32], r: &[f32]) -> usize {
    lab::click_flags(l, SR as f32, 3.0).len() + lab::click_flags(r, SR as f32, 3.0).len()
}

#[test]
fn stealing_retriggering_legato_and_preset_changes_do_not_click() {
    let sr = SR as f32;
    let ms = |x: f32| (x * sr / 1000.0) as u32;
    let all = presets::all();
    for name in ["808 Czysty", "808 Brudny", "Supersaw Lead", "Bas Pluck", "Pluck Trap", "EP Klasyczny"] {
        let i = all.iter().position(|p| p.name == name).unwrap();
        let (p, next) = (&all[i], &all[(i + 1) % all.len()]);
        let key = p.probe_key();
        // 20 stacked notes: 4 voices are stolen
        let mut s = SynthInst::new(1, p.patch, sr);
        for n in 0..20u32 {
            s.note(ms(30.0 * n as f32), ms(1000.0), (key as u32 - 12 + 2 * n) as u8, 0.8, Dest::Click);
        }
        let (l, r) = lab::run(&mut s, 1.5, sr);
        assert!(s.active_voices() <= 20);
        assert_eq!(clicks(&l, &r), 0, "{name}: steal");
        // retrigger with gaps and overlaps, mono and legato with glide, and the preset's own
        // mode (poly overlaps of distorted 808s are left out: two square-ish waves whose edges
        // happen to coincide read as a spike to the detector, see docs/synth.md)
        let own = p.patch[ix::V_MODE];
        for mode in [own, 1.0, 2.0] {
            let mut q = p.patch;
            q[ix::V_MODE] = mode;
            q[ix::V_GLIDE] = 80.0;
            let mut s = SynthInst::new(1, q, sr);
            for n in 0..6 {
                s.note(ms(150.0 * n as f32), ms(if n % 2 == 0 { 120.0 } else { 220.0 }), key + [0, 7, 3, 12, 5, 0][n], 0.8, Dest::Click);
            }
            let (l, r) = lab::run(&mut s, 1.6, sr);
            assert_eq!(clicks(&l, &r), 0, "{name}: retrigger in mode {mode}");
        }
        // preset change on held notes
        let mut s = SynthInst::new(1, p.patch, sr);
        s.note(0, ms(1200.0), key, 0.8, Dest::Click);
        s.note(ms(100.0), ms(1100.0), key + 7, 0.8, Dest::Click);
        let (mut l, mut r) = lab::run(&mut s, 0.6, sr);
        s.set_patch(&next.patch);
        let (l2, r2) = lab::run(&mut s, 1.2, sr);
        l.extend(l2);
        r.extend(r2);
        assert_eq!(clicks(&l, &r), 0, "{name} → {}: preset change", next.name);
    }
}

#[test]
fn presets_are_loudness_matched() {
    let sr = SR as f32;
    for p in presets::all() {
        let (l, r) = lab::probe(&p, sr);
        let (lufs, tp) = lab::loudness(&l, &r, sr);
        let target = if matches!(p.category, "808" | "Bas" | "Pad") { -19.0 } else { -18.0 };
        assert!(tp <= -2.9, "{}: true peak {tp:.1} dBTP", p.name);
        // short, peaky sounds hit the true-peak ceiling before the loudness target
        let peak_limited = tp > -3.2;
        assert!((lufs - target).abs() <= 1.5 || (peak_limited && lufs < target), "{}: {lufs:.1} LUFS (target {target}), {tp:.1} dBTP", p.name);
    }
}

#[test]
fn a_patch_change_on_an_idle_instance_applies_at_once() {
    let sr = SR as f32;
    let all = presets::all();
    let mut s = SynthInst::new(1, all[0].patch, sr);
    s.set_patch(&all[1].patch);
    s.note(0, 4800, 48, 0.8, Dest::Click);
    let (a, _) = lab::run(&mut s, 0.3, sr);
    let mut t = SynthInst::new(1, all[1].patch, sr);
    t.note(0, 4800, 48, 0.8, Dest::Click);
    let (b, _) = lab::run(&mut t, 0.3, sr);
    assert_eq!(a, b);
}

/// The in-browser mock backend serves the same synth catalog as the engine: a generated copy in
/// ui/src/api/synthCatalog.ts. Regenerate with `WAVE_UPDATE_UI=1 cargo test -p wave-engine --test synth`.
#[test]
fn the_ui_mock_catalog_is_current() {
    let json = serde_json::to_string_pretty(&poly::catalog()).unwrap();
    let want = format!("// Generated from crates/engine/src/poly by `WAVE_UPDATE_UI=1 cargo test -p wave-engine --test synth`. Do not edit.\nimport type {{ SynthCatalog }} from './types';\n\nexport const SYNTH_CATALOG: SynthCatalog = {json};\n");
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/src/api/synthCatalog.ts");
    if std::env::var("WAVE_UPDATE_UI").is_ok() {
        std::fs::write(&path, &want).unwrap();
    }
    let have = std::fs::read_to_string(&path).unwrap_or_default().replace("\r\n", "\n");
    assert!(have == want, "ui/src/api/synthCatalog.ts is stale: run WAVE_UPDATE_UI=1 cargo test -p wave-engine --test synth");
}

#[test]
fn chords_stay_below_full_scale_without_clicks() {
    // presets are levelled for one note; an 8-note chord of a keys patch used to reach +9 dBFS
    let sr = SR as f32;
    for p in presets::all() {
        let mut s = SynthInst::new(1, p.patch, sr);
        let k0 = p.probe_key();
        for (i, d) in [0u8, 4, 7, 11, 14, 16, 19, 24].into_iter().enumerate() {
            s.note((i as f32 * 0.003 * sr) as u32, (1.0 * sr) as u32, k0 + d, 1.0, Dest::Click);
        }
        let (l, r) = lab::run(&mut s, 1.6, sr);
        let pk = peak(&l).max(peak(&r));
        assert!(pk <= 0.9, "{}: chord peak {:.1} dBFS", p.name, 20.0 * pk.log10());
        assert_eq!(clicks(&l, &r), 0, "{}: chord", p.name);
    }
}
