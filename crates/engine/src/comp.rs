//! Playing a comp: the regions of a [`Comp`] resolved off-thread into segments the
//! audio thread (and the offline renderer, and "flatten") can mix without allocating.
//!
//! Every region boundary is an equal-power crossfade centred on the boundary: the
//! outgoing segment fades with `cos`, the incoming one with `sin`, so the summed
//! power stays constant and nothing clicks. A boundary next to silence is a short
//! fade of the same length. Takes are never cut, so both sides of a crossfade read
//! real audio past the boundary.

use std::collections::HashMap;
use std::f64::consts::FRAC_PI_2;
use std::sync::Arc;

use crate::model::{Clip, ClipKind, Comp, Id, Project, Take, DEFAULT_XFADE};
use crate::sources::Audio;

/// One region of a comp, with its fades, in comp beats.
pub struct CompSeg {
    pub audio: Arc<Audio>,
    /// Gain starts rising here (`from − xin/2`).
    pub a: f64,
    /// Gain is zero again from here on (`to + xout/2`).
    pub b: f64,
    /// Fade-in / fade-out lengths in beats (0 = hard edge at `from` / `to`).
    pub xin: f64,
    pub xout: f64,
    /// Source beat = comp beat + `src0`.
    pub src0: f64,
}

impl CompSeg {
    #[inline]
    fn gain(&self, t: f64) -> f32 {
        if t < self.a || t >= self.b {
            return 0.0;
        }
        let mut g = 1.0;
        if self.xin > 0.0 {
            let x = (t - self.a) / self.xin;
            if x < 1.0 {
                g *= (x * FRAC_PI_2).sin();
            }
        }
        if self.xout > 0.0 {
            let x = (self.b - t) / self.xout;
            if x < 1.0 {
                g *= (x * FRAC_PI_2).sin();
            }
        }
        g as f32
    }
}

/// A comp ready to play. Segments are sorted by `a`.
pub struct CompRt {
    pub segs: Vec<CompSeg>,
}

impl CompRt {
    /// Adds `n` frames of the comp to `l`/`r`, starting at comp beat `t0` and
    /// advancing `dt` beats per frame. `spb` = seconds per beat. RT-safe.
    pub fn render_add(&self, t0: f64, dt: f64, l: &mut [f32], r: &mut [f32], spb: f64) {
        let n = l.len().min(r.len());
        if n == 0 || dt <= 0.0 {
            return;
        }
        let t1 = t0 + dt * n as f64;
        for s in &self.segs {
            if s.b <= t0 || s.a >= t1 {
                continue;
            }
            let i0 = (((s.a - t0) / dt).ceil().max(0.0) as usize).min(n);
            let i1 = (((s.b - t0) / dt).ceil().max(0.0) as usize).min(n);
            let fs = s.audio.sr as f64 * spb;
            for i in i0..i1 {
                let t = t0 + i as f64 * dt;
                let g = s.gain(t);
                if g == 0.0 {
                    continue;
                }
                let (x, y) = s.audio.sample((t + s.src0) * fs);
                l[i] += x * g;
                r[i] += y * g;
            }
        }
    }
}

/// Resolves a comp. `audition` plays one whole take instead of the comp.
/// Regions whose take has no loaded audio are silent.
pub fn build(comp: &Comp, spb: f64, audition: Option<Id>, audio: &HashMap<Id, Arc<Audio>>) -> CompRt {
    let mut segs = vec![];
    if let Some(t) = audition.and_then(|id| comp.take(id)) {
        if let Some(a) = audio.get(&t.source) {
            segs.push(CompSeg { audio: a.clone(), a: t.start, b: t.end(), xin: 0.0, xout: 0.0, src0: t.offset - t.start });
        }
        return CompRt { segs };
    }
    let rs = &comp.regions;
    let to_beats = |s: f32| (s as f64 / spb).max(0.0);
    let def = comp.xfade;
    let adjacent = |i: usize| i + 1 < rs.len() && (rs[i + 1].from - rs[i].to).abs() < 1e-9;
    let len = |i: usize| rs[i].to - rs[i].from;
    // fade length at each region's start: shared with the previous region when they
    // touch, and never longer than either side (so both halves of every fade fit)
    let fade_in: Vec<f64> = (0..rs.len())
        .map(|i| {
            let x = to_beats(rs[i].xfade.unwrap_or(def)).min(len(i));
            if i > 0 && adjacent(i - 1) {
                x.min(len(i - 1))
            } else {
                x
            }
        })
        .collect();
    for (i, r) in rs.iter().enumerate() {
        let Some(t) = comp.take(r.take) else { continue };
        let Some(a) = audio.get(&t.source) else { continue };
        let xin = fade_in[i];
        let xout = if adjacent(i) { fade_in[i + 1] } else { to_beats(r.xfade.unwrap_or(def)).min(len(i)) };
        segs.push(CompSeg { audio: a.clone(), a: r.from - xin / 2.0, b: r.to + xout / 2.0, xin, xout, src0: t.offset - t.start });
    }
    segs.sort_by(|x, y| x.a.total_cmp(&y.a));
    CompRt { segs }
}

/// Puts freshly recorded passes — `(source, timeline start, length)` in beats, oldest
/// first — on `track`:
///
/// * over a comp clip: they become new takes of its comp (the clip grows to show them);
/// * over plain audio clips (an earlier single take): those become the first takes of a
///   new comp, the passes the next ones — recording never overwrites;
/// * elsewhere: one pass is an ordinary audio clip, several passes make a new comp.
///
/// The newest take becomes active: the comp plays it over its whole range (a last pass
/// cut short by Stop leaves the rest of the range to the pass before it).
pub fn land_takes(p: &mut Project, track: u32, passes: &[(Id, f64, f64)]) {
    if passes.is_empty() {
        return;
    }
    let r0 = passes.iter().map(|x| x.1).fold(f64::INFINITY, f64::min);
    let r1 = passes.iter().map(|x| x.1 + x.2).fold(f64::NEG_INFINITY, f64::max);
    let overlaps = |c: &Clip| c.track == track && c.start < r1 - 1e-9 && c.start + c.len > r0 + 1e-9;

    // 1. recording over a comp: more takes for it
    let hit = p.clips.iter().position(|c| c.kind == ClipKind::Comp && overlaps(c) && p.comps.iter().any(|x| x.id == c.source));
    if let Some(ci) = hit {
        let comp_id = p.clips[ci].source;
        let origin = p.clips[ci].start - p.clips[ci].offset;
        let mut comp = p.comps.iter().find(|x| x.id == comp_id).unwrap().clone();
        let mut fresh = vec![];
        for &(src, start, len) in passes {
            let id = p.alloc_id();
            fresh.push(id);
            comp.takes.push(Take { id, name: format!("Take {}", comp.takes.len() + 1), source: src, start: start - origin, len, offset: 0.0, color: None });
        }
        // comp time stays ≥ 0: shift it (and every clip playing it) when a take starts earlier
        let lo = comp.extent().0;
        if lo < 0.0 {
            shift(&mut comp, -lo);
            for c in p.clips.iter_mut().filter(|c| c.kind == ClipKind::Comp && c.source == comp_id) {
                c.offset -= lo;
            }
        }
        // oldest first, so the newest pass plays wherever it has audio (a pass cut
        // short by Stop leaves the rest of the range to the pass before it)
        for id in fresh {
            let t = comp.take(id).unwrap().clone();
            comp.write(id, t.start, t.end());
        }
        let c = &mut p.clips[ci];
        let (ns, ne) = (c.start.min(r0), (c.start + c.len).max(r1));
        c.offset -= c.start - ns;
        c.start = ns;
        c.len = ne - ns;
        *p.comps.iter_mut().find(|x| x.id == comp_id).unwrap() = comp;
        return;
    }

    // 2. recording over plain audio clips: they become the first takes
    let mut old: Vec<Clip> = p
        .clips
        .iter()
        .filter(|c| c.kind == ClipKind::Audio && overlaps(c) && !c.rev && !c.norm && c.tune.as_ref().is_none_or(|t| t.notes.is_empty() && t.pencil.is_empty()))
        .cloned()
        .collect();
    old.sort_by(|a, b| a.start.total_cmp(&b.start));
    if old.is_empty() && passes.len() == 1 {
        let (src, start, len) = passes[0];
        let id = p.alloc_id();
        p.clips.push(Clip { id, track, start, len, offset: 0.0, kind: ClipKind::Audio, source: src, muted: false, gain: 0.0, fade_in: 0.0, fade_out: 0.0, pan: 0.0, norm: false, rev: false, tune: None, stretch: 1.0, resample: false, pitch: 0.0 });
        return;
    }
    // (source, timeline start, len, source offset)
    let mut all: Vec<(Id, f64, f64, f64)> = old.iter().map(|c| (c.source, c.start, c.len, c.offset)).collect();
    all.extend(passes.iter().map(|&(s, a, l)| (s, a, l, 0.0)));
    let origin = all.iter().map(|x| x.1).fold(f64::INFINITY, f64::min);
    let end = all.iter().map(|x| x.1 + x.2).fold(f64::NEG_INFINITY, f64::max);
    let id = p.alloc_id();
    let mut comp = Comp { id, name: format!("Comp {}", p.comps.len() + 1), takes: vec![], regions: vec![], xfade: DEFAULT_XFADE };
    for (k, &(src, start, len, offset)) in all.iter().enumerate() {
        let tid = p.alloc_id();
        comp.takes.push(Take { id: tid, name: format!("Take {}", k + 1), source: src, start: start - origin, len, offset, color: None });
    }
    // what played before, then each pass on top, oldest first: the newest plays
    // wherever it has audio
    for t in comp.takes.clone() {
        comp.write(t.id, t.start, t.end());
    }
    let (gain, pan) = old.first().map(|c| (c.gain, c.pan)).unwrap_or((0.0, 0.0));
    let gone: Vec<Id> = old.iter().map(|c| c.id).collect();
    p.clips.retain(|c| !gone.contains(&c.id));
    let cid = p.alloc_id();
    p.clips.push(Clip { id: cid, track, start: origin, len: end - origin, offset: 0.0, kind: ClipKind::Comp, source: id, muted: false, gain, fade_in: 0.0, fade_out: 0.0, pan, norm: false, rev: false, tune: None, stretch: 1.0, resample: false, pitch: 0.0 });
    p.comps.push(comp);
}

/// Moves everything in a comp `d` beats later in comp time.
fn shift(comp: &mut Comp, d: f64) {
    for t in comp.takes.iter_mut() {
        t.start += d;
    }
    for r in comp.regions.iter_mut() {
        r.from += d;
        r.to += d;
    }
}
