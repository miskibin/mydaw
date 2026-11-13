//! Note transforms: the deterministic muscle behind the piano roll's Tools strip and
//! `wave notes`. Every verb is a pure function over a pattern's notes — same inputs and
//! seed, byte-identical notes — so a run is reproducible and the tests can assert exact
//! results. [`SplitMix64`] is the whole randomness story: no deps, ten lines, stable.

use crate::model::Note;
use crate::pitch;

/// One deterministic RNG for all of this: `splitmix64`, the smallest PRNG whose output
/// passes a casual eye test for music purposes and is trivially reproducible anywhere
/// (the TypeScript mock mirrors it bit for bit).
pub struct SplitMix64(pub u64);

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_add(0x9E37_79B9_7F4A_7C15))
    }

    /// Next raw 64 bits; never all-zero. (Not `Iterator::next` — hence the name.)
    pub fn raw(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform −1.0..1.0.
    pub fn pm1(&mut self) -> f32 {
        (self.raw() >> 11) as f32 / ((1u64 << 53) as f32) * 2.0 - 1.0
    }

    /// Uniform 0..1.
    pub fn unit(&mut self) -> f32 {
        (self.raw() >> 11) as f32 / ((1u64 << 53) as f32)
    }

    /// Uniform integer `0..n` (n > 0).
    fn below(&mut self, n: usize) -> usize {
        (self.raw() % n as u64) as usize
    }
}

/// A generated/filled drum voice: channel `ch`, `k` onsets over `n` slots, rotated.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DrumVoice {
    pub ch: Id,
    pub k: u32,
    pub n: u32,
    #[serde(default)]
    pub rotate: i32,
    /// Velocity of ordinary onsets; every fourth is `accent`ed to 1.0.
    #[serde(default = "drum_vel")]
    pub vel: f32,
}

fn drum_vel() -> f32 {
    0.7
}

use crate::model::Id;

// ---------------------------------------------------------------------------
// verbs (the wire shape of `transformNotes`)
// ---------------------------------------------------------------------------

/// The note transformation a `transformNotes` op applies. Params clamp into range;
/// every verb is deterministic in `seed` and the pattern's notes.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum TransformVerb {
    /// Random timing and velocity jitter.
    Humanize {
        #[serde(default = "half")]
        timing: f32,
        #[serde(default = "half")]
        velocity: f32,
    },
    /// Snap note starts to `grid` steps (1 = 16th, 2 = 8th, 4 = quarter) by `amount`
    /// 0..1; `scaleAware` also snaps pitches into the project key/scale.
    Quantize {
        #[serde(default = "one_f")]
        grid: f32,
        #[serde(default = "one_f")]
        amount: f32,
        #[serde(default)]
        scale_aware: bool,
    },
    /// Replace a channel's notes with `k` onsets over `n` slots, rotated.
    EuclidFill {
        ch: Id,
        k: u32,
        n: u32,
        #[serde(default)]
        rotate: i32,
        #[serde(default = "drum_vel")]
        vel: f32,
    },
    /// Spread same-start notes by pitch (chords become strums).
    Strum {
        #[serde(default = "eighth")]
        width: f32,
        #[serde(default)]
        ramp: f32,
    },
    /// Generate one Euclidean drum voice per entry across `bars` bars.
    EuclidDrums {
        voices: Vec<DrumVoice>,
        #[serde(default = "two_bars")]
        bars: u32,
    },
    /// Generate a scale-constrained melody on one channel.
    Melody {
        ch: Id,
        #[serde(default = "two_bars")]
        bars: u32,
        #[serde(default = "half")]
        density: f32,
        #[serde(default = "one_u")]
        repeat: u32,
    },
}

fn half() -> f32 {
    0.5
}
fn one_f() -> f32 {
    1.0
}
fn one_u() -> u32 {
    1
}
fn eighth() -> f32 {
    0.5
}
fn two_bars() -> u32 {
    2
}

// ---------------------------------------------------------------------------
// transformations
// ---------------------------------------------------------------------------

/// Random timing and velocity jitter. `timing` 0..1 scales a maximum jitter of a third
/// of a 16th (notes stay recognisable); `velocity` 0..1 scales a ±50 % multiplicative
/// nudge. Notes never start before the pattern.
pub fn humanize(notes: &mut [Note], timing: f32, velocity: f32, rng: &mut SplitMix64) {
    let max_jitter = 0.34 * timing.clamp(0.0, 1.0);
    for n in notes {
        n.step = (n.step + rng.pm1() * max_jitter).max(0.0);
        let nudge = 1.0 + rng.pm1() * 0.5 * velocity.clamp(0.0, 1.0);
        // 0..=1 is the whole valid velocity range: amount 0 must leave every note as it was
        n.v = (n.v * nudge).clamp(0.0, 1.0);
    }
}

/// Snap note starts to `grid` steps (1 = 16th, 2 = 8th, 4 = quarter) by `amount` 0..1.
/// With a key set (`Some((key, scale_id))`), pitches are snapped into the scale —
/// `Note::p` becomes explicit so the result no longer depends on the channel's root.
pub fn quantize(notes: &mut [Note], grid: f32, amount: f32, key_scale: Option<(u8, u32)>, roots: &[(Id, u8)]) {
    let grid = if grid < 0.25 { 0.25 } else { grid };
    let amount = amount.clamp(0.0, 1.0);
    let ks = key_scale.map(|(k, s)| (k as u32, pitch::scale_mask(s)));
    for n in notes {
        let target = (n.step / grid).round() * grid;
        n.step += (target - n.step) * amount;
        if let Some((key, mask)) = ks {
            let root = roots.iter().find(|(id, _)| *id == n.ch).map(|(_, r)| *r).unwrap_or(60);
            let midi = n.p.unwrap_or(root) as f32;
            n.p = Some(snap_midi(midi, key, mask));
        }
    }
}

/// [`pitch::snap`] limited to MIDI keys: the nearest in-scale key within 0..=127 (ties
/// keep the lower). Unbounded snapping can leave the range at the edges — 127 in C#
/// major pentatonic is nearest 128 — which `check_notes` would reject for the whole op.
fn snap_midi(m: f32, key: u32, mask: u16) -> u8 {
    let base = m.round() as i32;
    let mut best: Option<i32> = None;
    let mut bd = f32::MAX;
    for d in -11..=11 {
        let n = base + d;
        if !(0..=127).contains(&n) {
            continue;
        }
        let pc = (n - key as i32).rem_euclid(12) as u16;
        if mask & (1 << pc) != 0 {
            let dist = (n as f32 - m).abs();
            if dist < bd {
                bd = dist;
                best = Some(n);
            }
        }
    }
    // an empty mask has no in-scale key: leave the note where it is
    best.unwrap_or(base).clamp(0, 127) as u8
}

/// Bjorklund's Euclidean onsets: `k` hits spread as evenly as `n` slots allows, rotated
/// by `rotate` slots. Slot `i` is an onset when `(i × k) mod n < k`.
fn euclidean_onsets(k: u32, n: u32, rotate: i32) -> Vec<bool> {
    let (k, n) = (k.max(1), n.max(1));
    (0..n)
        .map(|i| {
            let slot = (i as i32 + rotate).rem_euclid(n as i32) as u32;
            slot.wrapping_mul(k) % n < k
        })
        .collect()
}

/// Replace channel `ch`'s notes with `k` onsets over `n` slots across `len_steps`
/// (the pattern length in 16ths), rotated. Notes are 1 step long, the channel's root,
/// velocity `vel` — every fourth *emitted* onset accents to 1.0, whatever rotation does
/// to the slot positions.
pub fn euclidean_fill(notes: &mut Vec<Note>, ch: Id, k: u32, n: u32, rotate: i32, vel: f32, len_steps: u32) {
    notes.retain(|n| n.ch != ch);
    let onsets = euclidean_onsets(k, n, rotate);
    let slot = len_steps as f32 / n.max(1) as f32;
    let mut hits = 0usize;
    for (i, on) in onsets.iter().enumerate() {
        if *on {
            notes.push(Note {
                ch,
                step: i as f32 * slot,
                len: slot.clamp(0.25, 1.0),
                p: None,
                v: if hits.is_multiple_of(4) { 1.0 } else { vel },
            });
            hits += 1;
        }
    }
}

/// Spread simultaneous notes (same channel, same start — wherever they sit in the
/// array) by pitch, offsetting the i-th by `width` steps and nudging velocity by `ramp`
/// per note: chords become strums.
pub fn strum(notes: &mut [Note], width: f32, ramp: f32) {
    let mut groups: std::collections::HashMap<(Id, u64), Vec<usize>> = std::collections::HashMap::new();
    for (i, n) in notes.iter().enumerate() {
        groups.entry((n.ch, n.step.to_bits() as u64)).or_default().push(i);
    }
    let mut groups: Vec<Vec<usize>> = groups.into_values().filter(|g| g.len() > 1).collect();
    for g in &mut groups {
        g.sort_by_key(|&i| notes[i].p.unwrap_or(60));
        for (k, &i) in g.iter().enumerate() {
            let n = &mut notes[i];
            n.step += k as f32 * width;
            n.v = (n.v + k as f32 * ramp).clamp(0.0, 1.0);
        }
    }
}

/// Replace channel `ch`'s notes with a scale-constrained melody: `bars` bars of 16th
/// slots, a note on every `density` slot, the pitch a small random walk over scale
/// degrees. Bars past `repeat` replay that cycle's rhythm with the walk nudged ±1
/// degree, so the melody repeats instead of wandering. Like [`euclidean_fill`], a second
/// run (another seed) replaces the first instead of stacking on it.
pub fn melody(notes: &mut Vec<Note>, ch: Id, key_scale: (u8, u32), bars: u32, density: f32, repeat: u32, seed: u64) {
    notes.retain(|n| n.ch != ch);
    let rng = &mut SplitMix64::new(seed);
    let (key, scale_id) = key_scale;
    let mask = pitch::scale_mask(scale_id);
    let degrees: Vec<u8> = (0..12u16).filter(|pc| mask & (1 << pc) != 0).map(|pc| pc as u8).collect();
    let n = degrees.len() as i32;
    let mut deg: i32 = 0;
    let mut seq: Vec<(i32, f32)> = Vec::new(); // per-slot (degree index, len); -1 = rest
    let rep = repeat.max(1) as usize;
    for bar in 0..bars.max(1) as usize {
        // the first `repeat` bars invent the motif; every later bar replays its cycle's
        // rhythm with the walk nudged ±1 degree, so the melody repeats instead of wandering
        let build = bar < rep;
        for slot in 0..16usize {
            let (d, len) = if build {
                let len = if rng.unit() < 0.3 { 2.0 } else { 1.0 };
                if rng.unit() < density.clamp(0.05, 1.0) {
                    deg = (deg + [0, -1, 1, 2, -2][rng.below(5)]).clamp(0, n - 1);
                    seq.push((deg, len));
                    (deg, len)
                } else {
                    seq.push((-1, len));
                    (-1, len)
                }
            } else {
                let (sd, sl) = seq[(bar % rep) * 16 + slot];
                if sd < 0 {
                    (-1, sl)
                } else {
                    deg = (deg + [0, -1, 1][rng.below(3)]).clamp(0, n - 1);
                    (deg, sl)
                }
            };
            if d < 0 {
                continue;
            }
            let midi = (48 + key as i32 + degrees[d as usize] as i32) as u8;
            notes.push(Note { ch, step: (bar * 16 + slot) as f32, len, p: Some(midi), v: 0.8 });
        }
    }
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn note(ch: Id, step: f32, v: f32) -> Note {
        Note { ch, step, len: 1.0, p: Some(60), v }
    }

    #[test]
    fn same_seed_same_notes() {
        let a = &mut [note(1, 4.0, 0.8), note(1, 8.0, 0.9)];
        let b = &mut [note(1, 4.0, 0.8), note(1, 8.0, 0.9)];
        humanize(a, 0.5, 0.4, &mut SplitMix64::new(41));
        humanize(b, 0.5, 0.4, &mut SplitMix64::new(41));
        assert_eq!(a, b);
    }

    #[test]
    fn different_seed_different_notes() {
        let a = &mut [note(1, 4.0, 0.8)];
        let b = &mut [note(1, 4.0, 0.8)];
        humanize(a, 0.5, 0.4, &mut SplitMix64::new(1));
        humanize(b, 0.5, 0.4, &mut SplitMix64::new(2));
        assert_ne!(a, b);
    }

    #[test]
    fn humanize_zero_is_identity() {
        let a = &mut [note(1, 4.0, 0.8)];
        let b = [note(1, 4.0, 0.8)];
        humanize(a, 0.0, 0.0, &mut SplitMix64::new(9));
        assert_eq!(a[0].step, b[0].step);
        assert_eq!(a[0].v, b[0].v);
    }

    #[test]
    fn humanize_never_starts_before_zero() {
        let a = &mut [note(1, 0.05, 0.8)];
        humanize(a, 1.0, 0.0, &mut SplitMix64::new(3));
        assert!(a[0].step >= 0.0);
    }

    #[test]
    fn quantize_snaps_to_the_grid_by_amount() {
        let a = &mut [note(1, 4.4, 0.8)];
        quantize(a, 1.0, 1.0, None, &[]);
        assert_eq!(a[0].step, 4.0);
        let b = &mut [note(1, 4.4, 0.8)];
        quantize(b, 1.0, 0.5, None, &[]);
        assert!((b[0].step - 4.2).abs() < 1e-5);
    }

    #[test]
    fn scale_aware_quantize_snaps_pitch_into_the_scale() {
        // C major: the C# is equidistant from C and D; pitch::snap keeps the lower
        let a = &mut [Note { ch: 1, step: 4.0, len: 1.0, p: Some(61), v: 0.8 }];
        quantize(a, 1.0, 1.0, Some((0, 1)), &[(1, 60)]);
        assert_eq!(a[0].p, Some(60));
        // the D# is equidistant from D and E; pitch::snap keeps the lower
        let b = &mut [Note { ch: 1, step: 4.0, len: 1.0, p: Some(63), v: 0.8 }];
        quantize(b, 1.0, 1.0, Some((0, 1)), &[(1, 60)]);
        assert_eq!(b[0].p, Some(62));
        // every out-of-scale chromatic note ties between its in-scale neighbours;
        // pitch::snap keeps the lower — F# (66) lands on F (65)
        let c = &mut [Note { ch: 1, step: 4.0, len: 1.0, p: Some(66), v: 0.8 }];
        quantize(c, 1.0, 1.0, Some((0, 1)), &[(1, 60)]);
        assert_eq!(c[0].p, Some(65));
    }

    #[test]
    fn zero_amount_keeps_quiet_velocities() {
        // velocities below the old 0.05 floor must survive amount 0 unchanged
        let h = &mut [note(1, 4.0, 0.02)];
        humanize(h, 0.0, 0.0, &mut SplitMix64::new(9));
        assert_eq!(h[0].v, 0.02);
        let s = &mut [note(1, 4.0, 0.02), Note { ch: 1, step: 4.0, len: 1.0, p: Some(64), v: 0.02 }];
        strum(s, 0.25, 0.0);
        assert!(s.iter().all(|n| n.v == 0.02));
        // and a full nudge still stays inside 0..=1
        let h = &mut [note(1, 4.0, 0.02), note(1, 5.0, 1.0)];
        humanize(h, 0.0, 1.0, &mut SplitMix64::new(9));
        assert!(h.iter().all(|n| (0.0..=1.0).contains(&n.v)));
    }

    #[test]
    fn scale_aware_quantize_stays_in_midi_range() {
        // C# major pentatonic (key 1, scale 3): 127 would snap up to 128
        let a = &mut [Note { ch: 1, step: 0.0, len: 1.0, p: Some(127), v: 0.8 }];
        quantize(a, 1.0, 1.0, Some((1, 3)), &[]);
        assert_eq!(a[0].p, Some(125));
        // the bottom edge: 0 (C) in B major pentatonic (B C# D# F# G#) — nearest in-range is 1
        let b = &mut [Note { ch: 1, step: 0.0, len: 1.0, p: Some(0), v: 0.8 }];
        quantize(b, 1.0, 1.0, Some((11, 3)), &[]);
        assert_eq!(b[0].p, Some(1));
        // everywhere in range, the bounded snap agrees with pitch::snap
        for key in 0..12u32 {
            for scale in 0..5u32 {
                let mask = pitch::scale_mask(scale);
                for m in 0..=127u8 {
                    let s = pitch::snap(m as f32, key, mask);
                    if (0.0..=127.0).contains(&s) {
                        assert_eq!(snap_midi(m as f32, key, mask), s as u8, "key {key} scale {scale} m {m}");
                    }
                }
            }
        }
    }

    #[test]
    fn melody_replaces_its_channel() {
        let a = &mut vec![note(3, 0.5, 0.9), note(4, 2.0, 0.9)];
        melody(a, 3, (0, 1), 1, 0.5, 1, 1);
        melody(a, 3, (0, 1), 1, 0.5, 1, 2);
        let b = &mut vec![note(4, 2.0, 0.9)];
        melody(b, 3, (0, 1), 1, 0.5, 1, 2);
        assert_eq!(a, b, "a second run replaces the first; other channels stay");
    }

    #[test]
    fn euclidean_four_on_floor() {
        let onsets = euclidean_onsets(4, 16, 0);
        assert_eq!(onsets.iter().filter(|o| **o).count(), 4);
        assert!(onsets[0] && onsets[4] && onsets[8] && onsets[12]);
    }

    #[test]
    fn euclidean_fill_replaces_its_channel_and_keeps_the_others() {
        let notes = &mut vec![note(1, 0.0, 1.0), note(2, 2.0, 1.0)];
        euclidean_fill(notes, 1, 5, 16, 0, 0.7, 16);
        assert_eq!(notes.iter().filter(|n| n.ch == 1).count(), 5);
        assert!(notes.iter().any(|n| n.ch == 2 && n.step == 2.0)); // the other channel survives
    }

    #[test]
    fn strum_spreads_a_chord_by_pitch() {
        let notes = &mut [
            note(1, 4.0, 0.8),
            Note { ch: 1, step: 4.0, len: 1.0, p: Some(64), v: 0.8 },
            Note { ch: 1, step: 4.0, len: 1.0, p: Some(57), v: 0.8 },
        ];
        strum(notes, 0.25, 0.05);
        // array positions stay, offsets follow pitch rank: lowest starts first
        let by = |p: u8| notes.iter().find(|n| n.p == Some(p)).unwrap().step;
        assert_eq!(by(57), 4.0);
        assert_eq!(by(60), 4.25);
        assert_eq!(by(64), 4.5);
        let vof = |p: u8| notes.iter().find(|n| n.p == Some(p)).unwrap().v;
        assert_eq!(vof(57), 0.8);
        assert_eq!(vof(60), 0.85);
        assert_eq!(vof(64), 0.90000004);
    }

    #[test]
    fn melody_is_scale_constrained_and_deterministic() {
        let a = &mut vec![];
        melody(a, 3, (0, 1), 2, 0.5, 1, 77);
        assert!(a.iter().all(|n| n.ch == 3));
        assert!(a.iter().all(|n| n.p.is_some()));
        // every pitch in C major
        assert!(a.iter().all(|n| [0, 2, 4, 5, 7, 9, 11].contains(&(n.p.unwrap() % 12))));
        let b = &mut vec![];
        melody(b, 3, (0, 1), 2, 0.5, 1, 77);
        assert_eq!(a, b);
    }

    #[test]
    fn melody_repeat_replays_the_motif() {
        let a = &mut vec![];
        melody(a, 3, (0, 1), 2, 0.5, 2, 41);
        let cycle = a.len();
        assert!(cycle > 0);
        let b = &mut vec![];
        melody(b, 3, (0, 1), 4, 0.5, 2, 41);
        // bars 3–4 replay bars 1–2's rhythm: exactly twice the notes, first half identical
        assert_eq!(b.len(), cycle * 2);
        assert_eq!(&b[..cycle], &a[..]);
        // the replay nudges the walk, so it is not a byte-copy of the cycle
        let same = a.iter().zip(b[cycle..].iter()).filter(|(x, y)| x.p == y.p).count();
        assert!(same < cycle, "replayed bars must deviate from the motif");
    }

    #[test]
    fn strum_groups_nonadjacent_same_step_notes() {
        let notes = &mut [
            note(1, 4.0, 0.8),
            Note { ch: 1, step: 8.0, len: 1.0, p: Some(64), v: 0.8 },
            Note { ch: 1, step: 4.0, len: 1.0, p: Some(57), v: 0.8 },
        ];
        strum(notes, 0.25, 0.0);
        // the two step-4 notes form one chord (lowest first) even though they are
        // separated by the step-8 note; the lone step-8 note does not move
        assert!(notes.iter().any(|n| n.p == Some(57) && (n.step - 4.0).abs() < 1e-4));
        assert!(notes.iter().any(|n| n.p == Some(60) && (n.step - 4.25).abs() < 1e-4));
        assert!(notes.iter().any(|n| n.p == Some(64) && (n.step - 8.0).abs() < 1e-4));
    }

    /// Golden values for one fixed seed per verb; the TS mirror pins the same numbers
    /// (`ui/src/lib/transforms.test.ts`), so engine and browser mock agree bit for bit.
    /// Refresh by running with `--nocapture` and copying the H/Q/S/E/M lines.
    #[test]
    fn golden_fixed_seed_values() {
        let h = &mut [note(1, 4.0, 0.8)];
        humanize(h, 0.5, 0.4, &mut SplitMix64::new(41));
        assert_eq!(h[0].step, 4.033452);
        assert_eq!(h[0].v, 0.67452526);
        let q = &mut [note(1, 4.3, 0.8)];
        quantize(q, 1.0, 1.0, None, &[]);
        assert_eq!(q[0].step, 4.0);
        let s = &mut [
            note(1, 4.0, 0.8),
            Note { ch: 1, step: 4.0, len: 1.0, p: Some(64), v: 0.8 },
            Note { ch: 1, step: 4.0, len: 1.0, p: Some(57), v: 0.8 },
        ];
        strum(s, 0.25, 0.05);
        // array positions stay; the lowest pitch starts first, each next +width
        let by = |p: u8| s.iter().find(|n| n.p == Some(p)).unwrap();
        assert_eq!(by(57).step, 4.0);
        assert_eq!(by(60).step, 4.25);
        assert_eq!(by(64).step, 4.5);
        assert_eq!(by(64).v, 0.90000004);
        let e = &mut vec![note(1, 2.0, 0.9)];
        euclidean_fill(e, 1, 5, 16, 0, 0.7, 16);
        let steps: Vec<f32> = e.iter().map(|n| n.step).collect();
        assert_eq!(steps, [0.0, 4.0, 7.0, 10.0, 13.0]);
        let vel: Vec<f32> = e.iter().map(|n| n.v).collect();
        assert_eq!(vel, [1.0, 0.7, 0.7, 0.7, 1.0]);
        let m = &mut vec![];
        melody(m, 3, (9, 2), 2, 0.55, 1, 7);
        let mp: Vec<u8> = m.iter().map(|n| n.p.unwrap()).collect();
        assert_eq!(mp, [57, 57, 57, 59, 59, 59, 62, 65, 64, 62, 64, 64, 62, 62, 62, 62]);
        let ms: Vec<f32> = m.iter().map(|n| n.step).collect();
        assert_eq!(ms, [1.0, 2.0, 3.0, 5.0, 8.0, 10.0, 12.0, 15.0, 17.0, 18.0, 19.0, 21.0, 24.0, 26.0, 28.0, 31.0]);
    }
}
