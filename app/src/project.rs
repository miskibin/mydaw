//! The project: what tracks there are, and what each one is set to.
//!
//! This is the **non-RT, editable** side of the mixer, in the same relationship
//! to [`MixerParams`](mydaw_rt_core::MixerParams) that
//! [`Pattern`](mydaw_plugin_sequencer::Pattern) has to `CompiledPattern`: the
//! shapes here are what a project file will hold and what the UI edits, and the
//! audio thread never sees them. What crosses the wall is a graph compiled from
//! this, plus a block of atomics the strips read.
//!
//! ```text
//!   webview            app (non-RT, here)                 audio thread
//!   -------            ------------------                 ------------
//!   set_track_gain --> TrackDef.gain_db --> MixerParams --> TrackStrip
//!   set_track_solo --> solo set --> effective mutes ----->  (mute is a ramp)
//!   add_track ------>  Project.tracks --> GraphSpec -----> CompiledGraph
//! ```
//!
//! # Why gain is stored in decibels and published in linear
//! A fader is a dB control -- that is the scale it is drawn on, the scale a user
//! thinks in, and the scale a project file should record, because -6.0 dB is
//! exact and 0.5011872 is a rounding of it. The audio thread wants a multiplier
//! and must not call `powf` per block, so the conversion happens exactly once,
//! here, on the way in.
//!
//! # Why solo is not a thing the audio thread knows about
//! "Is this track audible" is a question about the *whole project*: track 2 is
//! silent because track 0 is soloed, which the strip for track 2 cannot see
//! without reading every other track's state every block. So solo is resolved
//! here into an **effective mute** per track, and the audio thread only ever
//! reads one bool. Soloing is then exactly as click-free as muting, because it
//! *is* muting.

use mydaw_plugin_effects::EffectRegistry;
use mydaw_plugin_instruments::SingleDrum;
use mydaw_plugin_sampler::SamplerRegistry;
use mydaw_rt_core::{
    db_to_gain, gain_to_db, max_send_db, MixerParams, StripId, MAX_BUSES, MAX_GAIN_DB,
    MAX_SEND_LEVEL, MAX_TRACKS, SILENCE_DB,
};
use serde::Serialize;

use crate::clap_bridge::{ClapBoard, ClapSlotDto};
use crate::effects::{chain_dto, InsertDef, InsertDto, InsertError, InsertTarget, MAX_INSERTS};
use crate::source::{SamplerSourceRef, Source};
use crate::vst3_bridge::{Vst3Board, Vst3SlotDto};

/// What a fresh project puts on its melodic channel.
///
/// The first sampled instrument the registry has, if any -- in practice the
/// Salamander piano -- and otherwise the poly synth. A channel promising a
/// piano on a machine whose samples have not been downloaded would be five
/// minutes of silence and a bug report, and the honest fallback is a synth that
/// says it is one.
pub fn default_piano(sampler: &SamplerRegistry) -> Source {
    sampler
        .instruments()
        .first()
        .map_or(Source::PolySynth, |info| {
            Source::Sampler(SamplerSourceRef::from(info))
        })
}

/// One track, as the session holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackDef {
    /// Stable across renumbering, so the UI can key a list by it. Not the note
    /// channel -- see [`Project`].
    pub id: u32,
    pub name: String,
    /// What sits at the head of this track's chain.
    pub source: Source,
    /// Fader position, in dB. [`SILENCE_DB`] is off.
    pub gain_db: f32,
    /// Pan, `-1.0` hard left to `1.0` hard right.
    pub pan: f32,
    pub muted: bool,
    pub soloed: bool,
    /// Internal effects, in signal order, between the source and the gate.
    ///
    /// The parameter blocks inside these are shared with the nodes the graph is
    /// running -- see [`crate::effects`] -- so cloning a `TrackDef` shares them
    /// rather than copying them, which is what a DTO and a save both want.
    pub inserts: Vec<InsertDef>,
}

impl TrackDef {
    /// A track at unity gain, centred, unmuted, with an empty chain.
    pub fn new(id: u32, name: impl Into<String>, source: Source) -> Self {
        Self {
            id,
            name: name.into(),
            source,
            gain_db: 0.0,
            pan: 0.0,
            muted: false,
            soloed: false,
            inserts: Vec::new(),
        }
    }

    /// The linear multiplier this track's fader asks for.
    pub fn gain(&self) -> f32 {
        db_to_gain(self.gain_db)
    }
}

/// One bus, as the session holds it.
///
/// A track without a source. It sums whatever sends into it, runs its own
/// insert chain, and has a strip whose main output goes to the master -- see
/// [`mydaw_rt_core::graph`] for the whole topology and why a main output is
/// never rerouted.
#[derive(Debug, Clone, PartialEq)]
pub struct BusDef {
    /// Stable across renumbering. Key a UI list on it.
    pub id: u32,
    pub name: String,
    /// A colour the UI paints the strip and every send knob feeding it with.
    ///
    /// `Option`, and **absent** rather than empty when there is none: the engine
    /// has no palette, and inventing one would put a design decision in a file
    /// format. Same rule as a pattern's colour.
    pub color: Option<String>,
    pub gain_db: f32,
    pub pan: f32,
    pub muted: bool,
    pub soloed: bool,
    /// Internal effects on the summed input, before the strip.
    pub inserts: Vec<InsertDef>,
}

impl BusDef {
    /// A bus at unity gain, centred, unmuted, with an empty chain.
    pub fn new(id: u32, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            color: None,
            gain_db: 0.0,
            pan: 0.0,
            muted: false,
            soloed: false,
            inserts: Vec::new(),
        }
    }

    /// The linear multiplier this bus's fader asks for.
    pub fn gain(&self) -> f32 {
        db_to_gain(self.gain_db)
    }
}

/// One send tap, as the session holds it.
///
/// A tap is **additive**: it does not take the signal away from the source's
/// main output, it adds a copy of it to a bus. The source keeps its own path to
/// the master, and "wet only" is a pre-fader tap with the fader down.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SendDef {
    /// Stable across renumbering of either end. What the level and pre/post
    /// commands address, so a send survives a bus being deleted above it.
    pub id: u32,
    /// The strip the copy is taken from.
    pub source: StripId,
    /// The bus it is added into. The master is never a send destination --
    /// every strip already has an implicit main output there.
    pub dest: usize,
    /// Linear level, `0.0..=`[`MAX_SEND_LEVEL`]. `1.0` is unity.
    pub level: f32,
    /// Whether the copy is taken before the source's fader, pan and mute.
    pub pre: bool,
}

impl SendDef {
    /// A send at unity, post-fader -- what a freshly created one should be.
    pub const fn new(id: u32, source: StripId, dest: usize) -> Self {
        Self {
            id,
            source,
            dest,
            level: 1.0,
            pre: false,
        }
    }

    /// This send's level in decibels, for a UI that draws a dB scale.
    pub fn level_db(&self) -> f32 {
        gain_to_db(self.level)
    }
}

/// Why a track edit was refused.
///
/// Every variant is reachable from the webview, which can send any index it
/// likes, plus one that is a genuine engine limitation.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum TrackError {
    #[error("there is no track {0}")]
    NoSuchTrack(usize),
    #[error("a project may have at most {MAX_TRACKS} tracks")]
    TooManyTracks,
    #[error("a project must keep at least one track")]
    LastTrack,
    #[error("gain {0} dB is not a number between {SILENCE_DB} and {MAX_GAIN_DB}")]
    InvalidGain(f32),
    #[error("pan {0} is not a number between -1.0 and 1.0")]
    InvalidPan(f32),
}

/// Why a bus or send edit was refused.
///
/// Every variant is reachable from the webview, which can send any index it
/// likes, plus two that are genuine engine limitations and one -- [`Cycle`] --
/// that is the routing rule this whole type exists for.
///
/// [`Cycle`]: Self::Cycle
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RouteError {
    #[error("there is no bus {0}")]
    NoSuchBus(usize),
    #[error("there is no track {0}")]
    NoSuchTrack(usize),
    #[error("there is no send {0}")]
    NoSuchSend(u32),
    #[error("a project may have at most {MAX_BUSES} buses")]
    TooManyBuses,
    /// The field is `from` and not `source` because `thiserror` reads a field
    /// called `source` as the error's *cause* and demands it be an error.
    #[error("{from} already sends to bus {dest}")]
    DuplicateSend { from: StripId, dest: usize },
    #[error("a bus cannot send to itself")]
    SelfSend,
    /// The edit would make a bus feed itself, directly or through others.
    ///
    /// Refused at **edit time**, so the audio thread never has to know that a
    /// cycle is possible. The message names the whole loop, because "that would
    /// be a cycle" is not something a user can act on and "bus 1 -> bus 3 -> bus
    /// 1" is.
    #[error("that would feed a bus into itself: {0}")]
    Cycle(String),
    #[error("send level {0} is not a number between 0.0 and {MAX_SEND_LEVEL}")]
    InvalidLevel(f32),
    #[error("a bus must have a name")]
    BlankName,
}

/// The session's tracks and master fader.
///
/// # Track index is note channel
/// A track's **index** in [`tracks`](Self::tracks) is the note channel it
/// answers to: a sequencer row on channel 1 plays track 1, and the keyboard
/// piano sends on the selected track's index. That is why `id` exists as a
/// separate field -- removing a track renumbers everything after it, and a UI
/// list keyed on position would animate wrongly. It also means removing a track
/// re-points any sequencer row addressed at a later one, which is a known
/// sharp edge of this step rather than a design.
#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    pub tracks: Vec<TrackDef>,
    /// The buses, in strip order. A bus's **index** is what a
    /// [`SendDef::dest`] names and what the audio thread's parameter block is
    /// keyed by, exactly as a track's index is.
    pub buses: Vec<BusDef>,
    /// Every send tap in the session, in creation order.
    ///
    /// One flat list rather than a list per strip: a send is a *relationship*,
    /// the UI draws it from both ends ("sends from this strip" and "what feeds
    /// this bus"), and a copy kept on each end is a copy that can disagree.
    pub sends: Vec<SendDef>,
    /// Master fader, in dB.
    pub master_gain_db: f32,
    /// Internal effects on the master bus, **ahead of** the hosted insert. See
    /// [`crate::effects`] on why that order.
    pub master_inserts: Vec<InsertDef>,
    next_id: u32,
    next_bus_id: u32,
    /// Next insert identity. Session-wide rather than per chain, so an insert
    /// moved from one strip to another could one day keep its id.
    next_insert_id: u32,
    next_send_id: u32,
}

impl Default for Project {
    /// The starter rack, with the poly synth standing in for the piano.
    ///
    /// [`Project::starter`] is the real constructor; this is the one the tests
    /// and the offline paths use, where there is no sampler registry to ask
    /// whether the piano is installed.
    fn default() -> Self {
        Self::starter(Source::PolySynth)
    }
}

impl Project {
    /// The rack a fresh session opens with: four drums and a melodic channel.
    ///
    /// One instrument per channel, which is the whole reason the drum kit was
    /// split ([`SingleDrum`](mydaw_plugin_instruments::SingleDrum)): a channel
    /// has one fader, one pan and one insert chain, and a kick that cannot be
    /// turned down independently of the hats is not a channel, it is four
    /// channels wearing a coat.
    ///
    /// Five rather than two because this is a rack now rather than a proof that
    /// two tracks sum, and because a drum pattern you can immediately click in
    /// is the point of the first screen. Still well short of
    /// [`MAX_TRACKS`], so there is room to add.
    ///
    /// `piano` is whatever the caller found for the melodic channel --
    /// [`default_piano`] picks the sampled piano when its assets are installed
    /// and the poly synth when they are not. The name on that channel follows
    /// the source, so a session with no samples says "Poly Synth" rather than
    /// promising a piano it does not have.
    pub fn starter(piano: Source) -> Self {
        let mut tracks: Vec<TrackDef> = SingleDrum::ALL
            .iter()
            .enumerate()
            .map(|(index, drum)| {
                TrackDef::new(index as u32, drum.display_name(), Source::Drum(*drum))
            })
            .collect();
        let id = tracks.len() as u32;
        tracks.push(TrackDef::new(id, piano.label(), piano));
        Self {
            tracks,
            next_id: id + 1,
            ..Self::empty()
        }
    }

    /// A project with no tracks at all. Renders silence.
    pub fn empty() -> Self {
        Self {
            tracks: Vec::new(),
            buses: Vec::new(),
            sends: Vec::new(),
            master_gain_db: 0.0,
            master_inserts: Vec::new(),
            next_id: 0,
            next_bus_id: 0,
            next_insert_id: 0,
            next_send_id: 0,
        }
    }

    /// A project read back from a file.
    ///
    /// Ids are restored rather than regenerated -- a track keeps its identity
    /// across a save and load, which is what lets a UI list keyed on it animate
    /// correctly rather than replacing every row. The next id continues past the
    /// highest one in the file, so a track added after a load cannot collide
    /// with one that was in it.
    pub fn restored(tracks: Vec<TrackDef>, master_gain_db: f32) -> Self {
        Self::restored_with(tracks, master_gain_db, Vec::new(), Vec::new(), Vec::new())
    }

    /// A whole project read back from a version 4 file.
    ///
    /// Every id -- track, bus, insert and send -- is restored rather than
    /// regenerated, and each counter continues past the highest one in the file,
    /// so anything added after a load cannot collide with what was in it.
    pub fn restored_with(
        tracks: Vec<TrackDef>,
        master_gain_db: f32,
        buses: Vec<BusDef>,
        sends: Vec<SendDef>,
        master_inserts: Vec<InsertDef>,
    ) -> Self {
        let past = |highest: Option<u32>| highest.map_or(0, |id| id.saturating_add(1));
        let next_id = past(tracks.iter().map(|track| track.id).max());
        let next_bus_id = past(buses.iter().map(|bus| bus.id).max());
        let next_insert_id = past(
            tracks
                .iter()
                .flat_map(|track| &track.inserts)
                .chain(buses.iter().flat_map(|bus| &bus.inserts))
                .chain(&master_inserts)
                .map(|insert| insert.id)
                .max(),
        );
        let next_send_id = past(sends.iter().map(|send| send.id).max());
        Self {
            tracks,
            buses,
            sends,
            master_gain_db,
            master_inserts,
            next_id,
            next_bus_id,
            next_insert_id,
            next_send_id,
        }
    }

    /// The id the next added track will get. Diagnostics and tests.
    pub const fn next_id(&self) -> u32 {
        self.next_id
    }

    /// The id the next added bus will get.
    pub const fn next_bus_id(&self) -> u32 {
        self.next_bus_id
    }

    /// The id the next added insert will get.
    pub const fn next_insert_id(&self) -> u32 {
        self.next_insert_id
    }

    /// The id the next added send will get.
    pub const fn next_send_id(&self) -> u32 {
        self.next_send_id
    }

    // --------------------------------------------------------- insert chains

    /// The chain `target` names, or `None` for a strip that is not there.
    pub fn chain(&self, target: InsertTarget) -> Option<&Vec<InsertDef>> {
        match target {
            InsertTarget::Master => Some(&self.master_inserts),
            InsertTarget::Track(index) => self.tracks.get(index).map(|track| &track.inserts),
            InsertTarget::Bus(index) => self.buses.get(index).map(|bus| &bus.inserts),
        }
    }

    /// The chain `target` names, mutably.
    pub fn chain_mut(&mut self, target: InsertTarget) -> Option<&mut Vec<InsertDef>> {
        match target {
            InsertTarget::Master => Some(&mut self.master_inserts),
            InsertTarget::Track(index) => self.tracks.get_mut(index).map(|t| &mut t.inserts),
            InsertTarget::Bus(index) => self.buses.get_mut(index).map(|b| &mut b.inserts),
        }
    }

    /// The insert `target` and `index` name.
    pub fn insert(&self, target: InsertTarget, index: usize) -> Option<&InsertDef> {
        self.chain(target).and_then(|chain| chain.get(index))
    }

    /// Every chain in the project, in the order the graph builds them.
    pub fn chains(&self) -> impl Iterator<Item = (InsertTarget, &Vec<InsertDef>)> {
        self.tracks
            .iter()
            .enumerate()
            .map(|(index, track)| (InsertTarget::Track(index), &track.inserts))
            .chain(
                self.buses
                    .iter()
                    .enumerate()
                    .map(|(index, bus)| (InsertTarget::Bus(index), &bus.inserts)),
            )
            .chain(std::iter::once((
                InsertTarget::Master,
                &self.master_inserts,
            )))
    }

    /// Add an internal effect to a chain, returning where it landed.
    ///
    /// A `position` past the end appends, which is what dropping onto empty
    /// space means.
    ///
    /// # Errors
    /// [`InsertError::UnknownEffect`], [`InsertError::NoSuchTrack`],
    /// [`InsertError::NoSuchBus`] or [`InsertError::TooManyInserts`].
    pub fn add_insert(
        &mut self,
        target: InsertTarget,
        type_id: &str,
        position: Option<usize>,
        registry: &EffectRegistry,
    ) -> Result<usize, InsertError> {
        // Built before anything is borrowed or mutated, so an unknown effect
        // leaves the project exactly as it was.
        let def = InsertDef::new(self.next_insert_id, type_id, registry)?;
        let chain = self
            .chain_mut(target)
            .ok_or(InsertError::no_such_chain(target))?;
        if chain.len() >= MAX_INSERTS {
            return Err(InsertError::TooManyInserts);
        }
        let at = position.unwrap_or(chain.len()).min(chain.len());
        chain.insert(at, def);
        self.next_insert_id = self.next_insert_id.saturating_add(1);
        Ok(at)
    }

    /// Remove an insert, returning it.
    ///
    /// # Errors
    /// [`InsertError::NoSuchTrack`], [`InsertError::NoSuchBus`] or
    /// [`InsertError::NoSuchInsert`].
    pub fn remove_insert(
        &mut self,
        target: InsertTarget,
        index: usize,
    ) -> Result<InsertDef, InsertError> {
        let chain = self
            .chain_mut(target)
            .ok_or(InsertError::no_such_chain(target))?;
        if index >= chain.len() {
            return Err(InsertError::NoSuchInsert { target, index });
        }
        Ok(chain.remove(index))
    }

    /// Move an insert so that it ends up at `to`.
    ///
    /// # Errors
    /// [`InsertError::NoSuchTrack`], [`InsertError::NoSuchBus`] or
    /// [`InsertError::NoSuchInsert`].
    pub fn move_insert(
        &mut self,
        target: InsertTarget,
        from: usize,
        to: usize,
    ) -> Result<(), InsertError> {
        let chain = self
            .chain_mut(target)
            .ok_or(InsertError::no_such_chain(target))?;
        if from >= chain.len() {
            return Err(InsertError::NoSuchInsert {
                target,
                index: from,
            });
        }
        if to >= chain.len() {
            return Err(InsertError::NoSuchInsert { target, index: to });
        }
        let def = chain.remove(from);
        chain.insert(to, def);
        Ok(())
    }

    /// Whether any chain holds anything at all.
    pub fn has_inserts(&self) -> bool {
        self.chains().any(|(_, chain)| !chain.is_empty())
    }

    // ------------------------------------------------------------ buses

    /// Append a bus, returning its index.
    ///
    /// # Errors
    /// [`RouteError::TooManyBuses`] past [`MAX_BUSES`], or
    /// [`RouteError::BlankName`].
    pub fn add_bus(&mut self, name: Option<String>) -> Result<usize, RouteError> {
        if self.buses.len() >= MAX_BUSES {
            return Err(RouteError::TooManyBuses);
        }
        let id = self.next_bus_id;
        let name = match name {
            Some(name) if name.trim().is_empty() => return Err(RouteError::BlankName),
            Some(name) => name,
            // Numbered by position, which is what the user is looking at, not by
            // id -- "Bus 3" after two deletions should be the third strip.
            None => format!("Bus {}", self.buses.len() + 1),
        };
        self.next_bus_id = self.next_bus_id.saturating_add(1);
        self.buses.push(BusDef::new(id, name));
        Ok(self.buses.len() - 1)
    }

    /// Remove a bus, **and every send at either end of it**.
    ///
    /// Both ends, because a bus index is a *position*: a send left pointing at
    /// the bus that has moved into the gap would silently reroute somebody's
    /// reverb, and a send *from* the deleted bus has no source at all. Sends
    /// naming a later bus are renumbered down, for the same reason removing a
    /// track renumbers the ones after it.
    ///
    /// # Errors
    /// [`RouteError::NoSuchBus`].
    pub fn remove_bus(&mut self, bus: usize) -> Result<BusDef, RouteError> {
        if bus >= self.buses.len() {
            return Err(RouteError::NoSuchBus(bus));
        }
        self.sends
            .retain(|send| send.dest != bus && send.source != StripId::Bus(bus));
        for send in &mut self.sends {
            if send.dest > bus {
                send.dest -= 1;
            }
            if let StripId::Bus(index) = send.source {
                if index > bus {
                    send.source = StripId::Bus(index - 1);
                }
            }
        }
        Ok(self.buses.remove(bus))
    }

    /// Apply `edit` to one bus.
    ///
    /// # Errors
    /// [`RouteError::NoSuchBus`].
    pub fn edit_bus(
        &mut self,
        bus: usize,
        edit: impl FnOnce(&mut BusDef),
    ) -> Result<(), RouteError> {
        let def = self.buses.get_mut(bus).ok_or(RouteError::NoSuchBus(bus))?;
        edit(def);
        Ok(())
    }

    /// Whether any bus is soloed.
    pub fn any_bus_soloed(&self) -> bool {
        self.buses.iter().any(|bus| bus.soloed)
    }

    /// Whether bus `index` is silent right now, solo resolved.
    ///
    /// Buses and tracks share one solo scope: soloing a track silences every bus
    /// that is not itself soloed, which is what makes "solo the kick" audible
    /// through a drum bus only if the user soloed that too. The alternative --
    /// two independent solo systems -- produces a soloed track that is silent
    /// because its group is not, which nobody would understand.
    pub fn bus_effective_mute(&self, index: usize) -> bool {
        let Some(bus) = self.buses.get(index) else {
            return true;
        };
        if self.any_soloed() || self.any_bus_soloed() {
            !bus.soloed
        } else {
            bus.muted
        }
    }

    // ------------------------------------------------------------ sends

    /// Whether `source` names a strip this project has.
    fn has_strip(&self, source: StripId) -> bool {
        match source {
            StripId::Track(index) => index < self.tracks.len(),
            StripId::Bus(index) => index < self.buses.len(),
        }
    }

    /// The send with this id.
    pub fn send(&self, id: u32) -> Option<&SendDef> {
        self.sends.iter().find(|send| send.id == id)
    }

    /// Add a send tap, returning its id.
    ///
    /// Refuses anything the graph could not run: a strip that is not there, a
    /// second tap between the same two strips, a bus feeding itself, and -- the
    /// interesting one -- any tap that would close a loop of buses.
    ///
    /// # Errors
    /// [`RouteError::NoSuchTrack`], [`RouteError::NoSuchBus`],
    /// [`RouteError::DuplicateSend`], [`RouteError::SelfSend`],
    /// [`RouteError::Cycle`].
    pub fn add_send(&mut self, source: StripId, dest: usize) -> Result<u32, RouteError> {
        if dest >= self.buses.len() {
            return Err(RouteError::NoSuchBus(dest));
        }
        if !self.has_strip(source) {
            return Err(match source {
                StripId::Track(index) => RouteError::NoSuchTrack(index),
                StripId::Bus(index) => RouteError::NoSuchBus(index),
            });
        }
        if source == StripId::Bus(dest) {
            return Err(RouteError::SelfSend);
        }
        if self
            .sends
            .iter()
            .any(|send| send.source == source && send.dest == dest)
        {
            return Err(RouteError::DuplicateSend { from: source, dest });
        }
        if let StripId::Bus(from) = source {
            if let Some(loop_path) = self.cycle_through(from, dest) {
                return Err(RouteError::Cycle(loop_path));
            }
        }
        let id = self.next_send_id;
        self.next_send_id = self.next_send_id.saturating_add(1);
        self.sends.push(SendDef::new(id, source, dest));
        Ok(id)
    }

    /// Remove a send by id.
    ///
    /// # Errors
    /// [`RouteError::NoSuchSend`].
    pub fn remove_send(&mut self, id: u32) -> Result<SendDef, RouteError> {
        let at = self
            .sends
            .iter()
            .position(|send| send.id == id)
            .ok_or(RouteError::NoSuchSend(id))?;
        Ok(self.sends.remove(at))
    }

    /// Apply `edit` to one send.
    ///
    /// # Errors
    /// [`RouteError::NoSuchSend`].
    pub fn edit_send(
        &mut self,
        id: u32,
        edit: impl FnOnce(&mut SendDef),
    ) -> Result<(), RouteError> {
        let def = self
            .sends
            .iter_mut()
            .find(|send| send.id == id)
            .ok_or(RouteError::NoSuchSend(id))?;
        edit(def);
        Ok(())
    }

    /// The loop a tap `from -> dest` would close, written out, or `None`.
    ///
    /// A depth-first walk from `dest` along the bus-to-bus taps that already
    /// exist, looking for `from`. Naming the whole path is the point: "that
    /// would be a cycle" is not something a user can act on, and
    /// `"bus 1 -> bus 3 -> bus 1"` is.
    fn cycle_through(&self, from: usize, dest: usize) -> Option<String> {
        if from == dest {
            return Some(format!("{} -> {}", self.bus_name(from), self.bus_name(dest)));
        }
        // Iterative DFS with an explicit path, so a deep chain cannot blow the
        // stack and so the path is available when the target is found.
        let mut stack: Vec<Vec<usize>> = vec![vec![dest]];
        let mut seen = vec![false; self.buses.len()];
        while let Some(path) = stack.pop() {
            let tip = *path.last().expect("a path is never empty");
            if tip == from {
                let mut names: Vec<String> =
                    std::iter::once(from).chain(path).map(|bus| self.bus_name(bus)).collect();
                names.dedup();
                return Some(names.join(" -> "));
            }
            if seen.get(tip).copied().unwrap_or(true) {
                continue;
            }
            seen[tip] = true;
            for send in &self.sends {
                if send.source == StripId::Bus(tip) {
                    let mut next = path.clone();
                    next.push(send.dest);
                    stack.push(next);
                }
            }
        }
        None
    }

    /// A bus's name for an error message, or its index when it has none.
    fn bus_name(&self, bus: usize) -> String {
        self.buses
            .get(bus)
            .map_or_else(|| format!("bus {bus}"), |def| def.name.clone())
    }

    /// Drop and renumber every send after track `index` was removed.
    ///
    /// A track index is a position, so the same argument as [`remove_bus`] holds
    /// at the other end of the matrix.
    ///
    /// [`remove_bus`]: Self::remove_bus
    fn retarget_sends_after_removing_track(&mut self, index: usize) {
        self.sends.retain(|send| send.source != StripId::Track(index));
        for send in &mut self.sends {
            if let StripId::Track(track) = send.source {
                if track > index {
                    send.source = StripId::Track(track - 1);
                }
            }
        }
    }

    /// Tracks in this project.
    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    /// Whether any track is soloed.
    pub fn any_soloed(&self) -> bool {
        self.tracks.iter().any(|track| track.soloed)
    }

    /// Whether `index` is silent right now, solo included.
    ///
    /// This is the whole of the solo rule: with nothing soloed a track is
    /// audible unless it is muted; with anything soloed a track is audible only
    /// if it is itself soloed. A soloed *and* muted track stays audible, which
    /// is what every DAW does -- solo is an override, not a vote.
    pub fn effective_mute(&self, index: usize) -> bool {
        let Some(track) = self.tracks.get(index) else {
            return true;
        };
        if self.any_soloed() {
            !track.soloed
        } else {
            track.muted
        }
    }

    /// Append a track named after its source, returning its index.
    ///
    /// # Errors
    /// [`TrackError::TooManyTracks`] past [`MAX_TRACKS`].
    pub fn add(&mut self, source: Source, name: Option<String>) -> Result<usize, TrackError> {
        if self.tracks.len() >= MAX_TRACKS {
            return Err(TrackError::TooManyTracks);
        }
        let id = self.next_id;
        self.next_id += 1;
        let name = name.unwrap_or_else(|| source.label());
        self.tracks.push(TrackDef::new(id, name, source));
        Ok(self.tracks.len() - 1)
    }

    /// Remove track `index`, renumbering the ones after it.
    ///
    /// # Errors
    /// [`TrackError::NoSuchTrack`], or [`TrackError::LastTrack`] -- a project
    /// with no tracks has no keyboard target and no meters, and "delete
    /// everything" is not a mixer gesture.
    pub fn remove(&mut self, index: usize) -> Result<TrackDef, TrackError> {
        if index >= self.tracks.len() {
            return Err(TrackError::NoSuchTrack(index));
        }
        if self.tracks.len() == 1 {
            return Err(TrackError::LastTrack);
        }
        self.retarget_sends_after_removing_track(index);
        Ok(self.tracks.remove(index))
    }

    /// Whether any track is headed by a hosted CLAP instrument.
    pub fn has_clap_source(&self) -> bool {
        self.tracks.iter().any(|track| track.source.is_clap())
    }

    /// Whether any track is headed by a hosted VST3 instrument.
    pub fn has_vst3_source(&self) -> bool {
        self.tracks.iter().any(|track| track.source.is_vst3())
    }

    /// Whether any track is headed by a hosted plugin of either format.
    ///
    /// The question a *graph rebuild* asks: either kind needs its own host's
    /// main thread to re-attach it, and neither can be built from a factory.
    pub fn has_hosted_source(&self) -> bool {
        self.tracks.iter().any(|track| track.source.is_hosted())
    }

    /// Publish every track's gain, pan and effective mute, plus the master
    /// fader, into the block the audio thread reads.
    ///
    /// Idempotent and cheap (three relaxed stores per track), so callers push
    /// the whole project rather than tracking which field changed.
    pub fn publish(&self, params: &MixerParams) {
        for index in 0..MAX_TRACKS {
            match self.tracks.get(index) {
                Some(track) => {
                    params.set_track_gain(index, track.gain());
                    params.set_track_pan(index, track.pan);
                    params.set_track_muted(index, self.effective_mute(index));
                }
                // A slot with no track behind it is silent, so a stale graph
                // that still has one cannot make a sound.
                None => params.set_track_muted(index, true),
            }
        }
        for index in 0..MAX_BUSES {
            match self.buses.get(index) {
                Some(bus) => {
                    params.set_bus_gain(index, bus.gain());
                    params.set_bus_pan(index, bus.pan);
                    params.set_bus_muted(index, self.bus_effective_mute(index));
                }
                None => params.set_bus_muted(index, true),
            }
        }
        // The whole send matrix, not only the taps that exist: a cell a tap no
        // longer points at must not keep a level that a *later* tap between the
        // same two strips would silently inherit.
        for source in (0..MAX_TRACKS)
            .map(StripId::Track)
            .chain((0..MAX_BUSES).map(StripId::Bus))
        {
            for dest in 0..MAX_BUSES {
                match self
                    .sends
                    .iter()
                    .find(|send| send.source == source && send.dest == dest)
                {
                    Some(send) => {
                        params.set_send_level(source, dest, send.level);
                        params.set_send_pre(source, dest, send.pre);
                    }
                    None => {
                        params.set_send_level(source, dest, 1.0);
                        params.set_send_pre(source, dest, false);
                    }
                }
            }
        }
        params.set_master_gain(db_to_gain(self.master_gain_db));
    }
}

/// Validate a send level the webview sent.
///
/// # Errors
/// [`RouteError::InvalidLevel`] -- rejected rather than clamped, so a UI bug is
/// a status bar sentence instead of a silently different mix.
pub fn check_send_level(level: f32) -> Result<f32, RouteError> {
    if !level.is_finite() || !(0.0..=MAX_SEND_LEVEL).contains(&level) {
        return Err(RouteError::InvalidLevel(level));
    }
    Ok(level)
}

/// Validate a fader value the webview sent.
///
/// # Errors
/// [`TrackError::InvalidGain`] for NaN, infinity or anything outside the
/// fader's own scale -- rejected rather than clamped, so a UI bug is a status
/// bar sentence instead of a silently different mix.
pub fn check_gain_db(db: f32) -> Result<f32, TrackError> {
    if !db.is_finite() || !(SILENCE_DB..=MAX_GAIN_DB).contains(&db) {
        return Err(TrackError::InvalidGain(db));
    }
    Ok(db)
}

/// Validate a pan value the webview sent.
///
/// # Errors
/// [`TrackError::InvalidPan`].
pub fn check_pan(pan: f32) -> Result<f32, TrackError> {
    if !pan.is_finite() || !(-1.0..=1.0).contains(&pan) {
        return Err(TrackError::InvalidPan(pan));
    }
    Ok(pan)
}

// --------------------------------------------------------------- missing plugins

/// Which of the project's plugins could not be loaded, and why.
///
/// # Why a project can be missing a plugin, and why that must not be fatal
/// A project file records a plugin by **path and id** (see
/// [`crate::project_file`]). Both can stop resolving without anything being
/// wrong with the project: the user opened it on their laptop, uninstalled the
/// plugin, or the vendor's updater moved the bundle. A DAW that refused the
/// whole session over one of those would be refusing to open a user's work
/// because of somebody else's software.
///
/// So a load leaves such a track **silent, with its reference intact** and
/// records the reason here. The mixer draws a warning on that strip, and a save
/// writes the reference back out unchanged -- which is what makes the project
/// still open correctly on the machine that does have the plugin.
///
/// Published by the main thread after a load (it is the only thread that can
/// discover the absence, because it is the only one that can try to create an
/// instance) and read from command threads, exactly like
/// [`ClapBoard`](crate::clap_bridge::ClapBoard).
#[derive(Debug, Default)]
pub struct MissingPlugins {
    /// One entry per track index; `None` where the track is fine. Republished
    /// whole, so a stale entry cannot outlive the track it described.
    tracks: parking_lot::Mutex<Vec<Option<MissingPluginDto>>>,
    master: parking_lot::Mutex<Option<MissingPluginDto>>,
}

impl MissingPlugins {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the whole picture. Called by the main thread after every load.
    pub fn publish(&self, tracks: Vec<Option<MissingPluginDto>>, master: Option<MissingPluginDto>) {
        *self.tracks.lock() = tracks;
        *self.master.lock() = master;
    }

    /// Forget every warning. Called before a load, and by "new project".
    pub fn clear(&self) {
        self.publish(Vec::new(), None);
    }

    /// The warning on `track`, if any.
    pub fn track(&self, track: usize) -> Option<MissingPluginDto> {
        self.tracks.lock().get(track).cloned().flatten()
    }

    /// The warning on the master insert, if any.
    pub fn master(&self) -> Option<MissingPluginDto> {
        self.master.lock().clone()
    }

    /// How many plugins the current project is missing.
    pub fn len(&self) -> usize {
        self.tracks
            .lock()
            .iter()
            .filter(|slot| slot.is_some())
            .count()
            + usize::from(self.master.lock().is_some())
    }

    /// Whether everything the project asked for is loaded.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One plugin a project asked for and did not get.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MissingPluginDto {
    /// What the project called it -- the display name from the scan that placed
    /// it, falling back to the plugin id. Shown on the strip, because a user
    /// recognises "Diva" and does not recognise a path.
    pub name: String,
    /// `clap` or `vst3`, so the warning can say which format is absent.
    pub format: String,
    /// Where the project expected to find it. The tooltip; often the whole
    /// explanation ("that drive is not plugged in").
    pub path: String,
    /// Why it could not be loaded, in the loader's own words.
    pub reason: String,
}

// -------------------------------------------------------------------- the wire

/// One mixer strip, as the webview draws it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackDto {
    /// Position, which is also this track's note channel.
    pub index: usize,
    /// Stable identity across renumbering. Key a UI list on this.
    pub id: u32,
    pub name: String,
    /// Source id, as [`get_sources`](crate::commands::get_sources) reports them.
    pub source_id: String,
    pub source_label: String,
    pub gain_db: f32,
    pub pan: f32,
    /// The button state, not the outcome. See [`silenced`](Self::silenced).
    pub muted: bool,
    pub soloed: bool,
    /// Whether this track is actually silent right now, solo resolved.
    ///
    /// Sent so the mixer can grey a track that another track's solo has
    /// silenced, which is the difference between a mute button and a mute.
    pub silenced: bool,
    /// Whether the keyboard piano and the source selector address this track.
    pub selected: bool,
    /// The hosted CLAP plugin heading this track, when its source is one.
    ///
    /// Carries what a strip needs to draw an editor button without asking a
    /// second question: whether the plugin *has* an editor, and whether it is
    /// open right now. Published by the main thread (see
    /// [`ClapBoard`](crate::clap_bridge::ClapBoard)), so `get_tracks` -- which
    /// runs after every fader move -- reads it with no round trip.
    pub clap: Option<ClapSlotDto>,
    /// The hosted VST3 instrument heading this track, when its source is one.
    ///
    /// Mutually exclusive with [`clap`](Self::clap): a track has one source. Two
    /// fields rather than one tagged union because the two formats identify a
    /// plugin differently (a CLAP id is a string, a VST3 class id is 32 hex
    /// digits) and collapsing them would mean calling one by the other's name.
    pub vst3: Option<Vst3SlotDto>,
    /// Set when the project asked for a plugin on this track and it could not be
    /// loaded. The strip draws a warning badge and renders silence; the
    /// reference is still in the project and will be saved back out. See
    /// [`MissingPlugins`].
    pub missing: Option<MissingPluginDto>,
    /// Internal effects on this track, in signal order.
    pub inserts: Vec<InsertDto>,
}

/// One bus strip, as the webview draws it.
///
/// Deliberately the same field names as [`TrackDto`] where they mean the same
/// thing, so a mixer can draw a strip from either with one component.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BusDto {
    /// Position, which is what a send's `dest` names.
    pub index: usize,
    /// Stable identity across renumbering. Key a UI list on this.
    pub id: u32,
    pub name: String,
    /// Absent when the engine has none -- see [`BusDef::color`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    pub gain_db: f32,
    pub pan: f32,
    /// The button state, not the outcome. See [`silenced`](Self::silenced).
    pub muted: bool,
    pub soloed: bool,
    /// Whether this bus is actually silent right now, solo resolved across
    /// tracks *and* buses.
    pub silenced: bool,
    pub inserts: Vec<InsertDto>,
}

/// One send tap, as the webview draws it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendDto {
    /// Stable identity. **Every send command addresses this**, not a pair of
    /// indices, so a send survives a bus being deleted above it.
    pub id: u32,
    /// `"track"` or `"bus"` -- the same vocabulary an insert target uses.
    pub source_kind: String,
    pub source_index: usize,
    /// Index of the destination bus.
    pub dest: usize,
    /// Linear level, `0.0..=`[`MAX_SEND_LEVEL`]. `1.0` is unity.
    pub level: f32,
    /// The same level in decibels, so a knob can lay its range out
    /// logarithmically without the UI keeping its own copy of the conversion.
    /// [`SILENCE_DB`] at zero.
    pub level_db: f32,
    /// Whether the tap is taken before the source's fader, pan and mute.
    pub pre: bool,
}

impl SendDto {
    fn new(send: &SendDef) -> Self {
        Self {
            id: send.id,
            source_kind: if send.source.is_bus() { "bus" } else { "track" }.to_owned(),
            source_index: send.source.index(),
            dest: send.dest,
            level: send.level,
            level_db: send.level_db(),
            pre: send.pre,
        }
    }
}

/// The master strip.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MasterDto {
    pub gain_db: f32,
    /// The CLAP audio effect on the master insert, or `null` when there is none.
    pub clap: Option<ClapSlotDto>,
    /// The VST3 audio effect on the master insert.
    ///
    /// The insert holds **one** plugin, so at most one of `clap` and `vst3` is
    /// ever set. Whichever host places one evicts the other's -- see
    /// `crate::vst3_host`.
    pub vst3: Option<Vst3SlotDto>,
    /// The master effect the project asked for and did not get. See
    /// [`MissingPlugins`].
    pub missing: Option<MissingPluginDto>,
    /// Internal effects on the master bus, **ahead of** the hosted insert.
    pub inserts: Vec<InsertDto>,
}

/// Everything the mixer panel needs in one message.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MixerStateDto {
    pub tracks: Vec<TrackDto>,
    /// The buses, in strip order. Empty in a project that has none.
    pub buses: Vec<BusDto>,
    /// Every send tap, from both kinds of source. The UI draws "sends from this
    /// strip" and "what feeds this bus" by filtering this one list, which is why
    /// it is not split per strip -- two copies could disagree.
    pub sends: Vec<SendDto>,
    pub master: MasterDto,
    /// Index of the track the keyboard and the source selector address.
    pub selected: usize,
    /// The engine's fixed track limit, so the UI can grey out "add".
    pub max_tracks: usize,
    /// The engine's fixed bus limit, likewise.
    pub max_buses: usize,
    /// The engine's fixed per-chain insert limit.
    pub max_inserts: usize,
    /// Fader bounds, so the UI does not keep its own copy of the scale.
    pub min_gain_db: f32,
    pub max_gain_db: f32,
    /// Send-knob bounds: linear `0.0..=max_send_level`, which is
    /// `min_gain_db..=max_send_db` on a dB scale.
    pub max_send_level: f32,
    pub max_send_db: f32,
}

/// Render `project` for the webview.
///
/// `board` is the main thread's published record of which hosted plugin sits
/// where; it is read here rather than derived from `Source::Clap` because only
/// the main thread knows whether a plugin has an editor and whether that editor
/// is open, and the mixer draws both.
pub fn mixer_state_dto(
    project: &Project,
    selected: usize,
    clap: &ClapBoard,
    vst3: &Vst3Board,
    missing: &MissingPlugins,
    effects: &EffectRegistry,
) -> MixerStateDto {
    MixerStateDto {
        tracks: project
            .tracks
            .iter()
            .enumerate()
            .map(|(index, track)| TrackDto {
                index,
                id: track.id,
                name: track.name.clone(),
                source_id: track.source.id(),
                source_label: track.source.label(),
                gain_db: track.gain_db,
                pan: track.pan,
                muted: track.muted,
                soloed: track.soloed,
                silenced: project.effective_mute(index),
                selected: index == selected,
                clap: clap.track(index),
                vst3: vst3.track(index),
                missing: missing.track(index),
                inserts: chain_dto(&track.inserts, effects),
            })
            .collect(),
        buses: project
            .buses
            .iter()
            .enumerate()
            .map(|(index, bus)| BusDto {
                index,
                id: bus.id,
                name: bus.name.clone(),
                color: bus.color.clone(),
                gain_db: bus.gain_db,
                pan: bus.pan,
                muted: bus.muted,
                soloed: bus.soloed,
                silenced: project.bus_effective_mute(index),
                inserts: chain_dto(&bus.inserts, effects),
            })
            .collect(),
        sends: project.sends.iter().map(SendDto::new).collect(),
        master: MasterDto {
            gain_db: project.master_gain_db,
            clap: clap.master(),
            vst3: vst3.master(),
            missing: missing.master(),
            inserts: chain_dto(&project.master_inserts, effects),
        },
        selected,
        max_tracks: MAX_TRACKS,
        max_buses: MAX_BUSES,
        max_inserts: MAX_INSERTS,
        min_gain_db: SILENCE_DB,
        max_gain_db: MAX_GAIN_DB,
        max_send_level: MAX_SEND_LEVEL,
        max_send_db: max_send_db(),
    }
}

/// The dB a linear multiplier corresponds to. Re-exported for the UI's readouts
/// and for tests that measure gain rather than set it.
pub fn linear_to_db(gain: f32) -> f32 {
    gain_to_db(gain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::ClapSourceRef;

    fn clap_ref() -> ClapSourceRef {
        ClapSourceRef::new(r"C:\plugins\thing.clap", "com.acme.thing").named("Acme Thing")
    }

    #[test]
    fn a_fresh_project_is_four_drum_channels_and_a_melodic_one() {
        let project = Project::default();
        assert_eq!(project.len(), 5);
        assert_eq!(
            project
                .tracks
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Kick", "Snare", "Hat Closed", "Hat Open", "Poly Synth"],
            "one instrument per channel, named after the instrument"
        );
        assert_eq!(
            project
                .tracks
                .iter()
                .map(|t| t.source.clone())
                .collect::<Vec<_>>(),
            vec![
                Source::Drum(SingleDrum::Kick),
                Source::Drum(SingleDrum::Snare),
                Source::Drum(SingleDrum::ClosedHat),
                Source::Drum(SingleDrum::OpenHat),
                Source::PolySynth,
            ]
        );
        assert!(project
            .tracks
            .iter()
            .all(|t| t.gain_db == 0.0 && t.pan == 0.0 && !t.muted && !t.soloed));
        assert_eq!(project.master_gain_db, 0.0);
        // Ids are dense and start at zero, and the next one continues past them.
        assert_eq!(
            project.tracks.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4]
        );
        assert_eq!(project.next_id(), 5);
        assert!(
            project.len() < MAX_TRACKS,
            "a starter rack must leave room to add to"
        );
    }

    /// The kit is not gone, only unused: a project saved before the split names
    /// it, and that project has to open.
    #[test]
    fn the_starter_rack_does_not_use_the_legacy_kit() {
        assert!(Project::default()
            .tracks
            .iter()
            .all(|t| t.source != Source::DrumKit));
    }

    #[test]
    fn the_melodic_channel_is_the_piano_when_its_samples_are_installed() {
        // With no registry content there is nothing to promise, so the honest
        // answer is the synth -- and the channel is named after it.
        let empty = SamplerRegistry::detached();
        assert_eq!(default_piano(&empty), Source::PolySynth);
        assert_eq!(
            Project::starter(default_piano(&empty)).tracks[4].name,
            "Poly Synth"
        );

        // And when there is one, the channel takes its name too.
        let piano = Source::Sampler(SamplerSourceRef::new("salamander-piano").named("Piano"));
        let project = Project::starter(piano.clone());
        assert_eq!(project.tracks[4].source, piano);
        assert_eq!(project.tracks[4].name, "Piano");
    }

    #[test]
    fn a_fader_in_decibels_becomes_the_multiplier_the_strip_wants() {
        let mut track = TrackDef::new(0, "T", Source::PolySynth);
        assert_eq!(track.gain(), 1.0);
        track.gain_db = -6.0206;
        assert!((track.gain() - 0.5).abs() < 1e-4);
        track.gain_db = SILENCE_DB;
        assert_eq!(track.gain(), 0.0, "the bottom of the fader is silence");
        assert!((linear_to_db(1.0)).abs() < 1e-6);
    }

    #[test]
    fn solo_silences_everything_that_is_not_soloed_and_un_solo_restores() {
        let mut project = Project::default();
        project.add(Source::TestTone, None).unwrap();
        let count = project.len();
        assert!(!project.any_soloed());
        assert!((0..count).all(|i| !project.effective_mute(i)));

        project.tracks[1].soloed = true;
        assert!(project.any_soloed());
        assert!(project.effective_mute(0));
        assert!(!project.effective_mute(1));
        assert!(project.effective_mute(2));

        // A second solo adds to the set rather than replacing it.
        project.tracks[2].soloed = true;
        assert!(!project.effective_mute(2));
        assert!(project.effective_mute(0));

        project.tracks[1].soloed = false;
        project.tracks[2].soloed = false;
        assert!((0..count).all(|i| !project.effective_mute(i)), "restored");
    }

    #[test]
    fn a_soloed_track_is_audible_even_if_it_is_also_muted() {
        // Solo is an override, not a vote: every DAW does this, and the
        // alternative is a solo button that sometimes does nothing.
        let mut project = Project::default();
        project.tracks[0].muted = true;
        project.tracks[0].soloed = true;
        assert!(!project.effective_mute(0));
        assert!(project.effective_mute(1));
    }

    #[test]
    fn a_mute_only_matters_while_nothing_is_soloed() {
        let mut project = Project::default();
        project.tracks[0].muted = true;
        assert!(project.effective_mute(0));
        assert!(!project.effective_mute(1));

        project.tracks[1].soloed = true;
        assert!(project.effective_mute(0), "still silent, for a new reason");
        assert!(!project.effective_mute(1));
    }

    #[test]
    fn publishing_pushes_every_track_and_mutes_the_slots_that_have_none() {
        let mut project = Project::default();
        project.tracks[0].gain_db = -6.0206;
        project.tracks[1].pan = -1.0;
        project.tracks[1].muted = true;
        project.master_gain_db = -12.0;

        let params = MixerParams::new();
        project.publish(&params);

        assert!((params.track_gain(0) - 0.5).abs() < 1e-4);
        assert!(!params.track_muted(0));
        assert_eq!(params.track_pan(1), -1.0);
        assert!(params.track_muted(1));
        assert!((params.master_gain() - 0.251_188_6).abs() < 1e-5);

        // A slot with no track behind it must be silent, so a graph that still
        // has one cannot make a sound.
        assert!(params.track_muted(project.len()));
        assert!(params.track_muted(MAX_TRACKS - 1));
    }

    #[test]
    fn tracks_are_added_up_to_the_engines_limit_and_never_past_it() {
        let mut project = Project::default();
        while project.len() < MAX_TRACKS {
            project.add(Source::PolySynth, None).unwrap();
        }
        assert_eq!(
            project.add(Source::PolySynth, None),
            Err(TrackError::TooManyTracks)
        );
        assert_eq!(project.len(), MAX_TRACKS);
    }

    #[test]
    fn removing_a_track_renumbers_the_ones_after_it_but_keeps_their_ids() {
        let mut project = Project::default();
        project
            .add(Source::TestTone, Some("Tone".to_owned()))
            .unwrap();
        let ids: Vec<u32> = project.tracks.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![0, 1, 2, 3, 4, 5]);

        let removed = project.remove(0).unwrap();
        assert_eq!(removed.name, "Kick");
        assert_eq!(
            project.tracks.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5],
            "ids are stable; only the index -- and therefore the note channel -- moved"
        );

        assert_eq!(project.remove(9), Err(TrackError::NoSuchTrack(9)));
        while project.len() > 1 {
            project.remove(0).unwrap();
        }
        assert_eq!(project.remove(0), Err(TrackError::LastTrack));
    }

    #[test]
    fn a_new_track_is_named_after_its_source_unless_told_otherwise() {
        let mut project = Project::empty();
        let index = project.add(Source::DrumKit, None).unwrap();
        assert_eq!(project.tracks[index].name, "Drum Kit");
        let index = project
            .add(Source::TestTone, Some("Ref".to_owned()))
            .unwrap();
        assert_eq!(project.tracks[index].name, "Ref");
        assert!(!project.has_clap_source());
        project.tracks[0].source = Source::Clap(clap_ref());
        assert!(project.has_clap_source());
    }

    #[test]
    fn a_clap_headed_track_is_named_after_the_plugin_and_records_where_it_came_from() {
        // What a project file will serialise: the name a user sees, and the
        // bundle plus id that let a reload find the same plugin.
        let mut project = Project::empty();
        let index = project.add(Source::Clap(clap_ref()), None).unwrap();
        assert_eq!(project.tracks[index].name, "Acme Thing");
        let reference = project.tracks[index]
            .source
            .clap_ref()
            .expect("a CLAP source");
        assert_eq!(reference.plugin_id, "com.acme.thing");
        assert!(reference.bundle_path.ends_with("thing.clap"));
    }

    #[test]
    fn values_the_webview_should_never_have_sent_are_named_rather_than_clamped() {
        assert_eq!(check_gain_db(0.0), Ok(0.0));
        assert_eq!(check_gain_db(SILENCE_DB), Ok(SILENCE_DB));
        assert_eq!(check_gain_db(MAX_GAIN_DB), Ok(MAX_GAIN_DB));
        for bad in [f32::NAN, f32::INFINITY, -61.0, 7.0] {
            assert!(
                matches!(check_gain_db(bad), Err(TrackError::InvalidGain(_))),
                "{bad}"
            );
        }

        assert_eq!(check_pan(-1.0), Ok(-1.0));
        assert_eq!(check_pan(1.0), Ok(1.0));
        for bad in [f32::NAN, 1.5, -2.0] {
            assert!(
                matches!(check_pan(bad), Err(TrackError::InvalidPan(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn the_wire_shape_carries_what_the_mixer_panel_draws() {
        let mut project = Project::default();
        project.tracks[0].soloed = true;
        let dto = mixer_state_dto(
            &project,
            1,
            &ClapBoard::new(),
            &Vst3Board::new(),
            &MissingPlugins::new(),
            &EffectRegistry::new(),
        );

        assert_eq!(dto.tracks.len(), project.len());
        assert_eq!(dto.selected, 1);
        assert_eq!(dto.max_tracks, MAX_TRACKS);
        assert_eq!(dto.min_gain_db, SILENCE_DB);
        assert_eq!(dto.tracks[0].index, 0);
        assert_eq!(dto.tracks[0].source_id, "kick");
        assert!(dto.tracks[0].soloed && !dto.tracks[0].silenced);
        assert!(
            dto.tracks[1].silenced && !dto.tracks[1].muted,
            "silenced by somebody else's solo, with its own button still off"
        );
        assert!(dto.tracks[1].selected && !dto.tracks[0].selected);
        assert!(
            dto.tracks.iter().all(|track| track.clap.is_none()) && dto.master.clap.is_none(),
            "nothing is hosted in this project"
        );

        // Mirrors `MixerState` in ui/src/api/types.ts, key for key.
        let json: serde_json::Value = serde_json::to_value(&dto).unwrap();
        let mut keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "buses",
                "master",
                "maxBuses",
                "maxGainDb",
                "maxInserts",
                "maxSendDb",
                "maxSendLevel",
                "maxTracks",
                "minGainDb",
                "selected",
                "sends",
                "tracks"
            ]
        );
        let mut track_keys: Vec<&str> = json["tracks"][0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        track_keys.sort_unstable();
        assert_eq!(
            track_keys,
            [
                "clap",
                "gainDb",
                "id",
                "index",
                "inserts",
                "missing",
                "muted",
                "name",
                "pan",
                "selected",
                "silenced",
                "soloed",
                "sourceId",
                "sourceLabel",
                "vst3"
            ]
        );
        assert_eq!(json["master"]["gainDb"], 0.0);
        assert!(json["tracks"][0]["missing"].is_null());
        assert!(json["master"]["missing"].is_null());
    }

    #[test]
    fn a_plugin_the_project_could_not_load_reaches_the_strip_that_asked_for_it() {
        // What a mixer needs in order to draw a warning badge rather than an
        // inexplicably silent track.
        let mut project = Project::default();
        project.tracks[1].source = Source::Clap(clap_ref());

        let missing = MissingPlugins::new();
        assert!(missing.is_empty());
        let warning = MissingPluginDto {
            name: "Acme Thing".to_owned(),
            format: "clap".to_owned(),
            path: r"C:\plugins\thing.clap".to_owned(),
            reason: "there is no CLAP bundle at C:\\plugins\\thing.clap".to_owned(),
        };
        missing.publish(vec![None, Some(warning.clone())], None);
        assert_eq!(missing.len(), 1);
        assert!(missing.track(0).is_none());
        assert!(missing.track(99).is_none(), "past the end is not a warning");

        let dto = mixer_state_dto(
            &project,
            0,
            &ClapBoard::new(),
            &Vst3Board::new(),
            &missing,
            &EffectRegistry::new(),
        );
        assert!(dto.tracks[0].missing.is_none());
        assert_eq!(dto.tracks[1].missing.as_ref(), Some(&warning));
        assert_eq!(
            dto.tracks[1].source_id, "clap:com.acme.thing",
            "the reference stays in the project, so a save writes it back out"
        );

        let json: serde_json::Value = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["tracks"][1]["missing"]["name"], "Acme Thing");
        assert_eq!(json["tracks"][1]["missing"]["format"], "clap");
        assert!(json["tracks"][1]["missing"]["reason"].is_string());

        // A republish cannot leave a stale warning behind.
        missing.clear();
        assert!(missing.is_empty());
        assert!(
            mixer_state_dto(
            &project,
            0,
            &ClapBoard::new(),
            &Vst3Board::new(),
            &missing,
            &EffectRegistry::new(),
        ).tracks[1]
                .missing
                .is_none()
        );
    }

    #[test]
    fn a_restored_project_keeps_its_track_ids_and_numbers_new_ones_past_them() {
        // A UI list is keyed on the id, so a load that regenerated them would
        // look like every track being replaced.
        let restored = Project::restored(
            vec![
                TrackDef::new(7, "Kit", Source::DrumKit),
                TrackDef::new(3, "Lead", Source::PolySynth),
            ],
            -3.0,
        );
        assert_eq!(
            restored.tracks.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![7, 3]
        );
        assert_eq!(restored.master_gain_db, -3.0);
        assert_eq!(restored.next_id(), 8, "past the highest, not past the last");

        let mut restored = restored;
        let index = restored.add(Source::TestTone, None).unwrap();
        assert_eq!(restored.tracks[index].id, 8);
        assert_eq!(Project::restored(Vec::new(), 0.0).next_id(), 0);
    }

    #[test]
    fn a_hosted_plugin_reaches_the_strip_that_holds_it_and_nothing_else() {
        // The mixer draws an editor button from this, so a plugin appearing on
        // the wrong strip would put the button on the wrong strip too.
        let mut project = Project::default();
        project.tracks[1].source = Source::Clap(clap_ref());

        let board = ClapBoard::new();
        let slot = ClapSlotDto {
            instance: 3,
            plugin_id: "com.acme.thing".to_owned(),
            name: "Acme Thing".to_owned(),
            bundle_path: r"C:\plugins\thing.clap".to_owned(),
            has_editor: true,
            gui_open: true,
        };
        board.publish(vec![None, Some(slot.clone())], Some(slot));

        let dto = mixer_state_dto(
            &project,
            0,
            &board,
            &Vst3Board::new(),
            &MissingPlugins::new(),
            &EffectRegistry::new(),
        );
        assert!(dto.tracks[0].clap.is_none());
        let placed = dto.tracks[1].clap.as_ref().expect("track 1 is hosted");
        assert_eq!(placed.instance, 3);
        assert!(placed.has_editor && placed.gui_open);
        assert_eq!(dto.tracks[1].source_id, "clap:com.acme.thing");
        assert_eq!(dto.tracks[1].source_label, "Acme Thing");
        assert!(dto.master.clap.is_some());
    }
}
