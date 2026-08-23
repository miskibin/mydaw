//! Insert chains: what an internal effect is to the session, and how the
//! webview sees it.
//!
//! ```text
//!   per track:  SourceSlot -> [insert..] -> TransportGate -> [strip] --\
//!                                                                       sum
//!   master:            [insert..] -> InsertSlot -> [master fader] <----/
//!                      (internal)     (hosted)
//! ```
//!
//! # Two kinds of edit, and only one rebuilds the graph
//!
//! **A parameter move** is a store into an atomic. The
//! [`ParamBlock`](mydaw_rt_core::ParamBlock) is owned *here*, on the non-RT
//! side, and shared with whatever node is currently reading it -- exactly the
//! relationship [`MixerParams`](mydaw_rt_core::MixerParams) has with the strips.
//! Nothing is rebuilt, nothing is allocated, and the audio thread hears it on
//! its next block with a 10 ms ramp.
//!
//! **A structural edit** -- adding, removing or reordering an insert -- changes
//! the number of nodes in a chain, which is fixed once a graph is compiled. So
//! it rebuilds, through exactly the machinery a track add already uses. Because
//! the parameter blocks live here rather than in the nodes, **every other
//! insert's settings survive the rebuild**: adding a delay after an EQ does not
//! reset the EQ, in the same way that adding a track does not reset a fader.
//!
//! # Where the master's hosted plugin goes
//! The master bus carries the internal chain **first** and the one hosted
//! CLAP/VST3 insert **after** it. That order is the one a user gets by
//! inserting the internal effects (which is all this step offers on the master)
//! and then reaching for a plugin: the plugin lands at the end, where a
//! "finalise the bus" effect belongs. It is also the only order that keeps the
//! hosted slot's rebuild-free swap intact -- see [`crate::insert`] -- because the
//! slot stays a single unconditional node at a fixed position rather than
//! becoming a member of a chain whose length changes.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

use mydaw_plugin_effects::{EffectInfo, EffectRegistry, BYPASS_PARAM};
use mydaw_rt_core::{
    decode_key, encode_key, ParamBlock, ParamCurve, ParamDef, ReadoutDef, StripId, KEY_NONE,
};
use serde::Serialize;

/// Inserts one chain may hold.
///
/// Fixed, and small on purpose: a chain is rebuilt into a `Vec<Box<dyn RtNode>>`
/// per track, and eight internal effects on one track is already more DSP than a
/// track needs. The limit exists so that a webview that sends "add" in a loop
/// gets a sentence rather than an out-of-memory.
pub const MAX_INSERTS: usize = 8;

/// Which chain an edit is about.
///
/// The three places an internal effect can go, and the same three words the
/// routing commands use for a strip -- so `"track"`, `"bus"` and `"master"` mean
/// one thing across the whole command surface. It is a superset of
/// [`ClapTarget`](crate::clap_bridge::ClapTarget), which names the two places a
/// *hosted* plugin can go; a bus carries internal effects only, for now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertTarget {
    /// A track's chain, by index.
    Track(usize),
    /// A bus's chain, by index. Runs on the bus's summed input, before its
    /// strip.
    Bus(usize),
    /// The master bus's internal chain, ahead of the hosted insert.
    Master,
}

impl InsertTarget {
    /// Parse the pair the webview sends: a target name and an index.
    ///
    /// # Errors
    /// [`InsertError::UnknownTarget`] for a name that is not `track`, `bus` or
    /// `master`.
    pub fn parse(target: &str, index: usize) -> Result<Self, InsertError> {
        match target {
            "track" => Ok(Self::Track(index)),
            "bus" => Ok(Self::Bus(index)),
            "master" => Ok(Self::Master),
            other => Err(InsertError::UnknownTarget(other.to_owned())),
        }
    }

    /// The wire name this target parses from.
    pub const fn kind(self) -> &'static str {
        match self {
            Self::Track(_) => "track",
            Self::Bus(_) => "bus",
            Self::Master => "master",
        }
    }

    /// The index the wire carries alongside [`kind`](Self::kind). Zero for the
    /// master, which has only one of itself.
    pub const fn index(self) -> usize {
        match self {
            Self::Track(index) | Self::Bus(index) => index,
            Self::Master => 0,
        }
    }
}

impl std::fmt::Display for InsertTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Track(index) => write!(f, "track {index}"),
            Self::Bus(index) => write!(f, "bus {index}"),
            Self::Master => f.write_str("the master bus"),
        }
    }
}

/// A structural change to a chain. The kind of edit that rebuilds the graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertEdit {
    /// Insert `type_id` at `position`, or at the end when it is `None`.
    Add {
        target: InsertTarget,
        type_id: String,
        position: Option<usize>,
    },
    /// Remove the insert at `index`.
    Remove { target: InsertTarget, index: usize },
    /// Move the insert at `from` so that it ends up at `to`.
    Move {
        target: InsertTarget,
        from: usize,
        to: usize,
    },
}

impl InsertEdit {
    /// Which chain this edit is about.
    pub const fn target(&self) -> InsertTarget {
        match self {
            Self::Add { target, .. } | Self::Remove { target, .. } | Self::Move { target, .. } => {
                *target
            }
        }
    }
}

/// Why an insert edit was refused.
///
/// Every variant is reachable from the webview, which can send any index and any
/// string it likes.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum InsertError {
    #[error("'{0}' is not an internal effect")]
    UnknownEffect(String),
    #[error("'{0}' is not a place to put an effect; expected 'track', 'bus' or 'master'")]
    UnknownTarget(String),
    #[error("there is no track {0}")]
    NoSuchTrack(usize),
    #[error("there is no bus {0}")]
    NoSuchBus(usize),
    #[error("{target} has no insert {index}")]
    NoSuchInsert { target: InsertTarget, index: usize },
    #[error("a chain may hold at most {MAX_INSERTS} effects")]
    TooManyInserts,
    #[error("'{effect}' has no parameter called '{param}'")]
    NoSuchParam { effect: String, param: String },
    #[error("there is no {} {} to key off", if .0.is_bus() { "bus" } else { "track" }, .0.index())]
    NoSuchKeySource(StripId),
}

impl InsertError {
    /// The "that chain is not there" error for a target.
    pub const fn no_such_chain(target: InsertTarget) -> Self {
        match target {
            InsertTarget::Track(index) => Self::NoSuchTrack(index),
            InsertTarget::Bus(index) => Self::NoSuchBus(index),
            // Unreachable: the master's chain always exists.
            InsertTarget::Master => Self::NoSuchTrack(usize::MAX),
        }
    }
}

/// One insert, as the session holds it.
///
/// The `Arc<ParamBlock>` is the load-bearing part: the shell owns it, the node
/// reads it, and a rebuilt node is handed the *same* one. That is what makes a
/// structural edit leave every other effect's settings alone.
#[derive(Debug, Clone)]
pub struct InsertDef {
    /// Stable across reordering, so a UI list can key on it. Not the position.
    pub id: u32,
    /// Which internal effect. The `type_id` a project file records.
    pub type_id: String,
    /// What the audio thread reads.
    pub params: Arc<ParamBlock>,
    /// Hold-to-solo band index for the EQ, or -1. Not persisted.
    pub listen: Arc<AtomicI32>,
    /// Which strip feeds the compressor's detector, as a
    /// [`SidechainBus`](mydaw_rt_core::SidechainBus) slot, or
    /// [`KEY_NONE`]. **Persisted** -- unlike `listen`, this is a route the user
    /// drew and expects to find again, not a momentary gesture.
    ///
    /// An `Arc<AtomicI32>` and not a parameter for the same reason `listen` is
    /// not one: the value is a strip index, it has no range, no curve and no
    /// ramp, and a project file that recorded it as a float would be recording
    /// a pointer as a knob. The node reads it once per block, so repointing a
    /// sidechain is a store rather than a graph rebuild.
    pub sidechain: Arc<AtomicI32>,
}

impl InsertDef {
    /// An insert at `type_id`'s defaults.
    ///
    /// # Errors
    /// [`InsertError::UnknownEffect`] for a `type_id` the registry does not
    /// know -- reported rather than substituted, because an effect that silently
    /// became a different effect would be far worse than an error message.
    pub fn new(id: u32, type_id: &str, registry: &EffectRegistry) -> Result<Self, InsertError> {
        let params = registry
            .default_params(type_id)
            .ok_or_else(|| InsertError::UnknownEffect(type_id.to_owned()))?;
        Ok(Self {
            id,
            type_id: type_id.to_owned(),
            params,
            listen: Arc::new(AtomicI32::new(-1)),
            sidechain: Arc::new(AtomicI32::new(KEY_NONE)),
        })
    }

    /// Which strip this insert keys off, if any.
    ///
    /// Decoded rather than indexed, so a value this build has no slot for comes
    /// back as `None` instead of as some other strip.
    pub fn sidechain_source(&self) -> Option<StripId> {
        decode_key(self.sidechain.load(Ordering::Relaxed))
    }

    /// Point this insert's key at `source`, or unroute it with `None`.
    pub fn set_sidechain_source(&self, source: Option<StripId>) {
        self.sidechain.store(encode_key(source), Ordering::Relaxed);
    }

    /// Whether this insert's bypass switch is on.
    pub fn is_bypassed(&self) -> bool {
        self.params.get_by_id(BYPASS_PARAM).unwrap_or(0.0) >= 0.5
    }

    /// Every parameter as an `(id, value)` pair, for a DTO or a project file.
    pub fn values(&self) -> Vec<(&'static str, f32)> {
        self.params.snapshot()
    }
}

/// Two inserts are the same when they are the same effect, with the same
/// identity, at the same settings.
///
/// Hand-written because [`ParamBlock`] holds atomics and cannot derive it, and
/// because comparing the `Arc`s by pointer would make a loaded project unequal
/// to the one that saved it.
impl PartialEq for InsertDef {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.type_id == other.type_id
            && self.values() == other.values()
            && self.sidechain_source() == other.sidechain_source()
    }
}

// ------------------------------------------------------------------- the wire

/// One parameter's metadata, as the webview reads it.
///
/// This is the whole reason the parameter system exists as data rather than as
/// code: a UI that can render *this* can render a panel for any effect the
/// engine grows, including ones it has never heard of.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParamDefDto {
    pub id: String,
    pub name: String,
    /// `"dB"`, `"Hz"`, `"ms"`, `"%"`, `":1"`, or empty.
    pub unit: String,
    pub min: f32,
    pub max: f32,
    /// Named `default_value` because `default` is a reserved word in enough
    /// languages that a generated client would have to rename it anyway.
    pub default_value: f32,
    /// `"linear"`, `"log"` or `"db"`. How a knob should lay the range out --
    /// see [`ParamCurve`].
    pub curve: String,
    /// Quantisation in the parameter's own unit, or `null` for continuous.
    pub step: Option<f32>,
    /// Ramp length in milliseconds. `0` marks a switch: a UI should draw it as
    /// one, and must not animate it.
    pub smoothing_ms: f32,
}

impl From<&ParamDef> for ParamDefDto {
    fn from(def: &ParamDef) -> Self {
        Self {
            id: def.id.to_owned(),
            name: def.name.to_owned(),
            unit: def.unit.to_owned(),
            min: def.min,
            max: def.max,
            default_value: def.default,
            curve: match def.curve {
                ParamCurve::Linear => "linear",
                ParamCurve::Log => "log",
                ParamCurve::Db => "db",
            }
            .to_owned(),
            step: def.step,
            smoothing_ms: def.smoothing_ms,
        }
    }
}

/// One meter's metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadoutDefDto {
    pub id: String,
    pub name: String,
    pub unit: String,
}

impl From<&ReadoutDef> for ReadoutDefDto {
    fn from(def: &ReadoutDef) -> Self {
        Self {
            id: def.id.to_owned(),
            name: def.name.to_owned(),
            unit: def.unit.to_owned(),
        }
    }
}

/// One internal effect the session can insert, with everything a panel needs.
///
/// Static: the same for every instance, so the webview fetches this once at
/// startup and never again.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectInfoDto {
    pub type_id: String,
    pub name: String,
    pub description: String,
    pub params: Vec<ParamDefDto>,
    pub readouts: Vec<ReadoutDefDto>,
}

impl From<&EffectInfo> for EffectInfoDto {
    fn from(info: &EffectInfo) -> Self {
        Self {
            type_id: info.type_id.to_owned(),
            name: info.name.to_owned(),
            description: info.description.to_owned(),
            params: info.params.iter().map(ParamDefDto::from).collect(),
            readouts: info.readouts.iter().map(ReadoutDefDto::from).collect(),
        }
    }
}

/// One parameter or meter value.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParamValueDto {
    pub id: String,
    pub value: f32,
}

/// One insert on a chain, as the mixer draws it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InsertDto {
    /// Position in the chain, which is also the index every command takes.
    pub index: usize,
    /// Stable identity across reordering. Key a UI list on this.
    pub id: u32,
    pub type_id: String,
    /// Display name, from the registry, so a strip does not have to map ids.
    pub name: String,
    /// The bypass switch's position, lifted out because every strip draws it.
    pub bypassed: bool,
    /// Every parameter's current value, in declaration order.
    pub params: Vec<ParamValueDto>,
    /// Every meter's current value. Empty for the effects that publish none.
    pub readouts: Vec<ParamValueDto>,
    /// Which strip feeds this insert's detector, or `null`. Only the compressor
    /// reads it; every other effect reports `null` for ever.
    pub sidechain: Option<StripRefDto>,
}

/// A strip, as the webview names one. The same two words a send's source uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StripRefDto {
    /// `"track"` or `"bus"`.
    pub kind: &'static str,
    pub index: usize,
}

impl From<StripId> for StripRefDto {
    fn from(strip: StripId) -> Self {
        match strip {
            StripId::Track(index) => Self {
                kind: "track",
                index,
            },
            StripId::Bus(index) => Self { kind: "bus", index },
        }
    }
}

/// Render one insert for the webview.
///
/// `registry` supplies the display name; everything else is read live from the
/// shared block, so a DTO taken after a knob move shows the new value with no
/// round trip to the audio thread.
pub fn insert_dto(index: usize, def: &InsertDef, registry: &EffectRegistry) -> InsertDto {
    InsertDto {
        index,
        id: def.id,
        type_id: def.type_id.clone(),
        name: registry
            .info(&def.type_id)
            .map_or_else(|| def.type_id.clone(), |info| info.name.to_owned()),
        bypassed: def.is_bypassed(),
        params: def
            .params
            .snapshot()
            .into_iter()
            .map(|(id, value)| ParamValueDto {
                id: id.to_owned(),
                value,
            })
            .collect(),
        readouts: def
            .params
            .readout_snapshot()
            .into_iter()
            .map(|(id, value)| ParamValueDto {
                id: id.to_owned(),
                value,
            })
            .collect(),
        sidechain: def.sidechain_source().map(StripRefDto::from),
    }
}

/// Render a whole chain.
pub fn chain_dto(chain: &[InsertDef], registry: &EffectRegistry) -> Vec<InsertDto> {
    chain
        .iter()
        .enumerate()
        .map(|(index, def)| insert_dto(index, def, registry))
        .collect()
}

/// Just the meters of one chain, keyed by insert index.
///
/// The polled half of the parameter system, deliberately separate from
/// [`InsertDto`]: a gain-reduction bar wants to move at frame rate, and sending
/// twenty knob positions sixty times a second to animate one bar is exactly the
/// mistake the [`ReadoutDef`] split exists to prevent.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InsertReadoutsDto {
    /// Position in the chain the values belong to.
    pub index: usize,
    /// Stable identity, so a UI can tell a stale frame from a fresh one after a
    /// reorder.
    pub id: u32,
    pub readouts: Vec<ParamValueDto>,
}

/// One hop of analysis for the focused insert editor.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpectrumDto {
    pub bins_pre: Vec<f32>,
    pub bins_post: Vec<f32>,
    pub scope_l: Vec<f32>,
    pub scope_r: Vec<f32>,
    pub correlation: f32,
}

impl From<mydaw_ipc_bridge::SpectrumFrame> for SpectrumDto {
    fn from(frame: mydaw_ipc_bridge::SpectrumFrame) -> Self {
        Self {
            bins_pre: frame.bins_pre,
            bins_post: frame.bins_post,
            scope_l: frame.scope_l,
            scope_r: frame.scope_r,
            correlation: frame.correlation,
        }
    }
}

/// Every meter in one chain, in chain order.
///
/// Inserts that publish nothing are **included with an empty list** rather than
/// skipped, so the answer's length is the chain's length and a UI can index it
/// directly.
pub fn chain_readouts_dto(chain: &[InsertDef]) -> Vec<InsertReadoutsDto> {
    chain
        .iter()
        .enumerate()
        .map(|(index, def)| InsertReadoutsDto {
            index,
            id: def.id,
            readouts: def
                .params
                .readout_snapshot()
                .into_iter()
                .map(|(id, value)| ParamValueDto {
                    id: id.to_owned(),
                    value,
                })
                .collect(),
        })
        .collect()
}

/// Every internal effect the session can offer, in menu order.
pub fn catalogue(registry: &EffectRegistry) -> Vec<EffectInfoDto> {
    registry.effects().iter().map(EffectInfoDto::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> EffectRegistry {
        EffectRegistry::new()
    }

    #[test]
    fn a_fresh_insert_holds_the_effects_defaults_and_is_not_bypassed() {
        let registry = registry();
        let def = InsertDef::new(7, "effects.eq", &registry).expect("the EQ exists");
        assert_eq!(def.id, 7);
        assert!(def.params.is_default());
        assert!(!def.is_bypassed());
        assert!(def.values().iter().any(|(id, _)| *id == "bell1.freq"));
    }

    #[test]
    fn an_effect_this_build_does_not_have_is_refused_rather_than_substituted() {
        let registry = registry();
        assert_eq!(
            InsertDef::new(0, "effects.not-an-effect", &registry).unwrap_err(),
            InsertError::UnknownEffect("effects.not-an-effect".to_owned())
        );
    }

    #[test]
    fn two_inserts_are_equal_when_their_settings_are_equal() {
        let registry = registry();
        let a = InsertDef::new(1, "effects.utility", &registry).unwrap();
        let b = InsertDef::new(1, "effects.utility", &registry).unwrap();
        assert_eq!(a, b, "same effect, same id, same settings");

        b.params.set_by_id("gain", -6.0);
        assert_ne!(a, b, "a moved knob makes them different");

        let c = InsertDef::new(2, "effects.utility", &registry).unwrap();
        assert_ne!(a, c, "identity is part of it");
    }

    #[test]
    fn a_target_is_parsed_from_what_a_webview_can_send_and_round_trips() {
        assert_eq!(InsertTarget::parse("track", 3), Ok(InsertTarget::Track(3)));
        assert_eq!(InsertTarget::parse("bus", 2), Ok(InsertTarget::Bus(2)));
        assert_eq!(InsertTarget::parse("master", 0), Ok(InsertTarget::Master));
        assert_eq!(
            InsertTarget::parse("aux", 0),
            Err(InsertError::UnknownTarget("aux".to_owned()))
        );

        for target in [
            InsertTarget::Track(3),
            InsertTarget::Bus(2),
            InsertTarget::Master,
        ] {
            assert_eq!(
                InsertTarget::parse(target.kind(), target.index()),
                Ok(target),
                "{target} does not survive the wire"
            );
        }
        assert_eq!(InsertTarget::Bus(1).to_string(), "bus 1");
    }

    #[test]
    fn the_catalogue_carries_everything_a_generic_knob_panel_needs() {
        let registry = registry();
        let catalogue = catalogue(&registry);
        assert_eq!(catalogue.len(), 7);

        let eq = catalogue
            .iter()
            .find(|e| e.type_id == "effects.eq")
            .expect("the EQ");
        assert_eq!(eq.name, "EQ");
        assert!(eq.params.len() > 15);

        let freq = eq
            .params
            .iter()
            .find(|p| p.id == "bell1.freq")
            .expect("a bell frequency");
        assert_eq!(freq.unit, "Hz");
        assert_eq!(freq.curve, "log", "a frequency knob must be logarithmic");
        assert_eq!(freq.min, 20.0);
        assert_eq!(freq.max, 20_000.0);
        assert_eq!(freq.step, None);
        assert!(freq.smoothing_ms > 0.0);

        // A switch is legible as one: stepped, and explicitly unsmoothed so a
        // UI knows not to animate it.
        let slope = eq
            .params
            .iter()
            .find(|p| p.id == "hp.slope")
            .expect("the slope switch");
        assert_eq!(slope.step, Some(1.0));
        assert_eq!(slope.smoothing_ms, 0.0);
        assert_eq!(slope.min, 0.0);
        assert_eq!(slope.max, 1.0);
    }

    #[test]
    fn the_compressor_advertises_the_meters_a_panel_has_to_draw() {
        let registry = registry();
        let catalogue = catalogue(&registry);
        let comp = catalogue
            .iter()
            .find(|e| e.type_id == "effects.compressor")
            .expect("the compressor");
        let ids: Vec<&str> = comp.readouts.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["gr", "gr_max"]);
        assert!(comp.readouts.iter().all(|r| r.unit == "dB"));
    }

    #[test]
    fn a_dto_reads_the_live_block_rather_than_a_copy_taken_earlier() {
        let registry = registry();
        let def = InsertDef::new(3, "effects.utility", &registry).unwrap();
        def.params.set_by_id("gain", 4.5);
        let dto = insert_dto(2, &def, &registry);

        assert_eq!(dto.index, 2);
        assert_eq!(dto.id, 3);
        assert_eq!(dto.name, "Utility");
        assert!(!dto.bypassed);
        let gain = dto.params.iter().find(|p| p.id == "gain").unwrap();
        assert_eq!(gain.value, 4.5);

        def.params.set_by_id(BYPASS_PARAM, 1.0);
        assert!(insert_dto(2, &def, &registry).bypassed);
    }

    #[test]
    fn the_wire_shape_is_camel_case_all_the_way_down() {
        let registry = registry();
        let def = InsertDef::new(1, "effects.compressor", &registry).unwrap();
        let json = serde_json::to_string(&insert_dto(0, &def, &registry)).unwrap();
        assert!(json.contains(r#""typeId":"effects.compressor""#), "{json}");
        assert!(json.contains(r#""bypassed":false"#));
        assert!(json.contains(r#""readouts":[{"id":"gr""#), "{json}");

        let catalogue = serde_json::to_string(&catalogue(&registry)).unwrap();
        assert!(catalogue.contains(r#""defaultValue""#));
        assert!(catalogue.contains(r#""smoothingMs""#));
        assert!(!catalogue.contains(r#""default_value""#));
    }
}
