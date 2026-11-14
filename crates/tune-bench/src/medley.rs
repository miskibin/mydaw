//! MedleyDB (the openly licensed `MedleyDB_Sample`, CC BY-SA 4.0, Zenodo 1438309; the full
//! dataset needs an access request): a real song's lead vocal stem with a human-corrected pitch
//! annotation, and its backing vocal stems (other singers, other notes, the same words).
//! The lead is cut into phrases at its rests.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use wave_engine::pitch::{analyze_with, hz_to_midi, F0Estimator};
use wave_engine::sources::{mono_of, Audio};

use crate::agent::Label;
use crate::cases::{inject, median, plan, Owned};
use crate::vocalset::edges;

pub struct Song {
    pub name: String,
    pub lead: PathBuf,
    pub harmonies: Vec<PathBuf>,
    /// (seconds, Hz) of the lead's voiced frames.
    pub f0: Vec<(f64, f32)>,
}

/// `LizNelson_Rainfall`: lead (female singer) STEM_01 annotated, STEM_02/03 backing vocals.
pub fn index(root: &Path) -> Result<Vec<Song>, String> {
    let base = root.join("MedleyDB_sample");
    let mut out = vec![];
    // the sample's songs with an annotated lead vocal and backing vocal stems
    #[allow(clippy::single_element_loop)]
    for (name, lead, harm) in [("LizNelson_Rainfall", "STEM_01", ["STEM_02", "STEM_03"])] {
        let stems = base.join("Audio").join(name).join(format!("{name}_STEMS"));
        let ann = base.join("Annotations").join("Pitch_Annotations").join(format!("{name}_{lead}.csv"));
        let text = std::fs::read_to_string(&ann).map_err(|e| format!("{}: {e}", ann.display()))?;
        let f0: Vec<(f64, f32)> = text
            .lines()
            .filter_map(|l| {
                let mut c = l.split(',');
                Some((c.next()?.trim().parse().ok()?, c.next()?.trim().parse().ok()?))
            })
            .filter(|x: &(f64, f32)| x.1 > 0.0)
            .collect();
        out.push(Song {
            name: name.into(),
            lead: stems.join(format!("{name}_{lead}.wav")),
            harmonies: harm.iter().map(|h| stems.join(format!("{name}_{h}.wav"))).collect(),
            f0,
        });
    }
    Ok(out)
}

/// Annotated pitch at 200 frames/s over `[t0, t0 + frames/200)` (MIDI; NaN where unvoiced).
fn truth_at(f0: &[(f64, f32)], t0: f64, frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|f| {
            let t = t0 + f as f64 / 200.0;
            let i = f0.partition_point(|x| x.0 <= t);
            if i == 0 || i >= f0.len() {
                return f32::NAN;
            }
            let (a, b) = (f0[i - 1], f0[i]);
            if b.0 - a.0 > 0.012 {
                return f32::NAN;
            }
            let w = ((t - a.0) / (b.0 - a.0)) as f32;
            hz_to_midi(a.1) + (hz_to_midi(b.1) - hz_to_midi(a.1)) * w
        })
        .collect()
}

/// Phrases (seconds): voiced stretches joined across gaps under 0.6 s, 3–12 s long.
fn phrases(f0: &[(f64, f32)]) -> Vec<(f64, f64)> {
    let mut runs: Vec<(f64, f64)> = vec![];
    for &(t, _) in f0 {
        match runs.last_mut() {
            Some(r) if t - r.1 < 0.6 => r.1 = t,
            _ => runs.push((t, t)),
        }
    }
    let mut out = vec![];
    for (a, b) in runs {
        let mut s = a;
        while b - s > 12.0 {
            out.push((s, s + 10.0));
            s += 10.0;
        }
        if b - s >= 3.0 {
            out.push((s, b));
        }
    }
    out
}

fn cut(a: &Audio, from: f64, to: f64) -> Audio {
    let (i0, i1) = (((from.max(0.0)) * a.sr as f64) as usize, ((to * a.sr as f64) as usize).min(a.frames));
    Audio::from_stereo(a.content()[i0 * 2..i1.max(i0) * 2].to_vec(), a.sr, a.channels)
}

fn voiced_share(a: &Audio, est: Option<&dyn F0Estimator>) -> f32 {
    let an = analyze_with(&mono_of(a), a.sr as f32, est);
    an.curve.iter().filter(|x| x.is_finite()).count() as f32 / an.curve.len().max(1) as f32
}

pub fn cases(
    songs: &[Song],
    limit: usize,
    est: Option<&dyn F0Estimator>,
    failed: &mut Vec<(String, String)>,
    skipped: &mut Vec<String>,
) -> Vec<Owned> {
    let mut out = vec![];
    for song in songs {
        let (lead, harms) = match Audio::load(&song.lead) {
            Ok(l) => (l, song.harmonies.iter().filter_map(|h| Audio::load(h).ok()).collect::<Vec<_>>()),
            Err(e) => {
                failed.push((song.name.clone(), format!("{}: {e}", song.lead.display())));
                continue;
            }
        };
        let ph = phrases(&song.f0);
        for (i, &(a, b)) in crate::vocalset::spread(&ph, limit).into_iter().enumerate() {
            let (t0, t1) = (a - 0.15, b + 0.15);
            let take0 = cut(&lead, t0, t1);
            let frames = (take0.seconds() * 200.0) as usize + 1;
            let truth = truth_at(&song.f0, t0, frames);
            // the notes: the detector's segmentation of the untouched phrase, scored against the annotation
            let an = analyze_with(&mono_of(&take0), take0.sr as f32, est);
            let spans: Vec<(f64, f64)> = an.notes.iter().filter(|n| n.end - n.start >= 0.12).map(|n| (n.start, n.end)).collect();
            let nt: Vec<Option<f32>> = spans
                .iter()
                .map(|&(s, e)| median(truth[((s + 0.05) * 200.0) as usize..(((e - 0.03) * 200.0) as usize).min(frames)].to_vec()))
                .collect();
            let p = plan("slips", spans.len(), i);
            let name = format!("{} {:.1}–{:.1} s", song.name, a, b);
            let (take, sung, notes) = inject(&take0, &spans, &nt, &truth, &p, 0.0, est);
            out.push(Owned {
                name: name.clone(),
                group: "song lead, slips".into(),
                take: Audio::from_stereo(take.content().to_vec(), take.sr, take.channels),
                reference: Some(Audio::from_stereo(take0.content().to_vec(), take0.sr, take0.channels)),
                edge: edges(&truth, &spans),
                truth: truth.clone(),
                sung: sung.clone(),
                notes: notes.clone(),
                expect_related: Some(true),
                expect_interval: Some(0),
                pairs: spans.iter().map(|&s| (s, s)).collect(),
                truth_kind: "annotation",
            });
            // a backing vocal as the reference: other notes on the same words. Whatever the
            // comparison concludes, the lead's correct notes must stay where they are.
            for (h, harm) in harms.iter().enumerate() {
                let r = cut(harm, t0, t1);
                if voiced_share(&r, est) < 0.3 {
                    skipped.push(format!("{name} ← harmony {}: the backing vocal is silent there", h + 2));
                    continue;
                }
                let notes = notes.iter().cloned().map(|mut n| {
                    if n.label == Label::Slip {
                        n.label = Label::Natural;
                    }
                    n
                });
                out.push(Owned {
                    name: format!("{name} ← harmony {}", h + 2),
                    group: "song lead, backing vocal as reference".into(),
                    take: Audio::from_stereo(take.content().to_vec(), take.sr, take.channels),
                    reference: Some(r),
                    edge: edges(&truth, &spans),
                    truth: truth.clone(),
                    sung: sung.clone(),
                    notes: notes.collect(),
                    expect_related: None,
                    expect_interval: None,
                    pairs: vec![],
                    truth_kind: "annotation",
                });
            }
        }
    }
    out
}

/// The detector on the whole lead stem against the annotation (MIREX-style): raw pitch accuracy
/// within 50 cents over annotated voiced frames, cents error, voicing recall and false alarms.
pub fn estimation(songs: &[Song], est: Option<&dyn F0Estimator>) -> Value {
    let mut rows = vec![];
    for s in songs {
        let Ok(a) = Audio::load(&s.lead) else {
            rows.push(json!({ "song": s.name, "error": "lead stem missing" }));
            continue;
        };
        let an = analyze_with(&mono_of(&a), a.sr as f32, est);
        let truth = truth_at(&s.f0, 0.0, an.curve.len());
        let (mut voiced, mut hit, mut fa, mut unv, mut errs) = (0usize, 0usize, 0usize, 0usize, vec![]);
        for (t, d) in truth.iter().zip(&an.curve) {
            match (t.is_finite(), d.is_finite()) {
                (true, true) => {
                    voiced += 1;
                    hit += 1;
                    errs.push(((d - t) * 100.0).abs());
                }
                (true, false) => voiced += 1,
                (false, true) => {
                    unv += 1;
                    fa += 1;
                }
                (false, false) => unv += 1,
            }
        }
        let rpa = errs.iter().filter(|e| **e < 50.0).count() as f32 / voiced.max(1) as f32;
        rows.push(json!({
            "song": s.name, "annotatedVoicedFrames": voiced, "rawPitchAccuracy50ct": rpa,
            "medianAbsCents": crate::eval::quantile(&errs, 0.5), "p90AbsCents": crate::eval::quantile(&errs, 0.9),
            "octaveErrors": errs.iter().filter(|e| **e > 600.0).count(),
            "voicingRecall": hit as f32 / voiced.max(1) as f32, "voicingFalseAlarm": fa as f32 / unv.max(1) as f32,
        }));
    }
    json!(rows)
}
