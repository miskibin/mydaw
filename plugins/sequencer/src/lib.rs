//! `mydaw-plugin-sequencer` -- the step sequencer: the first thing in MYDAW the
//! user programs rather than plays.
//!
//! Step 3 gave the engine instruments and a computer-keyboard piano. That makes
//! sounds, but a DAW is not a synthesiser: it is a machine for laying sounds out
//! *in time*. This crate is that machine -- an FL-style step grid for drums and
//! a piano roll for everything with a pitch, looping together while the
//! transport runs.
//!
//! | Module | What |
//! | --- | --- |
//! | [`pattern`] | [`Pattern`] / [`PatternRow`] / [`Step`] / [`MelodicNote`]: the editable pattern, its bounds, and its typed rejections. |
//! | [`compiled`] | [`CompiledPattern`]: the same pattern in fixed arrays, plus the `basedrop` channel that carries it across the RT wall. |
//! | [`schedule`] | [`Scheduler`]: pattern + transport window -> note events, sample-accurate and drift-free. |
//!
//! # The one idea worth reading
//! Everything about the timing follows from refusing to accumulate. A step
//! boundary is `round(n * step_len)` for the **absolute** step index `n`, so
//! there is no running total to drift, no phase to reset at the loop point, and
//! no state to get wrong after a seek. Melodic notes live on a finer grid --
//! [`PPQ`] ticks per quarter note -- but on the *same* clock: `tick_boundary` is
//! written so that a tick on a step line reduces to that identical expression,
//! and a note drawn on the grid lands on the same sample as a drum hit there.
//!
//! The scheduler's only state is the set of note-offs it still owes -- and it
//! owes those because notes have length, not because time does.
//!
//! See `docs/superpowers/specs/2026-08-19-step4-step-sequencer.md` and
//! `docs/superpowers/specs/2026-08-19-step9-piano-roll.md`.
//!
//! # Using it
//! ```no_run
//! use mydaw_kernel::Kernel;
//! use mydaw_plugin_sequencer::{SequencerPlugin, SequencerService};
//!
//! let mut kernel = Kernel::new();
//! kernel.add_plugin(Box::new(SequencerPlugin::new())).unwrap();
//! kernel.boot().unwrap();
//!
//! let sequencer = kernel.ctx().services.get::<SequencerService>().unwrap();
//! let compiled = sequencer.pattern().compile(); // hand this to the audio thread
//! ```

pub mod compiled;
pub mod pattern;
pub mod schedule;

pub use compiled::{
    pattern_channel, CompiledNote, CompiledPattern, CompiledRow, PatternHandle,
    PatternInstallError, PatternSlot, PATTERN_QUEUE_CAPACITY,
};
pub use pattern::{
    row_label, MelodicNote, Pattern, PatternError, PatternRow, Step, ACCENT_THRESHOLD,
    ACCENT_VELOCITY, DEFAULT_ROW_KEYS, DEFAULT_STEPS, DEFAULT_STEP_VELOCITY, MAX_CHANNEL,
    MAX_NOTES, MAX_ROWS, MAX_STEPS, PATTERN_LENGTHS, PPQ, STEPS_PER_BEAT, TICKS_PER_STEP,
};
pub use schedule::{
    step_boundary, step_len_samples, tick_boundary, Scheduler, TransportWindow, GATE_FRACTION,
    MAX_PENDING,
};

use std::sync::Arc;

use mydaw_kernel::{KernelCtx, PanelManifest, Plugin, PluginManifest};
use mydaw_rt_core::{clamp_bpm, DEFAULT_BPM};
use parking_lot::Mutex;

/// Kernel id of this plugin.
pub const PLUGIN_ID: &str = "sequencer";

/// Display name in the plugin list.
pub const PLUGIN_NAME: &str = "Step Sequencer";

/// Panel id and frontend component key. Must match the key in
/// `ui/src/panels/registry.ts`.
pub const SEQUENCER_PANEL: &str = "step-sequencer";

/// Panel id and frontend component key of the piano roll.
///
/// Contributed by this plugin rather than by the shell, because the pattern the
/// roll edits is this plugin's: a panel is offered by whoever owns the state it
/// draws. Must match the key in `ui/src/panels/registry.ts`.
pub const PIANO_ROLL_PANEL: &str = "piano-roll";

/// The authoritative, non-RT copy of the pattern and the tempo.
///
/// # Why the tempo lives here as well as on the clock
/// [`TransportClock`](mydaw_rt_core::TransportClock) is built around a device
/// sample rate and therefore only exists while a stream is open. A user who
/// changes the tempo on a machine with no sound card, or before the device has
/// come up, has still changed the tempo -- and the value has to survive until
/// there is a clock to publish it to. So this service owns the tempo the
/// *session* is at, and the shell pushes it to each new stream.
///
/// The same reasoning applies to the pattern, which is project data and will one
/// day be saved to a file; the compiled copy on the audio thread is a
/// projection of it, never the original.
///
/// A `Mutex` (never locked from the audio thread) rather than an `ArcSwap`: the
/// edit path is a webview command at human speed, and "read, modify, write"
/// under one lock is the shape a project file will want too.
pub struct SequencerService {
    pattern: Mutex<Pattern>,
    bpm: Mutex<f64>,
}

impl Default for SequencerService {
    fn default() -> Self {
        Self::new()
    }
}

impl SequencerService {
    /// The empty four-row drum pattern at [`DEFAULT_BPM`].
    pub fn new() -> Self {
        Self {
            pattern: Mutex::new(Pattern::default()),
            bpm: Mutex::new(DEFAULT_BPM),
        }
    }

    /// A copy of the current pattern.
    pub fn pattern(&self) -> Pattern {
        self.pattern.lock().clone()
    }

    /// Replace the pattern.
    ///
    /// # Errors
    /// [`PatternError`] when the pattern would not fit the compiled form. The
    /// stored pattern is left untouched, so a rejected edit cannot leave the
    /// session half-changed.
    pub fn set_pattern(&self, pattern: Pattern) -> Result<(), PatternError> {
        pattern.validate()?;
        *self.pattern.lock() = pattern;
        Ok(())
    }

    /// The session tempo.
    pub fn bpm(&self) -> f64 {
        *self.bpm.lock()
    }

    /// Set the session tempo, clamped to
    /// `MIN_BPM..=MAX_BPM`(mydaw_rt_core::MIN_BPM). Returns what was stored.
    pub fn set_bpm(&self, bpm: f64) -> f64 {
        let clamped = clamp_bpm(bpm);
        *self.bpm.lock() = clamped;
        clamped
    }

    /// The current pattern, compiled for the audio thread. **Non-RT.**
    pub fn compiled(&self) -> CompiledPattern {
        self.pattern.lock().compile()
    }
}

impl std::fmt::Debug for SequencerService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SequencerService")
            .field("bpm", &self.bpm())
            .field("rows", &self.pattern.lock().rows.len())
            .field("hits", &self.pattern.lock().hit_count())
            .finish()
    }
}

/// Provides [`SequencerService`] and contributes the step-sequencer panel.
///
/// No RT node factory: the sequencer is not a node. It runs *before* the graph,
/// filling the block's note buffer, which is why its scheduler lives in
/// `audio-io`'s RT state rather than in the chain (see the spec).
#[derive(Debug, Default)]
pub struct SequencerPlugin;

impl SequencerPlugin {
    pub const fn new() -> Self {
        Self
    }
}

impl Plugin for SequencerPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest::new(PLUGIN_ID, PLUGIN_NAME)
    }

    fn register(&mut self, ctx: &mut KernelCtx) {
        // `provide` only fails on a duplicate, which would mean the plugin was
        // registered twice -- a wiring bug in the shell, not a runtime state.
        ctx.services
            .provide(Arc::new(SequencerService::new()))
            .expect("SequencerService provided twice: the plugin is registered more than once");
        ctx.contribute_panel(PanelManifest::new(
            SEQUENCER_PANEL,
            "Step Sequencer",
            SEQUENCER_PANEL,
        ));
        ctx.contribute_panel(PanelManifest::new(
            PIANO_ROLL_PANEL,
            "Piano Roll",
            PIANO_ROLL_PANEL,
        ));
    }

    fn start(&mut self, _ctx: &KernelCtx) {}

    fn stop(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_kernel::Kernel;
    use mydaw_rt_core::{MAX_BPM, MIN_BPM};

    fn booted() -> Kernel {
        let mut kernel = Kernel::new();
        kernel.add_plugin(Box::new(SequencerPlugin::new())).unwrap();
        kernel.boot().unwrap();
        kernel
    }

    #[test]
    fn the_plugin_provides_the_service_and_contributes_both_pattern_panels() {
        let kernel = booted();
        assert!(kernel.ctx().services.get::<SequencerService>().is_some());

        let panels = kernel.ui_panels();
        assert_eq!(panels.len(), 2, "the step grid and the piano roll");
        assert_eq!(panels[0].id, SEQUENCER_PANEL);
        assert_eq!(panels[0].title, "Step Sequencer");
        assert_eq!(panels[1].id, PIANO_ROLL_PANEL);
        assert_eq!(panels[1].title, "Piano Roll");
        // Keys into ui/src/panels/registry.ts -- renaming these breaks the UI.
        assert_eq!(panels[0].component, "step-sequencer");
        assert_eq!(panels[1].component, "piano-roll");
    }

    #[test]
    fn the_plugin_contributes_no_rt_node_factory() {
        // The sequencer fills the block's note buffer before the graph runs; it
        // is not a node, and a factory here would be a lie a project file could
        // later try to instantiate.
        let kernel = booted();
        assert!(kernel.ctx().rt_node_factories.is_empty());
    }

    #[test]
    fn a_fresh_service_holds_the_empty_default_pattern_at_the_default_tempo() {
        let service = SequencerService::new();
        assert_eq!(service.bpm(), DEFAULT_BPM);
        let pattern = service.pattern();
        assert_eq!(pattern.rows.len(), 4);
        assert!(pattern.is_empty());
        assert!(service.compiled().is_silent());
    }

    #[test]
    fn setting_a_pattern_stores_it_and_a_rejected_one_changes_nothing() {
        let service = SequencerService::new();
        let mut good = Pattern::default();
        good.rows[0] = PatternRow::empty(36).with_steps(&[0, 4, 8, 12]);
        service.set_pattern(good.clone()).unwrap();
        assert_eq!(service.pattern(), good);
        assert_eq!(service.compiled().row_count(), 4);

        let bad = Pattern {
            rows: vec![PatternRow::empty(200)],
            ..Pattern::silent()
        };
        assert!(service.set_pattern(bad).is_err());
        assert_eq!(
            service.pattern(),
            good,
            "a rejected edit must not touch the stored pattern"
        );
    }

    #[test]
    fn the_service_clamps_the_tempo_the_same_way_the_clock_does() {
        let service = SequencerService::new();
        assert_eq!(service.set_bpm(174.3), 174.3);
        assert_eq!(service.bpm(), 174.3);
        assert_eq!(service.set_bpm(0.0), MIN_BPM);
        assert_eq!(service.set_bpm(5_000.0), MAX_BPM);
        assert_eq!(service.set_bpm(f64::NAN), DEFAULT_BPM);
    }

    #[test]
    fn the_manifest_names_the_plugin_stably() {
        assert_eq!(SequencerPlugin::new().manifest().id, PLUGIN_ID);
        assert_eq!(PLUGIN_ID, "sequencer");
        assert_eq!(SEQUENCER_PANEL, "step-sequencer");
    }
}
