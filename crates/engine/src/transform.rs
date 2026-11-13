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
    EuclidFill { ch: Id, k: u32, n: u32, #[serde(default)] rotate: i32, #[serde(default = "drum_vel")] vel: f32 },
    /// Spread same-start notes by pitch (chords become strums).
    Strum {
        #[serde(default = "eighth")]
        width: f32,
        #[serde(default)]
        ramp: f32,
    },
    /// Generate one Euclidean drum voice per entry across `bars` bars.
    EuclidDrums { voices: Vec<DrumVoice>, #[serde(default = "two_bars")] bars: u32 },
    /// Generate a scale-constrained melody on one channel.
    Melody { ch: Id, #[serde(default = "two_bars")] bars: u32, #[serde(default = "half")] density: f32, #[serde(default = "one_u")] repeat: u32 },
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
        n.v = (n.v * nudge).clamp(0.05, 1.0);
    }
}

/// Snap note starts to `grid` steps (1 = 16th, 2 = 8th, 4 = quarter) by `amount` 0..1.
/// With a key set (`Some((key, scale_id))`), pitches are snapped into the scale —
/// `Note::p` becomes explicit so the result no longer depends on the channel's root.
pub fn quantize(
    notes: &mut [Note],
    grid: f32,
    amount: f32,
    key_scale: Option<(u8, u32)>,
    roots: &[(Id, u8)],
) {
    let grid = if grid < 0.25 { 0.25 } else { grid };
    let amount = amount.clamp(0.0, 1.0);
    let ks = key_scale.map(|(k, s)| (k as u32, pitch::scale_mask(s)));
    for n in notes {
        let target = (n.step / grid).round() * grid;
        n.step += (target - n.step) * amount;
        if let Some((key, mask)) = ks {
            let root = roots.iter().find(|(id, _)| *id == n.ch).map(|(_, r)| *r).unwrap_or(60);
            let midi = n.p.unwrap_or(root) as f32;
            let snapped = pitch::snap(midi, key, mask);
            n.p = Some(snapped.round() as u8);
        }
    }
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
/// velocity `vel` (every fourth onset accents to 1.0).
pub fn euclidean_fill(notes: &mut Vec<Note>, ch: Id, k: u32, n: u32, rotate: i32, vel: f32, len_steps: u32) {
    notes.retain(|n| n.ch != ch);
    let onsets = euclidean_onsets(k, n, rotate);
    let slot = len_steps as f32 / n.max(1) as f32;
    for (i, on) in onsets.iter().enumerate() {
        if *on {
            notes.push(Note {
                ch,
                step: (i as f32 * slot).round() * 0.001 + i as f32 * slot,
                len: slot.clamp(0.25, 1.0),
                p: None,
                v: if i % 4 == 0 { 1.0 } else { vel },
            });
        }
    }
}

/// Spread simultaneous notes (same channel, same start) by pitch, offsetting the i-th
/// by `width` steps and nudging velocity by `ramp` per note: chords become strums.
pub fn strum(notes: &mut [Note], width: f32, ramp: f32) {
    let mut i = 0;
    while i < notes.len() {
        let mut j = i;
        while j < notes.len() && notes[j].step == notes[i].step {
            j += 1;
        }
        let group = &mut notes[i..j];
        if group.len() > 1 {
            group.sort_by_key(|n| n.p.unwrap_or(60));
            for (k, n) in group.iter_mut().enumerate() {
                n.step += k as f32 * width;
                n.v = (n.v + k as f32 * ramp).clamp(0.05, 1.0);
            }
        }
        i = j;
    }
}

/// Generate a scale-constrained melody for `ch`: `bars` bars of 16th slots, a note on
/// every `density` slot, the pitch a small random walk over scale degrees. Bars past
/// `repeat` replay that cycle's rhythm with the walk nudged ±1 degree, so the melody
/// repeats instead of wandering.
pub fn melody(notes: &mut Vec<Note>, ch: Id, key_scale: (u8, u32), bars: u32, density: f32, repeat: u32, seed: u64) {
    let rng = &mut SplitMix64::new(seed);
    let (key, scale_id) = key_scale;
    let mask = pitch::scale_mask(scale_id);
    let degrees: Vec<u8> = (0..12u16).filter(|pc| mask & (1 << pc) != 0).map(|pc| pc as u8).collect();
    let n = degrees.len() as i32;
    let mut deg: i32 = 0;
    let mut seq: Vec<(i32, f32)> = Vec::new(); // per-slot (degree index, len); -1 = rest
    let rep = repeat.max(1) as usize;
    for bar in 0..bars.max(1) as usize {
        for slot in 0..16usize {
            let idx = bar * 16 + slot;
            let (d, len) = if bar % rep == 0 || seq.len() <= idx {
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
                // repeat cycle: replay this slot's rhythm, nudge the degree
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
        let notes = &mut vec![note(1, 4.0, 0.8), Note { ch: 1, step: 4.0, len: 1.0, p: Some(64), v: 0.8 }, Note { ch: 1, step: 4.0, len: 1.0, p: Some(57), v: 0.8 }];
        strum(notes, 0.25, 0.05);
        assert!(notes[0].step < notes[1].step && notes[1].step < notes[2].step); // lowest first
        assert!((notes[2].step - notes[1].step - 0.25).abs() < 1e-5);
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
}
