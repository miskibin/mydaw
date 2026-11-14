//! `editTune`: note-level edits of an audio clip's pitch edits (`clip.tune`), the operations of
//! the pitch editor as one op. The editor and the CLI both send it, so splitting, merging,
//! moving, resetting and drawing a note are implemented once (`ui/src/lib/pitch.ts`
//! `applyTuneEdit` is the browser mock's port, checked against the same fixture).
//!
//! Every index in an edit names a note of the **base** segmentation — the notes as they were
//! before this op (or `init` when the clip has none yet) — whatever order the edits are listed
//! in, so a batch reads like the `wave pitch inspect` it was planned from. The parts apply in a
//! fixed order: `clear`, `reset`, `notes`, `split`, `merge`, `erasePencil`, `pencil`.
//! `base`, when given, is the fingerprint of that segmentation ([`fingerprint`]): if the notes
//! changed since it was read (another edit, a re-analysis with another detector), the op is
//! refused rather than editing the wrong notes.

use serde::{Deserialize, Serialize};

use crate::model::{ClipTune, NoteEdit};

/// Shortest note a split may leave, seconds (`MIN_NOTE` in the editor).
pub const MIN_NOTE: f64 = 0.02;
/// Largest `shift` / `formant`, cents.
pub const MAX_CENTS: f32 = 2400.0;
/// Level change range, dB.
pub const GAIN_RANGE: (f32, f32) = (-24.0, 12.0);

/// New values for one note. Absent fields keep their value; `speed: null` clears the note's own
/// retune speed (the reference's, else immediate). `shiftBy` adds to `shift` (after `shift`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NotePatch {
    /// Index of the note in the base segmentation.
    pub i: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shift: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shift_by: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drift: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formant: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain: Option<f32>,
    /// `Some(None)` = clear (JSON null), `None` = keep.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "double_option")]
    pub speed: Option<Option<f32>>,
}

mod double_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn serialize<S: Serializer>(v: &Option<Option<f32>>, s: S) -> Result<S::Ok, S::Error> {
        v.as_ref().unwrap_or(&None).serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<f32>>, D::Error> {
        Option::<f32>::deserialize(d).map(Some)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SplitAt {
    pub i: usize,
    /// Seconds of the source, inside the note (at least `MIN_NOTE` from both its ends).
    pub at: f64,
}

/// The edits of one `editTune` op (everything but the clip id).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TuneEdit {
    /// Fingerprint of the segmentation the indices were read from; refused if it differs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// The notes `(start, end)` to start from when the clip has no notes yet (the analysis'
    /// segmentation, as `pitch inspect` lists it). Ignored when the clip already has notes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub init: Option<Vec<[f64; 2]>>,
    /// Remove every note edit and pencil stroke (a Tune to Reference setting stays).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clear: bool,
    /// Notes back to as sung: shift, drift, formant and gain 0, no own speed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reset: Vec<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<NotePatch>,
    /// Split notes; both halves keep the note's edits.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub split: Vec<SplitAt>,
    /// Merge note `i` with note `i + 1` (they must touch); the result keeps `i`'s edits.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merge: Vec<usize>,
    /// Remove pencil points within each `[from, to]` (seconds).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub erase_pencil: Vec<[f32; 2]>,
    /// One pencil stroke `[[seconds, midi], …]` (≥ 2 points, < 30 ms apart to be one run),
    /// replacing what was drawn over its span.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pencil: Vec<[f32; 2]>,
}

/// Fingerprint of a segmentation: FNV-1a 64 over the note edges in whole microseconds.
/// The same function is in `ui/src/lib/pitch.ts` (`segmentationFingerprint`).
pub fn fingerprint(spans: &[(f64, f64)]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |x: i64| {
        for b in x.to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    eat(spans.len() as i64);
    for &(a, b) in spans {
        eat((a * 1e6).round() as i64);
        eat((b * 1e6).round() as i64);
    }
    format!("{h:016x}")
}

fn spans(notes: &[NoteEdit]) -> Vec<(f64, f64)> {
    notes.iter().map(|n| (n.start, n.end)).collect()
}

fn blank(start: f64, end: f64) -> NoteEdit {
    NoteEdit { start, end, shift: 0.0, drift: 0.0, formant: 0.0, gain: 0.0, speed: None }
}

/// Checks a segmentation: finite, non-negative, each note longer than zero, sorted, no overlap.
pub fn check_notes(notes: &[NoteEdit]) -> Result<(), String> {
    for (k, n) in notes.iter().enumerate() {
        if !(n.start.is_finite() && n.end.is_finite() && n.start >= 0.0 && n.end > n.start) {
            return Err(format!("note {k} has an invalid range"));
        }
        if k > 0 && n.start < notes[k - 1].end - 1e-9 {
            return Err(format!("notes {} and {k} overlap or are out of order", k - 1));
        }
        check_values(k, n)?;
    }
    Ok(())
}

fn check_values(k: usize, n: &NoteEdit) -> Result<(), String> {
    if !(n.shift.is_finite() && n.shift.abs() <= MAX_CENTS) {
        return Err(format!("note {k}: shift must be within ±{MAX_CENTS} cents"));
    }
    if !(n.formant.is_finite() && n.formant.abs() <= MAX_CENTS) {
        return Err(format!("note {k}: formant must be within ±{MAX_CENTS} cents"));
    }
    if !(0.0..=1.0).contains(&n.drift) {
        return Err(format!("note {k}: drift must be 0..1"));
    }
    if !(GAIN_RANGE.0..=GAIN_RANGE.1).contains(&n.gain) {
        return Err(format!("note {k}: gain must be {}..{} dB", GAIN_RANGE.0, GAIN_RANGE.1));
    }
    if n.speed.is_some_and(|s| !(0.0..=1000.0).contains(&s)) {
        return Err(format!("note {k}: speed must be 0..1000 ms"));
    }
    Ok(())
}

/// Applies `e` to `tune` (the clip's, `None` = no edits yet). `Ok(None)` = nothing left to
/// render (the clip plays its source). Errors name the first problem; nothing is applied then.
pub fn apply(tune: Option<&ClipTune>, e: &TuneEdit) -> Result<Option<ClipTune>, String> {
    let mut t = tune.cloned().unwrap_or_default();
    if t.notes.is_empty() {
        if let Some(init) = &e.init {
            t.notes = init.iter().map(|s| blank(s[0], s[1])).collect();
            check_notes(&t.notes).map_err(|m| format!("init: {m}"))?;
        }
    }
    let n = t.notes.len();
    if let Some(b) = &e.base {
        let now = fingerprint(&spans(&t.notes));
        if *b != now {
            return Err(format!("STALE_NOTES: the clip's notes changed since they were read (base {b}, now {now}); inspect it again"));
        }
    }
    let idx = |i: usize, what: &str| -> Result<usize, String> {
        if i < n {
            Ok(i)
        } else if n == 0 {
            Err(format!("{what}: the clip has no notes yet; pass `init` (the analysis' notes from `pitch inspect`)"))
        } else {
            Err(format!("{what}: no note {i} (the clip has {n})"))
        }
    };
    // which base notes a part names, to refuse contradictory batches
    let mut patched = vec![false; n];
    if e.clear {
        if !(e.reset.is_empty() && e.notes.is_empty() && e.split.is_empty() && e.merge.is_empty()) {
            return Err("`clear` removes every note edit; it cannot be combined with reset, notes, split or merge".into());
        }
        t.notes.clear();
        t.pencil.clear();
    }
    for &i in &e.reset {
        let i = idx(i, "reset")?;
        let nt = &mut t.notes[i];
        *nt = blank(nt.start, nt.end);
    }
    for p in &e.notes {
        let i = idx(p.i, "notes")?;
        if std::mem::replace(&mut patched[i], true) {
            return Err(format!("notes: note {i} is listed twice"));
        }
        let nt = &mut t.notes[i];
        if let Some(v) = p.shift {
            nt.shift = v;
        }
        if let Some(v) = p.shift_by {
            nt.shift += v;
        }
        if let Some(v) = p.drift {
            nt.drift = v;
        }
        if let Some(v) = p.formant {
            nt.formant = v;
        }
        if let Some(v) = p.gain {
            nt.gain = v;
        }
        if let Some(v) = p.speed {
            nt.speed = v;
        }
        check_values(i, nt)?;
    }
    // structure: splits and merges both name base notes; a note is split or merged, not both
    let mut cuts: Vec<(usize, f64)> = vec![];
    for s in &e.split {
        let i = idx(s.i, "split")?;
        let nt = &t.notes[i];
        if !(s.at.is_finite() && s.at - nt.start >= MIN_NOTE - 1e-9 && nt.end - s.at >= MIN_NOTE - 1e-9) {
            return Err(format!(
                "split: {:.3} s is not inside note {i} ({:.3}–{:.3} s) with {} ms to spare",
                s.at,
                nt.start,
                nt.end,
                MIN_NOTE * 1000.0
            ));
        }
        cuts.push((i, s.at));
    }
    cuts.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    if cuts.windows(2).any(|w| w[0].0 == w[1].0 && w[1].1 - w[0].1 < MIN_NOTE - 1e-9) {
        return Err(format!("split: two cuts in one note are closer than {} ms", MIN_NOTE * 1000.0));
    }
    let mut merges = e.merge.clone();
    merges.sort_unstable();
    merges.dedup();
    for &i in &merges {
        idx(i, "merge")?;
        idx(i + 1, "merge").map_err(|_| format!("merge: note {i} is the last one; it has no next note to merge with"))?;
        if (t.notes[i].end - t.notes[i + 1].start).abs() > 1e-6 {
            return Err(format!("merge: notes {i} and {} do not touch", i + 1));
        }
        for j in [i, i + 1] {
            if cuts.iter().any(|c| c.0 == j) {
                return Err(format!("note {j} is both split and merged in one edit"));
            }
        }
        if patched[i + 1] {
            return Err(format!("merge: note {} is merged into note {i}, its own edits in `notes` would be lost", i + 1));
        }
    }
    // rebuild: walk the base notes once, cutting and joining
    let mut out: Vec<NoteEdit> = Vec::with_capacity(n + cuts.len());
    let mut k = 0usize;
    while k < t.notes.len() {
        let mut nt = t.notes[k];
        // a run of merges k, k+1, …: k's edits over the whole span
        let mut j = k;
        while merges.binary_search(&j).is_ok() {
            j += 1;
            nt.end = t.notes[j].end;
        }
        let here: Vec<f64> = cuts.iter().filter(|c| c.0 == k).map(|c| c.1).collect();
        let mut from = nt.start;
        for at in here {
            out.push(NoteEdit { start: from, end: at, ..nt });
            from = at;
        }
        out.push(NoteEdit { start: from, ..nt });
        k = j + 1;
    }
    t.notes = out;
    for r in &e.erase_pencil {
        if !(r[0].is_finite() && r[1].is_finite() && r[1] >= r[0]) {
            return Err("erasePencil: each range is [from, to] seconds with to ≥ from".into());
        }
        t.pencil.retain(|p| p[0] < r[0] || p[0] > r[1]);
    }
    if !e.pencil.is_empty() {
        if e.pencil.len() < 2 {
            return Err("pencil: a stroke needs at least 2 points".into());
        }
        if e.pencil.iter().any(|p| !(p[0].is_finite() && p[0] >= 0.0 && p[1].is_finite() && (0.0..=127.0).contains(&p[1]))) {
            return Err("pencil: points are [seconds ≥ 0, midi 0..127]".into());
        }
        let mut s = e.pencil.clone();
        s.sort_by(|a, b| a[0].total_cmp(&b[0]));
        let (t0, t1) = (s[0][0], s[s.len() - 1][0]);
        t.pencil.retain(|p| p[0] < t0 - 1e-4 || p[0] > t1 + 1e-4);
        t.pencil.extend(s);
        t.pencil.sort_by(|a, b| a[0].total_cmp(&b[0]));
    }
    Ok(t.is_active().then_some(t))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn tune(spans: &[(f64, f64)]) -> ClipTune {
        ClipTune { notes: spans.iter().map(|&(a, b)| blank(a, b)).collect(), pencil: vec![], reference: None }
    }

    fn edit(v: Value) -> TuneEdit {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn indices_name_the_base_notes_whatever_the_order() {
        let t = tune(&[(0.0, 1.0), (1.0, 2.0), (2.5, 3.0), (3.0, 4.0)]);
        // split 0, merge 2+3 and move note 3 (merged away) is refused; move 1 and split 0 is fine
        let r = apply(Some(&t), &edit(json!({"split": [{"i": 0, "at": 0.5}], "notes": [{"i": 1, "shift": 30}], "merge": [2]})))
            .unwrap()
            .unwrap();
        let s: Vec<(f64, f64, f32)> = r.notes.iter().map(|n| (n.start, n.end, n.shift)).collect();
        assert_eq!(s, vec![(0.0, 0.5, 0.0), (0.5, 1.0, 0.0), (1.0, 2.0, 30.0), (2.5, 4.0, 0.0)]);
    }

    #[test]
    fn stale_and_invalid_edits_are_refused_without_changes() {
        let t = tune(&[(0.0, 1.0), (1.0, 2.0)]);
        let fp = fingerprint(&[(0.0, 1.0), (1.0, 2.0)]);
        assert!(apply(Some(&t), &edit(json!({"base": fp, "notes": [{"i": 1, "shift": 10}]}))).is_ok());
        let e = apply(Some(&t), &edit(json!({"base": "0000000000000000", "notes": [{"i": 1, "shift": 10}]}))).unwrap_err();
        assert!(e.starts_with("STALE_NOTES"), "{e}");
        for (v, want) in [
            (json!({"notes": [{"i": 2, "shift": 1}]}), "no note 2"),
            (json!({"notes": [{"i": 0, "shift": 1}, {"i": 0, "drift": 1}]}), "twice"),
            (json!({"notes": [{"i": 0, "drift": 2}]}), "drift"),
            (json!({"split": [{"i": 0, "at": 0.99}]}), "split"),
            (json!({"merge": [1]}), "last one"),
            (json!({"merge": [0], "split": [{"i": 1, "at": 1.5}]}), "both split and merged"),
            (json!({"clear": true, "reset": [0]}), "clear"),
            (json!({"pencil": [[0.1, 60]]}), "2 points"),
        ] {
            let err = apply(Some(&t), &edit(v.clone())).unwrap_err();
            assert!(err.contains(want), "{v}: {err}");
        }
        assert!(serde_json::from_value::<TuneEdit>(json!({"notez": []})).is_err(), "unknown fields are refused");
    }

    #[test]
    fn a_clip_without_notes_needs_init_and_reset_clears_speed() {
        let e = apply(None, &edit(json!({"notes": [{"i": 0, "shift": 10}]}))).unwrap_err();
        assert!(e.contains("init"), "{e}");
        let r =
            apply(None, &edit(json!({"init": [[0.0, 1.0], [1.2, 2.0]], "notes": [{"i": 1, "shift": -40, "speed": 30}]}))).unwrap().unwrap();
        assert_eq!(r.notes[1].shift, -40.0);
        assert_eq!(r.notes[1].speed, Some(30.0));
        let r2 = apply(Some(&r), &edit(json!({"notes": [{"i": 1, "shiftBy": 15, "speed": null}]}))).unwrap().unwrap();
        assert_eq!((r2.notes[1].shift, r2.notes[1].speed), (-25.0, None));
        let r3 = apply(Some(&r2), &edit(json!({"reset": [1]}))).unwrap().unwrap();
        assert_eq!(r3.notes[1], blank(1.2, 2.0));
        // init is ignored once the clip has notes
        let r4 = apply(Some(&r3), &edit(json!({"init": [[5.0, 6.0]], "notes": [{"i": 0, "gain": -3}]}))).unwrap().unwrap();
        assert_eq!(r4.notes.len(), 2);
        assert!(apply(Some(&r4), &edit(json!({"clear": true}))).unwrap().is_none());
    }

    #[test]
    fn pencil_strokes_replace_their_span() {
        let t = tune(&[(0.0, 1.0)]);
        let r = apply(Some(&t), &edit(json!({"pencil": [[0.2, 60], [0.1, 61], [0.12, 62]]}))).unwrap().unwrap();
        assert_eq!(r.pencil, vec![[0.1, 61.0], [0.12, 62.0], [0.2, 60.0]]);
        let r = apply(Some(&r), &edit(json!({"pencil": [[0.11, 64], [0.13, 64]]}))).unwrap().unwrap();
        assert_eq!(r.pencil, vec![[0.1, 61.0], [0.11, 64.0], [0.13, 64.0], [0.2, 60.0]]);
        let r = apply(Some(&r), &edit(json!({"erasePencil": [[0.0, 0.12]]}))).unwrap().unwrap();
        assert_eq!(r.pencil, vec![[0.13, 64.0], [0.2, 60.0]]);
    }

    #[test]
    fn fingerprints_follow_the_edges_only() {
        let a = fingerprint(&[(0.1, 0.5), (0.5, 0.9)]);
        assert_eq!(a, fingerprint(&[(0.1000000001, 0.5), (0.5, 0.9)]));
        assert_ne!(a, fingerprint(&[(0.1, 0.5), (0.5, 0.91)]));
        assert_ne!(a, fingerprint(&[(0.1, 0.9)]));
    }

    /// The browser mock ports this op (`ui/src/lib/pitch.ts` `applyTuneEdit`); both check the
    /// same cases. `WAVE_WRITE_FIXTURES=1` rewrites the fixture.
    #[test]
    fn the_ui_fixture_matches() {
        let base = tune(&[(0.0, 0.5), (0.5, 1.0), (1.2, 2.0), (2.0, 2.6)]);
        let mut withp = base.clone();
        withp.pencil = vec![[1.3, 60.0], [1.32, 60.5]];
        withp.notes[1].shift = 25.0;
        withp.notes[1].speed = Some(40.0);
        let cases: Vec<(&str, Option<ClipTune>, Value)> = vec![
            (
                "move two notes",
                Some(base.clone()),
                json!({"notes": [{"i": 0, "shift": -35}, {"i": 2, "shiftBy": 12.5, "drift": 0.4, "speed": 60}]}),
            ),
            ("split and merge", Some(withp.clone()), json!({"split": [{"i": 2, "at": 1.6}], "merge": [0]})),
            ("two cuts in one note", Some(base.clone()), json!({"split": [{"i": 3, "at": 2.4}, {"i": 3, "at": 2.2}]})),
            ("reset clears speed", Some(withp.clone()), json!({"reset": [1]})),
            ("clear keeps nothing", Some(withp.clone()), json!({"clear": true})),
            ("init then edit", None, json!({"init": [[0.0, 0.4], [0.4, 0.9]], "notes": [{"i": 1, "formant": -200, "gain": -3}]})),
            ("pencil", Some(withp.clone()), json!({"erasePencil": [[1.31, 1.4]], "pencil": [[1.5, 62], [1.52, 62.25]]})),
            ("stale base", Some(base.clone()), json!({"base": "0123456789abcdef", "notes": [{"i": 0, "shift": 1}]})),
            ("right base", Some(base.clone()), json!({"base": fingerprint(&spans(&base.notes)), "notes": [{"i": 0, "shift": 1}]})),
            ("bad index", Some(base.clone()), json!({"notes": [{"i": 9, "shift": 1}]})),
            ("no init", None, json!({"notes": [{"i": 0, "shift": 1}]})),
            ("merge not touching", Some(base.clone()), json!({"merge": [1]})),
            ("split too close", Some(base.clone()), json!({"split": [{"i": 0, "at": 0.01}]})),
            ("split and merge one note", Some(base.clone()), json!({"merge": [0], "split": [{"i": 1, "at": 0.7}]})),
            ("drift out of range", Some(base.clone()), json!({"notes": [{"i": 0, "drift": 1.5}]})),
        ];
        let rows: Vec<Value> = cases
            .into_iter()
            .map(|(name, t, e)| {
                let r = apply(t.as_ref(), &edit(e.clone()));
                json!({ "name": name, "tune": t, "edit": e, "result": r.as_ref().ok(), "error": r.as_ref().err().is_some() })
            })
            .collect();
        let fixture = json!({ "fingerprints": [
                { "spans": [[0.0, 0.5], [0.5, 1.0], [1.2, 2.0], [2.0, 2.6]], "hash": fingerprint(&spans(&base.notes)) },
                { "spans": [], "hash": fingerprint(&[]) },
                { "spans": [[0.123456789, 1.987654321]], "hash": fingerprint(&[(0.123456789, 1.987654321)]) },
            ], "cases": rows });
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/src/lib/__fixtures__/tune-edit.json");
        let text = serde_json::to_string_pretty(&fixture).unwrap() + "\n";
        if std::env::var("WAVE_WRITE_FIXTURES").is_ok() {
            std::fs::write(&path, &text).unwrap();
        }
        let on_disk = std::fs::read_to_string(&path).unwrap_or_default().replace("\r\n", "\n");
        assert_eq!(on_disk, text, "ui/src/lib/__fixtures__/tune-edit.json is stale: run with WAVE_WRITE_FIXTURES=1");
    }
}
