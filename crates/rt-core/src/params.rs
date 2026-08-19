//! Parameters: what a node exposes, how a value crosses the RT wall, and how it
//! gets there without clicking.
//!
//! ```text
//!   non-RT (UI command)                    audio thread (per block)
//!   -------------------                    ------------------------
//!   ParamBlock::set(id, value)  --> atomics --> SmoothedParams::begin_block
//!        ^                                            |
//!        |                                            v  (per sample)
//!   ParamDef metadata --> DTO --> knob panel     ParamSmoother::next
//! ```
//!
//! # Three things, deliberately separate
//!
//! [`ParamDef`] is **metadata**: an id, a range, a curve, a default. It is
//! `&'static` data owned by the node *type*, which is what lets the shell draw a
//! knob panel for an effect that has not been instantiated yet, and what lets a
//! project file be validated against a range without building a node.
//!
//! [`ParamBlock`] is **the value**, as the audio thread reads it: one
//! `AtomicU32` per parameter holding `f32::to_bits`. It is shared by `Arc`
//! between the shell and whatever node is currently reading it, for exactly the
//! reason [`MixerParams`](crate::MixerParams) is: a graph rebuild hands the
//! *same* block to the new node, so inserting a track does not reset an EQ.
//!
//! [`SmoothedParams`] is **how the value arrives**: a per-parameter linear ramp,
//! the same mechanism [`TrackStrip`](crate::TrackStrip) uses for a fader, so a
//! knob drag is a slope and not a staircase.
//!
//! # Why atomics and not a queue
//! The argument is [`crate::mixer`]'s and it has not changed: a parameter is a
//! *level*, not an event. A dropped queue entry would leave a filter at the
//! wrong frequency for ever, and a queue deep enough never to drop one is a
//! queue sized for a mouse. One relaxed load per parameter per block costs
//! nothing and cannot overflow.
//!
//! # Why the metadata is `&'static`
//! A node's parameter list is a property of its *type* and never changes at
//! runtime. Making it static means [`ParamBlock`] can hold the slice rather than
//! a copy, `params()` costs nothing to call, and no allocation is ever needed to
//! answer "what does this effect expose".
//!
//! # Smoothing, and what must not be smoothed
//! [`ParamDef::smoothing_ms`] is per parameter because "10 ms for everything" is
//! wrong in both directions. A gain, a mix and a filter frequency want a ramp. A
//! *switch* -- a filter slope, a detector mode, a tempo division -- does not:
//! there is no meaningful value halfway between "peak" and "RMS", and ramping
//! towards one would spend 10 ms at a setting that does not exist. Those declare
//! `smoothing_ms: 0.0` and land on the block boundary.

use std::sync::atomic::{AtomicU32, Ordering};

/// Default parameter ramp, in milliseconds.
///
/// The same 10 ms as [`PARAM_RAMP_SECONDS`](crate::PARAM_RAMP_SECONDS), and for
/// the same reason: below the ~20 ms at which a change is heard as a gesture
/// rather than an edit, far above the ~0.1 ms at which a step becomes a click.
pub const DEFAULT_SMOOTHING_MS: f32 = 10.0;

/// How a parameter's range maps onto the 0..1 position of a knob.
///
/// This is a *perceptual* statement, not a storage one: the value in
/// [`ParamBlock`] is always the real value in the parameter's own unit. The
/// curve says how a UI should lay that range out under a finger, and it is
/// carried to the UI so that the shell does not keep a second, drifting copy of
/// every effect's taper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamCurve {
    /// Position and value are proportional. Pan, width, ratio-free percentages.
    Linear,
    /// Position is proportional to `log(value)`. Frequencies, times, Q --
    /// anything where "twice as much" is the constant-sized step the ear hears.
    ///
    /// Requires `min > 0`; a logarithmic axis has no room for zero.
    Log,
    /// The value *is* decibels, and a linear slider in decibels is already the
    /// logarithmic amplitude taper the ear wants.
    ///
    /// Distinct from [`Linear`](Self::Linear) so a readout knows to write
    /// `-6.0 dB` and a fader knows to draw a dB scale, and distinct from
    /// [`Log`](Self::Log) because a dB range legitimately spans zero and
    /// negatives.
    Db,
}

/// One parameter a node exposes.
///
/// Everything a knob needs and nothing a knob does not: no colour, no group, no
/// automation curve. The moment this grows presentation the shell stops being
/// able to render it generically, which is the entire point of it existing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamDef {
    /// Stable identifier, e.g. `"band1.freq"`. **Persisted in project files**,
    /// so renaming one invalidates saved sessions.
    pub id: &'static str,
    /// Name for a knob label.
    pub name: &'static str,
    /// Unit for a readout: `"dB"`, `"Hz"`, `"ms"`, `"%"`, `":1"`, or `""`.
    pub unit: &'static str,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub curve: ParamCurve,
    /// Quantisation, in the parameter's own unit. `None` is continuous;
    /// `Some(1.0)` with an integer range is how a switch or an enum is declared.
    pub step: Option<f32>,
    /// Ramp length in milliseconds. `0.0` means "land immediately" -- see the
    /// module docs on what must not be smoothed.
    pub smoothing_ms: f32,
}

impl ParamDef {
    /// A continuous parameter with the default ramp.
    pub const fn new(
        id: &'static str,
        name: &'static str,
        unit: &'static str,
        min: f32,
        max: f32,
        default: f32,
        curve: ParamCurve,
    ) -> Self {
        Self {
            id,
            name,
            unit,
            min,
            max,
            default,
            curve,
            step: None,
            smoothing_ms: DEFAULT_SMOOTHING_MS,
        }
    }

    /// A switch or enumeration: integer-stepped, and **not** smoothed.
    ///
    /// `count` is how many positions it has, so `choice("mode", .., 3, 0)` is a
    /// three-way switch whose value is `0.0`, `1.0` or `2.0`.
    pub const fn choice(id: &'static str, name: &'static str, count: u32, default: u32) -> Self {
        Self {
            id,
            name,
            unit: "",
            min: 0.0,
            max: (count - 1) as f32,
            default: default as f32,
            curve: ParamCurve::Linear,
            step: Some(1.0),
            smoothing_ms: 0.0,
        }
    }

    /// An on/off parameter: `0.0` or `1.0`, unsmoothed.
    ///
    /// A toggle whose *effect* should fade -- bypass, most obviously -- declares
    /// itself here and does the fading in its own DSP, because what should be
    /// crossfaded is the processed signal and not the flag.
    pub const fn toggle(id: &'static str, name: &'static str, default: bool) -> Self {
        Self::choice(id, name, 2, default as u32)
    }

    /// The same parameter with a different ramp length.
    #[must_use]
    pub const fn smoothed(mut self, ms: f32) -> Self {
        self.smoothing_ms = ms;
        self
    }

    /// The same parameter, quantised to `step` in its own unit.
    #[must_use]
    pub const fn stepped(mut self, step: f32) -> Self {
        self.step = Some(step);
        self
    }

    /// The same parameter with an explicit unit.
    #[must_use]
    pub const fn with_unit(mut self, unit: &'static str) -> Self {
        self.unit = unit;
        self
    }

    /// Whether this parameter is a switch: integer-stepped and unsmoothed.
    pub fn is_switch(&self) -> bool {
        self.step.is_some() && self.smoothing_ms <= 0.0
    }

    /// Force `value` into something this parameter can actually hold: finite,
    /// inside the range, and on the step grid if there is one.
    ///
    /// A non-finite value becomes the default rather than the minimum. A NaN
    /// arrived from somewhere that had no value at all -- a webview field being
    /// edited, a damaged project file -- and the default is the only answer that
    /// is not a silent, arbitrary setting.
    pub fn sanitise(&self, value: f32) -> f32 {
        if !value.is_finite() {
            return self.default;
        }
        let (low, high) = if self.min <= self.max {
            (self.min, self.max)
        } else {
            (self.max, self.min)
        };
        let clamped = value.clamp(low, high);
        match self.step {
            Some(step) if step > 0.0 => {
                let quantised = low + ((clamped - low) / step).round() * step;
                quantised.clamp(low, high)
            }
            _ => clamped,
        }
    }

    /// Where `value` sits on a 0..1 knob, under this parameter's curve.
    ///
    /// Used by the UI and by tests; never by the audio thread.
    pub fn to_normalised(&self, value: f32) -> f32 {
        let value = self.sanitise(value);
        let span = self.max - self.min;
        match self.curve {
            ParamCurve::Log if self.min > 0.0 && self.max > self.min => {
                (value / self.min).ln() / (self.max / self.min).ln()
            }
            // A degenerate range is a bug in a declaration, not a runtime
            // condition; answering 0.0 keeps it from becoming a NaN in a UI.
            _ if span.abs() <= f32::EPSILON => 0.0,
            _ => (value - self.min) / span,
        }
        .clamp(0.0, 1.0)
    }

    /// The value at knob position `t`, the inverse of
    /// [`to_normalised`](Self::to_normalised).
    pub fn from_normalised(&self, t: f32) -> f32 {
        let t = if t.is_finite() {
            t.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let value = match self.curve {
            ParamCurve::Log if self.min > 0.0 && self.max > self.min => {
                self.min * (self.max / self.min).powf(t)
            }
            _ => self.min + t * (self.max - self.min),
        };
        self.sanitise(value)
    }
}

/// An `f32` that crosses the RT wall, stored as its bits.
///
/// There is no `AtomicF32` in the standard library and there does not need to
/// be: a bit pattern is a bit pattern. Relaxed ordering throughout, because a
/// parameter carries no other memory with it -- the audio thread wants *a*
/// recent value, not a synchronisation point.
#[derive(Debug)]
pub struct AtomicF32(AtomicU32);

impl AtomicF32 {
    pub fn new(value: f32) -> Self {
        Self(AtomicU32::new(value.to_bits()))
    }

    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }

    pub fn set(&self, value: f32) {
        self.0.store(value.to_bits(), Ordering::Relaxed);
    }
}

impl Default for AtomicF32 {
    fn default() -> Self {
        Self::new(0.0)
    }
}

/// A value a node *publishes* rather than reads: a meter, in effect.
///
/// The mirror image of [`ParamDef`], and separate from it on purpose. A
/// parameter is an input the shell owns and the node obeys; a readout is an
/// output the node owns and the shell draws. Collapsing them would mean the UI
/// could "set" a gain-reduction meter, and a project file would try to save one.
///
/// Declared statically for the same reason parameters are: the shell can lay out
/// a meter panel for an effect type before an instance of it exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadoutDef {
    /// Stable identifier, e.g. `"gr"`. **Not** persisted -- a meter has no
    /// saved value -- but it does appear in DTOs, so the UI keys on it.
    pub id: &'static str,
    /// Name for a meter label.
    pub name: &'static str,
    /// Unit for a readout: `"dB"`, `"%"`, `""`.
    pub unit: &'static str,
}

impl ReadoutDef {
    pub const fn new(id: &'static str, name: &'static str, unit: &'static str) -> Self {
        Self { id, name, unit }
    }
}

/// One node instance's parameter values, shared across the RT wall.
///
/// Built from a `&'static [ParamDef]`, so the block and the metadata can never
/// disagree about how many parameters there are or what they mean.
///
/// # Why the shell holds the `Arc` and not the node
/// A node is rebuilt whenever the graph is -- adding a track recreates every
/// insert. If the values lived *in* the node they would reset on every
/// structural edit. They live here instead, the shell keeps the `Arc`, and a
/// rebuilt node is handed the same block: an EQ keeps its curve across "add
/// track", exactly as a fader keeps its position.
#[derive(Debug)]
pub struct ParamBlock {
    defs: &'static [ParamDef],
    cells: Box<[AtomicF32]>,
    readout_defs: &'static [ReadoutDef],
    readouts: Box<[AtomicF32]>,
}

impl ParamBlock {
    /// A block at every parameter's default, with no readouts.
    ///
    /// **Non-RT**: this allocates.
    pub fn new(defs: &'static [ParamDef]) -> Self {
        Self::with_readouts(defs, &[])
    }

    /// A block that also carries the meters `readout_defs` describes.
    ///
    /// **Non-RT**: this allocates.
    pub fn with_readouts(defs: &'static [ParamDef], readout_defs: &'static [ReadoutDef]) -> Self {
        let cells = defs
            .iter()
            .map(|def| AtomicF32::new(def.sanitise(def.default)))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let readouts = readout_defs
            .iter()
            .map(|_| AtomicF32::new(0.0))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            defs,
            cells,
            readout_defs,
            readouts,
        }
    }

    /// The metadata this block was built from.
    pub fn defs(&self) -> &'static [ParamDef] {
        self.defs
    }

    /// The meters this block carries.
    pub fn readout_defs(&self) -> &'static [ReadoutDef] {
        self.readout_defs
    }

    /// Publish a meter value. Called from the audio thread.
    ///
    /// A non-finite value is stored as zero rather than propagated: a NaN in a
    /// meter would paint a bar of undefined length for ever.
    pub fn set_readout(&self, index: usize, value: f32) {
        if let Some(cell) = self.readouts.get(index) {
            cell.set(if value.is_finite() { value } else { 0.0 });
        }
    }

    /// Read a meter. Called from the shell.
    pub fn readout(&self, index: usize) -> f32 {
        self.readouts.get(index).map_or(0.0, AtomicF32::get)
    }

    /// Position of the meter called `id`. **Non-RT**.
    pub fn readout_index_of(&self, id: &str) -> Option<usize> {
        self.readout_defs.iter().position(|def| def.id == id)
    }

    /// Every `(id, value)` meter pair, for a DTO. **Non-RT**: allocates.
    pub fn readout_snapshot(&self) -> Vec<(&'static str, f32)> {
        self.readout_defs
            .iter()
            .enumerate()
            .map(|(index, def)| (def.id, self.readout(index)))
            .collect()
    }

    /// Parameters in this block.
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Position of the parameter called `id`.
    ///
    /// Linear search over a handful of `&'static str`s, which is faster than a
    /// map at these sizes and costs no allocation. **Non-RT**: the audio thread
    /// addresses parameters by index, resolved once when the node is built.
    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.defs.iter().position(|def| def.id == id)
    }

    /// The value of parameter `index`, or `0.0` past the end.
    pub fn get(&self, index: usize) -> f32 {
        self.cells.get(index).map_or(0.0, AtomicF32::get)
    }

    /// The value of the parameter called `id`.
    pub fn get_by_id(&self, id: &str) -> Option<f32> {
        self.index_of(id).map(|index| self.get(index))
    }

    /// Store `value` into parameter `index`, sanitised into range.
    ///
    /// Returns what was actually stored, which is what the shell should echo
    /// back to the UI -- a knob that sent 40 kHz to a 20 Hz..20 kHz parameter
    /// should snap to the top rather than draw itself somewhere the engine is
    /// not.
    pub fn set(&self, index: usize, value: f32) -> Option<f32> {
        let def = self.defs.get(index)?;
        let cell = self.cells.get(index)?;
        let sanitised = def.sanitise(value);
        cell.set(sanitised);
        Some(sanitised)
    }

    /// Store into the parameter called `id`.
    pub fn set_by_id(&self, id: &str, value: f32) -> Option<f32> {
        self.index_of(id).and_then(|index| self.set(index, value))
    }

    /// Put every parameter back to its default.
    pub fn reset(&self) {
        for (index, def) in self.defs.iter().enumerate() {
            self.cells[index].set(def.sanitise(def.default));
        }
    }

    /// Every `(id, value)` pair, for a DTO or a project file.
    ///
    /// **Non-RT**: allocates.
    pub fn snapshot(&self) -> Vec<(&'static str, f32)> {
        self.defs
            .iter()
            .enumerate()
            .map(|(index, def)| (def.id, self.get(index)))
            .collect()
    }

    /// Whether every parameter is at its default.
    ///
    /// What lets a project file omit a chain entry's values when nothing was
    /// touched, and what "reset" can check itself against.
    pub fn is_default(&self) -> bool {
        self.defs
            .iter()
            .enumerate()
            .all(|(index, def)| self.get(index) == def.sanitise(def.default))
    }

    /// Apply stored `(id, value)` pairs, ignoring ids this build does not know.
    ///
    /// Ignoring rather than refusing is the same forward-compatibility rule the
    /// project file follows for unknown fields: a session saved by a later build
    /// that added a parameter must still open here, minus that parameter.
    /// Returns how many were applied.
    pub fn apply<'a>(&self, values: impl IntoIterator<Item = (&'a str, f32)>) -> usize {
        values
            .into_iter()
            .filter(|(id, value)| self.set_by_id(id, *value).is_some())
            .count()
    }
}

/// A parameter's journey from where it is to where it was asked to be.
///
/// The same shape as one channel of a [`TrackStrip`](crate::TrackStrip): a
/// current value, a goal, and the frames left to reach it. Restarting from
/// wherever the value happens to be is what makes a knob moved *during* a ramp
/// continuous rather than a jump back to the old start.
#[derive(Debug, Clone, Copy)]
pub struct ParamSmoother {
    current: f32,
    goal: f32,
    remaining: u32,
    ramp_frames: u32,
}

impl ParamSmoother {
    /// A smoother sitting at `value`, ramping over `ramp_frames`.
    ///
    /// `ramp_frames == 0` is a smoother that always lands immediately, which is
    /// how a switch is expressed.
    pub const fn new(value: f32, ramp_frames: u32) -> Self {
        Self {
            current: value,
            goal: value,
            remaining: 0,
            ramp_frames,
        }
    }

    /// Frames a full-distance ramp takes.
    pub const fn ramp_frames(&self) -> u32 {
        self.ramp_frames
    }

    /// Aim at `goal`, restarting the ramp from the current value if it moved.
    pub fn target(&mut self, goal: f32) {
        if goal == self.goal {
            return;
        }
        self.goal = goal;
        self.remaining = if self.current == goal || self.ramp_frames == 0 {
            self.current = goal;
            0
        } else {
            self.ramp_frames
        };
    }

    /// Land on `value` with no ramp at all. For a reset, or a fresh node.
    pub fn jump(&mut self, value: f32) {
        self.current = value;
        self.goal = value;
        self.remaining = 0;
    }

    /// The value in force right now, without advancing.
    pub const fn value(&self) -> f32 {
        self.current
    }

    /// Where this parameter is heading. A switch reads this rather than
    /// [`value`](Self::value) when it wants the setting rather than the fade.
    pub const fn goal(&self) -> f32 {
        self.goal
    }

    /// Whether the ramp has finished.
    pub const fn is_settled(&self) -> bool {
        self.remaining == 0
    }

    /// Advance one sample and return the value in force for it.
    ///
    /// The step is derived from the frames *left*, so the ramp always takes
    /// exactly `ramp_frames` from the moment the control moved and lands on the
    /// goal exactly rather than near it.
    pub fn advance(&mut self) -> f32 {
        if self.remaining == 0 {
            return self.current;
        }
        self.remaining -= 1;
        self.current = if self.remaining == 0 {
            self.goal
        } else {
            self.current + (self.goal - self.current) / (self.remaining + 1) as f32
        };
        self.current
    }

    /// Advance `frames` samples at once, for a parameter whose value is only
    /// read at block rate.
    pub fn skip(&mut self, frames: usize) {
        for _ in 0..frames {
            if self.remaining == 0 {
                break;
            }
            self.advance();
        }
    }
}

/// Ramp length in frames for a parameter, at `sample_rate`.
///
/// Zero for an unsmoothed parameter. Otherwise at least
/// [`PARAM_RAMP_MIN_FRAMES`](crate::PARAM_RAMP_MIN_FRAMES) for the same reason
/// the mixer has a floor: at the artificially low rates tests use, a two-frame
/// ramp is a step in all but name.
pub fn smoothing_frames(smoothing_ms: f32, sample_rate: f64) -> u32 {
    if !smoothing_ms.is_finite() || smoothing_ms <= 0.0 {
        return 0;
    }
    let rate = if sample_rate.is_finite() && sample_rate > 0.0 {
        sample_rate
    } else {
        crate::DEFAULT_SAMPLE_RATE
    };
    let frames = (rate * f64::from(smoothing_ms) / 1000.0).round() as u32;
    frames.max(crate::PARAM_RAMP_MIN_FRAMES as u32)
}

/// A node's read side of a [`ParamBlock`]: one smoother per parameter.
///
/// The audio thread's whole parameter story is three calls:
///
/// ```text
/// begin_block()          once per block  -- read the atomics, aim the ramps
/// next(i) / value(i)     per sample      -- the value for this sample
/// goal(i)                any time        -- the setting, ignoring the ramp
/// ```
///
/// # RT safety
/// [`begin_block`](Self::begin_block) is one relaxed load per parameter and
/// [`next`](Self::next) is a compare, a subtract, a divide and an add. Nothing
/// here allocates: the smoother array is built in [`new`](Self::new), which runs
/// on the graph-building thread.
#[derive(Debug)]
pub struct SmoothedParams {
    block: std::sync::Arc<ParamBlock>,
    smoothers: Box<[ParamSmoother]>,
}

impl SmoothedParams {
    /// Read side of `block`, with every smoother already sitting at the block's
    /// current value.
    ///
    /// Starting *at* the value rather than at the default is what makes a graph
    /// rebuild inaudible: a node built into a new graph picks the parameters up
    /// where the old one left them instead of sweeping to them over 10 ms.
    ///
    /// **Non-RT**: this allocates.
    pub fn new(block: std::sync::Arc<ParamBlock>, sample_rate: f64) -> Self {
        let smoothers = block
            .defs()
            .iter()
            .enumerate()
            .map(|(index, def)| {
                ParamSmoother::new(
                    block.get(index),
                    smoothing_frames(def.smoothing_ms, sample_rate),
                )
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self { block, smoothers }
    }

    /// The shared block, for a node that wants to publish something back.
    pub fn block(&self) -> &std::sync::Arc<ParamBlock> {
        &self.block
    }

    /// Parameters.
    pub fn len(&self) -> usize {
        self.smoothers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.smoothers.is_empty()
    }

    /// Read every atomic once and aim every ramp. Call at the top of a block.
    pub fn begin_block(&mut self) {
        for (index, smoother) in self.smoothers.iter_mut().enumerate() {
            smoother.target(self.block.get(index));
        }
    }

    /// Advance parameter `index` one sample and return its value.
    ///
    /// Out of range reads `0.0`, which cannot happen for a node that resolved
    /// its indices at construction and is the only answer that is not a panic on
    /// the audio thread.
    pub fn next(&mut self, index: usize) -> f32 {
        self.smoothers
            .get_mut(index)
            .map_or(0.0, ParamSmoother::advance)
    }

    /// The value of `index` in force right now, without advancing.
    pub fn value(&self, index: usize) -> f32 {
        self.smoothers.get(index).map_or(0.0, ParamSmoother::value)
    }

    /// Where `index` is heading -- the *setting*, ignoring the ramp.
    ///
    /// What a switch reads, and what a coefficient that is only recomputed when
    /// the parameter settles should compare against.
    pub fn goal(&self, index: usize) -> f32 {
        self.smoothers.get(index).map_or(0.0, ParamSmoother::goal)
    }

    /// A switch's position as an index: the goal, rounded and floored at zero.
    pub fn choice(&self, index: usize) -> usize {
        let goal = self.goal(index);
        if goal.is_finite() && goal > 0.0 {
            goal.round() as usize
        } else {
            0
        }
    }

    /// Whether `index` is mid-ramp.
    pub fn is_smoothing(&self, index: usize) -> bool {
        self.smoothers.get(index).is_some_and(|s| !s.is_settled())
    }

    /// Whether **any** parameter is mid-ramp.
    ///
    /// What a filter checks to decide between recomputing its coefficients per
    /// sample (for the 10 ms after a knob moves) and running the tight loop with
    /// frozen ones (for all the rest of the time).
    pub fn any_smoothing(&self) -> bool {
        self.smoothers.iter().any(|s| !s.is_settled())
    }

    /// Advance every parameter one sample.
    ///
    /// For a node that reads some parameters at block rate: the ramps still have
    /// to move, or a settled check would never come true.
    pub fn tick_all(&mut self) {
        for smoother in self.smoothers.iter_mut() {
            smoother.advance();
        }
    }

    /// Advance every parameter `frames` samples, for a block that took an early
    /// exit.
    pub fn skip_all(&mut self, frames: usize) {
        for smoother in self.smoothers.iter_mut() {
            smoother.skip(frames);
        }
    }

    /// Land every parameter on its target with no ramp. For a reset.
    pub fn settle(&mut self) {
        for (index, smoother) in self.smoothers.iter_mut().enumerate() {
            smoother.jump(self.block.get(index));
        }
    }
}

/// A node type that declares parameters and can be built against a shared
/// [`ParamBlock`].
///
/// # Why this and not a method on `RtNode`
/// The shell has to answer "what knobs does an EQ have" *before* there is an EQ
/// -- to draw a panel for an effect being chosen from a menu, and to validate a
/// project file that names one. A method on the node could only answer it for a
/// node that already exists. So the declaration lives on the factory, which is
/// the thing the registry holds one of per type.
///
/// It also keeps [`RtNode`](crate::RtNode) exactly as small as it was: one
/// method, object-safe, with nothing on it that an audio callback does not need.
pub trait ParamNodeFactory: crate::RtNodeFactory {
    /// What this node type exposes. Static, and the same for every instance.
    fn params(&self) -> &'static [ParamDef];

    /// What this node type publishes back. Empty for most.
    fn readouts(&self) -> &'static [ReadoutDef] {
        &[]
    }

    /// Build a node reading `params`.
    ///
    /// The caller owns the block and keeps it across graph rebuilds; see
    /// [`ParamBlock`].
    ///
    /// **Non-RT**: allocates.
    fn create_with_params(
        &self,
        sample_rate: f64,
        max_block: usize,
        params: std::sync::Arc<ParamBlock>,
    ) -> Box<dyn crate::RtNode>;

    /// A block at this node type's defaults. Convenience for tests and for the
    /// plain [`RtNodeFactory::create`](crate::RtNodeFactory::create) path.
    fn default_params(&self) -> std::sync::Arc<ParamBlock> {
        std::sync::Arc::new(ParamBlock::with_readouts(self.params(), self.readouts()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const SR: f64 = 48_000.0;

    static DEFS: &[ParamDef] = &[
        ParamDef::new("gain", "Gain", "dB", -24.0, 24.0, 0.0, ParamCurve::Db),
        ParamDef::new(
            "freq",
            "Frequency",
            "Hz",
            20.0,
            20_000.0,
            1_000.0,
            ParamCurve::Log,
        ),
        ParamDef::new("mix", "Mix", "%", 0.0, 100.0, 100.0, ParamCurve::Linear),
        ParamDef::choice("mode", "Mode", 3, 1),
        ParamDef::toggle("bypass", "Bypass", false),
    ];

    #[test]
    fn a_log_parameter_puts_its_geometric_middle_under_the_middle_of_the_knob() {
        let freq = DEFS[1];
        // sqrt(20 * 20000) = 632.5 Hz: the point a musician expects halfway
        // along a frequency slider, and nowhere near the arithmetic mean.
        let middle = freq.from_normalised(0.5);
        assert!((middle - 632.45).abs() < 0.5, "{middle}");
        assert!(freq.to_normalised(20.0).abs() < 1e-6);
        assert!((freq.to_normalised(20_000.0) - 1.0).abs() < 1e-6);
        // Round trip, over the whole range.
        for step in 0..=20 {
            let t = step as f32 / 20.0;
            let round = freq.to_normalised(freq.from_normalised(t));
            assert!((round - t).abs() < 1e-4, "t={t} came back {round}");
        }
    }

    #[test]
    fn a_db_parameter_is_linear_in_decibels_and_spans_zero() {
        let gain = DEFS[0];
        assert!(
            (gain.from_normalised(0.5) - 0.0).abs() < 1e-4,
            "unity at centre"
        );
        assert!((gain.to_normalised(-24.0)).abs() < 1e-6);
        assert!((gain.to_normalised(24.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn values_outside_the_range_are_pulled_in_and_nonsense_becomes_the_default() {
        let freq = DEFS[1];
        assert_eq!(freq.sanitise(1.0), 20.0);
        assert_eq!(freq.sanitise(96_000.0), 20_000.0);
        assert_eq!(freq.sanitise(f32::NAN), 1_000.0, "a NaN is not a setting");
        assert_eq!(freq.sanitise(f32::INFINITY), 1_000.0);
    }

    #[test]
    fn a_switch_lands_on_whole_positions_and_declares_itself_unsmoothed() {
        let mode = DEFS[3];
        assert!(mode.is_switch());
        assert_eq!(mode.smoothing_ms, 0.0);
        assert_eq!(mode.sanitise(0.4), 0.0);
        assert_eq!(mode.sanitise(0.6), 1.0);
        assert_eq!(mode.sanitise(9.0), 2.0, "clamped to the top position");
        assert_eq!(mode.default, 1.0);

        let bypass = DEFS[4];
        assert_eq!(bypass.max, 1.0);
        assert_eq!(bypass.default, 0.0, "a fresh effect is not bypassed");
        assert!(bypass.is_switch());
    }

    #[test]
    fn a_fresh_block_holds_the_defaults_and_answers_by_id() {
        let block = ParamBlock::new(DEFS);
        assert_eq!(block.len(), 5);
        assert!(block.is_default());
        assert_eq!(block.get_by_id("freq"), Some(1_000.0));
        assert_eq!(block.get_by_id("nope"), None);
        assert_eq!(block.index_of("mix"), Some(2));

        assert_eq!(block.set_by_id("freq", 440.0), Some(440.0));
        assert!(!block.is_default());
        assert_eq!(block.get_by_id("freq"), Some(440.0));

        // Out of range comes back as what was actually stored, so the UI can
        // snap its knob to the truth rather than draw where the engine is not.
        assert_eq!(block.set_by_id("freq", 1e9), Some(20_000.0));
        assert_eq!(block.set_by_id("nope", 1.0), None);

        block.reset();
        assert!(block.is_default());
    }

    #[test]
    fn a_saved_value_for_a_parameter_this_build_does_not_have_is_ignored_not_fatal() {
        // The project file's forward-compatibility rule, at parameter
        // granularity: a session from a later build opens minus what is new.
        let block = ParamBlock::new(DEFS);
        let applied = block.apply([("gain", -6.0), ("resonance", 0.7), ("mix", 50.0)]);
        assert_eq!(applied, 2);
        assert_eq!(block.get_by_id("gain"), Some(-6.0));
        assert_eq!(block.get_by_id("mix"), Some(50.0));
    }

    #[test]
    fn a_smoother_walks_to_its_goal_in_exactly_the_advertised_time() {
        let frames = smoothing_frames(10.0, SR);
        assert_eq!(frames, 480, "10 ms at 48 kHz");
        let mut smoother = ParamSmoother::new(0.0, frames);
        smoother.target(1.0);

        let mut worst = 0.0_f32;
        let mut previous = 0.0_f32;
        for _ in 0..frames {
            let value = smoother.advance();
            worst = worst.max((value - previous).abs());
            previous = value;
        }
        assert_eq!(smoother.value(), 1.0, "it lands exactly");
        assert!(smoother.is_settled());
        assert!(
            worst <= 1.0 / frames as f32 + 1e-6,
            "step {worst} exceeds the ramp slope"
        );

        // Settled: further samples do not move.
        assert_eq!(smoother.advance(), 1.0);
    }

    #[test]
    fn a_goal_moved_mid_ramp_continues_from_where_the_value_is() {
        let mut smoother = ParamSmoother::new(0.0, 100);
        smoother.target(1.0);
        for _ in 0..50 {
            smoother.advance();
        }
        let midway = smoother.value();
        assert!(midway > 0.4 && midway < 0.6, "{midway}");

        smoother.target(0.0);
        // No jump back to 1.0 or to 0.0: the next sample is a step away from
        // where it already was.
        let after = smoother.advance();
        assert!((after - midway).abs() < 0.02, "{midway} -> {after}");
    }

    #[test]
    fn an_unsmoothed_parameter_lands_on_the_block_boundary() {
        assert_eq!(smoothing_frames(0.0, SR), 0);
        let mut switch = ParamSmoother::new(0.0, 0);
        switch.target(2.0);
        assert_eq!(switch.value(), 2.0, "a switch has no halfway");
        assert!(switch.is_settled());
    }

    #[test]
    fn the_ramp_length_has_a_floor_for_absurd_sample_rates() {
        assert_eq!(smoothing_frames(10.0, 44_100.0), 441);
        assert_eq!(
            smoothing_frames(10.0, 1_000.0),
            crate::PARAM_RAMP_MIN_FRAMES as u32
        );
        assert_eq!(
            smoothing_frames(10.0, f64::NAN),
            480,
            "an unusable rate falls back rather than producing an infinity"
        );
    }

    #[test]
    fn a_node_picks_the_parameters_up_where_the_last_one_left_them() {
        // The graph-rebuild property, at parameter granularity.
        let block = Arc::new(ParamBlock::new(DEFS));
        block.set_by_id("gain", -12.0);

        let mut params = SmoothedParams::new(Arc::clone(&block), SR);
        assert_eq!(
            params.value(0),
            -12.0,
            "a rebuilt node must not sweep to its own settings"
        );
        assert!(!params.any_smoothing());

        // A change now does ramp.
        block.set_by_id("gain", 0.0);
        params.begin_block();
        assert!(params.is_smoothing(0));
        assert_eq!(params.goal(0), 0.0);
        assert_eq!(params.value(0), -12.0, "the ramp has not started yet");
    }

    #[test]
    fn a_switch_reads_its_position_the_moment_the_block_starts() {
        let block = Arc::new(ParamBlock::new(DEFS));
        let mut params = SmoothedParams::new(Arc::clone(&block), SR);
        assert_eq!(params.choice(3), 1);

        block.set_by_id("mode", 2.0);
        params.begin_block();
        assert_eq!(
            params.choice(3),
            2,
            "no 10 ms of a mode that does not exist"
        );
        assert!(!params.is_smoothing(3));
    }

    #[test]
    fn settling_lands_every_ramp_without_running_the_samples() {
        let block = Arc::new(ParamBlock::new(DEFS));
        let mut params = SmoothedParams::new(Arc::clone(&block), SR);
        block.set_by_id("mix", 0.0);
        params.begin_block();
        assert!(params.any_smoothing());
        params.settle();
        assert!(!params.any_smoothing());
        assert_eq!(params.value(2), 0.0);
    }
}
