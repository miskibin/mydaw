//! The main thread's half of the project story: gathering a session, and
//! rebuilding one.
//!
//! [`crate::project_file`] knows what a `.mydaw` file *is*. This knows how to
//! fill one in from a running session and how to turn a running session into
//! what one describes -- which is main-thread work from end to end, because both
//! halves have to talk to [`ClapHost`] and [`Vst3Host`], and a hosted plugin is
//! `!Send` by construction.
//!
//! # Saving is a fan-out
//! ```text
//!   TrackHost::project()  -> tracks, faders, pans, mutes, solos, sources
//!   SequencerService      -> pattern, tempo
//!   ClapHost / Vst3Host   -> one state blob per placed instance
//!                            + what is on the master insert
//!                                     |
//!                                     v
//!                            ProjectFile -> atomic write
//! ```
//! A plugin that refuses to save is a **warning, not a failure**. Its track is
//! written out with an empty blob and the user is told; refusing to save the
//! whole session because one plugin's serialiser returned an error would lose
//! far more than it protected.
//!
//! # Loading is a teardown and a rebuild, in that order
//!
//! The obvious implementation -- walk the file and set each thing -- does not
//! work, for the reason that has shaped every plugin-facing decision in this
//! codebase: **a hosted plugin cannot be in two graphs at once**, and the
//! instances that are loaded now have nothing to do with the ones the file asks
//! for. So a load is four phases, and the order between them is forced:
//!
//! 1. **Stop.** The transport stops. Sequenced notes end with it.
//! 2. **Strip.** Every placed instance is replaced by a built-in and destroyed,
//!    and the master insert is emptied. This is exactly what the mixer's own
//!    source picker does when a user switches a track off a plugin, so it is the
//!    path that is already proven -- and afterwards nothing is hosted, which is
//!    what makes phase 3 an ordinary graph rebuild rather than a re-attach
//!    dance.
//! 3. **Rebuild.** The new project is installed and a new graph compiled from
//!    it. A track the file says is plugin-headed compiles to
//!    [`SilentNode`](crate::source::SilentNode) -- the placeholder that has
//!    existed since step 6 for exactly this moment. **Every instrument node in
//!    the session is new, which is also what flushes any note a user was
//!    holding**: there is no old voice left anywhere to hang.
//! 4. **Attach.** Each plugin the file names is created, activated, swapped into
//!    its slot and given its saved state.
//!
//! Only phase 4 can partially fail, and it is designed to: see
//! [`MissingPlugins`].
//!
//! # Why "new project" is a load
//! [`ProjectHost::new_project`] builds the default [`Project`] into a
//! [`ProjectFile`] and loads it. One code path, so "New" cannot drift away from
//! "Open" in what it resets -- which is the bug every DAW with two of these has
//! had at least once.

use std::path::{Path, PathBuf};

use mydaw_plugin_audio_io::{AudioIoError, AudioIoService};
use mydaw_plugin_sequencer::{PlayMode, Playlist, SequencerService};
use mydaw_rt_core::DEFAULT_BPM;

use crate::clap_bridge::{ClapBoard, ClapTarget};
use crate::clap_host::ClapHost;
use crate::project::{MissingPluginDto, MissingPlugins, Project};
use crate::project_bridge::{reply, ProjectAnswer, ProjectBridge, ProjectCommand, ProjectInfoDto};
use crate::project_file::{
    project_name, read_project_file, write_project_file, EffectEntry, EffectRef, ProjectFile,
    ProjectFileError, Song, SourceEntry,
};
use crate::tracks::TrackHost;
use crate::vst3_bridge::Vst3Board;
use crate::vst3_host::Vst3Host;

/// The `Send + Sync` half of the session: everything a project touches that is
/// not a plugin instance.
///
/// A borrow bundle rather than an owned type, because every one of these already
/// lives in an `Arc` in [`AppState`](crate::AppState) and the run loop holds
/// clones of them. Grouping them keeps the signatures below readable and keeps
/// "what a project consists of" in one place.
pub struct Session<'a> {
    pub tracks: &'a TrackHost,
    pub audio: &'a AudioIoService,
    pub sequencer: &'a SequencerService,
}

/// The two plugin hosts and the boards they publish to.
///
/// Both hosts travel together because a project spans both: a session can have
/// CLAP instruments on some tracks, VST3 on others, and either format on the one
/// master insert. The boards come along because a load changes every placement
/// and nothing else will republish them -- each host's own `tick` publishes only
/// when *its* mailbox had traffic, and a project load goes through neither.
pub struct Hosts<'a> {
    pub clap: &'a mut ClapHost,
    pub clap_board: &'a ClapBoard,
    pub vst3: &'a mut Vst3Host,
    pub vst3_board: &'a Vst3Board,
}

/// What a load did, beyond succeeding.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LoadReport {
    /// One entry per track, in track order: the plugin that track asked for and
    /// did not get.
    pub missing_tracks: Vec<Option<MissingPluginDto>>,
    /// The master effect that could not be loaded.
    pub missing_master: Option<MissingPluginDto>,
    /// Sentences for the status bar: plugins that refused their saved state, a
    /// load with no audio device, a graph that would not install.
    pub warnings: Vec<String>,
}

impl LoadReport {
    /// How many plugins the project asked for and did not get.
    pub fn missing_count(&self) -> usize {
        self.missing_tracks
            .iter()
            .filter(|slot| slot.is_some())
            .count()
            + usize::from(self.missing_master.is_some())
    }

    /// Push this report onto the board the mixer reads.
    pub fn publish(&self, missing: &MissingPlugins) {
        missing.publish(self.missing_tracks.clone(), self.missing_master.clone());
    }
}

/// Owns which project file the session came from.
///
/// **Main thread only**, like its siblings -- not because this type is `!Send`
/// (it is a `PathBuf`) but because every method takes the plugin hosts, which
/// are.
#[derive(Debug, Default)]
pub struct ProjectHost {
    path: Option<PathBuf>,
}

impl ProjectHost {
    /// A host on an unsaved session.
    pub fn new() -> Self {
        Self::default()
    }

    /// The file the session came from, if any.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// One pass of the run loop's project obligations.
    ///
    /// Answers every queued request in order, then republishes the project
    /// information and the missing-plugin warnings -- and the two plugin boards,
    /// because a load moved everything on them.
    pub fn tick(&mut self, session: &Session<'_>, bridge: &ProjectBridge, hosts: &mut Hosts<'_>) {
        let pending = bridge.drain();
        if pending.is_empty() {
            return;
        }
        for (command, channel) in pending {
            let answer = self.serve(command, session, bridge, hosts);
            reply(&channel, answer.map(ProjectAnswer::Info));
        }
        hosts.clap.publish(session.tracks, hosts.clap_board);
        hosts.vst3.publish(session.tracks, hosts.vst3_board);
    }

    fn serve(
        &mut self,
        command: ProjectCommand,
        session: &Session<'_>,
        bridge: &ProjectBridge,
        hosts: &mut Hosts<'_>,
    ) -> Result<ProjectInfoDto, String> {
        match command {
            ProjectCommand::New => self.new_project(session, bridge, hosts),
            ProjectCommand::Open { path } => self.open(session, bridge, hosts, &path),
            ProjectCommand::Save { path } => self.save(session, bridge, hosts, &path),
        }
    }

    // ------------------------------------------------------------------ save

    /// Write the session to `path`, which becomes the current project.
    ///
    /// # Errors
    /// A sentence naming the file: it could not be written, or the session
    /// somehow holds a value that would not survive JSON (see
    /// [`ProjectFile::validate`]).
    pub fn save(
        &mut self,
        session: &Session<'_>,
        bridge: &ProjectBridge,
        hosts: &mut Hosts<'_>,
        path: &Path,
    ) -> Result<ProjectInfoDto, String> {
        let (file, warnings) = capture(session, hosts);
        write_project_file(path, &file).map_err(|err| named(path, &err))?;
        log::info!(
            "saved {} ({} track(s), {} pattern(s), {} clip(s))",
            path.display(),
            file.tracks.len(),
            file.patterns.len(),
            file.playlist.clips.len()
        );

        self.path = Some(path.to_path_buf());
        // A save does not change which plugins are missing -- a track whose
        // plugin never loaded is still missing it, and its reference has just
        // been written back out, which is the point.
        let info = ProjectInfoDto {
            missing_plugins: bridge.missing().len(),
            warnings,
            ..ProjectInfoDto::at(Some(path))
        };
        bridge.publish(info.clone());
        Ok(info)
    }

    // ------------------------------------------------------------------ open

    /// Rebuild the whole session from `path`.
    ///
    /// # Errors
    /// A sentence naming the file, for anything that makes it unreadable or
    /// unbelievable: it is not there, it is not a project, it was written by a
    /// newer MYDAW, or its contents are not a session the engine could run. **A
    /// plugin that is merely absent from this machine is not one of those** --
    /// see the module docs.
    pub fn open(
        &mut self,
        session: &Session<'_>,
        bridge: &ProjectBridge,
        hosts: &mut Hosts<'_>,
        path: &Path,
    ) -> Result<ProjectInfoDto, String> {
        let file = read_project_file(path).map_err(|err| named(path, &err))?;
        let report = self
            .load(session, hosts, &file)
            .map_err(|err| named(path, &err))?;
        self.path = Some(path.to_path_buf());
        Ok(self.publish_loaded(bridge, Some(path), &report))
    }

    /// Reset the session to a fresh default project: Drums and Synth, an empty
    /// pattern, the default tempo.
    ///
    /// # Errors
    /// Only what a load of the default project could report, which is nothing a
    /// user can cause -- the default project is built here, not read.
    pub fn new_project(
        &mut self,
        session: &Session<'_>,
        bridge: &ProjectBridge,
        hosts: &mut Hosts<'_>,
    ) -> Result<ProjectInfoDto, String> {
        let file = default_project_file();
        let report = self
            .load(session, hosts, &file)
            .map_err(|err| err.to_string())?;
        self.path = None;
        Ok(self.publish_loaded(bridge, None, &report))
    }

    /// Publish everything a finished load changed, and describe it.
    fn publish_loaded(
        &self,
        bridge: &ProjectBridge,
        path: Option<&Path>,
        report: &LoadReport,
    ) -> ProjectInfoDto {
        report.publish(bridge.missing());
        let info = ProjectInfoDto {
            path: path.map(|p| p.display().to_string()),
            name: project_name(path),
            missing_plugins: report.missing_count(),
            warnings: report.warnings.clone(),
        };
        bridge.publish(info.clone());
        for warning in &report.warnings {
            log::warn!("project: {warning}");
        }
        log::info!(
            "opened '{}': {} missing plugin(s), {} warning(s)",
            info.name,
            info.missing_plugins,
            info.warnings.len()
        );
        info
    }

    /// Load an already-parsed project: stop, strip, rebuild, attach.
    ///
    /// # Errors
    /// [`ProjectFileError`] for a file whose contents the engine could not run.
    /// **Nothing is changed when this fails**: the whole file is validated
    /// before the session is touched.
    pub fn load(
        &mut self,
        session: &Session<'_>,
        hosts: &mut Hosts<'_>,
        file: &ProjectFile,
    ) -> Result<LoadReport, ProjectFileError> {
        let stream = session
            .audio
            .stream_info()
            .map(|info| (f64::from(info.sample_rate), info.max_block));
        let mut report = self.prepare(session, hosts, file, stream)?;
        match stream {
            Some((sample_rate, max_block)) => {
                attach(session, hosts, file, sample_rate, max_block, &mut report);
            }
            None if file_has_plugins(file) => report.warnings.push(
                "there is no audio device, so this project's plugins were not loaded".to_owned(),
            ),
            None => {}
        }
        Ok(report)
    }

    /// Phases 1 to 3 of a load: stop, strip, rebuild -- everything up to but not
    /// including the plugins.
    ///
    /// Split from [`attach`] and made public for the same reason
    /// [`ClapHost::place_track_source`] takes an audio configuration rather than
    /// looking one up: the step between them is *installing the graph*, which is
    /// the audio device's job, and an offline test has to do it by hand. Running
    /// the two halves is then the same sequence in the application and in the
    /// test, rather than two implementations of it.
    ///
    /// `stream` is the sample rate and maximum block the plugins will be
    /// activated for, or `None` when there is no device -- in which case nothing
    /// can be hosted and the strip is a teardown rather than a swap.
    ///
    /// # Errors
    /// See [`load`](Self::load). Nothing is changed when this fails.
    pub fn prepare(
        &mut self,
        session: &Session<'_>,
        hosts: &mut Hosts<'_>,
        file: &ProjectFile,
        stream: Option<(f64, usize)>,
    ) -> Result<LoadReport, ProjectFileError> {
        // Validate everything first, so a file the engine cannot run leaves the
        // session exactly as it was rather than half replaced.
        let (project, selected) = file.to_project()?;
        let (patterns, playlist) = file.to_song()?;
        let mode = file.to_play_mode();
        let bpm = file.to_bpm()?;

        let mut report = LoadReport {
            missing_tracks: vec![None; project.len()],
            ..LoadReport::default()
        };

        // 1. Stop. Sequenced notes end here; held ones end in phase 3, when
        //    every instrument node in the session is replaced.
        if let Err(err) = session.audio.set_playing(false) {
            if !matches!(err, AudioIoError::NotRunning) {
                report
                    .warnings
                    .push(format!("the transport did not stop: {err}"));
            }
        }

        // 2. Strip: nothing hosted may survive into the new session.
        strip(session, hosts, stream, &mut report);

        // 3. Rebuild.
        session.tracks.set_project(project);
        match session.tracks.install_graph(session.audio) {
            Ok(()) | Err(AudioIoError::NotRunning) => {}
            Err(err) => report
                .warnings
                .push(format!("the audio graph could not be rebuilt: {err}")),
        }
        // The song and the tempo were validated above, so none of these can
        // reject; publishing them to the stream can, and a busy queue is not a
        // reason to fail a load.
        let _ = session.sequencer.set_song(patterns, playlist);
        let _ = session.audio.install_song(session.sequencer.compile());
        session.sequencer.set_bpm(bpm);
        let _ = session.audio.set_bpm(bpm);
        session.sequencer.set_play_mode(mode);
        let _ = session.audio.set_play_mode(mode);
        let _ = session.tracks.set_selected(selected);

        Ok(report)
    }
}

// ---------------------------------------------------------------------------
// The phases
// ---------------------------------------------------------------------------

/// Gather the session into a file, and everything worth warning about.
///
/// Public so the offline tests can assert on what a save *would* write without
/// going near a filesystem.
pub fn capture(session: &Session<'_>, hosts: &mut Hosts<'_>) -> (ProjectFile, Vec<String>) {
    let project = session.tracks.project();
    let mut warnings = Vec::new();
    let mut states = Vec::with_capacity(project.len());

    for index in 0..project.len() {
        let name = project.tracks[index].name.clone();
        states.push(save_state_at(
            hosts,
            ClapTarget::Track { index },
            &format!("track {index} ('{name}')"),
            &mut warnings,
        ));
    }

    let master_effect = capture_master(hosts, &mut warnings);
    let (patterns, playlist) = session.sequencer.song();
    let file = ProjectFile::capture(
        &project,
        session.tracks.selected(),
        session.sequencer.bpm(),
        &Song {
            patterns: &patterns,
            playlist: &playlist,
            mode: session.sequencer.play_mode(),
            audio_sources: Vec::new(),
        },
        &states,
        master_effect,
    );
    (file, warnings)
}

/// One slot's plugin state, or an empty blob and a warning.
fn save_state_at(
    hosts: &mut Hosts<'_>,
    target: ClapTarget,
    label: &str,
    warnings: &mut Vec<String>,
) -> Vec<u8> {
    if let Some(instance) = hosts.clap.instance_at(target) {
        return match hosts.clap.instances_mut().save_state(instance) {
            Ok(bytes) => bytes,
            Err(err) => {
                // A plugin that will not serialise costs that plugin's settings,
                // not the user's session.
                warnings.push(format!(
                    "{label}: the plugin would not save its settings ({err}); \
                     the rest of the project was saved"
                ));
                Vec::new()
            }
        };
    }
    if let Some(instance) = hosts.vst3.instance_at(target) {
        return match hosts.vst3.instances_mut().save_state(instance) {
            Ok(bytes) => bytes,
            Err(err) => {
                warnings.push(format!(
                    "{label}: the plugin would not save its settings ({err}); \
                     the rest of the project was saved"
                ));
                Vec::new()
            }
        };
    }
    Vec::new()
}

/// The master insert, as a file records it.
///
/// Unlike a track source, the insert's plugin is not in the [`Project`] at all
/// -- the mixer has no field for it -- so its locator is read back out of
/// whichever host is holding the instance.
fn capture_master(hosts: &mut Hosts<'_>, warnings: &mut Vec<String>) -> Option<EffectEntry> {
    let target = ClapTarget::Master;
    let plugin = if let Some(instance) = hosts.clap.instance_at(target) {
        let (path, plugin_id) = hosts.clap.instances_mut().locator(instance)?;
        let name = hosts
            .clap
            .instances_mut()
            .info(instance)
            .map_or_else(String::new, |info| info.name.clone());
        EffectRef::clap(path.to_string_lossy().into_owned(), plugin_id, name)
    } else if let Some(instance) = hosts.vst3.instance_at(target) {
        let (path, cid) = hosts.vst3.instances_mut().locator(instance)?;
        let name = hosts
            .vst3
            .instances_mut()
            .info(instance)
            .map_or_else(String::new, |info| info.name.clone());
        EffectRef::vst3(path.to_string_lossy().into_owned(), cid.to_hex(), name)
    } else {
        return None;
    };

    let bytes = save_state_at(hosts, target, "the master effect", warnings);
    Some(EffectEntry {
        plugin,
        state_blob: crate::clap_bridge::to_base64(&bytes),
    })
}

/// Phase 2: leave nothing hosted.
///
/// Every placed instrument is replaced by a built-in and destroyed, and the
/// master insert is emptied. With no device there can be no live nodes to
/// displace, so the instances are simply torn down where they stand.
fn strip(
    session: &Session<'_>,
    hosts: &mut Hosts<'_>,
    stream: Option<(f64, usize)>,
    report: &mut LoadReport,
) {
    let Some((rate, block)) = stream else {
        if hosts.clap.instance_count() + hosts.vst3.instance_count() > 0 {
            hosts.clap.shutdown();
            hosts.vst3.shutdown();
        }
        return;
    };
    let fallback = crate::source::Source::DEFAULT.id();

    for index in 0..session.tracks.track_count() {
        let target = ClapTarget::Track { index };
        if hosts.clap.instance_at(target).is_some() {
            if let Err(err) =
                hosts
                    .clap
                    .replace_with_built_in(session.tracks, index, &fallback, rate, block)
            {
                report.warnings.push(format!(
                    "track {index}: the plugin that was there could not be unloaded ({err})"
                ));
            }
        }
        if hosts.vst3.instance_at(target).is_some() {
            if let Err(err) =
                hosts
                    .vst3
                    .replace_with_built_in(session.tracks, index, &fallback, rate, block)
            {
                report.warnings.push(format!(
                    "track {index}: the plugin that was there could not be unloaded ({err})"
                ));
            }
        }
    }

    for outcome in [
        hosts.clap.clear_master_effect(session.tracks),
        hosts.vst3.clear_master_effect(session.tracks),
    ] {
        if let Err(err) = outcome {
            report
                .warnings
                .push(format!("the master effect could not be unloaded ({err})"));
        }
    }
}

/// Phase 4: create every plugin the file names, and give it its state.
///
/// Public, with the audio configuration supplied rather than looked up, for the
/// same reason [`ClapHost::place_track_source`] is: an offline test can build
/// and install a real graph but cannot open a real device.
pub fn attach(
    session: &Session<'_>,
    hosts: &mut Hosts<'_>,
    file: &ProjectFile,
    sample_rate: f64,
    max_block: usize,
    report: &mut LoadReport,
) {
    report.missing_tracks.resize(file.tracks.len(), None);

    for (index, entry) in file.tracks.iter().enumerate() {
        if !entry.source.is_hosted() {
            continue;
        }
        let target = ClapTarget::Track { index };
        let label = format!("track {index} ('{}')", entry.name);
        let placed = match &entry.source {
            SourceEntry::Clap(clap) => hosts.clap.place_track_source(
                session.tracks,
                index,
                Path::new(&clap.bundle_path),
                &clap.plugin_id,
                sample_rate,
                max_block,
            ),
            SourceEntry::Vst3(vst3) => crate::vst3_host::parse_cid(&vst3.cid).and_then(|cid| {
                hosts.vst3.place_track_source(
                    session.tracks,
                    index,
                    Path::new(&vst3.module_path),
                    &cid,
                    sample_rate,
                    max_block,
                )
            }),
            SourceEntry::Internal { .. } => unreachable!("filtered by is_hosted above"),
        };

        match placed {
            Ok(()) => {
                let state = file.track_state(index).unwrap_or_default();
                restore_state(hosts, target, &state, &label, &mut report.warnings);
            }
            Err(reason) => {
                report.missing_tracks[index] = Some(missing_dto(&entry.source, reason));
            }
        }
    }

    let Some(effect) = &file.master_effect else {
        return;
    };
    let Hosts { clap, vst3, .. } = hosts;
    let placed = match &effect.plugin {
        EffectRef::Clap(entry) => clap.place_master_effect(
            session.tracks,
            Path::new(&entry.bundle_path),
            &entry.plugin_id,
            sample_rate,
            max_block,
        ),
        EffectRef::Vst3(entry) => crate::vst3_host::parse_cid(&entry.cid).and_then(|cid| {
            vst3.place_master_effect(
                session.tracks,
                Path::new(&entry.module_path),
                &cid,
                sample_rate,
                max_block,
                clap,
            )
        }),
    };
    match placed {
        Ok(()) => {
            let state = file.master_state().unwrap_or_default();
            restore_state(
                hosts,
                ClapTarget::Master,
                &state,
                "the master effect",
                &mut report.warnings,
            );
        }
        Err(reason) => {
            report.missing_master = Some(MissingPluginDto {
                name: effect.plugin.label(),
                format: match effect.plugin {
                    EffectRef::Clap(_) => "clap",
                    EffectRef::Vst3(_) => "vst3",
                }
                .to_owned(),
                path: effect.plugin.path().to_owned(),
                reason,
            });
        }
    }
}

/// Hand a placed instance its saved settings.
///
/// An empty blob is nothing to do -- a plugin with no state extension saves one,
/// and that is a normal answer. A blob the plugin *refuses* is a warning and
/// nothing more: the plugin is loaded and playable, just at its defaults, and
/// throwing the whole track away over it would be a far larger loss.
fn restore_state(
    hosts: &mut Hosts<'_>,
    target: ClapTarget,
    state: &[u8],
    label: &str,
    warnings: &mut Vec<String>,
) {
    if state.is_empty() {
        return;
    }
    let outcome = if let Some(instance) = hosts.clap.instance_at(target) {
        hosts
            .clap
            .instances_mut()
            .load_state(instance, state)
            .map_err(|err| err.to_string())
    } else if let Some(instance) = hosts.vst3.instance_at(target) {
        hosts
            .vst3
            .instances_mut()
            .load_state(instance, state)
            .map_err(|err| err.to_string())
    } else {
        return;
    };
    if let Err(err) = outcome {
        warnings.push(format!(
            "{label}: the plugin refused its saved settings ({err}); it is at its defaults"
        ));
    }
}

/// The warning a strip draws for a plugin that would not load.
fn missing_dto(source: &SourceEntry, reason: String) -> MissingPluginDto {
    let (format, path) = match source {
        SourceEntry::Clap(entry) => ("clap", entry.bundle_path.clone()),
        SourceEntry::Vst3(entry) => ("vst3", entry.module_path.clone()),
        SourceEntry::Internal { id } => ("internal", id.clone()),
    };
    MissingPluginDto {
        name: source.label(),
        format: format.to_owned(),
        path,
        reason,
    }
}

/// Whether this project needs a plugin host at all.
fn file_has_plugins(file: &ProjectFile) -> bool {
    file.master_effect.is_some() || file.tracks.iter().any(|track| track.source.is_hosted())
}

/// The file "New project" loads: the shell's own default session.
///
/// A file rather than a special case, so New and Open reset exactly the same
/// things -- see the module docs.
pub fn default_project_file() -> ProjectFile {
    let project = Project::default();
    let patterns = crate::state::starter_pattern_set(&project);
    ProjectFile::capture(
        &project,
        0,
        DEFAULT_BPM,
        &Song {
            patterns: &patterns,
            playlist: &Playlist::default(),
            mode: PlayMode::Pattern,
            audio_sources: Vec::new(),
        },
        &[],
        None,
    )
}

/// Put the file name in front of the reason, so a status bar sentence says
/// which project it is about.
fn named(path: &Path, err: &ProjectFileError) -> String {
    match path.file_name() {
        Some(name) => format!("{}: {err}", name.to_string_lossy()),
        None => err.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_file::{ClapEntry, TrackEntry};
    use crate::source::{ClapSourceRef, Source};
    use mydaw_plugin_instruments::InstrumentRegistry;
    use mydaw_plugin_sequencer::PatternSet;
    use mydaw_plugin_test_tone::TestToneFactory;
    use mydaw_plugin_windows::PluginWindowService;
    use std::sync::Arc;

    /// A one-pattern song with a kick on `hits`, plus an arrangement of it.
    ///
    /// Both halves, because a load has to restore both and a test that only
    /// carried patterns would not notice an arrangement being dropped.
    fn beat(hits: &[usize]) -> (PatternSet, Playlist) {
        let mut pattern =
            mydaw_plugin_sequencer::Pattern::with_channels(1, "Pattern 1", &[0, 1], |_| 36);
        pattern.channels[0].steps =
            Some(mydaw_plugin_sequencer::StepRow::empty(36).with_steps(hits));
        let set = PatternSet::single(pattern);

        let mut playlist = Playlist::default();
        playlist.add_pattern_clip(1, 0, 0, 4 * 3_840).unwrap();
        (set, playlist)
    }

    /// A session with no audio device: the headless half of everything a load
    /// does, which is every field except the plugin instances.
    struct Rig {
        tracks: Arc<TrackHost>,
        audio: Arc<AudioIoService>,
        sequencer: Arc<SequencerService>,
        clap: ClapHost,
        clap_board: ClapBoard,
        vst3: Vst3Host,
        vst3_board: Vst3Board,
        host: ProjectHost,
        bridge: ProjectBridge,
    }

    impl Rig {
        fn new() -> Self {
            let windows = Arc::new(PluginWindowService::new());
            Self {
                tracks: Arc::new(TrackHost::new(
                    Arc::new(TestToneFactory::new()),
                    Arc::new(InstrumentRegistry::new()),
                    Arc::new(mydaw_plugin_sampler::SamplerRegistry::detached()),
                )),
                audio: Arc::new(AudioIoService::default()),
                sequencer: Arc::new(SequencerService::new()),
                clap: ClapHost::new(Arc::clone(&windows)),
                clap_board: ClapBoard::new(),
                vst3: Vst3Host::new(windows),
                vst3_board: Vst3Board::new(),
                host: ProjectHost::new(),
                bridge: ProjectBridge::new(),
            }
        }

        /// Borrow the whole rig apart, once, so a test can call anything on it.
        ///
        /// The pieces are disjoint fields of one struct, which borrowck is happy
        /// with here and would not be if they were handed out one accessor at a
        /// time.
        fn with<R>(
            &mut self,
            body: impl FnOnce(&mut ProjectHost, &Session<'_>, &ProjectBridge, &mut Hosts<'_>) -> R,
        ) -> R {
            let session = Session {
                tracks: &self.tracks,
                audio: &self.audio,
                sequencer: &self.sequencer,
            };
            let mut hosts = Hosts {
                clap: &mut self.clap,
                clap_board: &self.clap_board,
                vst3: &mut self.vst3,
                vst3_board: &self.vst3_board,
            };
            body(&mut self.host, &session, &self.bridge, &mut hosts)
        }

        fn load(&mut self, file: &ProjectFile) -> Result<LoadReport, ProjectFileError> {
            self.with(|host, session, _, hosts| host.load(session, hosts, file))
        }

        fn capture(&mut self) -> ProjectFile {
            self.with(|_, session, _, hosts| capture(session, hosts).0)
        }
    }

    fn clap_track(index: usize, bundle: &str) -> TrackEntry {
        TrackEntry {
            id: index as u32,
            name: format!("Hosted {index}"),
            source: SourceEntry::Clap(ClapEntry {
                bundle_path: bundle.to_owned(),
                plugin_id: "com.acme.thing".to_owned(),
                name: "Acme Thing".to_owned(),
            }),
            gain_db: 0.0,
            pan: 0.0,
            muted: false,
            soloed: false,
            state_blob: String::new(),
        }
    }

    #[test]
    fn a_load_restores_the_mixer_the_pattern_the_tempo_and_the_selection_exactly() {
        // The whole non-plugin half of a project, which is most of it, and none
        // of which needs a sound card.
        let mut rig = Rig::new();
        let mut project = Project::default();
        project
            .add(Source::TestTone, Some("Tone".to_owned()))
            .unwrap();
        project.tracks[0].gain_db = -9.5;
        project.tracks[0].pan = -0.75;
        project.tracks[0].muted = true;
        project.tracks[1].soloed = true;
        project.tracks[2].gain_db = 4.0;
        project.master_gain_db = -2.5;
        let (patterns, playlist) = beat(&[0, 4, 8, 12]);

        let file = ProjectFile::capture(
            &project,
            2,
            147.0,
            &Song {
                patterns: &patterns,
                playlist: &playlist,
                mode: PlayMode::Song,
                audio_sources: Vec::new(),
            },
            &[],
            None,
        );
        let report = rig.load(&file).expect("a valid project");
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert_eq!(report.missing_count(), 0);

        assert_eq!(rig.tracks.project(), project);
        assert_eq!(rig.tracks.selected(), 2);
        assert_eq!(rig.sequencer.bpm(), 147.0);
        assert_eq!(rig.sequencer.patterns(), patterns);
        assert_eq!(rig.sequencer.playlist(), playlist);
        assert_eq!(
            rig.sequencer.play_mode(),
            PlayMode::Song,
            "the mode is session state and a load restores it"
        );

        // ...and the atomics the audio thread actually reads, solo resolved.
        let params = rig.tracks.params();
        assert!((params.track_gain(0) - mydaw_rt_core::db_to_gain(-9.5)).abs() < 1e-6);
        assert_eq!(params.track_pan(0), -0.75);
        assert!(params.track_muted(0), "not soloed, and track 1 is");
        assert!(!params.track_muted(1));
        assert!(params.track_muted(2), "not soloed either");
        assert!((params.master_gain() - mydaw_rt_core::db_to_gain(-2.5)).abs() < 1e-6);
        assert!(params.track_muted(3), "a slot with no track is silent");
    }

    #[test]
    fn a_load_round_trips_through_capture_unchanged() {
        let mut rig = Rig::new();
        let mut project = Project::default();
        project.tracks[0].gain_db = -6.5;
        project.tracks[1].pan = 0.5;
        project.master_gain_db = 1.5;
        let (patterns, playlist) = beat(&[4, 12]);

        let file = ProjectFile::capture(
            &project,
            1,
            92.5,
            &Song {
                patterns: &patterns,
                playlist: &playlist,
                mode: PlayMode::Pattern,
                audio_sources: Vec::new(),
            },
            &[],
            None,
        );
        rig.load(&file).unwrap();
        let recaptured = rig.capture();
        assert_eq!(recaptured, file);
        assert_eq!(recaptured.to_json().unwrap(), file.to_json().unwrap());
    }

    #[test]
    fn a_file_the_engine_cannot_run_leaves_the_session_exactly_as_it_was() {
        // Validation happens before anything is touched, so a bad project is a
        // status bar sentence rather than a half-replaced session.
        let mut rig = Rig::new();
        let before = rig.capture();
        rig.sequencer.set_bpm(155.0);
        rig.tracks.set_gain_db(0, -4.0).unwrap();
        let good = rig.capture();

        for mut bad in [
            {
                let mut f = good.clone();
                f.tracks.clear();
                f
            },
            {
                let mut f = good.clone();
                f.tracks[0].gain_db = 99.0;
                f
            },
            {
                let mut f = good.clone();
                f.bpm = 4_000.0;
                f
            },
            {
                let mut f = good.clone();
                f.patterns[0].steps = 24;
                f
            },
        ] {
            if let Some(track) = bad.tracks.first_mut() {
                track.name = "Poison".to_owned();
            }
            assert!(rig.load(&bad).is_err(), "{bad:?} should have been refused");
            assert_eq!(rig.capture(), good, "nothing may have changed");
        }
        assert_ne!(good, before, "the test actually changed something first");
    }

    #[test]
    fn a_plugin_that_is_not_on_this_machine_leaves_a_silent_track_and_a_warning() {
        // The policy that matters most: a project opened on a machine without
        // one of its plugins still opens, and still remembers the plugin.
        let mut rig = Rig::new();
        let missing = std::env::temp_dir().join("mydaw-no-such-bundle-a71c.clap");
        let mut project = Project::default();
        project.tracks[1].source =
            Source::Clap(ClapSourceRef::new(&missing, "com.acme.thing").named("Acme Thing"));
        let (patterns, playlist) = beat(&[0]);
        let file = ProjectFile::capture(
            &project,
            0,
            130.0,
            &Song {
                patterns: &patterns,
                playlist: &playlist,
                mode: PlayMode::Pattern,
                audio_sources: Vec::new(),
            },
            &[],
            None,
        );

        let report = rig.load(&file).expect("one absent plugin is not a refusal");
        assert_eq!(
            rig.tracks.track_count(),
            project.len(),
            "the project loaded"
        );
        assert_eq!(
            rig.tracks.project().tracks[1].source,
            project.tracks[1].source,
            "the reference is kept, so a save writes it back out"
        );
        // With no audio device the attach phase does not run at all, and says so
        // rather than blaming the plugin.
        assert_eq!(report.missing_count(), 0);
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("no audio device")),
            "{:?}",
            report.warnings
        );

        // ...and the reference survives a save.
        let recaptured = rig.capture();
        assert_eq!(recaptured.tracks[1].source, file.tracks[1].source);
    }

    #[test]
    fn a_project_with_no_plugins_and_no_device_produces_no_warnings_at_all() {
        let mut rig = Rig::new();
        let report = rig.load(&default_project_file()).unwrap();
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    }

    #[test]
    fn new_project_is_the_default_session_and_goes_through_the_same_load() {
        let mut rig = Rig::new();
        rig.sequencer.set_bpm(174.0);
        let (patterns, playlist) = beat(&[0, 8]);
        rig.sequencer.set_song(patterns, playlist).unwrap();
        rig.tracks.set_gain_db(0, -20.0).unwrap();
        rig.tracks.set_soloed(1, true).unwrap();

        let info = rig
            .with(|host, session, bridge, hosts| host.new_project(session, bridge, hosts))
            .expect("the default project always loads");

        assert_eq!(info.name, crate::project_file::UNTITLED);
        assert!(info.path.is_none());
        assert_eq!(info.missing_plugins, 0);
        assert_eq!(rig.tracks.project(), Project::default());
        assert_eq!(rig.sequencer.bpm(), DEFAULT_BPM);
        assert!(rig.sequencer.active_pattern().is_empty());
        assert_eq!(rig.tracks.selected(), 0);
        assert!(rig.host.path().is_none());
        assert_eq!(rig.bridge.info().name, crate::project_file::UNTITLED);
    }

    #[test]
    fn saving_and_opening_a_file_names_it_everywhere_the_ui_looks() {
        let dir = std::env::temp_dir().join(format!(
            "mydaw-project-host-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("My Track.mydaw");

        let mut rig = Rig::new();
        rig.sequencer.set_bpm(123.0);
        rig.tracks.set_pan(1, -0.5).unwrap();
        let info = rig
            .with(|host, session, bridge, hosts| host.save(session, bridge, hosts, &path))
            .expect("a save with no plugins cannot fail");
        assert_eq!(info.name, "My Track");
        assert_eq!(
            info.path.as_deref(),
            Some(path.display().to_string().as_str())
        );
        assert_eq!(rig.host.path(), Some(path.as_path()));
        assert_eq!(rig.bridge.current_path().as_deref(), Some(path.as_path()));

        // A different session, opening the file, gets the first one back.
        let mut second = Rig::new();
        let info = second
            .with(|host, session, bridge, hosts| host.open(session, bridge, hosts, &path))
            .expect("what we just wrote");
        assert_eq!(info.name, "My Track");
        assert_eq!(second.sequencer.bpm(), 123.0);
        assert_eq!(second.tracks.project().tracks[1].pan, -0.5);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn opening_something_that_is_not_a_project_says_which_file_and_why() {
        let dir = std::env::temp_dir().join(format!(
            "mydaw-project-host-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("broken.mydaw");
        std::fs::write(&path, b"{ this is not json").unwrap();

        let mut rig = Rig::new();
        let before = rig.capture();
        let err = rig
            .with(|host, session, bridge, hosts| host.open(session, bridge, hosts, &path))
            .unwrap_err();
        assert!(err.starts_with("broken.mydaw: "), "{err}");
        assert!(err.contains("not a MYDAW project"), "{err}");
        assert_eq!(rig.capture(), before, "a refused open changes nothing");
        assert!(rig.host.path().is_none(), "and does not become the project");

        let missing = dir.join("gone.mydaw");
        let err = rig
            .with(|host, session, bridge, hosts| host.open(session, bridge, hosts, &missing))
            .unwrap_err();
        assert!(err.contains("gone.mydaw"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_hosted_track_with_no_instance_records_an_empty_state_blob() {
        // What a headless capture of a plugin-headed project looks like: the
        // locator is there, the state is not, and nothing pretends otherwise.
        let mut rig = Rig::new();
        let mut file = default_project_file();
        file.tracks.push(clap_track(2, r"C:\CLAP\Thing.clap"));
        rig.load(&file).unwrap();

        let captured = rig.capture();
        let hosted = captured.tracks.len() - 1;
        assert_eq!(captured.tracks.len(), file.tracks.len());
        assert!(captured.tracks[hosted].source.is_hosted());
        assert_eq!(captured.tracks[hosted].state_blob, "");
        assert_eq!(captured.tracks[hosted].name, "Hosted 2");
        assert_eq!(captured.tracks[hosted].id, 2);
    }

    #[test]
    fn the_default_project_file_is_the_shells_own_starting_session() {
        let file = default_project_file();
        assert_eq!(file.format_version, crate::project_file::FORMAT_VERSION);
        assert_eq!(file.bpm, DEFAULT_BPM);
        assert_eq!(file.bpm, 130.0);
        assert_eq!(file.selected_track, 0);
        assert!(file.master_effect.is_none());
        assert_eq!(
            file.tracks
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Kick", "Snare", "Hat Closed", "Hat Open", "Poly Synth"]
        );
        assert_eq!(file.patterns.len(), 1, "one empty pattern to start from");
        assert!(file.patterns[0].channels.iter().all(|clip| clip
            .steps
            .as_ref()
            .is_none_or(|row| row.steps.iter().all(|step| !step.on))));
        assert!(file.playlist.clips.is_empty());
        assert_eq!(file.play_mode, "pat");
        assert!(!file_has_plugins(&file));
    }

    #[test]
    fn a_headless_session_answers_project_commands_at_once_rather_than_timing_out() {
        let bridge = ProjectBridge::new();
        assert!(!bridge.is_served());
        let started = std::time::Instant::now();
        for command in [
            ProjectCommand::New,
            ProjectCommand::Open {
                path: PathBuf::from("x.mydaw"),
            },
            ProjectCommand::Save {
                path: PathBuf::from("x.mydaw"),
            },
        ] {
            assert_eq!(
                bridge.request(command).unwrap_err(),
                crate::project_bridge::NO_MAIN_THREAD
            );
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn a_tick_with_an_empty_mailbox_does_nothing_at_all() {
        let mut rig = Rig::new();
        rig.with(|host, session, bridge, hosts| host.tick(session, bridge, hosts));
        assert_eq!(rig.bridge.pending(), 0);
        assert!(rig.host.path().is_none());
    }
}
