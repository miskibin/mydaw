//! Offline note-editor correction (the "Popraw wysokość" macro): every note's centre moved to
//! the nearest scale note by `center` %, drift flattened by `drift` %, rendered with tune_render.
//! `editorcorrect in.wav out.wav KEY SCALE CENTER% DRIFT%`
use wave_engine::model::{ClipTune, NoteEdit};
use wave_engine::pitch::{analyze, scale_mask, snap};
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let au = wave_engine::sources::Audio::load(std::path::Path::new(&a[1])).unwrap();
    let (key, scale): (u32, u32) = (a[3].parse().unwrap(), a[4].parse().unwrap());
    let (cp, dp): (f32, f32) = (a[5].parse().unwrap(), a[6].parse().unwrap());
    let mono: Vec<f32> = au
        .content()
        .chunks(2)
        .map(|f| (f[0] + f[1]) * 0.5)
        .collect();
    let an = analyze(&mono, au.sr as f32);
    let mask = scale_mask(scale);
    let notes = an
        .notes
        .iter()
        .map(|n| {
            let (i0, i1) = (
                (n.start * an.rate as f64) as usize,
                ((n.end * an.rate as f64) as usize).min(an.curve.len()),
            );
            let mut v: Vec<f32> = an.curve[i0..i1]
                .iter()
                .copied()
                .filter(|x| !x.is_nan())
                .collect();
            v.sort_by(f32::total_cmp);
            let shift = if v.is_empty() {
                0.0
            } else {
                let c = v[v.len() / 2];
                (snap(c, key, mask) - c) * 100.0 * cp / 100.0
            };
            NoteEdit {
                start: n.start,
                end: n.end,
                shift,
                drift: dp / 100.0,
                formant: 0.0,
                gain: 0.0,
            }
        })
        .collect();
    let out = wave_engine::tune_render::render(
        &au,
        &an,
        &ClipTune {
            notes,
            pencil: vec![],
            reference: None,
        },
        None,
    );
    wave_engine::sources::write_wav(std::path::Path::new(&a[2]), out.content(), au.sr).unwrap();
    eprintln!("{} notes corrected", an.notes.len());
}
