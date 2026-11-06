//! Every change to a [`Project`] is an [`Op`]. The GUI, the CLI and undo all go through here.
//!
//! Ops are deliberately coarse "put" operations: the caller sends the whole new
//! clip / track / effect. That keeps the wire format obvious, makes ops
//! idempotent, and means the engine never has to merge field patches.

use serde::{Deserialize, Serialize};

use crate::model::*;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Op {
    /// Insert or replace clips by id.
    PutClips { clips: Vec<Clip> },
    RemoveClips { ids: Vec<Id> },
    /// Insert (new id) or replace a track. New tracks are appended.
    PutTrack { track: Track },
    /// Remove a track (lane) with its clips; lanes below move up one.
    RemoveTrack { id: Id },
    /// Insert or replace a mixer insert *without* touching its effect chain
    /// (`fx` in the payload is ignored for an existing insert).
    PutInsert { insert: Insert },
    RemoveInsert { id: Id },
    /// Insert or replace an effect. `index` places a new effect (default: end of chain).
    PutFx { insert_id: Id, fx: Fx, #[serde(default)] index: Option<usize> },
    RemoveFx { fx_id: Id },
    MoveFx { fx_id: Id, to: usize },
    PutPattern { pattern: Pattern },
    /// Rename / recolour a source. Audio itself is changed with dedicated commands.
    PutSource { source: Source },
    RemoveSource { id: Id },
    SetBpm { bpm: f64 },
    SetLoop { #[serde(rename = "loop")] range: Option<LoopRange> },
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum OpError {
    #[error("no {what} with id {id}")]
    Missing { what: &'static str, id: Id },
    #[error("{0}")]
    Invalid(String),
}

/// What an op touched, so the engine knows how much of the audio graph to rebuild.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Dirty {
    /// Clips, patterns, sources, tempo, loop: rebuild the timeline.
    pub timeline: bool,
    /// Inserts, routing, effect chains: rebuild the mixer graph.
    pub mixer: bool,
    /// Only parameters of existing things changed: the fast path is enough.
    pub params: bool,
}

impl Dirty {
    pub fn merge(&mut self, o: Dirty) {
        self.timeline |= o.timeline;
        self.mixer |= o.mixer;
        self.params |= o.params;
    }
    pub fn any(&self) -> bool {
        self.timeline || self.mixer || self.params
    }
}

fn bump(p: &mut Project, id: Id) {
    if id >= p.next_id {
        p.next_id = id + 1;
    }
}

/// Applies one op. On error the project is unchanged.
pub fn apply(p: &mut Project, op: &Op) -> Result<Dirty, OpError> {
    let mut d = Dirty::default();
    match op {
        Op::PutClips { clips } => {
            for c in clips {
                if c.len <= 0.0 || c.start < 0.0 || !c.start.is_finite() || !c.len.is_finite() {
                    return Err(OpError::Invalid(format!("clip {} has an invalid range", c.id)));
                }
                if c.track as usize >= p.tracks.len() {
                    return Err(OpError::Invalid(format!("clip {} is on track {} which does not exist", c.id, c.track)));
                }
                let known = match c.kind {
                    ClipKind::Pat => p.patterns.iter().any(|x| x.id == c.source),
                    ClipKind::Audio => p.sources.iter().any(|x| x.id == c.source),
                };
                if !known {
                    return Err(OpError::Missing { what: "clip source", id: c.source });
                }
            }
            for c in clips {
                bump(p, c.id);
                match p.clips.iter_mut().find(|x| x.id == c.id) {
                    Some(x) => *x = c.clone(),
                    None => p.clips.push(c.clone()),
                }
            }
            d.timeline = true;
        }
        Op::RemoveClips { ids } => {
            p.clips.retain(|c| !ids.contains(&c.id));
            d.timeline = true;
        }
        Op::PutTrack { track } => {
            if p.insert(track.insert).is_none() {
                return Err(OpError::Missing { what: "insert", id: track.insert });
            }
            bump(p, track.id);
            match p.tracks.iter_mut().find(|x| x.id == track.id) {
                Some(x) => {
                    if x.insert != track.insert {
                        d.mixer = true;
                    }
                    *x = track.clone();
                }
                None => {
                    p.tracks.push(track.clone());
                    d.mixer = true;
                }
            }
            d.params = true;
            d.timeline = true;
        }
        Op::RemoveTrack { id } => {
            let idx = p.tracks.iter().position(|t| t.id == *id).ok_or(OpError::Missing { what: "track", id: *id })? as u32;
            if p.tracks.len() == 1 {
                return Err(OpError::Invalid("the last track cannot be removed".into()));
            }
            p.tracks.remove(idx as usize);
            p.clips.retain(|c| c.track != idx);
            for c in p.clips.iter_mut() {
                if c.track > idx {
                    c.track -= 1;
                }
            }
            d.timeline = true;
            d.mixer = true;
        }
        Op::PutInsert { insert } => {
            bump(p, insert.id);
            match p.inserts.iter_mut().find(|x| x.id == insert.id) {
                Some(x) => {
                    let fx = std::mem::take(&mut x.fx);
                    *x = Insert { fx, ..insert.clone() };
                    d.params = true;
                }
                None => {
                    let mut m = insert.clone();
                    for f in &m.fx {
                        bump(p, f.id);
                    }
                    m.fx.retain(|f| f.kind != FxKind::Vst3);
                    p.inserts.push(m);
                    d.mixer = true;
                }
            }
        }
        Op::RemoveInsert { id } => {
            if *id == 0 {
                return Err(OpError::Invalid("the master cannot be removed".into()));
            }
            if p.insert(*id).is_none() {
                return Err(OpError::Missing { what: "insert", id: *id });
            }
            p.inserts.retain(|m| m.id != *id);
            for t in &mut p.tracks {
                if t.insert == *id {
                    t.insert = 0;
                }
            }
            d.mixer = true;
        }
        Op::PutFx { insert_id, fx, index } => {
            validate_fx(fx)?;
            if let Some((m, old)) = p.find_fx(fx.id) {
                if m.id != *insert_id || old.kind != fx.kind {
                    return Err(OpError::Invalid(format!("effect {} cannot change insert or type; remove and add it", fx.id)));
                }
                let plugin_changed = old.plugin.as_ref().map(|x| (&x.path, &x.cid)) != fx.plugin.as_ref().map(|x| (&x.path, &x.cid));
                if plugin_changed {
                    return Err(OpError::Invalid("a VST3 slot cannot swap its plugin; remove and add it".into()));
                }
                let bypass_changed = old.bypass != fx.bypass;
                let m = p.inserts.iter_mut().find(|m| m.id == *insert_id).unwrap();
                let slot = m.fx.iter_mut().find(|f| f.id == fx.id).unwrap();
                *slot = fx.clone();
                d.params = true;
                d.mixer |= bypass_changed;
            } else {
                let m = p
                    .inserts
                    .iter_mut()
                    .find(|m| m.id == *insert_id)
                    .ok_or(OpError::Missing { what: "insert", id: *insert_id })?;
                let at = index.unwrap_or(m.fx.len()).min(m.fx.len());
                m.fx.insert(at, fx.clone());
                bump(p, fx.id);
                d.mixer = true;
            }
        }
        Op::RemoveFx { fx_id } => {
            let mut found = false;
            for m in &mut p.inserts {
                let n = m.fx.len();
                m.fx.retain(|f| f.id != *fx_id);
                found |= m.fx.len() != n;
            }
            if !found {
                return Err(OpError::Missing { what: "effect", id: *fx_id });
            }
            d.mixer = true;
        }
        Op::MoveFx { fx_id, to } => {
            let m = p
                .inserts
                .iter_mut()
                .find(|m| m.fx.iter().any(|f| f.id == *fx_id))
                .ok_or(OpError::Missing { what: "effect", id: *fx_id })?;
            let from = m.fx.iter().position(|f| f.id == *fx_id).unwrap();
            let f = m.fx.remove(from);
            let to = (*to).min(m.fx.len());
            m.fx.insert(to, f);
            d.mixer = true;
        }
        Op::PutPattern { pattern } => {
            if pattern.len <= 0.0 {
                return Err(OpError::Invalid("pattern length must be positive".into()));
            }
            bump(p, pattern.id);
            match p.patterns.iter_mut().find(|x| x.id == pattern.id) {
                Some(x) => *x = pattern.clone(),
                None => p.patterns.push(pattern.clone()),
            }
            d.timeline = true;
        }
        Op::PutSource { source } => {
            let s = p
                .sources
                .iter_mut()
                .find(|x| x.id == source.id)
                .ok_or(OpError::Missing { what: "source", id: source.id })?;
            s.name = source.name.clone();
            s.color = source.color.clone();
        }
        Op::RemoveSource { id } => {
            p.sources.retain(|s| s.id != *id);
            p.clips.retain(|c| !(c.kind == ClipKind::Audio && c.source == *id));
            d.timeline = true;
        }
        Op::SetBpm { bpm } => {
            if !(20.0..=999.0).contains(bpm) {
                return Err(OpError::Invalid("bpm must be 20..999".into()));
            }
            p.bpm = *bpm;
            d.timeline = true;
        }
        Op::SetLoop { range } => {
            if let Some(r) = range {
                if !(r.b > r.a && r.a >= 0.0) {
                    return Err(OpError::Invalid("loop end must be after its start".into()));
                }
            }
            p.loop_range = *range;
            d.timeline = true;
        }
    }
    Ok(d)
}

fn validate_fx(fx: &Fx) -> Result<(), OpError> {
    if fx.kind == FxKind::Eq {
        match &fx.bands {
            Some(b) if b.len() == 7 => {}
            _ => return Err(OpError::Invalid("an EQ needs exactly 7 bands".into())),
        }
    }
    if fx.kind == FxKind::Vst3 && fx.plugin.is_none() {
        return Err(OpError::Invalid("a VST3 slot needs `plugin`".into()));
    }
    for (k, v) in &fx.params {
        if !v.is_finite() {
            return Err(OpError::Invalid(format!("parameter {k} is not a number")));
        }
    }
    Ok(())
}

/// Applies a batch atomically: all ops or none.
pub fn apply_all(p: &mut Project, ops: &[Op]) -> Result<Dirty, OpError> {
    let mut work = p.clone();
    let mut d = Dirty::default();
    for op in ops {
        d.merge(apply(&mut work, op)?);
    }
    *p = work;
    Ok(d)
}
