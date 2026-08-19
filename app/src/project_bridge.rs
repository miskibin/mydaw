//! The mailbox between the Tauri commands and the main thread's project loader.
//!
//! The third of MYDAW's main-thread mailboxes, and it exists for the same
//! reason as [`ClapHostBridge`](crate::clap_bridge::ClapHostBridge) and
//! [`Vst3HostBridge`](crate::vst3_bridge::Vst3HostBridge): saving a project has
//! to ask every hosted plugin for its state, and loading one has to destroy
//! every instance and create new ones -- all `[main-thread]` work on `!Send`
//! values that a Tauri command thread cannot reach.
//!
//! ```text
//!   webview            command (worker pool)        main thread (tao loop)
//!   -------            ---------------------        ----------------------
//!   saveProject   -->  bridge.request(Save) ------> drain() in MainEventsCleared
//!                        blocks on a reply channel    ProjectHost asks ClapHost and
//!                                                     Vst3Host for every state blob,
//!                                                     writes the file atomically
//!                      <---------------------------- reply(Info { path, name, .. })
//!   window title  <--  Ok(ProjectInfoDto)
//! ```
//!
//! # Why it is not the CLAP bridge with two more commands
//! A load touches **both** plugin hosts, and neither of the existing bridges is
//! served with the other host in scope -- `ClapHost::tick` has no idea a VST3
//! host exists. The run loop is the only place that holds both, so the queue
//! that drives a load has to be drained there, by something that is handed both.
//! Bolting it onto one format's mailbox would have meant "the VST3 host loads
//! projects", which is not a sentence anybody should have to read.
//!
//! # Why the timeout is its own number
//! [`PROJECT_TIMEOUT`] is far longer than the plugin bridges'
//! [`REQUEST_TIMEOUT`](crate::clap_bridge::REQUEST_TIMEOUT), because a load is
//! not one plugin operation but *all of them*: every instance destroyed, every
//! instance in the file created, activated and given its state, plus a graph
//! rebuild. Five seconds is generous for one plugin and tight for sixteen.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::time::Duration;

use parking_lot::Mutex;
use serde::Serialize;

use crate::project::MissingPlugins;
use crate::project_file::{project_name, UNTITLED};

/// How long a Tauri command waits for a project operation.
///
/// See the module docs: a load is every plugin in the project, one after
/// another, each of which loads a bundle from disk and runs a vendor's
/// initialisation.
pub const PROJECT_TIMEOUT: Duration = Duration::from_secs(60);

/// What the UI can ask the main thread to do with the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectCommand {
    /// Reset the session to a fresh default project.
    New,
    /// Read `path` and rebuild the whole session from it.
    Open { path: PathBuf },
    /// Write the session to `path`, which becomes the current project.
    ///
    /// One command for both Save and Save As: the difference between them is
    /// entirely in *where the path comes from* (the current one, or a dialog),
    /// which is a question for the command layer and not for the main thread.
    Save { path: PathBuf },
}

/// What the main thread answers with: the project, as it now stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectAnswer {
    Info(ProjectInfoDto),
}

impl ProjectAnswer {
    /// The project information, for a caller that asked for it.
    ///
    /// # Errors
    /// Currently unreachable -- there is one answer shape. Written as a
    /// `Result` anyway so that adding a second one cannot turn into a panic on
    /// the main thread's caller.
    pub fn into_info(self) -> Result<ProjectInfoDto, String> {
        match self {
            Self::Info(info) => Ok(info),
        }
    }
}

/// Which project is open, as the title bar and the status bar draw it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInfoDto {
    /// Absolute path of the open project, or `null` for one that has never been
    /// saved. This is what tells the UI whether Save needs a dialog.
    pub path: Option<String>,
    /// The file name without its extension, or `Untitled`.
    pub name: String,
    /// How many of the project's plugins could not be loaded. The status bar
    /// shows a warning when this is not zero; the strips say which.
    pub missing_plugins: usize,
    /// Anything worth telling the user that was not an outright failure: a
    /// plugin that would not load, a state blob a plugin refused, a load with no
    /// audio device to attach plugins to.
    ///
    /// Sentences rather than codes, because they go straight into the status
    /// bar.
    pub warnings: Vec<String>,
}

impl ProjectInfoDto {
    /// The information for a session on `path`, with no warnings.
    pub fn at(path: Option<&std::path::Path>) -> Self {
        Self {
            path: path.map(|p| p.display().to_string()),
            name: project_name(path),
            missing_plugins: 0,
            warnings: Vec::new(),
        }
    }

    /// A never-saved session.
    pub fn untitled() -> Self {
        Self {
            path: None,
            name: UNTITLED.to_owned(),
            missing_plugins: 0,
            warnings: Vec::new(),
        }
    }
}

/// One queued request and the channel its answer goes back on.
type Pending = (ProjectCommand, SyncSender<Result<ProjectAnswer, String>>);

/// `Send + Sync` handle to the main thread's project loader.
///
/// Also owns the two pieces of published state a project has: which file is
/// open ([`ProjectInfoDto`]) and which of its plugins are missing
/// ([`MissingPlugins`]). Both are read by `get_tracks`, which runs after every
/// fader move and must not make a round trip to get them.
pub struct ProjectBridge {
    queue: Mutex<VecDeque<Pending>>,
    /// Whether a main thread is actually serving this bridge. Without it every
    /// project command in a headless build would block for [`PROJECT_TIMEOUT`]
    /// before failing -- a minute at a time.
    served: AtomicBool,
    info: Mutex<ProjectInfoDto>,
    missing: MissingPlugins,
    waker: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl Default for ProjectBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl ProjectBridge {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            served: AtomicBool::new(false),
            info: Mutex::new(ProjectInfoDto::untitled()),
            missing: MissingPlugins::new(),
            waker: Mutex::new(None),
        }
    }

    /// Record that a main thread is serving this bridge.
    pub fn set_served(&self, served: bool) {
        self.served.store(served, Ordering::Release);
    }

    /// Whether projects can be saved or loaded in this session at all.
    pub fn is_served(&self) -> bool {
        self.served.load(Ordering::Acquire)
    }

    /// Which project is open.
    pub fn info(&self) -> ProjectInfoDto {
        self.info.lock().clone()
    }

    /// Publish the open project. Main thread only.
    pub fn publish(&self, info: ProjectInfoDto) {
        *self.info.lock() = info;
    }

    /// Which of the project's plugins are missing.
    pub fn missing(&self) -> &MissingPlugins {
        &self.missing
    }

    /// The path of the open project, for a Save that must not ask again.
    pub fn current_path(&self) -> Option<PathBuf> {
        self.info.lock().path.as_ref().map(PathBuf::from)
    }

    /// Install the callback that wakes the event loop.
    pub fn set_waker(&self, waker: Box<dyn Fn() + Send + Sync>) {
        *self.waker.lock() = Some(waker);
    }

    /// Queue `command` and wait for the main thread's answer.
    ///
    /// # Errors
    /// [`NO_MAIN_THREAD`] without a round trip when nothing is serving the
    /// bridge, whatever the main thread reports, or a timeout message.
    pub fn request(&self, command: ProjectCommand) -> Result<ProjectAnswer, String> {
        if !self.is_served() {
            return Err(NO_MAIN_THREAD.to_owned());
        }
        let (reply, answer) = sync_channel(1);
        self.queue.lock().push_back((command, reply));
        if let Some(waker) = self.waker.lock().as_ref() {
            waker();
        }

        answer.recv_timeout(PROJECT_TIMEOUT).unwrap_or_else(|_| {
            Err(format!(
                "the project did not finish within {} s; is the main window still open?",
                PROJECT_TIMEOUT.as_secs()
            ))
        })
    }

    /// Take every queued request, in the order they arrived. Main thread only.
    pub fn drain(&self) -> Vec<Pending> {
        self.queue.lock().drain(..).collect()
    }

    /// How many requests are waiting. Diagnostics and tests.
    pub fn pending(&self) -> usize {
        self.queue.lock().len()
    }
}

impl std::fmt::Debug for ProjectBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProjectBridge")
            .field("served", &self.is_served())
            .field("pending", &self.pending())
            .field("project", &self.info.lock().name)
            .field("missing", &self.missing.len())
            .finish()
    }
}

/// What a project command answers when no event loop is running.
pub const NO_MAIN_THREAD: &str =
    "saving and loading projects needs MYDAW's main window, which is not running in this session";

/// Answer one request, ignoring a caller that has already given up.
pub fn reply(
    channel: &SyncSender<Result<ProjectAnswer, String>>,
    answer: Result<ProjectAnswer, String>,
) {
    let _ = channel.try_send(answer);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::MissingPluginDto;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Instant;

    #[test]
    fn a_request_with_no_main_thread_fails_immediately_rather_than_timing_out() {
        // A minute per command in a headless test run would be intolerable, and
        // the answer is knowable without asking anyone.
        let bridge = ProjectBridge::new();
        assert!(!bridge.is_served());
        let started = Instant::now();
        let err = bridge.request(ProjectCommand::New).unwrap_err();
        assert_eq!(err, NO_MAIN_THREAD);
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(bridge.pending(), 0, "nothing may be queued");
    }

    #[test]
    fn a_request_is_queued_answered_and_woken() {
        let bridge = Arc::new(ProjectBridge::new());
        bridge.set_served(true);
        let woken = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&woken);
        bridge.set_waker(Box::new(move || flag.store(true, Ordering::Release)));

        let main_thread = Arc::clone(&bridge);
        let loop_thread = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if let Some((command, channel)) = main_thread.drain().into_iter().next() {
                    assert_eq!(
                        command,
                        ProjectCommand::Open {
                            path: PathBuf::from(r"D:\Songs\track.mydaw")
                        }
                    );
                    reply(
                        &channel,
                        Ok(ProjectAnswer::Info(ProjectInfoDto::at(Some(Path::new(
                            r"D:\Songs\track.mydaw",
                        ))))),
                    );
                    return;
                }
                assert!(Instant::now() < deadline, "the request never arrived");
                std::thread::sleep(Duration::from_millis(5));
            }
        });

        let info = bridge
            .request(ProjectCommand::Open {
                path: PathBuf::from(r"D:\Songs\track.mydaw"),
            })
            .expect("answered")
            .into_info()
            .expect("project information");
        assert_eq!(info.name, "track");
        assert!(woken.load(Ordering::Acquire), "the loop must be poked");
        assert_eq!(bridge.pending(), 0);
        loop_thread.join().unwrap();
    }

    #[test]
    fn requests_are_drained_in_the_order_they_were_made() {
        // "Save this, then open that" must not be reordered into "open that,
        // then save it over the top".
        let bridge = ProjectBridge::new();
        let commands = [
            ProjectCommand::Save {
                path: PathBuf::from("a.mydaw"),
            },
            ProjectCommand::Open {
                path: PathBuf::from("b.mydaw"),
            },
            ProjectCommand::New,
        ];
        for command in &commands {
            let (channel, _answer) = sync_channel(1);
            bridge.queue.lock().push_back((command.clone(), channel));
        }
        let drained: Vec<ProjectCommand> = bridge
            .drain()
            .into_iter()
            .map(|(command, _)| command)
            .collect();
        assert_eq!(drained, commands.to_vec());
        assert_eq!(bridge.pending(), 0);
    }

    #[test]
    fn replying_to_a_caller_that_gave_up_is_not_a_panic() {
        let bridge = ProjectBridge::new();
        let (channel, answer) = sync_channel(1);
        bridge
            .queue
            .lock()
            .push_back((ProjectCommand::New, channel));
        drop(answer);
        let drained = bridge.drain();
        assert_eq!(drained.len(), 1);
        reply(
            &drained[0].1,
            Ok(ProjectAnswer::Info(ProjectInfoDto::untitled())),
        );
    }

    #[test]
    fn a_fresh_bridge_reports_an_untitled_project_with_nothing_missing() {
        let bridge = ProjectBridge::new();
        let info = bridge.info();
        assert_eq!(info.name, UNTITLED);
        assert!(info.path.is_none());
        assert_eq!(info.missing_plugins, 0);
        assert!(info.warnings.is_empty());
        assert!(bridge.current_path().is_none());
        assert!(bridge.missing().is_empty());
    }

    #[test]
    fn the_published_project_is_what_a_command_thread_reads() {
        let bridge = ProjectBridge::new();
        bridge.publish(ProjectInfoDto {
            missing_plugins: 1,
            warnings: vec!["'Diva' could not be loaded".to_owned()],
            ..ProjectInfoDto::at(Some(Path::new(r"D:\Songs\My Track.mydaw")))
        });
        let info = bridge.info();
        assert_eq!(info.name, "My Track");
        assert_eq!(info.path.as_deref(), Some(r"D:\Songs\My Track.mydaw"));
        assert_eq!(
            bridge.current_path(),
            Some(PathBuf::from(r"D:\Songs\My Track.mydaw")),
            "Save must not ask for a path it already has"
        );

        bridge.missing().publish(
            vec![Some(MissingPluginDto {
                name: "Diva".to_owned(),
                format: "clap".to_owned(),
                path: r"C:\CLAP\Diva.clap".to_owned(),
                reason: "no such bundle".to_owned(),
            })],
            None,
        );
        assert_eq!(bridge.missing().len(), 1);

        // The webview reads camelCase.
        let json = serde_json::to_string(&bridge.info()).unwrap();
        assert!(json.contains("\"missingPlugins\":1"), "{json}");
        assert!(json.contains("\"warnings\""), "{json}");
    }

    #[test]
    fn an_untitled_project_serialises_its_missing_path_as_null() {
        let json: serde_json::Value = serde_json::to_value(ProjectInfoDto::untitled()).unwrap();
        assert!(json["path"].is_null());
        assert_eq!(json["name"], UNTITLED);
    }
}
