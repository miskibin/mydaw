//! The main thread's half of the CLAP story: instances, placement, editors.
//!
//! [`ClapInstanceHost`] owns the plugins; [`ClapHost`] knows *where each one is*
//! and is the only thing allowed to change that. It is driven from Tauri's run
//! loop:
//!
//! ```text
//! RunEvent::MainEventsCleared  ->  ClapHost::tick(&tracks, &audio, &bridge)
//!                                     1. serve every queued request, in order
//!                                     2. pump every open editor
//!                                     3. give every plugin its main-thread slice
//!                                     4. publish the board if anything moved
//! RunEvent::Exit               ->  ClapHost::shutdown()
//! ```
//!
//! `MainEventsCleared` fires whenever the event loop has drained its queue, and
//! an open plugin window's own `WM_TIMER` (see `mydaw-plugin-windows`) keeps
//! waking that loop at ~60 Hz. That is the whole pump: no extra thread, no
//! `ControlFlow` games, and nothing for the webview to starve.
//!
//! # Placement
//! ```text
//!   track 0 ──▶ SourceSlot ◀── node from instance #3
//!   track 1 ──▶ SourceSlot        (no plugin: a built-in instrument)
//!   track 2 ──▶ SourceSlot ◀── node from instance #7
//!   master  ──▶ InsertSlot ◀── node from instance #4
//! ```
//! `tracks[i]` is the instance heading track `i`, or `None`. Track *indices*
//! move when the layout changes and instance ids do not, which is why the
//! placement is stored here rather than being derived from either.
//!
//! # Structural edits, which step 5 refused
//!
//! Adding or removing a track recompiles the whole graph, and a hosted CLAP
//! plugin cannot be in two graphs at once: the new graph needs a new node, and
//! the old node is not retired until the old graph is dropped -- which cannot
//! happen until the new graph is installed. Step 5 answered that circle by
//! refusing the edit. Step 6 breaks it by conceding the one thing that is
//! actually cheap:
//!
//! 1. install the new graph **with no hosted plugin in it at all** -- a
//!    CLAP-headed track renders [`SilentNode`](crate::source::SilentNode) and the
//!    master insert is empty;
//! 2. the old graph is now unreachable, so the audio thread drops it and the
//!    `basedrop` collector runs its destructors, which hands every processor
//!    back to its instance;
//! 3. activate each instance again and swap its node into the new graph's slot.
//!
//! The cost is **a few milliseconds of silence from the hosted plugins only** --
//! one or two collector sweeps, bounded by
//! [`ACTIVATE_WAIT`](mydaw_plugin_host_clap::ACTIVATE_WAIT). Every other track
//! keeps playing, because a rebuild has never interrupted those.
//!
//! ## Why the instance is kept rather than recreated
//!
//! The obvious alternative is save state → destroy → create → load state, and it
//! is what a *project load* does (see
//! [`crate::clap_bridge::ClapCommand::LoadState`]). For a rebuild it would be
//! strictly worse: it re-runs the bundle's initialisation, it closes the
//! plugin's editor, and it round-trips through a serialiser that some plugins
//! implement imperfectly -- all to reach a state the instance was already in.
//! CLAP requires state to survive `deactivate`/`activate`; keeping the instance
//! is therefore both faster and more faithful, and it keeps an open editor open
//! across a track being added. The save/load path is implemented and tested
//! regardless, because step 8 needs it.

use std::path::Path;
use std::sync::Arc;

use mydaw_plugin_audio_io::{AudioIoError, AudioIoService};
use mydaw_plugin_host_clap::{ClapInstanceHost, ClapInstanceId, ClapPortLayout};
use mydaw_plugin_windows::PluginWindowService;

use crate::clap_bridge::{
    reply, ClapAnswer, ClapBoard, ClapCommand, ClapGuiStatusDto, ClapHostBridge, ClapSlotDto,
    ClapTarget,
};
use crate::source::{ClapSourceRef, Source};
use crate::tracks::TrackHost;

/// Owns every hosted CLAP plugin and remembers where each one sits.
///
/// **Main thread only** -- see the module docs. Not `Send`, by construction.
pub struct ClapHost {
    instances: ClapInstanceHost,
    /// One entry per track, in track order. `None` where the track's source is
    /// a built-in.
    tracks: Vec<Option<ClapInstanceId>>,
    master: Option<ClapInstanceId>,
}

impl ClapHost {
    /// A host with nothing placed, opening editor windows through `windows`.
    pub fn new(windows: Arc<PluginWindowService>) -> Self {
        Self {
            instances: ClapInstanceHost::new(windows),
            tracks: Vec::new(),
            master: None,
        }
    }

    /// One pass of the run loop's CLAP obligations.
    pub fn tick(&mut self, tracks: &TrackHost, audio: &AudioIoService, bridge: &ClapHostBridge) {
        let pending = bridge.drain();
        let mut changed = !pending.is_empty();
        for (command, channel) in pending {
            let answer = self.serve(command, tracks, audio);
            reply(&channel, answer);
        }

        // Editors and timers are owed to every plugin whether or not anything
        // was asked of it: a plugin may schedule main-thread work at any time,
        // and a user may close an editor window with its own close button.
        let editors_before = self.instances.open_editors();
        self.instances.tick();
        changed |= editors_before != self.instances.open_editors();

        if changed {
            self.publish(tracks, bridge.board());
        }
    }

    /// Publish the current placement for the command threads to read.
    ///
    /// Called after every change, and once by the shell at startup so the first
    /// `get_tracks` sees the truth rather than an empty board.
    pub fn publish(&mut self, tracks: &TrackHost, board: &ClapBoard) {
        let count = tracks.track_count();
        self.tracks.resize(count, None);
        let placements: Vec<Option<ClapInstanceId>> = self.tracks[..count].to_vec();
        let slots = placements
            .into_iter()
            .map(|placement| placement.and_then(|id| self.slot_dto(id)))
            .collect();
        let master = self.master.and_then(|id| self.slot_dto(id));
        board.publish(slots, master);
    }

    /// Close every editor and destroy every instance, in the required order.
    ///
    /// The shell calls this on `RunEvent::Exit`, **after** audio has stopped --
    /// the graph has to be retired before a node can come home.
    pub fn shutdown(&mut self) {
        self.instances.shutdown();
        self.tracks.clear();
        self.master = None;
    }

    /// How many instances are hosted. Diagnostics and tests.
    pub fn instance_count(&self) -> usize {
        self.instances.len()
    }

    /// How many editor windows are open. Diagnostics and tests.
    pub fn open_editors(&self) -> usize {
        self.instances.open_editors()
    }

    /// The instance behind a target, for tests that need to reach past the
    /// command surface.
    pub fn instance_at(&self, target: ClapTarget) -> Option<ClapInstanceId> {
        match target {
            ClapTarget::Track { index } => self.tracks.get(index).copied().flatten(),
            ClapTarget::Master => self.master,
        }
    }

    /// The instance host, for tests and for a project loader.
    pub fn instances_mut(&mut self) -> &mut ClapInstanceHost {
        &mut self.instances
    }

    // ------------------------------------------------------------- dispatch

    fn serve(
        &mut self,
        command: ClapCommand,
        tracks: &TrackHost,
        audio: &AudioIoService,
    ) -> Result<ClapAnswer, String> {
        match command {
            ClapCommand::SetTrackSource {
                track,
                bundle_path,
                plugin_id,
            } => self
                .set_track_source(tracks, audio, track, &bundle_path, &plugin_id)
                .map(|()| ClapAnswer::Done),
            ClapCommand::SetMasterEffect {
                bundle_path,
                plugin_id,
            } => self
                .set_master_effect(tracks, audio, &bundle_path, &plugin_id)
                .map(|()| ClapAnswer::Done),
            ClapCommand::SetTrackBuiltIn { track, source_id } => self
                .set_track_built_in(tracks, audio, track, &source_id)
                .map(|()| ClapAnswer::Done),
            ClapCommand::ClearMasterEffect => {
                self.clear_master_effect(tracks).map(|()| ClapAnswer::Done)
            }
            ClapCommand::OpenGui(target) => self.open_gui(target).map(ClapAnswer::Gui),
            ClapCommand::CloseGui(target) => self.close_gui(target).map(ClapAnswer::Gui),
            ClapCommand::SaveState(target) => self.save_state(target).map(ClapAnswer::State),
            ClapCommand::LoadState { target, state } => {
                self.load_state(target, &state).map(|()| ClapAnswer::Done)
            }
            ClapCommand::AddTrack { source_id, name } => self
                .add_track(tracks, audio, &source_id, name)
                .map(|()| ClapAnswer::Done),
            ClapCommand::RemoveTrack { track } => self
                .remove_track(tracks, audio, track)
                .map(|()| ClapAnswer::Done),
        }
    }

    // -------------------------------------------------------------- placement

    /// Put a hosted CLAP instrument at the head of `track`.
    fn set_track_source(
        &mut self,
        tracks: &TrackHost,
        audio: &AudioIoService,
        track: usize,
        bundle_path: &Path,
        plugin_id: &str,
    ) -> Result<(), String> {
        let (sample_rate, max_block) = stream_info(audio)?;
        self.place_track_source(
            tracks,
            track,
            bundle_path,
            plugin_id,
            sample_rate,
            max_block,
        )
    }

    /// [`set_track_source`](Self::set_track_source) with the audio configuration
    /// supplied rather than looked up.
    ///
    /// Public because it is the whole of the interesting behaviour -- create,
    /// check the ports, activate, swap, destroy the old -- and an offline test
    /// can drive a real `GraphSpec` through a real graph channel but cannot open
    /// a real device to hand this an `AudioIoService`.
    ///
    /// # Errors
    /// A sentence for the status bar: a bundle that will not load, a plugin
    /// whose ports are not an instrument's, a slot with no stream behind it.
    pub fn place_track_source(
        &mut self,
        tracks: &TrackHost,
        track: usize,
        bundle_path: &Path,
        plugin_id: &str,
        sample_rate: f64,
        max_block: usize,
    ) -> Result<(), String> {
        if track >= tracks.track_count() {
            return Err(format!("there is no track {track}"));
        }

        let instance = self
            .instances
            .create(bundle_path, plugin_id)
            .map_err(|err| err.to_string())?;

        // The authoritative check. The picker filters by the descriptor's
        // features, which is free but is only the plugin's own claim; this is
        // what it actually declared to the host, and it is the first moment it
        // can be known.
        let outcome = self
            .require_layout(instance, ClapPortLayout::is_instrument, "an instrument")
            .and_then(|()| {
                self.instances
                    .create_node(instance, sample_rate, max_block)
                    .map_err(|err| err.to_string())
            })
            .and_then(|node| {
                let reference = self.reference(instance);
                tracks
                    .install_source(track, Source::Clap(reference), Box::new(node))
                    .map_err(|err| err.to_string())
            });

        if let Err(err) = outcome {
            // Nothing was placed, so nothing may be left hosted: an instance the
            // user cannot see is an instance nobody will ever destroy.
            self.instances.destroy(instance);
            return Err(err);
        }

        self.tracks
            .resize(tracks.track_count().max(track + 1), None);
        if let Some(previous) = self.tracks[track].replace(instance) {
            self.instances.destroy(previous);
        }
        Ok(())
    }

    /// Replace a track's hosted plugin with a built-in generator.
    ///
    /// The swap and the destroy are one operation on purpose: doing only the
    /// swap would leave an instance hosted, activated and unreachable -- a
    /// plugin nobody can see, edit or free, still holding whatever resources it
    /// opened. Ordered swap-then-destroy, because the destroy has to wait for
    /// the node the swap displaced to come home from the collector.
    fn set_track_built_in(
        &mut self,
        tracks: &TrackHost,
        audio: &AudioIoService,
        track: usize,
        source_id: &str,
    ) -> Result<(), String> {
        let (sample_rate, max_block) = stream_info(audio)?;
        self.replace_with_built_in(tracks, track, source_id, sample_rate, max_block)
    }

    /// [`set_track_built_in`](Self::set_track_built_in) with the audio
    /// configuration supplied rather than looked up. See
    /// [`place_track_source`](Self::place_track_source) for why it is public.
    ///
    /// # Errors
    /// A sentence for the status bar: an unknown source id, or a slot with no
    /// stream behind it.
    pub fn replace_with_built_in(
        &mut self,
        tracks: &TrackHost,
        track: usize,
        source_id: &str,
        sample_rate: f64,
        max_block: usize,
    ) -> Result<(), String> {
        let source = Source::parse(source_id).map_err(|err| err.to_string())?;
        let node = tracks
            .build_node(&source, sample_rate, max_block)
            .map_err(|err| err.to_string())?;
        tracks
            .install_source(track, source, node)
            .map_err(|err| err.to_string())?;
        if let Some(instance) = self.tracks.get_mut(track).and_then(Option::take) {
            self.instances.destroy(instance);
        }
        Ok(())
    }

    /// Put a hosted CLAP effect on the master bus.
    fn set_master_effect(
        &mut self,
        tracks: &TrackHost,
        audio: &AudioIoService,
        bundle_path: &Path,
        plugin_id: &str,
    ) -> Result<(), String> {
        let (sample_rate, max_block) = stream_info(audio)?;
        self.place_master_effect(tracks, bundle_path, plugin_id, sample_rate, max_block)
    }

    /// [`set_master_effect`](Self::set_master_effect) with the audio
    /// configuration supplied rather than looked up. See
    /// [`place_track_source`](Self::place_track_source) for why it is public.
    ///
    /// # Errors
    /// A sentence for the status bar.
    pub fn place_master_effect(
        &mut self,
        tracks: &TrackHost,
        bundle_path: &Path,
        plugin_id: &str,
        sample_rate: f64,
        max_block: usize,
    ) -> Result<(), String> {
        let instance = self
            .instances
            .create(bundle_path, plugin_id)
            .map_err(|err| err.to_string())?;

        let outcome = self
            .require_layout(
                instance,
                ClapPortLayout::is_stereo_effect,
                "a stereo audio effect",
            )
            .and_then(|()| {
                self.instances
                    .create_node(instance, sample_rate, max_block)
                    .map_err(|err| err.to_string())
            })
            .and_then(|node| {
                tracks
                    .master_insert()
                    .install(Some(Box::new(node)))
                    .map_err(|err| err.to_string())
            });

        if let Err(err) = outcome {
            self.instances.destroy(instance);
            return Err(err);
        }

        if let Some(previous) = self.master.replace(instance) {
            self.instances.destroy(previous);
        }
        Ok(())
    }

    /// Empty the master insert.
    fn clear_master_effect(&mut self, tracks: &TrackHost) -> Result<(), String> {
        // Emptying the slot first is what makes the destroy below able to
        // collect the node: the audio thread has to drop it before the
        // collector can.
        tracks
            .master_insert()
            .clear()
            .map_err(|err| err.to_string())?;
        if let Some(previous) = self.master.take() {
            self.instances.destroy(previous);
        }
        Ok(())
    }

    // ------------------------------------------------------------ layout edits

    /// Append a track, then re-attach every hosted plugin to the new graph.
    fn add_track(
        &mut self,
        tracks: &TrackHost,
        audio: &AudioIoService,
        source_id: &str,
        name: Option<String>,
    ) -> Result<(), String> {
        let source = Source::parse(source_id).map_err(|err| err.to_string())?;
        tracks
            .add_track(source, name, audio)
            .map_err(|err| err.to_string())?;
        self.tracks.resize(tracks.track_count(), None);
        self.reattach(tracks, audio)
    }

    /// Remove a track, renumber the placements after it, and re-attach.
    fn remove_track(
        &mut self,
        tracks: &TrackHost,
        audio: &AudioIoService,
        track: usize,
    ) -> Result<(), String> {
        let doomed = self.tracks.get(track).copied().flatten();
        tracks
            .remove_track(track, audio)
            .map_err(|err| err.to_string())?;
        if track < self.tracks.len() {
            self.tracks.remove(track);
        }
        self.tracks.resize(tracks.track_count(), None);

        // Re-attach first, destroy second: the removed track's node is in the
        // *old* graph, so it can only come home once the new one is installed.
        let outcome = self.reattach(tracks, audio);
        if let Some(instance) = doomed {
            self.instances.destroy(instance);
        }
        outcome
    }

    /// Give every placed instance a node in the graph that is now installed.
    fn reattach(&mut self, tracks: &TrackHost, audio: &AudioIoService) -> Result<(), String> {
        let (sample_rate, max_block) = stream_info(audio)?;
        self.reattach_all(tracks, sample_rate, max_block)
    }

    /// Re-activate every placed instance into the graph that is now installed.
    ///
    /// **This is the protocol that makes a layout edit possible with hosted
    /// plugins loaded.** The caller has already installed a CLAP-free graph;
    /// this walks the placements, waits for each instance's old node to come
    /// home from the `basedrop` collector, activates it again and swaps the new
    /// node into the new graph's slot. See the module docs for why the instance
    /// is kept rather than recreated, and for the gap it costs.
    ///
    /// Public for the same reason as
    /// [`place_track_source`](Self::place_track_source): an offline test can
    /// build and install a real graph, but not open a real device.
    ///
    /// # Errors
    /// A sentence naming every placement that could not be re-attached. The
    /// layout edit itself has already happened either way, and a track whose
    /// plugin did not come back renders silence -- which the user can see.
    pub fn reattach_all(
        &mut self,
        tracks: &TrackHost,
        sample_rate: f64,
        max_block: usize,
    ) -> Result<(), String> {
        let mut failures: Vec<String> = Vec::new();

        for index in 0..self.tracks.len() {
            let Some(instance) = self.tracks[index] else {
                continue;
            };
            match self.instances.create_node(instance, sample_rate, max_block) {
                Ok(node) => {
                    let reference = self.reference(instance);
                    if let Err(err) =
                        tracks.install_source(index, Source::Clap(reference), Box::new(node))
                    {
                        failures.push(format!("track {index}: {err}"));
                    }
                }
                Err(err) => failures.push(format!("track {index}: {err}")),
            }
        }

        if let Some(instance) = self.master {
            match self.instances.create_node(instance, sample_rate, max_block) {
                Ok(node) => {
                    if let Err(err) = tracks.master_insert().install(Some(Box::new(node))) {
                        failures.push(format!("master insert: {err}"));
                    }
                }
                Err(err) => failures.push(format!("master insert: {err}")),
            }
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "the graph was rebuilt, but a hosted plugin could not be re-attached: {}",
                failures.join("; ")
            ))
        }
    }

    // -------------------------------------------------------------------- gui

    fn open_gui(&mut self, target: ClapTarget) -> Result<ClapGuiStatusDto, String> {
        let instance = self.require(target)?;
        let title = self.instances.info(instance).map_or_else(
            || "CLAP plugin — MYDAW".to_owned(),
            |info| {
                format!(
                    "{} — MYDAW",
                    if info.name.is_empty() {
                        &info.id
                    } else {
                        &info.name
                    }
                )
            },
        );
        let state = self
            .instances
            .open_gui(instance, &title)
            .map_err(|err| user_message(&err))?;
        Ok(ClapGuiStatusDto {
            open: state.open,
            has_editor: state.has_editor,
            plugin_id: self.instances.info(instance).map(|info| info.id.clone()),
            width: state.width,
            height: state.height,
        })
    }

    fn close_gui(&mut self, target: ClapTarget) -> Result<ClapGuiStatusDto, String> {
        let instance = self.require(target)?;
        let state = self.instances.close_gui(instance);
        Ok(ClapGuiStatusDto {
            open: state.open,
            has_editor: state.has_editor,
            plugin_id: self.instances.info(instance).map(|info| info.id.clone()),
            width: state.width,
            height: state.height,
        })
    }

    // ------------------------------------------------------------------ state

    fn save_state(&mut self, target: ClapTarget) -> Result<Vec<u8>, String> {
        let instance = self.require(target)?;
        self.instances
            .save_state(instance)
            .map_err(|err| err.to_string())
    }

    fn load_state(&mut self, target: ClapTarget, bytes: &[u8]) -> Result<(), String> {
        let instance = self.require(target)?;
        self.instances
            .load_state(instance, bytes)
            .map_err(|err| err.to_string())
    }

    // ------------------------------------------------------------------ plumb

    /// The instance at `target`, or a sentence saying there is none.
    fn require(&self, target: ClapTarget) -> Result<ClapInstanceId, String> {
        self.instance_at(target)
            .ok_or_else(|| format!("there is no hosted CLAP plugin on {target}"))
    }

    /// Refuse a plugin whose declared ports are not the shape this slot needs.
    fn require_layout(
        &self,
        instance: ClapInstanceId,
        accepts: fn(&ClapPortLayout) -> bool,
        expected: &str,
    ) -> Result<(), String> {
        let Some(layout) = self.instances.port_layout(instance) else {
            return Err(format!("there is no hosted CLAP instance {instance}"));
        };
        if accepts(&layout) {
            return Ok(());
        }
        let name = self
            .instances
            .info(instance)
            .map_or_else(String::new, |info| info.id.clone());
        Err(format!(
            "'{name}' is not {expected}: it declares {} audio input port(s) x {} ch and \
             {} output port(s) x {} ch",
            layout.input_ports,
            layout.main_input_channels,
            layout.output_ports,
            layout.main_output_channels,
        ))
    }

    /// What a track's `TrackDef` should record for this instance.
    fn reference(&self, instance: ClapInstanceId) -> ClapSourceRef {
        self.instances.info(instance).map_or_else(
            || ClapSourceRef::new("", ""),
            |info| ClapSourceRef::new(info.path.clone(), info.id.clone()).named(info.name.clone()),
        )
    }

    /// One placed plugin, in the shape the mixer reads.
    fn slot_dto(&mut self, instance: ClapInstanceId) -> Option<ClapSlotDto> {
        let (plugin_id, name, bundle_path) = {
            let info = self.instances.info(instance)?;
            (
                info.id.clone(),
                if info.name.is_empty() {
                    info.id.clone()
                } else {
                    info.name.clone()
                },
                info.path.display().to_string(),
            )
        };
        let gui = self.instances.gui_state(instance).unwrap_or_default();
        Some(ClapSlotDto {
            instance: instance.get(),
            plugin_id,
            name,
            bundle_path,
            has_editor: gui.has_editor,
            gui_open: gui.open,
        })
    }
}

impl std::fmt::Debug for ClapHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClapHost")
            .field("instances", &self.instances.len())
            .field("tracks", &self.tracks)
            .field("master", &self.master)
            .field("open_editors", &self.instances.open_editors())
            .finish()
    }
}

/// The stream's sample rate and maximum block, or the sentence saying there is
/// no stream.
///
/// A CLAP plugin is activated *for* an audio configuration, so nothing here can
/// happen without one. Reported rather than deferred: a user who clicked a
/// plugin on a machine with no sound card deserves to be told why nothing
/// happened.
fn stream_info(audio: &AudioIoService) -> Result<(f64, usize), String> {
    audio
        .stream_info()
        .map(|info| (f64::from(info.sample_rate), info.max_block))
        .ok_or_else(|| AudioIoError::NotRunning.to_string())
}

/// The sentence the status bar shows for a failed editor open.
///
/// `NoGuiSupport` is the common one and deserves plain language: a plugin with
/// no editor is normal, not broken.
pub fn user_message(error: &mydaw_plugin_host_clap::ClapInstanceError) -> String {
    use mydaw_plugin_host_clap::{ClapGuiError, ClapInstanceError};
    match error {
        ClapInstanceError::Gui(ClapGuiError::NoGuiSupport) => {
            "This plugin has no editor.".to_owned()
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_plugin_host_clap::ClapGuiError;

    fn host() -> ClapHost {
        ClapHost::new(Arc::new(PluginWindowService::new()))
    }

    fn track_host() -> TrackHost {
        TrackHost::new(
            Arc::new(mydaw_plugin_test_tone::TestToneFactory::new()),
            Arc::new(mydaw_plugin_instruments::InstrumentRegistry::new()),
        )
    }

    #[test]
    fn a_fresh_host_has_nothing_placed_and_publishes_an_empty_board() {
        let mut host = host();
        let tracks = track_host();
        let board = ClapBoard::new();

        assert_eq!(host.instance_count(), 0);
        assert_eq!(host.open_editors(), 0);
        assert!(host.instance_at(ClapTarget::Master).is_none());

        host.publish(&tracks, &board);
        assert!(board.is_empty());
        assert!(board.track(0).is_none());
        assert!(board.master().is_none());
    }

    #[test]
    fn every_command_against_an_empty_host_is_answered_with_a_sentence() {
        // A stale UI -- an editor button clicked just after the source changed --
        // must get an explanation, not a hang and not a panic.
        let mut host = host();
        let tracks = track_host();
        let audio = AudioIoService::default();

        for target in [ClapTarget::Track { index: 0 }, ClapTarget::Master] {
            for command in [
                ClapCommand::OpenGui(target),
                ClapCommand::CloseGui(target),
                ClapCommand::SaveState(target),
                ClapCommand::LoadState {
                    target,
                    state: vec![1, 2, 3],
                },
            ] {
                let err = host
                    .serve(command.clone(), &tracks, &audio)
                    .expect_err("nothing is placed");
                assert!(
                    err.contains("no hosted CLAP plugin"),
                    "{command:?} produced {err}"
                );
            }
        }
    }

    #[test]
    fn a_clap_command_without_an_audio_device_names_that_rather_than_the_plugin() {
        // Activating a plugin needs the stream's sample rate and block size, so
        // "there is no device" is the honest answer and the first one to give.
        let mut host = host();
        let tracks = track_host();
        let audio = AudioIoService::default();

        let err = host
            .serve(
                ClapCommand::SetTrackSource {
                    track: 0,
                    bundle_path: std::env::temp_dir().join("nothing.clap"),
                    plugin_id: "com.acme.thing".to_owned(),
                },
                &tracks,
                &audio,
            )
            .expect_err("no device");
        assert_eq!(err, AudioIoError::NotRunning.to_string());
        assert_eq!(
            host.instance_count(),
            0,
            "nothing may be loaded before the device is checked"
        );
    }

    #[test]
    fn clearing_a_master_effect_that_is_not_there_reports_the_missing_stream() {
        let mut host = host();
        let tracks = track_host();
        let audio = AudioIoService::default();
        // The slot is not open (no stream), so clearing it is the audio error --
        // and, crucially, not a panic and not a silent success.
        assert!(host
            .serve(ClapCommand::ClearMasterEffect, &tracks, &audio)
            .is_err());
        assert!(host.master.is_none());
    }

    #[test]
    fn a_clap_source_cannot_be_added_as_a_brand_new_tracks_source() {
        // Adding a track and placing a plugin on it are two operations, and the
        // command that places a plugin is the one that carries a bundle path.
        let mut host = host();
        let tracks = track_host();
        let audio = AudioIoService::default();
        let err = host
            .serve(
                ClapCommand::AddTrack {
                    source_id: "clap:com.acme.thing".to_owned(),
                    name: None,
                },
                &tracks,
                &audio,
            )
            .expect_err("a bare clap id names no bundle");
        assert!(err.contains("set_track_source_clap"), "{err}");
        assert_eq!(tracks.track_count(), 2, "the project must not have changed");
    }

    #[test]
    fn the_no_editor_message_is_plain_language_and_everything_else_keeps_its_detail() {
        assert_eq!(
            user_message(&ClapGuiError::NoGuiSupport.into()),
            "This plugin has no editor."
        );
        let other: mydaw_plugin_host_clap::ClapInstanceError =
            ClapGuiError::UnsupportedApi { api: "win32" }.into();
        assert!(user_message(&other).contains("win32"));
    }

    #[test]
    fn the_board_is_published_with_one_entry_per_track_even_when_none_are_hosted() {
        // The mixer indexes into it, so a short vector would silently report
        // "no plugin" for the tracks past its end -- which happens to be right
        // here and would be wrong the moment one is placed.
        let mut host = host();
        let tracks = track_host();
        let board = ClapBoard::new();
        host.publish(&tracks, &board);
        assert_eq!(host.tracks.len(), tracks.track_count());
        assert_eq!(board.len(), 0);
    }
}
