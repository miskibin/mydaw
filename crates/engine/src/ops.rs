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
    /// Insert or replace a pattern. Notes may still say `inst` instead of `ch` (old
    /// scripts): they get that instrument's built-in channel (created if missing). `len` is
    /// derived (the notes' end rounded up to a bar, at least `minLen`); whatever is sent is
    /// replaced. When an existing pattern's length changes, its clips that show exactly one
    /// whole pass of it (`offset` 0, `len` = old length) take the new length; other clips
    /// keep theirs.
    PutPattern { pattern: Pattern },
    /// Remove a pattern and every clip that plays it.
    RemovePattern { id: Id },
    /// Insert or replace a Channel Rack channel. `index` places a new channel (default:
    /// last) or moves an existing one.
    PutChannel { channel: Channel, #[serde(default)] index: Option<usize> },
    /// Remove a channel and its notes in every pattern.
    RemoveChannel { id: Id },
    /// Rename / recolour a source. Audio itself is changed with dedicated commands.
    PutSource { source: Source },
    RemoveSource { id: Id },
    /// Insert or replace an automation envelope.
    PutAutomation { automation: Automation },
    /// Remove an automation and every clip that plays it.
    RemoveAutomation { id: Id },
    /// Insert or replace a take group with its comp (takes, regions, crossfade).
    PutComp { comp: Comp },
    /// Remove a take group and every clip that plays it.
    RemoveComp { id: Id },
    /// Replace the lyrics notebook (all pages, in tab order). Never touches the audio graph.
    PutNotebook { notebook: Notebook },
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
                if !(c.stretch.is_finite() && (crate::model::STRETCH_MIN - 1e-9..=crate::model::STRETCH_MAX + 1e-9).contains(&c.stretch)) {
                    return Err(OpError::Invalid(format!("clip {}: stretch must be {}..{}", c.id, crate::model::STRETCH_MIN, crate::model::STRETCH_MAX)));
                }
                if c.kind != ClipKind::Audio && (c.stretch != 1.0 || c.resample) {
                    return Err(OpError::Invalid(format!("clip {}: only audio clips can be stretched", c.id)));
                }
                if !(c.pitch.is_finite() && c.pitch.abs() <= crate::model::CLIP_PITCH + 1e-4) {
                    return Err(OpError::Invalid(format!("clip {}: pitch must be within ±{} semitones", c.id, crate::model::CLIP_PITCH)));
                }
                if c.kind != ClipKind::Audio && c.pitch != 0.0 {
                    return Err(OpError::Invalid(format!("clip {}: only audio clips can be transposed", c.id)));
                }
                let known = match c.kind {
                    ClipKind::Pat => p.patterns.iter().any(|x| x.id == c.source),
                    ClipKind::Audio => p.sources.iter().any(|x| x.id == c.source),
                    ClipKind::Auto => p.automations.iter().any(|x| x.id == c.source),
                    ClipKind::Comp => p.comps.iter().any(|x| x.id == c.source),
                };
                if !known {
                    return Err(OpError::Missing { what: "clip source", id: c.source });
                }
                if let Some(r) = c.tune.as_ref().and_then(|t| t.reference.as_ref()) {
                    if r.melody.is_none() && !p.sources.iter().any(|x| x.id == r.source) {
                        return Err(OpError::Missing { what: "reference source", id: r.source });
                    }
                    if let Some(m) = &r.melody {
                        if m.notes.iter().any(|n| n.end.partial_cmp(&n.start) != Some(std::cmp::Ordering::Greater) || n.key > 127) {
                            return Err(OpError::Invalid(format!("clip {}: melody notes need end > start and a key 0..127", c.id)));
                        }
                    }
                    if r.speed.is_some_and(|v| !(0.0..=1000.0).contains(&v)) {
                        return Err(OpError::Invalid(format!("clip {}: reference speed must be 0..1000 ms", c.id)));
                    }
                    if [r.correction, r.vibrato, r.influence].iter().any(|v| !(0.0..=1.0).contains(v)) {
                        return Err(OpError::Invalid(format!("clip {}: reference correction, vibrato and influence must be 0..1", c.id)));
                    }
                    if r.octave.is_some_and(|o| o % 12 != 0 || o.abs() > 36) || r.key > 11 || r.scale > 4 {
                        return Err(OpError::Invalid(format!("clip {}: reference octave must be a multiple of 12 within ±36, key 0..11, scale 0..4", c.id)));
                    }
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
                // a parameter that changes the effect's latency (look-ahead) re-plans the PDC
                let bypass_changed = old.bypass != fx.bypass || crate::catalog::latency(old, 48000.0) != crate::catalog::latency(fx, 48000.0);
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
            if !(pattern.swing.is_finite() && (0.0..=1.0).contains(&pattern.swing)) {
                return Err(OpError::Invalid("swing must be 0..1".into()));
            }
            let mut pt = pattern.clone();
            for n in &pt.notes {
                if !(n.step.is_finite() && n.step >= 0.0 && n.len.is_finite() && n.len > 0.0) {
                    return Err(OpError::Invalid(format!("pattern {}: a note has an invalid step/len", pt.id)));
                }
                if !(n.v.is_finite() && (0.0..=1.0).contains(&n.v)) {
                    return Err(OpError::Invalid("note velocity `v` must be 0..1".into()));
                }
                if n.p.is_some_and(|k| k > 127) {
                    return Err(OpError::Invalid("note key `p` must be 0..127".into()));
                }
                if (n.step + n.len) as f64 > TOTAL_BEATS * 4.0 {
                    return Err(OpError::Invalid(format!("pattern {}: notes must end within {TOTAL_BEATS} beats", pt.id)));
                }
            }
            if let Some(n) = pt.notes.iter().find(|n| p.channel(n.ch).is_none()) {
                return Err(OpError::Missing { what: "channel", id: n.ch });
            }
            pt.fit_len();
            bump(p, pt.id);
            let (id, len) = (pt.id, pt.len);
            match p.patterns.iter_mut().find(|x| x.id == id) {
                Some(x) => {
                    let old = x.len;
                    *x = pt;
                    if (old - len).abs() > 1e-9 {
                        follow_pattern_length(p, id, old, len);
                    }
                }
                None => p.patterns.push(pt),
            }
            d.timeline = true;
        }
        Op::PutChannel { channel, index } => {
            let c = channel;
            if !(c.vol.is_finite() && (0.0..=1.0).contains(&c.vol)) {
                return Err(OpError::Invalid("channel vol is a fader position 0..1".into()));
            }
            if !(c.pan.is_finite() && (-1.0..=1.0).contains(&c.pan)) {
                return Err(OpError::Invalid("channel pan must be -1..1".into()));
            }
            if c.root > 127 {
                return Err(OpError::Invalid("channel root must be 0..127".into()));
            }
            if let ChannelSrc::Sample { source, sampler } = &c.src {
                if !p.sources.iter().any(|s| s.id == *source) {
                    return Err(OpError::Missing { what: "source", id: *source });
                }
                sampler.check().map_err(OpError::Invalid)?;
            }
            if let ChannelSrc::Synth { synth } = &c.src {
                for (k, v) in &synth.params {
                    if crate::poly::params::index_of(k).is_none() {
                        return Err(OpError::Invalid(format!("synth has no parameter `{k}`")));
                    }
                    if !v.is_finite() {
                        return Err(OpError::Invalid(format!("synth parameter `{k}` must be a number")));
                    }
                }
            }
            if let ChannelSrc::Keys { keys } = &c.src {
                let (r, y) = (crate::keys::RELEASE_MS, crate::keys::DYNAMICS);
                if !(keys.release.is_finite() && (r.0..=r.1).contains(&keys.release)) {
                    return Err(OpError::Invalid(format!("keys release must be {}..{} ms", r.0, r.1)));
                }
                if !(keys.dynamics.is_finite() && (y.0..=y.1).contains(&keys.dynamics)) {
                    return Err(OpError::Invalid("keys dynamics must be 0..100 %".into()));
                }
            }
            if let Some(ins) = c.insert {
                if p.insert(ins).is_none() {
                    return Err(OpError::Missing { what: "insert", id: ins });
                }
            }
            bump(p, c.id);
            match p.channels.iter().position(|x| x.id == c.id) {
                Some(i) => {
                    let x = &p.channels[i];
                    // knobs and mute/solo take the fast path; what a note plays or where it goes rebuilds
                    // a synth's sound (its parameters) takes the fast path too
                    let same_kind = matches!((&x.src, &c.src), (ChannelSrc::Synth { .. }, ChannelSrc::Synth { .. }));
                    // (a piano's knobs are read from the channel table, which the timeline carries)
                    if (x.src != c.src && !same_kind) || x.insert != c.insert || x.root != c.root {
                        d.timeline = true;
                    } else {
                        d.params = true;
                    }
                    p.channels[i] = c.clone();
                    if let Some(to) = index {
                        let ch = p.channels.remove(i);
                        let to = (*to).min(p.channels.len());
                        p.channels.insert(to, ch);
                        d.timeline = true;
                    }
                }
                None => {
                    let at = index.unwrap_or(p.channels.len()).min(p.channels.len());
                    p.channels.insert(at, c.clone());
                    d.timeline = true;
                }
            }
        }
        Op::RemoveChannel { id } => {
            if p.channel(*id).is_none() {
                return Err(OpError::Missing { what: "channel", id: *id });
            }
            p.channels.retain(|c| c.id != *id);
            for pt in p.patterns.iter_mut() {
                pt.notes.retain(|n| n.ch != *id);
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
            // sampler channels playing it go, with their notes
            let gone: Vec<Id> = p.channels.iter().filter(|c| c.sample_source() == Some(*id)).map(|c| c.id).collect();
            p.channels.retain(|c| !gone.contains(&c.id));
            for pt in p.patterns.iter_mut() {
                pt.notes.retain(|n| !gone.contains(&n.ch));
            }
            p.clips.retain(|c| !(c.kind == ClipKind::Audio && c.source == *id));
            // takes recorded into this source go too; a comp left without takes goes with its clips
            for cp in p.comps.iter_mut() {
                let gone: Vec<Id> = cp.takes.iter().filter(|t| t.source == *id).map(|t| t.id).collect();
                for t in gone {
                    cp.remove_take(t);
                }
            }
            let empty: Vec<Id> = p.comps.iter().filter(|c| c.takes.is_empty()).map(|c| c.id).collect();
            p.comps.retain(|c| !c.takes.is_empty());
            p.clips.retain(|c| !(c.kind == ClipKind::Comp && empty.contains(&c.source)));
            for t in p.clips.iter_mut().filter_map(|c| c.tune.as_mut()) {
                if t.reference.as_ref().is_some_and(|r| r.melody.is_none() && r.source == *id) {
                    t.reference = None;
                }
            }
            d.timeline = true;
        }
        Op::PutAutomation { automation } => {
            let mut a = automation.clone();
            if a.points.iter().any(|q| !q.t.is_finite() || !q.v.is_finite() || !q.tension.is_finite()) {
                return Err(OpError::Invalid("automation points must be numbers".into()));
            }
            a.points.sort_by(|x, y| x.t.total_cmp(&y.t));
            for q in a.points.iter_mut() {
                q.v = q.v.clamp(0.0, 1.0);
                q.tension = q.tension.clamp(-1.0, 1.0);
            }
            bump(p, a.id);
            match p.automations.iter_mut().find(|x| x.id == a.id) {
                Some(x) => *x = a,
                None => p.automations.push(a),
            }
            d.timeline = true;
        }
        Op::RemovePattern { id } => {
            if !p.patterns.iter().any(|x| x.id == *id) {
                return Err(OpError::Missing { what: "pattern", id: *id });
            }
            p.patterns.retain(|x| x.id != *id);
            p.clips.retain(|c| !(c.kind == ClipKind::Pat && c.source == *id));
            d.timeline = true;
        }
        Op::RemoveAutomation { id } => {
            p.automations.retain(|a| a.id != *id);
            p.clips.retain(|c| !(c.kind == ClipKind::Auto && c.source == *id));
            d.timeline = true;
        }
        Op::PutComp { comp } => {
            let c = validate_comp(p, comp)?;
            bump(p, c.id);
            for t in &c.takes {
                bump(p, t.id);
            }
            match p.comps.iter_mut().find(|x| x.id == c.id) {
                Some(x) => *x = c,
                None => p.comps.push(c),
            }
            d.timeline = true;
        }
        Op::RemoveComp { id } => {
            if p.comp(*id).is_none() {
                return Err(OpError::Missing { what: "comp", id: *id });
            }
            p.comps.retain(|c| c.id != *id);
            p.clips.retain(|c| !(c.kind == ClipKind::Comp && c.source == *id));
            d.timeline = true;
        }
        Op::PutNotebook { notebook } => {
            validate_notebook(notebook)?;
            for pg in &notebook.pages {
                bump(p, pg.id);
            }
            p.notebook = notebook.clone();
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

/// A pattern's length went from `old` to `new` beats: clips that show exactly one whole
/// pass of it (offset 0, length = old length) follow; longer, shorter or offset clips keep
/// their length (they were sized on purpose). Clips never run past the playlist end.
fn follow_pattern_length(p: &mut Project, id: Id, old: f64, new: f64) {
    for c in p.clips.iter_mut() {
        if c.kind == ClipKind::Pat && c.source == id && c.offset.abs() < 1e-9 && (c.len - old).abs() < 1e-6 {
            c.len = new.min(TOTAL_BEATS - c.start).max(1.0 / 16.0);
        }
    }
}

/// Checks a notebook: unique page ids, bars 1..=NOTEBOOK_MAX_BAR, bounded size.
pub fn validate_notebook(nb: &Notebook) -> Result<(), OpError> {
    let bad = |m: String| Err(OpError::Invalid(m));
    if nb.pages.len() > NOTEBOOK_MAX_PAGES {
        return bad(format!("a notebook has at most {NOTEBOOK_MAX_PAGES} pages"));
    }
    let mut ids = std::collections::HashSet::new();
    for pg in &nb.pages {
        if !ids.insert(pg.id) {
            return bad(format!("notebook page id {} is used twice", pg.id));
        }
        if !(1..=NOTEBOOK_MAX_BAR).contains(&pg.start_bar) {
            return bad(format!("page {}: start bar must be 1..{NOTEBOOK_MAX_BAR}", pg.id));
        }
        if pg.text.len() > NOTEBOOK_MAX_TEXT {
            return bad(format!("page {}: text is longer than {NOTEBOOK_MAX_TEXT} bytes", pg.id));
        }
        if pg.name.chars().count() > 80 {
            return bad(format!("page {}: name is longer than 80 characters", pg.id));
        }
    }
    Ok(())
}

/// Checks a comp and returns it normalised (regions sorted, same-take neighbours joined).
pub fn validate_comp(p: &Project, comp: &Comp) -> Result<Comp, OpError> {
    let bad = |m: String| Err(OpError::Invalid(m));
    if comp.takes.is_empty() {
        return bad(format!("comp {} has no takes", comp.id));
    }
    if !(comp.xfade.is_finite() && (0.0..=MAX_XFADE).contains(&comp.xfade)) {
        return bad(format!("crossfade must be 0..{} s", MAX_XFADE));
    }
    for (i, t) in comp.takes.iter().enumerate() {
        if !(t.start.is_finite() && t.len.is_finite() && t.offset.is_finite()) || t.len <= 0.0 || t.offset < 0.0 {
            return bad(format!("take {} has an invalid range", t.id));
        }
        if comp.takes[..i].iter().any(|x| x.id == t.id) {
            return bad(format!("take id {} is used twice", t.id));
        }
        if !p.sources.iter().any(|s| s.id == t.source) {
            return Err(OpError::Missing { what: "take source", id: t.source });
        }
    }
    let mut regions = comp.regions.clone();
    for r in &regions {
        if !(r.from.is_finite() && r.to.is_finite()) || r.to <= r.from {
            return bad(format!("comp region {}..{} is empty or invalid", r.from, r.to));
        }
        if comp.take(r.take).is_none() {
            return Err(OpError::Missing { what: "take", id: r.take });
        }
        if let Some(x) = r.xfade {
            if !(x.is_finite() && (0.0..=MAX_XFADE).contains(&x)) {
                return bad(format!("crossfade must be 0..{} s", MAX_XFADE));
            }
        }
    }
    regions.sort_by(|a, b| a.from.total_cmp(&b.from));
    if regions.windows(2).any(|w| w[1].from < w[0].to - 1e-9) {
        return bad("comp regions overlap".into());
    }
    Ok(Comp { regions: merge_regions(regions), ..comp.clone() })
}

fn validate_fx(fx: &Fx) -> Result<(), OpError> {
    if fx.kind == FxKind::Eq {
        match &fx.bands {
            Some(b) if b.len() <= MAX_BANDS => {}
            _ => return Err(OpError::Invalid(format!("an EQ has 0..{MAX_BANDS} bands"))),
        }
    }
    if fx.kind == FxKind::Vst3 && fx.plugin.is_none() {
        return Err(OpError::Invalid("a VST3 slot needs `plugin`".into()));
    }
    match (&fx.maps, fx.kind == FxKind::Warp) {
        (None, true) => {
            return Err(OpError::Invalid(
                "a Warp needs `maps` (see fx_catalog().warp for the factory slots)".into(),
            ))
        }
        (Some(_), false) => return Err(OpError::Invalid("only a Warp has `maps`".into())),
        (Some(m), true) => validate_maps(m)?,
        (None, false) => {}
    }
    for (k, v) in &fx.params {
        if !v.is_finite() {
            return Err(OpError::Invalid(format!("parameter {k} is not a number")));
        }
    }
    Ok(())
}

fn validate_maps(m: &crate::model::WarpMaps) -> Result<(), OpError> {
    use crate::model::{WARP_BAR, WARP_POINTS, WARP_SLOTS};
    for (bank, slots) in [("time", &m.time), ("vol", &m.vol)] {
        if slots.len() > WARP_SLOTS {
            return Err(OpError::Invalid(format!(
                "Warp {bank} maps: at most {WARP_SLOTS} slots"
            )));
        }
        for (i, s) in slots.iter().enumerate() {
            let bad = |why: &str| {
                Err(OpError::Invalid(format!(
                    "Warp {bank} slot {}: {why}",
                    i + 1
                )))
            };
            if s.points.len() > WARP_POINTS {
                return bad(&format!("at most {WARP_POINTS} points"));
            }
            if s.points.iter().any(|p| {
                !(0.0..=WARP_BAR).contains(&p.t)
                    || !(0.0..=1.0).contains(&p.v)
                    || !p.tension.is_finite()
                    || p.tension.abs() > 1.0
            }) {
                return bad("points need t 0..4 beats, v 0..1, tension -1..1");
            }
            if s.points.windows(2).any(|w| w[1].t < w[0].t) {
                return bad("points must be in time order");
            }
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
