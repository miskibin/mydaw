//! The reverb's display (ui/src/views/fx/reverb.ts) draws the space, its early reflections and
//! the decay per band from `ui/src/api/reverbSpaces.json`, generated here from the DSP. After a
//! change to the spaces or `derive`, regenerate it:
//! `WAVE_UPDATE_FIXTURES=1 cargo test -p wave-engine --test reverb_spaces`.
//! `ui/src/lib/reverbSpace.test.ts` checks the UI reproduces the sample values below.

use wave_engine::dsp::reverb_v2::{self, idx};
use wave_engine::processor::MAX_PARAMS;

fn params(space: usize, size: f32, tone: f32, bass: f32) -> [f32; MAX_PARAMS] {
    let mut v = [0.0; MAX_PARAMS];
    v[idx::SPACE] = space as f32;
    v[idx::SIZE] = size;
    v[idx::TONE] = tone;
    v[idx::BASS] = bass;
    v
}

#[test]
fn the_display_fixture_matches_the_dsp() {
    let mut samples = vec![];
    for space in 0..4 {
        for size in [0.2f32, 0.6, 1.0, 2.0, 6.0] {
            for (tone, bass) in [(0.0f32, 1.0f32), (-100.0, 0.5), (100.0, 1.5)] {
                let v = params(space, size, tone, bass);
                let d = reverb_v2::derive(&v);
                samples.push(serde_json::json!({
                    "space": space, "size": size, "tone": tone, "bass": bass,
                    "tLow": d.t_low, "tMid": d.t_mid, "tHigh": d.t_high, "scale": d.scale,
                    "roomM": reverb_v2::equivalent_room_m(&v),
                }));
            }
        }
    }
    let want = serde_json::json!({
        "spaces": reverb_v2::space_info(),
        "scale": [reverb_v2::SCALE_LIMITS.0, reverb_v2::SCALE_LIMITS.1],
        "samples": samples,
    });
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../ui/src/api/reverbSpaces.json"
    );
    if std::env::var_os("WAVE_UPDATE_FIXTURES").is_some() {
        std::fs::write(path, serde_json::to_string(&want).unwrap() + "\n").unwrap();
    }
    let have: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("ui/src/api/reverbSpaces.json"))
            .unwrap();
    let norm = |v: &serde_json::Value| {
        serde_json::from_str::<serde_json::Value>(&serde_json::to_string(v).unwrap()).unwrap()
    };
    assert!(
        norm(&have) == norm(&want),
        "ui/src/api/reverbSpaces.json is out of date: see this test's comment"
    );
}

#[test]
fn rooms_grow_with_the_space_and_the_decay_within_the_scale_limits() {
    let m = |space, size| reverb_v2::equivalent_room_m(&params(space, size, 0.0, 1.0));
    // Ambience < Room < Hall at the same decay
    assert!(m(3, 1.0) < m(1, 1.0) && m(1, 1.0) < m(2, 1.0));
    // a longer decay is a bigger room, until the network scale hits its limit
    assert!(m(2, 1.0) < m(2, 2.0) && m(2, 2.0) < m(2, 4.0));
    assert_eq!(m(1, 6.0), m(1, 5.0), "Room is at its largest scale by 5 s");
    // plausible sizes (m) at each space's own reference decay
    for (space, t, lo, hi) in [
        (1, 0.6, 5.0, 12.0),
        (2, 2.0, 18.0, 35.0),
        (3, 0.5, 3.0, 9.0),
    ] {
        let r = m(space, t);
        assert!((lo..hi).contains(&r), "space {space} at {t} s: {r} m");
    }
}
