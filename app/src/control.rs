//! One executor and history for GUI and local clients. All work is control-thread
//! work; the audio callback only sees the existing bounded engine queues.
use std::collections::{HashMap, VecDeque};
use std::sync::OnceLock;

use mydaw_control_protocol::{ControlError, Operation, Reply, Request, PROTOCOL_VERSION};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{Emitter, Manager};
use tokio::sync::Mutex;

use crate::{AppState, ProjectAnswer, ProjectCommand, ProjectFile, Song};

pub const HISTORY_LIMIT: usize = 50;
const HISTORY_BYTES: usize = 64 * 1024 * 1024;
const RETRY_LIMIT: usize = 128;

pub(crate) fn resolve_track(
    app: &tauri::AppHandle,
    index: Option<usize>,
    id: Option<u32>,
) -> Result<usize, ControlError> {
    let state = app.state::<AppState>();
    let project = state.tracks().project();
    let index = match (index, id) {
        (Some(index), None) => index,
        (None, Some(id)) => project
            .tracks
            .iter()
            .position(|t| t.id == id)
            .ok_or_else(|| invalid(format!("trackId {id} does not exist")))?,
        _ => return Err(invalid("supply exactly one of trackId or track")),
    };
    if index >= project.tracks.len() {
        return Err(invalid("track index does not exist"));
    }
    Ok(index)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CommandClass {
    Read,
    Transient,
    Song,
    Mixer,
    Project,
    Barrier,
}

pub fn command_class(name: &str) -> CommandClass {
    if matches!(
        name,
        "set_track_gain"
            | "set_track_pan"
            | "set_track_muted"
            | "set_track_soloed"
            | "set_master_gain"
            | "set_bus_gain"
            | "set_bus_pan"
            | "set_bus_muted"
            | "set_bus_soloed"
            | "set_source_param"
            | "set_insert_param"
            | "set_insert_bypass"
            | "set_send_level"
            | "set_send_pre"
    ) {
        return CommandClass::Mixer;
    }
    if name == "add_audio_clip" {
        return CommandClass::Project;
    }
    if matches!(name, "open_project" | "new_project") {
        return CommandClass::Barrier;
    }
    if name.starts_with("get_")
        || name.starts_with("list_")
        || name.starts_with("search_")
        || matches!(
            name,
            "save_clap_state" | "save_vst3_state" | "plugin_parameters" | "job_status"
        )
    {
        return CommandClass::Read;
    }
    if name.starts_with("transport_")
        || name.starts_with("note_")
        || name.starts_with("preview_")
        || name.starts_with("open_")
        || name.starts_with("close_")
        || matches!(
            name,
            "render"
                | "analyze"
                | "diagnose"
                | "export_audio"
                | "cancel_job"
                | "save_plugin_preset"
                | "save_project"
                | "debug_capture"
                | "watch_spectrum"
                | "stop_browser_preview"
                | "set_selected_track"
                | "set_preview_volume"
                | "start_asset_download"
                | "set_audio_settings"
                | "set_input_device"
                | "set_input_armed"
                | "start_recording"
                | "cancel_recording"
                | "set_midi_port_enabled"
                | "add_library_folder"
                | "remove_library_folder"
                | "set_insert_listen"
        )
    {
        return CommandClass::Transient;
    }
    if name == "render_pattern_to_sample" {
        return CommandClass::Project;
    }
    if name.contains("pattern")
        || name.contains("playlist")
        || name.ends_with("_clip")
        || matches!(
            name,
            "set_clip_gain"
                | "set_clip_offset"
                | "set_bpm"
                | "set_loop"
                | "set_play_mode"
                | "patch_notes"
        )
    {
        return CommandClass::Song;
    }
    CommandClass::Project
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub class: CommandClass,
    pub schema: Value,
}
pub fn catalog() -> &'static [CommandInfo] {
    static CATALOG: OnceLock<Vec<CommandInfo>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut entries = crate::commands::control_catalog();
        for (name, description, class, schema) in [
            (
                "context",
                "Bounded session summary; musical detail and clip pagination on demand",
                CommandClass::Read,
                serde_json::to_value(schemars::schema_for!(ContextArgs)).expect("schema"),
            ),
            (
                "catalog",
                "Search command names without loading full schemas",
                CommandClass::Read,
                serde_json::to_value(schemars::schema_for!(CatalogArgs)).expect("schema"),
            ),
            (
                "schema",
                "Read exact command arguments or the request envelope",
                CommandClass::Read,
                serde_json::to_value(schemars::schema_for!(SchemaArgs)).expect("schema"),
            ),
            (
                "apply",
                "Apply 1..128 edits as one undoable transaction",
                CommandClass::Project,
                serde_json::to_value(schemars::schema_for!(BatchArgs)).expect("schema"),
            ),
            (
                "session_info",
                "Session identity, project and capabilities",
                CommandClass::Read,
                serde_json::to_value(schemars::schema_for!(EmptyArgs)).expect("schema"),
            ),
            (
                "history",
                "Current revision and undo/redo labels",
                CommandClass::Read,
                serde_json::to_value(schemars::schema_for!(EmptyArgs)).expect("schema"),
            ),
            (
                "undo",
                "Undo one shared edit; requires ifRevision in the request",
                CommandClass::Project,
                serde_json::to_value(schemars::schema_for!(EmptyArgs)).expect("schema"),
            ),
            (
                "redo",
                "Redo one shared edit; requires ifRevision in the request",
                CommandClass::Project,
                serde_json::to_value(schemars::schema_for!(EmptyArgs)).expect("schema"),
            ),
        ] {
            entries.push(CommandInfo {
                name,
                description,
                class,
                schema,
            });
        }
        entries.sort_by_key(|c| c.name);
        entries
    })
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ContextArgs {
    detail: Option<String>,
    pattern_id: Option<u64>,
    track_id: Option<u32>,
    clip_offset: Option<usize>,
    clip_limit: Option<usize>,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CatalogArgs {
    query: Option<String>,
    offset: Option<usize>,
    limit: Option<usize>,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SchemaArgs {
    command: String,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct BatchArgs {
    operations: Vec<Operation>,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}
fn decode_args<T: serde::de::DeserializeOwned>(args: &Value) -> Result<T, ControlError> {
    serde_json::from_value(args.clone()).map_err(|e| invalid(e.to_string()))
}

#[derive(Clone)]
struct Checkpoint {
    file: ProjectFile,
    looping: bool,
    song_only: bool,
    mixer_only: bool,
    size: usize,
}
impl Checkpoint {
    fn bytes(&self) -> usize {
        self.size
    }
    fn same(&self, other: &Self) -> bool {
        if self.song_only && other.song_only {
            self.file.patterns == other.file.patterns
                && self.file.playlist == other.file.playlist
                && self.file.bpm == other.file.bpm
                && self.file.play_mode == other.file.play_mode
                && self.file.active_pattern == other.file.active_pattern
                && self.looping == other.looping
        } else {
            self.file == other.file && self.looping == other.looping
        }
    }
}
struct HistoryEntry {
    label: String,
    origin: String,
    group: Option<String>,
    before: Checkpoint,
    after: Checkpoint,
}
#[derive(Default)]
struct Journal {
    revision: u64,
    past: VecDeque<HistoryEntry>,
    future: Vec<HistoryEntry>,
    retries: VecDeque<(String, String, Reply)>,
    editors: HashMap<String, std::time::Instant>,
}
impl Journal {
    fn status(&self) -> Value {
        json!({"revision": self.revision, "undoLabel": self.past.back().map(|e| &e.label),
            "redoLabel": self.future.last().map(|e| &e.label), "undoCount": self.past.len(), "redoCount": self.future.len()})
    }
    fn record(&mut self, entry: HistoryEntry) {
        self.future.clear();
        if let Some(previous) = self.past.back_mut() {
            if entry.group.is_some()
                && entry.group == previous.group
                && entry.origin == previous.origin
                && previous.after.same(&entry.before)
                && previous.before.song_only == entry.before.song_only
                && previous.before.mixer_only == entry.before.mixer_only
            {
                previous.after = entry.after;
            } else {
                self.past.push_back(entry);
            }
        } else {
            self.past.push_back(entry);
        }
        while self.past.len() > HISTORY_LIMIT
            || self
                .past
                .iter()
                .map(|e| e.before.bytes() + e.after.bytes())
                .sum::<usize>()
                > HISTORY_BYTES
        {
            self.past.pop_front();
        }
    }
}

pub struct SessionControl {
    pub session_id: String,
    journal: Mutex<Journal>,
    revision: std::sync::atomic::AtomicU64,
    native_change: std::sync::atomic::AtomicBool,
}
impl Default for SessionControl {
    fn default() -> Self {
        Self::new()
    }
}
impl SessionControl {
    pub fn new() -> Self {
        Self {
            session_id: uuid::Uuid::new_v4().to_string(),
            journal: Mutex::new(Journal::default()),
            revision: std::sync::atomic::AtomicU64::new(0),
            native_change: std::sync::atomic::AtomicBool::new(false),
        }
    }
    pub fn revision(&self) -> u64 {
        self.revision.load(std::sync::atomic::Ordering::Acquire)
    }
    /// Owner-thread callback. Native plugin editors cannot supply inverse edits;
    /// their opaque changes invalidate history. Never lose a notification while
    /// an async command owns the journal; reconcile it at the next boundary.
    pub fn native_editor_changed(&self, app: &tauri::AppHandle) {
        self.native_change
            .store(true, std::sync::atomic::Ordering::Release);
        if let Ok(mut journal) = self.journal.try_lock() {
            self.reconcile_native(app, &mut journal);
        }
    }
    fn invalidate_native(&self, journal: &mut Journal) -> bool {
        if !self
            .native_change
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            return false;
        }
        journal.revision += 1;
        journal.past.clear();
        journal.future.clear();
        self.revision
            .store(journal.revision, std::sync::atomic::Ordering::Release);
        true
    }
    fn reconcile_native(&self, app: &tauri::AppHandle, journal: &mut Journal) {
        if self.invalidate_native(journal) {
            emit(
                app,
                journal,
                &Request::new("native_plugin_editor", json!({})),
            );
        }
    }
    pub async fn execute(&self, app: &tauri::AppHandle, mut request: Request) -> Reply {
        request.token = None;
        // Discovery must remain responsive while a vendor loads or a large
        // file command owns the journal. Report busy, never hide a live session.
        if request.command == "session_info" {
            let result = (|| {
                if request.version != PROTOCOL_VERSION {
                    return Err(ControlError::new(
                        "PROTOCOL_VERSION",
                        "unsupported protocol version",
                    ));
                }
                decode_args::<EmptyArgs>(&request.args)?;
                let state = app.state::<AppState>();
                let mut journal = self.journal.try_lock().ok();
                if let Some(journal) = &mut journal {
                    self.reconcile_native(app, journal);
                }
                if request
                    .if_revision
                    .is_some_and(|expected| expected != self.revision())
                {
                    return Err(ControlError::new(
                        "REVISION_CONFLICT",
                        "the session revision changed",
                    ));
                }
                Ok(session_data(&state, &self.session_id, journal.as_deref()))
            })();
            return self.reply(self.revision(), result);
        }
        if request.command.starts_with("note_") || request.command.starts_with("transport_") {
            if request.version != PROTOCOL_VERSION {
                return self.reply(
                    self.revision(),
                    Err(ControlError::new(
                        "PROTOCOL_VERSION",
                        "unsupported protocol version",
                    )),
                );
            }
            return self.reply(
                self.revision(),
                crate::commands::control_dispatch(app, &request.command, request.args).await,
            );
        }
        let mut journal = self.journal.lock().await;
        self.reconcile_native(app, &mut journal);
        let fingerprint = serde_json::to_string(&request).unwrap_or_default();
        let retryable = !matches!(
            request.command.as_str(),
            "session_info" | "context" | "schema" | "catalog" | "history"
        ) && command_class(&request.command) != CommandClass::Read;
        if let Some(key) = request.idempotency_key.as_ref().filter(|_| retryable) {
            if let Some((_, previous, reply)) = journal.retries.iter().find(|(k, _, _)| k == key) {
                if previous == &fingerprint {
                    return reply.clone();
                }
                return self.reply(
                    journal.revision,
                    Err(ControlError::new(
                        "IDEMPOTENCY_CONFLICT",
                        "this key already identifies different arguments",
                    )),
                );
            }
        }
        let mut result = self.execute_locked(app, &request, &mut journal).await;
        self.reconcile_native(app, &mut journal);
        if matches!(request.command.as_str(), "session_info" | "history") {
            if let Ok(value) = &mut result {
                value["history"] = journal.status();
            }
        }
        self.revision
            .store(journal.revision, std::sync::atomic::Ordering::Release);
        let reply = self.reply(journal.revision, result);
        if let Some(key) = request.idempotency_key.filter(|_| retryable) {
            // Conflicts and invalid requests are not executed; callers may fix them.
            if reply.error.is_none() {
                journal.retries.push_back((key, fingerprint, reply.clone()));
                while journal.retries.len() > RETRY_LIMIT
                    || journal
                        .retries
                        .iter()
                        .map(|(_, fingerprint, reply)| {
                            fingerprint.len() + serde_json::to_vec(reply).map_or(0, |b| b.len())
                        })
                        .sum::<usize>()
                        > 16 * 1024 * 1024
                {
                    journal.retries.pop_front();
                }
            }
        }
        reply
    }
    fn reply(&self, revision: u64, result: Result<Value, ControlError>) -> Reply {
        match result {
            Ok(data) => Reply {
                version: PROTOCOL_VERSION,
                session_id: self.session_id.clone(),
                revision,
                data,
                error: None,
            },
            Err(error) => Reply {
                version: PROTOCOL_VERSION,
                session_id: self.session_id.clone(),
                revision,
                data: Value::Null,
                error: Some(error),
            },
        }
    }
    async fn execute_locked(
        &self,
        app: &tauri::AppHandle,
        request: &Request,
        journal: &mut Journal,
    ) -> Result<Value, ControlError> {
        if request.version != PROTOCOL_VERSION {
            return Err(ControlError::new(
                "PROTOCOL_VERSION",
                "unsupported protocol version",
            ));
        }
        if request
            .if_revision
            .is_some_and(|expected| expected != journal.revision)
        {
            return Err(ControlError {
                code: "REVISION_CONFLICT".into(),
                message: "the session changed; read the affected objects again".into(),
                details: Some(json!({"expected": request.if_revision, "actual": journal.revision})),
            });
        }
        let state = app.state::<AppState>();
        journal
            .editors
            .retain(|_, deadline| *deadline > std::time::Instant::now());
        match request.command.as_str() {
            "gui_edit_begin" | "gui_edit_end" => {
                if request.origin.as_deref() != Some("gui") {
                    return Err(ControlError::new(
                        "UNAUTHORIZED",
                        "only the GUI can register a gesture",
                    ));
                }
                let group = request
                    .args
                    .get("group")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("gesture requires group"))?;
                if request.command == "gui_edit_begin" {
                    journal.editors.insert(
                        group.into(),
                        std::time::Instant::now() + std::time::Duration::from_secs(30),
                    );
                } else {
                    journal.editors.remove(group);
                }
                return Ok(Value::Null);
            }
            "session_info" | "history" => {
                decode_args::<EmptyArgs>(&request.args)?;
                return Ok(session_data(&state, &self.session_id, Some(journal)));
            }
            "context" => {
                let mut result = context(&state, &request.args)?;
                result["guiEditing"] = json!(!journal.editors.is_empty());
                return Ok(result);
            }
            "catalog" => {
                let typed = decode_args::<CatalogArgs>(&request.args)?;
                let query = typed.query.as_deref().unwrap_or("").to_lowercase();
                let offset = typed.offset.unwrap_or(0);
                let limit = typed.limit.unwrap_or(40).min(100);
                let matches: Vec<_> = catalog()
                    .iter()
                    .filter(|c| c.name.contains(&query))
                    .collect();
                return Ok(
                    json!({"total": matches.len(), "offset": offset, "commands": matches.into_iter().skip(offset).take(limit)
                    .map(|c| json!({"name": c.name,"description": c.description,"class": c.class})).collect::<Vec<_>>(),
                    "sessionCommands": ["context","schema","apply","history","undo","redo","render","analyze","diagnose"]}),
                );
            }
            "schema" => {
                let typed = decode_args::<SchemaArgs>(&request.args)?;
                if typed.command == "request" {
                    return serde_json::to_value(schemars::schema_for!(Request))
                        .map_err(|e| invalid(e.to_string()));
                }
                let name = request
                    .args
                    .get("command")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("schema requires command"))?;
                return catalog()
                    .iter()
                    .find(|c| c.name == name)
                    .map(|c| json!(c))
                    .ok_or_else(|| ControlError::new("UNKNOWN_COMMAND", name));
            }
            "undo" | "redo" => {
                decode_args::<EmptyArgs>(&request.args)?;
                require_revision(request)?;
                if request.origin.as_deref() != Some("gui") && any_plugin_editor(&state) {
                    return Err(ControlError::new(
                        "PLUGIN_EDITOR_OPEN",
                        "close native plugin editors before undo/redo through CLI",
                    ));
                }
                if request.origin.as_deref() != Some("gui") && !journal.editors.is_empty() {
                    return Err(ControlError::new(
                        "GUI_EDIT_IN_PROGRESS",
                        "finish the GUI gesture before changing history",
                    ));
                }
                let redo = request.command == "redo";
                let entry = if redo {
                    journal.future.pop()
                } else {
                    journal.past.pop_back()
                };
                let Some(entry) = entry else {
                    return Ok(journal.status());
                };
                let target = if redo { &entry.after } else { &entry.before };
                if let Err(error) = restore(&state, target) {
                    // A vendor may reject a restore after other objects changed.
                    // Invalidate history and revision instead of asserting that
                    // the old session is still intact.
                    journal.past.clear();
                    journal.future.clear();
                    journal.revision += 1;
                    emit(app, journal, request);
                    return Err(error);
                }
                if redo {
                    journal.past.push_back(entry);
                } else {
                    journal.future.push(entry);
                }
                journal.revision += 1;
                emit(app, journal, request);
                return Ok(journal.status());
            }
            _ => {}
        }
        if request.command == "set_pattern" {
            check_pattern_precondition(&state, request)?;
        }
        let operations = if request.command == "apply" {
            let batch: BatchArgs =
                serde_json::from_value(request.args.clone()).map_err(|e| invalid(e.to_string()))?;
            if batch.operations.is_empty() || batch.operations.len() > 128 {
                return Err(invalid("apply requires 1..128 operations"));
            }
            for operation in &batch.operations {
                if !matches!(
                    command_class(&operation.command),
                    CommandClass::Song | CommandClass::Mixer | CommandClass::Project
                ) {
                    return Err(invalid(format!(
                        "{} cannot be included in an undoable batch",
                        operation.command
                    )));
                }
            }
            batch.operations
        } else {
            vec![Operation {
                command: request.command.clone(),
                args: request.args.clone(),
            }]
        };
        for operation in &operations {
            crate::commands::control_validate(&operation.command, operation.args.clone())?;
        }
        // GUI mode changes follow editor activation. Navigation must neither
        // consume Undo nor discard Redo; explicit CLI edits retain their class.
        let class =
            if request.command == "set_play_mode" && request.origin.as_deref() == Some("gui") {
                CommandClass::Transient
            } else if operations
                .iter()
                .any(|o| command_class(&o.command) == CommandClass::Project)
                || (operations
                    .iter()
                    .any(|o| command_class(&o.command) == CommandClass::Mixer)
                    && operations
                        .iter()
                        .any(|o| command_class(&o.command) == CommandClass::Song))
            {
                CommandClass::Project
            } else {
                command_class(&operations[0].command)
            };
        let editable = matches!(
            class,
            CommandClass::Song
                | CommandClass::Mixer
                | CommandClass::Project
                | CommandClass::Barrier
        );
        if editable && request.origin.as_deref() != Some("gui") && !journal.editors.is_empty() {
            return Err(ControlError::new("GUI_EDIT_IN_PROGRESS", "a mouse gesture is still being committed; read context and retry after it finishes"));
        }
        if let Some(group) = &request.group {
            if let Some(deadline) = journal.editors.get_mut(group) {
                *deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            }
        }
        if editable {
            require_revision(request)?;
        }
        if request.origin.as_deref() != Some("gui")
            && (matches!(class, CommandClass::Project | CommandClass::Barrier)
                || matches!(
                    request.command.as_str(),
                    "render" | "diagnose" | "export_audio"
                ))
            && any_plugin_editor(&state)
        {
            return Err(ControlError::new("PLUGIN_EDITOR_OPEN","close native plugin editors before a CLI edit that snapshots or restores plugin state"));
        }
        if request.origin.as_deref() != Some("gui") {
            if operations.iter().any(|o| {
                matches!(o.command.as_str(), "open_audio_file" | "add_library_folder")
                    || (matches!(o.command.as_str(), "open_project" | "export_audio")
                        && o.args.get("path").is_none_or(Value::is_null))
            }) {
                return Err(ControlError::new(
                    "INTERACTIVE_ONLY",
                    "use an explicit file path through CLI",
                ));
            }
            if operations.iter().any(|o| {
                o.command == "save_project"
                    && o.args.get("path").is_none_or(Value::is_null)
                    && state.project_path().is_none()
            }) {
                return Err(invalid(
                    "save_project requires a path for an unsaved session",
                ));
            }
        }
        let before = if matches!(
            class,
            CommandClass::Song | CommandClass::Mixer | CommandClass::Project
        ) {
            Some(checkpoint_class(&state, class)?)
        } else {
            None
        };
        let mut results = Vec::new();
        let mut acknowledgement = json!({"changed":false});
        for operation in &operations {
            match crate::commands::control_dispatch(app, &operation.command, operation.args.clone())
                .await
            {
                Ok(value) => results.push(value),
                Err(mut error) => {
                    if let Some(before) = &before {
                        // Validation can fail before touching the model. Avoid
                        // stopping playback and reloading every plugin then.
                        let changed =
                            !checkpoint_class(&state, class).is_ok_and(|now| before.same(&now));
                        if changed {
                            if let Err(rollback) = restore(&state, before) {
                                error.code = "ROLLBACK_FAILED".into();
                                error.message =
                                    format!("{}; rollback: {}", error.message, rollback);
                                journal.past.clear();
                                journal.future.clear();
                            }
                            journal.revision += 1;
                            emit(app, journal, request);
                        }
                    }
                    return Err(error);
                }
            }
        }
        if let Some(before) = before {
            let after = match checkpoint_class(&state, class) {
                Ok(after) => after,
                Err(mut error) => {
                    if let Err(rollback) = restore(&state, &before) {
                        error.code = "ROLLBACK_FAILED".into();
                        error.message = format!("{}; rollback: {}", error.message, rollback);
                        journal.past.clear();
                        journal.future.clear();
                    }
                    journal.revision += 1;
                    emit(app, journal, request);
                    return Err(error);
                }
            };
            if !before.same(&after) {
                acknowledgement = json!({"changed":true,
                    "createdTrackIds":after.file.tracks.iter().filter(|t|!before.file.tracks.iter().any(|old|old.id==t.id)).map(|t|t.id).collect::<Vec<_>>(),
                    "createdPatternIds":after.file.patterns.iter().filter(|p|!before.file.patterns.iter().any(|old|old.id==p.id)).map(|p|p.id).collect::<Vec<_>>(),
                    "createdClipIds":after.file.playlist.clips.iter().filter(|c|!before.file.playlist.clips.iter().any(|old|old.id==c.id)).map(|c|c.id).collect::<Vec<_>>()});
                journal.record(HistoryEntry {
                    label: request
                        .label
                        .clone()
                        .unwrap_or_else(|| request.command.replace('_', " ")),
                    origin: request.origin.clone().unwrap_or_else(|| "cli".into()),
                    group: request.group.clone(),
                    before,
                    after,
                });
                journal.revision += 1;
                emit(app, journal, request);
            }
        } else if class == CommandClass::Barrier {
            journal.past.clear();
            journal.future.clear();
            journal.revision += 1;
            emit(app, journal, request);
        }
        if request.command == "apply" {
            acknowledgement["applied"] = json!(operations.len());
            Ok(acknowledgement)
        } else if editable && request.origin.as_deref() != Some("gui") {
            let value = results.pop().unwrap_or(Value::Null);
            if !value.is_object() && !value.is_array() {
                acknowledgement["value"] = value;
            }
            Ok(acknowledgement)
        } else {
            Ok(results.pop().unwrap_or(Value::Null))
        }
    }
}

fn session_data(state: &AppState, id: &str, journal: Option<&Journal>) -> Value {
    json!({"sessionId":id,"project":state.project_info(),"busy":journal.is_none(),"history":journal.map(Journal::status),
        "capabilities":{"ppq":960,"meter":[4,4],"maxTracks":16,"maxPatterns":64,"maxPlaylistTracks":32,"patternSteps":[16,32,64],"sharedHistory":true,"batch":true,
            "pluginDelayCompensation":false,"sampleAccuratePluginAutomation":false}})
}

fn require_revision(request: &Request) -> Result<(), ControlError> {
    if request.origin.as_deref() != Some("gui") && request.if_revision.is_none() {
        return Err(ControlError::new(
            "PRECONDITION_REQUIRED",
            "supply ifRevision from context or session_info",
        ));
    }
    Ok(())
}
fn any_plugin_editor(state: &AppState) -> bool {
    (0..state.tracks().track_count()).any(|index| {
        state
            .clap_bridge()
            .board()
            .track(index)
            .is_some_and(|p| p.gui_open)
            || state
                .vst3_bridge()
                .board()
                .track(index)
                .is_some_and(|p| p.gui_open)
    }) || state
        .clap_bridge()
        .board()
        .master()
        .is_some_and(|p| p.gui_open)
        || state
            .vst3_bridge()
            .board()
            .master()
            .is_some_and(|p| p.gui_open)
}
fn invalid(message: impl Into<String>) -> ControlError {
    ControlError::new("INVALID_ARGUMENTS", message)
}
fn emit(app: &tauri::AppHandle, journal: &Journal, request: &Request) {
    let _ = app.emit(
        "mydaw:control-changed",
        json!({"revision": journal.revision, "origin": request.origin,
        "command": request.command, "history": journal.status()}),
    );
}
fn check_pattern_precondition(state: &AppState, request: &Request) -> Result<(), ControlError> {
    let Some(expected) = &request.expected_pattern else {
        if request.origin.as_deref() == Some("gui") {
            return Err(ControlError::new(
                "PRECONDITION_REQUIRED",
                "a GUI pattern edit requires its original pattern",
            ));
        }
        return Ok(());
    };
    let id = request
        .args
        .pointer("/pattern/id")
        .and_then(Value::as_u64)
        .filter(|id| *id != 0)
        .unwrap_or_else(|| state.sequencer().patterns().active_id());
    let patterns = state.sequencer().patterns();
    let actual = patterns
        .get(id)
        .ok_or_else(|| invalid(format!("pattern {id} no longer exists")))?;
    let value = serde_json::to_value(crate::PatternDto::from(actual))
        .map_err(|e| invalid(e.to_string()))?;
    if expected != &value {
        return Err(ControlError {
            code: "PATTERN_CONFLICT".into(),
            message: "this pattern was edited by another client".into(),
            details: Some(json!({"patternId": id,"actual": value})),
        });
    }
    Ok(())
}
fn model_file(state: &AppState) -> ProjectFile {
    let project = state.tracks().project();
    let (patterns, playlist) = state.sequencer().song();
    ProjectFile::capture(
        &project,
        state.tracks().selected(),
        state.bpm(),
        &Song {
            patterns: &patterns,
            playlist: &playlist,
            mode: state.sequencer().play_mode(),
            audio_sources: state
                .audio_sources()
                .list()
                .iter()
                .map(|s| crate::AudioSourceEntry {
                    id: s.id(),
                    path: s.path().to_string_lossy().into_owned(),
                })
                .collect(),
        },
        &vec![vec![]; project.tracks.len()],
        None,
    )
}
#[cfg(test)]
fn checkpoint(state: &AppState, song_only: bool) -> Result<Checkpoint, ControlError> {
    checkpoint_class(
        state,
        if song_only {
            CommandClass::Song
        } else {
            CommandClass::Project
        },
    )
}
fn checkpoint_class(state: &AppState, class: CommandClass) -> Result<Checkpoint, ControlError> {
    let song_only = class == CommandClass::Song;
    let mixer_only = class == CommandClass::Mixer;
    let file = if !song_only && !mixer_only && state.project_bridge().is_served() {
        match state
            .project_bridge()
            .request(ProjectCommand::Snapshot)
            .map_err(|e| ControlError::new("CHECKPOINT_FAILED", e))?
        {
            ProjectAnswer::Snapshot(file) => *file,
            _ => {
                return Err(ControlError::new(
                    "CHECKPOINT_FAILED",
                    "incorrect snapshot answer",
                ))
            }
        }
    } else {
        model_file(state)
    };
    let size = serde_json::to_vec(&file)
        .map_err(|e| invalid(e.to_string()))?
        .len();
    let checkpoint = Checkpoint {
        file,
        looping: state.is_looping(),
        song_only,
        mixer_only,
        size,
    };
    if checkpoint.bytes() > HISTORY_BYTES / 2 {
        return Err(ControlError::new(
            "HISTORY_LIMIT",
            "project snapshot exceeds the history memory limit",
        ));
    }
    Ok(checkpoint)
}
fn restore(state: &AppState, checkpoint: &Checkpoint) -> Result<(), ControlError> {
    if checkpoint.mixer_only {
        return restore_mixer(state, &checkpoint.file)
            .map_err(|e| ControlError::new("RESTORE_FAILED", e));
    }
    if checkpoint.song_only {
        return state
            .restore_control_song(&checkpoint.file, checkpoint.looping)
            .map_err(|e| ControlError::new("RESTORE_FAILED", e));
    }
    if state.project_bridge().is_served() {
        state
            .project_bridge()
            .request(ProjectCommand::Restore {
                file: Box::new(checkpoint.file.clone()),
            })
            .map_err(|e| ControlError::new("RESTORE_FAILED", e))?;
    } else {
        let (project, _) = checkpoint
            .file
            .to_project(state.tracks().effects())
            .map_err(|e| invalid(e.to_string()))?;
        state.tracks().set_project(project);
        state.tracks().ensure_file_samplers();
        state
            .restore_control_song(&checkpoint.file, checkpoint.looping)
            .map_err(|e| ControlError::new("RESTORE_FAILED", e))?;
    }
    state
        .set_looping(checkpoint.looping)
        .map_err(|e| invalid(e.to_string()))?;
    Ok(())
}

fn restore_mixer(state: &AppState, file: &ProjectFile) -> Result<(), String> {
    let current = state.tracks().project();
    if current.tracks.iter().map(|t| t.id).collect::<Vec<_>>()
        != file.tracks.iter().map(|t| t.id).collect::<Vec<_>>()
        || current.buses.iter().map(|b| b.id).collect::<Vec<_>>()
            != file.buses.iter().map(|b| b.id).collect::<Vec<_>>()
    {
        return Err("mixer topology changed; cannot restore controls".into());
    }
    let restore_inserts = |target: crate::InsertTarget,
                           entries: &[crate::project_file::InsertEntry]|
     -> Result<(), String> {
        for (index, entry) in entries.iter().enumerate() {
            for (id, value) in &entry.params {
                state
                    .set_insert_param(target, index, id, *value)
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    };
    for (index, track) in file.tracks.iter().enumerate() {
        state
            .set_track_gain_db(index, track.gain_db)
            .map_err(|e| e.to_string())?;
        state
            .set_track_pan(index, track.pan)
            .map_err(|e| e.to_string())?;
        state
            .set_track_muted(index, track.muted)
            .map_err(|e| e.to_string())?;
        state
            .set_track_soloed(index, track.soloed)
            .map_err(|e| e.to_string())?;
        for (id, value) in &track.source_params {
            state
                .set_source_param(index, id, *value)
                .map_err(|e| e.to_string())?;
        }
        restore_inserts(crate::InsertTarget::Track(index), &track.inserts)?;
    }
    for (index, bus) in file.buses.iter().enumerate() {
        state
            .set_bus_gain_db(index, bus.gain_db)
            .map_err(|e| e.to_string())?;
        state
            .set_bus_pan(index, bus.pan)
            .map_err(|e| e.to_string())?;
        state
            .set_bus_muted(index, bus.muted)
            .map_err(|e| e.to_string())?;
        state
            .set_bus_soloed(index, bus.soloed)
            .map_err(|e| e.to_string())?;
        restore_inserts(crate::InsertTarget::Bus(index), &bus.inserts)?;
    }
    for send in &file.sends {
        state
            .set_send_level(send.id, send.level)
            .map_err(|e| e.to_string())?;
        state
            .set_send_pre(send.id, send.pre)
            .map_err(|e| e.to_string())?;
    }
    state
        .set_master_gain_db(file.master_gain_db)
        .map_err(|e| e.to_string())?;
    restore_inserts(crate::InsertTarget::Master, &file.master_inserts)?;
    Ok(())
}

fn context(state: &AppState, args: &Value) -> Result<Value, ControlError> {
    let typed = decode_args::<ContextArgs>(args)?;
    let project = state.tracks().project();
    let patterns = state.sequencer().patterns();
    let active = if let Some(id) = typed.pattern_id {
        patterns
            .get(id)
            .cloned()
            .ok_or_else(|| invalid("patternId does not exist"))?
    } else {
        state.sequencer().active_pattern()
    };
    let pattern = crate::PatternDto::from(&active);
    let detail = typed.detail.as_deref().unwrap_or("summary");
    if !matches!(detail, "summary" | "musical") {
        return Err(invalid("detail must be summary or musical"));
    }
    let channel = if let Some(id) = typed.track_id {
        Some(
            project
                .tracks
                .iter()
                .position(|t| t.id == id)
                .ok_or_else(|| invalid("trackId does not exist"))? as u8,
        )
    } else {
        None
    };
    let music = if detail == "musical" {
        json!({"patternId":pattern.id,"ticks":u32::from(pattern.steps)*240,"noteColumns":["startTicks","lengthTicks","midiKey","velocity"],
        "channels":pattern.channels.iter().filter(|c|channel.is_none_or(|selected|selected==c.channel)).map(|c|json!({"trackId":project.tracks.get(c.channel as usize).map(|t|t.id),"mode":c.mode,
            "notes":c.notes.iter().map(|n|json!([n.start,n.length,n.key,n.velocity])).collect::<Vec<_>>(),
            "stepKey":c.steps.as_ref().map(|s|s.key),"hits":c.steps.as_ref().map(|s|s.steps.iter().enumerate().filter(|(_,s)|s.on).map(|(index,s)|json!([index*240,s.velocity])).collect::<Vec<_>>())})).collect::<Vec<_>>()})
    } else {
        Value::Null
    };
    let mut playlist =
        serde_json::to_value(crate::PlaylistDto::from(&state.sequencer().playlist()))
            .map_err(|e| invalid(e.to_string()))?;
    let offset = typed.clip_offset.unwrap_or(0);
    let limit = typed.clip_limit.unwrap_or(32).min(100);
    if let Some(clips) = playlist["clips"].as_array_mut() {
        let total = clips.len();
        *clips = clips.iter().skip(offset).take(limit).cloned().collect();
        playlist["clipTotal"] = json!(total);
        playlist["clipOffset"] = json!(offset);
    }
    Ok(
        json!({"project": state.project_info(), "bpm": state.bpm(), "ppq": 960, "ticksPerBar": 3840,
        "playMode": state.sequencer().play_mode().id(), "selectedTrackId": project.tracks.get(state.tracks().selected()).map(|t| t.id),
        "tracks": project.tracks.iter().enumerate().map(|(index,t)| json!({"id": t.id,"index": index,"name": t.name,"source": t.source.id(),
            "gainDb": t.gain_db,"pan":t.pan,"muted": t.muted,"soloed": t.soloed})).collect::<Vec<_>>(),
        "patterns": crate::PatternSetDto::from(&patterns).patterns, "activePatternId": pattern.id,
        "music": music,
        "playlist": playlist}),
    )
}

#[tauri::command]
pub async fn control_call(app: tauri::AppHandle, request: Request) -> Reply {
    let state = app.state::<AppState>();
    state.control().execute(&app, request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_native_change_is_reconciled_once_after_a_busy_command() {
        let control = SessionControl::new();
        let mut journal = control.journal.try_lock().unwrap();
        journal.revision = 7;
        control
            .native_change
            .store(true, std::sync::atomic::Ordering::Release);
        assert!(control.journal.try_lock().is_err());
        assert!(control.invalidate_native(&mut journal));
        assert_eq!(journal.revision, 8);
        assert_eq!(control.revision(), 8);
        assert!(!control.invalidate_native(&mut journal));
    }
    #[test]
    fn generated_catalog_covers_commands_and_rejects_typos() {
        assert!(catalog().len() > 100);
        assert!(catalog().iter().any(|c| c.name == "set_track_source_vst3"));
        assert!(
            crate::commands::control_validate("set_bpm", json!({"bpm":140,"typo":true})).is_err()
        );
        assert!(crate::commands::control_validate("set_bpm", json!({"bpm":140})).is_ok());
    }
    #[test]
    fn song_history_roundtrip_preserves_other_channels() {
        let mut kernel = crate::boot_kernel().unwrap();
        let state = AppState::from_kernel(&kernel).unwrap();
        let before = checkpoint(&state, true).unwrap();
        state.set_bpm(140.0).unwrap();
        let after = checkpoint(&state, true).unwrap();
        assert!(!before.same(&after));
        restore(&state, &before).unwrap();
        assert!(before.same(&checkpoint(&state, true).unwrap()));
        restore(&state, &after).unwrap();
        assert_eq!(state.bpm(), 140.0);
        kernel.shutdown();
    }
}
