//! The main thread's half of the VST3 story: instances, placement, editors.
//!
//! [`Vst3InstanceHost`] owns the plugins; [`Vst3Host`] knows *where each one is*.
//! The direct counterpart of [`ClapHost`](crate::clap_host::ClapHost), driven from
//! the same run loop:
//!
//! ```text
//! RunEvent::MainEventsCleared  ->  ClapHost::tick(..)     serve CLAP requests
//!                                  Vst3Host::tick(..)     serve VST3 requests
//! RunEvent::Exit               ->  both shutdown()
//! ```
//!
//! # The master insert holds one plugin, of either format
//!
//! `TrackHost::master_insert()` is a single slot. Installing a VST3 node into it
//! displaces whatever node was there, including a CLAP one -- and the *node* going
//! away is not the same as the *instance* going away. A host that stopped there
//! would leave a CLAP plugin loaded, activated, invisible and un-freeable, with
//! the mixer still drawing it.
//!
//! So placement is symmetric and explicit. Whichever host places a master effect
//! tells the other to forget its own ([`evict_master`](Vst3Host::evict_master),
//! and `ClapHost::evict_master`), which destroys the displaced instance and
//! clears it from the board. The run loop wires the direction this host cannot
//! see for itself; see `mydaw_app::run`.
//!
//! # Layout edits rebuild the graph for *both* formats
//!
//! Adding or removing a track recompiles the whole graph, and a hosted plugin
//! cannot be in two graphs at once. Step 6 solved that for CLAP by installing a
//! plugin-free graph and re-attaching afterwards, at the cost of a few
//! milliseconds of silence from the hosted plugins only.
//!
//! With two formats loaded the same rebuild has to re-attach both, and only one
//! of the two hosts can own the operation. [`Vst3Host::add_track`] and
//! [`Vst3Host::remove_track`] therefore take `&mut ClapHost` and re-attach it too;
//! `AppState` routes a layout edit here whenever any VST3 plugin is placed, and to
//! the CLAP host otherwise. One rebuild, both formats re-attached, in that order.

use std::path::Path;
use std::sync::Arc;

use mydaw_plugin_audio_io::{AudioIoError, AudioIoService};
use mydaw_plugin_host_vst3::{ClassId, Vst3BusLayout, Vst3InstanceHost, Vst3InstanceId};
use mydaw_plugin_windows::PluginWindowService;

use crate::clap_bridge::ClapTarget;
use crate::clap_host::ClapHost;
use crate::source::{Source, Vst3SourceRef};
use crate::tracks::TrackHost;
use crate::vst3_bridge::{
    reply, Vst3Answer, Vst3Board, Vst3Command, Vst3GuiStatusDto, Vst3HostBridge, Vst3SlotDto,
    Vst3Target,
};

/// Owns every hosted VST3 plugin and remembers where each one sits.
///
/// **Main thread only** -- see the module docs. Not `Send`, by construction.
pub struct Vst3Host {
    instances: Vst3InstanceHost,
    /// One entry per track, in track order. `None` where the track's source is
    /// not a VST3 plugin.
    tracks: Vec<Option<Vst3InstanceId>>,
    master: Option<Vst3InstanceId>,
}

impl Vst3Host {
    /// A host with nothing placed, opening editor windows through `windows`.
    pub fn new(windows: Arc<PluginWindowService>) -> Self {
        Self {
            instances: Vst3InstanceHost::new(windows),
            tracks: Vec::new(),
            master: None,
        }
    }

    /// One pass of the run loop's VST3 obligations.
    ///
    /// `clap` is here because two operations genuinely span both hosts: placing a
    /// master effect (which evicts the other format's) and a layout edit (which
    /// rebuilds one graph both must be re-attached to).
    pub fn tick(
        &mut self,
        tracks: &TrackHost,
        audio: &AudioIoService,
        bridge: &Vst3HostBridge,
        clap: &mut ClapHost,
    ) {
        let pending = bridge.drain();
        let mut changed = !pending.is_empty();
        for (command, channel) in pending {
            let answer = self.serve(command, tracks, audio, clap);
            reply(&channel, answer);
        }

        // Editors are owed to every instance whether or not anything was asked
        // of it: a user may close an editor with its own close button.
        let editors_before = self.instances.open_editors();
        self.instances.tick();
        changed |= editors_before != self.instances.open_editors();

        if changed {
            self.publish(tracks, bridge.board());
        }
    }

    /// Publish the current placement for the command threads to read.
    pub fn publish(&mut self, tracks: &TrackHost, board: &Vst3Board) {
        let count = tracks.track_count();
        self.tracks.resize(count, None);
        let placements: Vec<Option<Vst3InstanceId>> = self.tracks[..count].to_vec();
        let slots = placements
            .into_iter()
            .map(|placement| placement.and_then(|id| self.slot_dto(id)))
            .collect();
        let master = self.master.and_then(|id| self.slot_dto(id));
        board.publish(slots, master);
    }

    /// Close every editor and destroy every instance, in the required order.
    ///
    /// The shell calls this on `RunEvent::Exit`, **after** audio has stopped.
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

    /// The instance behind a target.
    pub fn instance_at(&self, target: Vst3Target) -> Option<Vst3InstanceId> {
        match target {
            ClapTarget::Track { index } => self.tracks.get(index).copied().flatten(),
            ClapTarget::Master => self.master,
        }
    }

    /// The instance host, for tests and for a project loader.
    pub fn instances_mut(&mut self) -> &mut Vst3InstanceHost {
        &mut self.instances
    }

    /// Forget the master placement, destroying the instance behind it.
    ///
    /// Called by the *other* format's host when it takes the master insert: the
    /// node this instance owned has already been displaced by that install, so
    /// there is nothing to un-install here -- only an instance to free and a
    /// board entry to clear. Doing it any other way leaves a plugin loaded that
    /// nothing points at.
    pub fn evict_master(&mut self) -> bool {
        match self.master.take() {
            Some(instance) => {
                log::info!("VST3 master effect displaced by a plugin of another format");
                self.instances.destroy(instance);
                true
            }
            None => false,
        }
    }

    // ------------------------------------------------------------- dispatch

    fn serve(
        &mut self,
        command: Vst3Command,
        tracks: &TrackHost,
        audio: &AudioIoService,
        clap: &mut ClapHost,
    ) -> Result<Vst3Answer, String> {
        match command {
            Vst3Command::SetTrackSource {
                track,
                module_path,
                cid,
            } => {
                let (sample_rate, max_block) = stream_info(audio)?;
                self.place_track_source(
                    tracks,
                    track,
                    &module_path,
                    &parse_cid(&cid)?,
                    sample_rate,
                    max_block,
                )
                .map(|()| Vst3Answer::Done)
            }
            Vst3Command::SetMasterEffect { module_path, cid } => {
                let (sample_rate, max_block) = stream_info(audio)?;
                self.place_master_effect(
                    tracks,
                    &module_path,
                    &parse_cid(&cid)?,
                    sample_rate,
                    max_block,
                    clap,
                )
                .map(|()| Vst3Answer::Done)
            }
            Vst3Command::SetTrackBuiltIn { track, source_id } => {
                let (sample_rate, max_block) = stream_info(audio)?;
                self.replace_with_built_in(tracks, track, &source_id, sample_rate, max_block)
                    .map(|()| Vst3Answer::Done)
            }
            Vst3Command::ClearMasterEffect => {
                self.clear_master_effect(tracks).map(|()| Vst3Answer::Done)
            }
            Vst3Command::OpenGui(target) => self.open_gui(target).map(Vst3Answer::Gui),
            Vst3Command::CloseGui(target) => self.close_gui(target).map(Vst3Answer::Gui),
            Vst3Command::SaveState(target) => self.save_state(target).map(Vst3Answer::State),
            Vst3Command::LoadState { target, state } => {
                self.load_state(target, &state).map(|()| Vst3Answer::Done)
            }
            Vst3Command::AddTrack { source_id, name } => self
                .add_track(tracks, audio, &source_id, name, clap)
                .map(|()| Vst3Answer::Done),
            Vst3Command::RemoveTrack { track } => self
                .remove_track(tracks, audio, track, clap)
                .map(|()| Vst3Answer::Done),
        }
    }

    // -------------------------------------------------------------- placement

    /// Put a hosted VST3 instrument at the head of `track`.
    ///
    /// # Errors
    /// A sentence for the status bar: a module that will not load, a plugin
    /// whose buses are not an instrument's, a slot with no stream behind it.
    pub fn place_track_source(
        &mut self,
        tracks: &TrackHost,
        track: usize,
        module_path: &Path,
        cid: &ClassId,
        sample_rate: f64,
        max_block: usize,
    ) -> Result<(), String> {
        if track >= tracks.track_count() {
            return Err(format!("there is no track {track}"));
        }

        let instance = self
            .instances
            .create(module_path, *cid)
            .map_err(|err| err.to_string())?;

        // The authoritative check. The picker filters by the class's declared
        // `subCategories`, which is free but is only the plugin's own claim;
        // this is the bus layout it reported to the host.
        let outcome = self
            .require_layout(instance, Vst3BusLayout::is_instrument, "an instrument")
            .and_then(|()| {
                self.instances
                    .create_node(instance, sample_rate, max_block)
                    .map_err(|err| err.to_string())
            })
            .and_then(|node| {
                let reference = self.reference(instance);
                tracks
                    .install_source(track, Source::Vst3(reference), Box::new(node))
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
    /// swap would leave an instance hosted, activated and unreachable.
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

    /// Put a hosted VST3 effect on the master bus, displacing any other.
    ///
    /// # Errors
    /// A sentence for the status bar.
    pub fn place_master_effect(
        &mut self,
        tracks: &TrackHost,
        module_path: &Path,
        cid: &ClassId,
        sample_rate: f64,
        max_block: usize,
        clap: &mut ClapHost,
    ) -> Result<(), String> {
        let instance = self
            .instances
            .create(module_path, *cid)
            .map_err(|err| err.to_string())?;

        let outcome = self
            .require_layout(
                instance,
                Vst3BusLayout::is_stereo_effect,
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

        // The install above displaced whatever node was in the slot. If that was
        // a CLAP plugin, its instance is now orphaned -- see the module docs.
        clap.evict_master();
        if let Some(previous) = self.master.replace(instance) {
            self.instances.destroy(previous);
        }
        Ok(())
    }

    /// Empty the master insert and destroy the instance that was in it.
    ///
    /// Public for the same reason its CLAP counterpart is: a project load
    /// strips the session down before it builds the new one.
    ///
    /// # Errors
    /// A sentence, when there is no stream to empty the slot in.
    pub fn clear_master_effect(&mut self, tracks: &TrackHost) -> Result<(), String> {
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

    /// Append a track, then re-attach every hosted plugin of both formats.
    fn add_track(
        &mut self,
        tracks: &TrackHost,
        audio: &AudioIoService,
        source_id: &str,
        name: Option<String>,
        clap: &mut ClapHost,
    ) -> Result<(), String> {
        let source = Source::parse(source_id).map_err(|err| err.to_string())?;
        tracks
            .add_track(source, name, audio)
            .map_err(|err| err.to_string())?;
        self.tracks.resize(tracks.track_count(), None);
        self.reattach_both(tracks, audio, clap)
    }

    /// Remove a track, renumber the placements after it, and re-attach.
    fn remove_track(
        &mut self,
        tracks: &TrackHost,
        audio: &AudioIoService,
        track: usize,
        clap: &mut ClapHost,
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
        let outcome = self.reattach_both(tracks, audio, clap);
        if let Some(instance) = doomed {
            self.instances.destroy(instance);
        }
        outcome
    }

    fn reattach_both(
        &mut self,
        tracks: &TrackHost,
        audio: &AudioIoService,
        clap: &mut ClapHost,
    ) -> Result<(), String> {
        let (sample_rate, max_block) = stream_info(audio)?;
        // VST3 first, then CLAP: the order does not matter to correctness (the
        // two touch disjoint slots) but a fixed order makes a failure message
        // reproducible.
        let ours = self.reattach_all(tracks, sample_rate, max_block);
        let theirs = clap.reattach_all(tracks, sample_rate, max_block);
        match (ours, theirs) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(one), Ok(())) | (Ok(()), Err(one)) => Err(one),
            (Err(one), Err(two)) => Err(format!("{one}; {two}")),
        }
    }

    /// Re-activate every placed VST3 instance into the graph that is now
    /// installed.
    ///
    /// Public for the same reason as its CLAP counterpart: an offline test can
    /// build and install a real graph but cannot open a real device.
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
                        tracks.install_source(index, Source::Vst3(reference), Box::new(node))
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
                "the graph was rebuilt, but a hosted VST3 plugin could not be re-attached: {}",
                failures.join("; ")
            ))
        }
    }

    // -------------------------------------------------------------------- gui

    fn open_gui(&mut self, target: Vst3Target) -> Result<Vst3GuiStatusDto, String> {
        let instance = self.require(target)?;
        let title = self.instances.info(instance).map_or_else(
            || "VST3 plugin — MYDAW".to_owned(),
            |info| {
                format!(
                    "{} — MYDAW",
                    if info.name.is_empty() {
                        info.cid.to_hex()
                    } else {
                        info.name.clone()
                    }
                )
            },
        );
        let state = self
            .instances
            .open_gui(instance, &title)
            .map_err(|err| user_message(&err))?;
        Ok(self.gui_status(instance, state))
    }

    fn close_gui(&mut self, target: Vst3Target) -> Result<Vst3GuiStatusDto, String> {
        let instance = self.require(target)?;
        let state = self.instances.close_gui(instance);
        Ok(self.gui_status(instance, state))
    }

    fn gui_status(
        &self,
        instance: Vst3InstanceId,
        state: mydaw_plugin_host_vst3::Vst3GuiState,
    ) -> Vst3GuiStatusDto {
        Vst3GuiStatusDto {
            open: state.open,
            has_editor: state.has_editor,
            cid: self.instances.info(instance).map(|info| info.cid.to_hex()),
            width: state.width,
            height: state.height,
        }
    }

    // ------------------------------------------------------------------ state

    fn save_state(&mut self, target: Vst3Target) -> Result<Vec<u8>, String> {
        let instance = self.require(target)?;
        self.instances
            .save_state(instance)
            .map_err(|err| err.to_string())
    }

    fn load_state(&mut self, target: Vst3Target, bytes: &[u8]) -> Result<(), String> {
        let instance = self.require(target)?;
        self.instances
            .load_state(instance, bytes)
            .map_err(|err| err.to_string())
    }

    // ------------------------------------------------------------------ plumb

    /// The instance at `target`, or a sentence saying there is none.
    fn require(&self, target: Vst3Target) -> Result<Vst3InstanceId, String> {
        self.instance_at(target)
            .ok_or_else(|| format!("there is no hosted VST3 plugin on {target}"))
    }

    /// Refuse a plugin whose declared buses are not the shape this slot needs.
    fn require_layout(
        &self,
        instance: Vst3InstanceId,
        accepts: fn(&Vst3BusLayout) -> bool,
        expected: &str,
    ) -> Result<(), String> {
        let Some(layout) = self.instances.bus_layout(instance) else {
            return Err(format!("there is no hosted VST3 instance {instance}"));
        };
        if accepts(&layout) {
            return Ok(());
        }
        let name = self
            .instances
            .info(instance)
            .map_or_else(String::new, |info| info.name.clone());
        Err(format!(
            "'{name}' is not {expected}: it declares {} audio input bus(es) x {} ch and \
             {} output bus(es) x {} ch",
            layout.audio_input_buses,
            layout.main_input_channels,
            layout.audio_output_buses,
            layout.main_output_channels,
        ))
    }

    /// What a track's `TrackDef` should record for this instance.
    fn reference(&self, instance: Vst3InstanceId) -> Vst3SourceRef {
        self.instances.info(instance).map_or_else(
            || Vst3SourceRef::new("", ClassId([0; 16])),
            |info| Vst3SourceRef::new(info.path.clone(), info.cid).named(info.name.clone()),
        )
    }

    /// One placed plugin, in the shape the mixer reads.
    fn slot_dto(&mut self, instance: Vst3InstanceId) -> Option<Vst3SlotDto> {
        let (cid, name, vendor, module_path) = {
            let info = self.instances.info(instance)?;
            (
                info.cid.to_hex(),
                if info.name.is_empty() {
                    info.cid.to_hex()
                } else {
                    info.name.clone()
                },
                info.vendor.clone(),
                info.path.display().to_string(),
            )
        };
        let gui = self.instances.gui_state(instance).unwrap_or_default();
        Some(Vst3SlotDto {
            instance: instance.get(),
            cid,
            name,
            vendor,
            module_path,
            has_editor: gui.has_editor,
            gui_open: gui.open,
        })
    }
}

impl std::fmt::Debug for Vst3Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vst3Host")
            .field("instances", &self.instances.len())
            .field("tracks", &self.tracks)
            .field("master", &self.master)
            .field("open_editors", &self.instances.open_editors())
            .finish()
    }
}

/// Parse a class id from the wire.
///
/// # Errors
/// A sentence, for a string that is not 32 hex digits -- reachable from a
/// hand-edited project file or a stale UI.
pub fn parse_cid(text: &str) -> Result<ClassId, String> {
    ClassId::from_hex(text).ok_or_else(|| {
        format!("'{text}' is not a VST3 class id (32 hexadecimal digits are expected)")
    })
}

/// The stream's sample rate and maximum block, or the sentence saying there is
/// no stream.
fn stream_info(audio: &AudioIoService) -> Result<(f64, usize), String> {
    audio
        .stream_info()
        .map(|info| (f64::from(info.sample_rate), info.max_block))
        .ok_or_else(|| AudioIoError::NotRunning.to_string())
}

/// The sentence the status bar shows for a failed editor open.
///
/// `NoEditor` is the common one and deserves plain language: a plugin with no
/// editor is normal, not broken.
pub fn user_message(error: &mydaw_plugin_host_vst3::Vst3InstanceError) -> String {
    use mydaw_plugin_host_vst3::{Vst3GuiError, Vst3InstanceError};
    match error {
        Vst3InstanceError::Gui(Vst3GuiError::NoEditor) => "This plugin has no editor.".to_owned(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_plugin_host_vst3::Vst3GuiError;

    fn host() -> Vst3Host {
        Vst3Host::new(Arc::new(PluginWindowService::new()))
    }

    fn clap_host() -> ClapHost {
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
        let board = Vst3Board::new();

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
        let mut clap = clap_host();
        let tracks = track_host();
        let audio = AudioIoService::default();

        for target in [ClapTarget::Track { index: 0 }, ClapTarget::Master] {
            for command in [
                Vst3Command::OpenGui(target),
                Vst3Command::CloseGui(target),
                Vst3Command::SaveState(target),
                Vst3Command::LoadState {
                    target,
                    state: vec![1, 2, 3],
                },
            ] {
                let err = host
                    .serve(command.clone(), &tracks, &audio, &mut clap)
                    .expect_err("nothing is placed");
                assert!(
                    err.contains("no hosted VST3 plugin"),
                    "{command:?} produced {err}"
                );
            }
        }
    }

    #[test]
    fn a_command_without_an_audio_device_names_that_rather_than_the_plugin() {
        // Activating a plugin needs the stream's sample rate and block size, so
        // "there is no device" is the honest answer and the first one to give.
        let mut host = host();
        let mut clap = clap_host();
        let tracks = track_host();
        let audio = AudioIoService::default();

        let err = host
            .serve(
                Vst3Command::SetTrackSource {
                    track: 0,
                    module_path: std::env::temp_dir().join("nothing.vst3"),
                    cid: "00000000000000000000000000000000".to_owned(),
                },
                &tracks,
                &audio,
                &mut clap,
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
    fn a_class_id_that_is_not_hex_is_reported_rather_than_guessed_at() {
        // Reachable from a hand-edited project file.
        assert!(parse_cid("not-a-class-id").is_err());
        assert!(parse_cid("").is_err());
        let err = parse_cid("zzzz").unwrap_err();
        assert!(err.contains("32 hexadecimal digits"), "{err}");

        let good = "0123456789ABCDEF0123456789ABCDEF";
        assert_eq!(parse_cid(good).unwrap().to_hex(), good);
    }

    #[test]
    fn evicting_a_master_that_is_not_there_is_not_an_error() {
        // The reverse-direction call the run loop makes unconditionally.
        let mut host = host();
        assert!(!host.evict_master());
        assert!(host.instance_at(ClapTarget::Master).is_none());
    }

    #[test]
    fn the_no_editor_message_is_plain_language_and_everything_else_keeps_its_detail() {
        assert_eq!(
            user_message(&Vst3GuiError::NoEditor.into()),
            "This plugin has no editor."
        );
        let other: mydaw_plugin_host_vst3::Vst3InstanceError =
            Vst3GuiError::UnsupportedApi { api: "HWND" }.into();
        assert!(user_message(&other).contains("HWND"));
    }

    #[test]
    fn the_board_is_published_with_one_entry_per_track_even_when_none_are_hosted() {
        // The mixer indexes into it, so a short vector would silently report
        // "no plugin" for the tracks past its end.
        let mut host = host();
        let tracks = track_host();
        let board = Vst3Board::new();
        host.publish(&tracks, &board);
        assert_eq!(host.tracks.len(), tracks.track_count());
        assert_eq!(board.len(), 0);
    }
}
