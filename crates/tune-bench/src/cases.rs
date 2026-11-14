//! Controlled cases built from real recordings for the agent suites: a real take with known
//! per-note errors injected by the editor's own PSOLA (so the truth is known), labelled note by
//! note as in tune, slip or intended difference, against a real reference.

use wave_engine::pitch::F0Estimator;
use wave_engine::sources::Audio;

use crate::agent::{Case, Label, Note};
use crate::vocalset::{detuned, offsets};

pub struct Owned {
    pub name: String,
    pub group: String,
    pub take: Audio,
    pub reference: Option<Audio>,
    pub truth: Vec<f32>,
    pub edge: Vec<bool>,
    pub sung: Vec<f32>,
    pub notes: Vec<Note>,
    pub expect_related: Option<bool>,
    pub expect_interval: Option<i32>,
    pub pairs: Vec<((f64, f64), (f64, f64))>,
    pub truth_kind: &'static str,
}

impl Owned {
    pub fn case(&self) -> Case<'_> {
        Case {
            name: self.name.clone(),
            group: self.group.clone(),
            take: &self.take,
            reference: self.reference.as_ref(),
            truth: self.truth.clone(),
            edge: self.edge.clone(),
            sung: self.sung.clone(),
            notes: self.notes.clone(),
            expect_related: self.expect_related,
            expect_interval: self.expect_interval,
            pairs: self.pairs.clone(),
            truth_kind: self.truth_kind,
        }
    }
}

/// A per-note plan: (cents put onto the note, its label).
pub type Plan = Vec<(f32, Label)>;

/// The plans of the controlled groups, for note `k` of case `i` (the sign alternates).
pub fn plan(group: &str, notes: usize, i: usize) -> Plan {
    let sign = |k: usize| if (k + i).is_multiple_of(2) { 1.0 } else { -1.0 };
    (0..notes)
        .map(|k| match group {
            "slips ±25/±50" => match k % 3 {
                0 => (0.0, Label::InTune),
                1 => (25.0 * sign(k), Label::Slip),
                _ => (50.0 * sign(k), Label::Slip),
            },
            "slips ±35 + drift" => (35.0 * sign(k), Label::Slip),
            "slips ±40 + intended ±200" => match k % 4 {
                0 => (200.0 * sign(k / 4), Label::Intended),
                2 => (40.0 * sign(k), Label::Slip),
                _ => (0.0, Label::InTune),
            },
            "large ±100" => {
                if k % 2 == 0 {
                    (100.0 * sign(k), Label::Slip)
                } else {
                    (0.0, Label::InTune)
                }
            }
            "large ±200" => {
                if k % 2 == 0 {
                    (200.0 * sign(k), Label::Slip)
                } else {
                    (0.0, Label::InTune)
                }
            }
            // cross-singer and transposed references: a slip on every third note
            _ => {
                if k % 3 == 1 {
                    (40.0 * sign(k), Label::Slip)
                } else {
                    (0.0, Label::InTune)
                }
            }
        })
        .collect()
}

pub const SELF_GROUPS: [&str; 5] = ["slips ±25/±50", "slips ±35 + drift", "slips ±40 + intended ±200", "large ±100", "large ±200"];

/// Applies a plan to a real take with known note spans and per-frame truth (MIDI at 200 frames/s):
/// the detuned take, what it sings, and the labelled notes (`note_truth` = each note's true pitch).
pub fn inject(
    audio: &Audio,
    spans: &[(f64, f64)],
    note_truth: &[Option<f32>],
    truth: &[f32],
    plan: &Plan,
    drift: f32,
    est: Option<&dyn F0Estimator>,
) -> (Audio, Vec<f32>, Vec<Note>) {
    let frames = truth.len();
    // a note the original itself sings more than 20 ct off equal temperament has no known
    // intention: it gets no injected error and is reported as natural, never scored as right or wrong
    let on_grid = |k: usize| note_truth.get(k).copied().flatten().is_some_and(|m| (m - m.round()).abs() <= 0.2);
    let plan: Plan = plan
        .iter()
        .enumerate()
        .map(|(k, &(d, l))| match l {
            Label::InTune | Label::Slip if !on_grid(k) => (0.0, Label::Natural),
            _ => (d, l),
        })
        .collect();
    let detune: Vec<f32> = plan.iter().map(|p| p.0).collect();
    let off = offsets(frames, spans, &detune, drift);
    let take = detuned(audio, &off, est);
    let sung: Vec<f32> = truth.iter().zip(&off).map(|(t, o)| t + o / 100.0).collect();
    let notes = spans
        .iter()
        .zip(&plan)
        .zip(note_truth)
        .filter_map(|((&(a, b), &(d, label)), &nt)| {
            let want = nt?;
            Some(Note { start: a, end: b, label, want: if label == Label::Intended { want + d / 100.0 } else { want } })
        })
        .collect();
    (take, sung, notes)
}

/// The recording moved by `semitones` everywhere (another key, the same voice and timing).
pub fn transposed(audio: &Audio, semitones: f32, est: Option<&dyn F0Estimator>) -> Audio {
    let frames = (audio.seconds() * 200.0) as usize + 1;
    detuned(audio, &vec![semitones * 100.0; frames], est)
}

pub fn median(mut v: Vec<f32>) -> Option<f32> {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    Some(v[v.len() / 2])
}
