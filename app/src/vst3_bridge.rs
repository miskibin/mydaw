//! The mailbox between the Tauri commands and the main thread's VST3 host.
//!
//! A sibling of [`crate::clap_bridge`], not a generalisation of it. The two
//! formats' commands carry different locators (a CLAP plugin id is a string, a
//! VST3 class id is sixteen bytes), their hosts own different types, and a spike
//! that unified them first would have had to get the abstraction right before
//! knowing whether the second implementation even worked. Merging them is a
//! contained piece of work once both are proven -- see the step 7 spec's parity
//! roadmap -- and the shapes here are deliberately identical so that it stays
//! contained.
//!
//! ```text
//!   webview                 command (worker pool)         main thread (tao loop)
//!   -------                 ---------------------         ----------------------
//!   setTrackSourceVst3  ->  bridge.request(..) ---------> drain() in MainEventsCleared
//!                             blocks on a reply channel     Vst3Host creates the instance,
//!                                                           activates it, swaps the node in
//!                           <----------------------------- reply(Done)
//!   mixer state         <-  Ok(())                          board.publish(..)
//! ```
//!
//! The base64 helpers, the timeout and the reply convention are shared with the
//! CLAP bridge rather than copied: see [`crate::clap_bridge::to_base64`],
//! [`REQUEST_TIMEOUT`](crate::clap_bridge::REQUEST_TIMEOUT) and
//! [`reply`](crate::clap_bridge::reply).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};

use parking_lot::Mutex;
use serde::Serialize;

use crate::clap_bridge::{ClapTarget, REQUEST_TIMEOUT};

/// Where a hosted plugin sits.
///
/// The same shape as [`ClapTarget`], and deliberately the *same type*: a slot is
/// a place in the mixer, and a place in the mixer does not have a plugin format.
/// The master insert in particular holds **one** plugin of **either** kind, so a
/// separate target type would have made the two hosts unable to talk about the
/// same slot.
pub type Vst3Target = ClapTarget;

/// What the UI can ask the main thread to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Vst3Command {
    /// Create an instance of `cid` from `module_path` and make it the source of
    /// `track`, destroying whatever was there.
    SetTrackSource {
        track: usize,
        module_path: PathBuf,
        cid: String,
    },
    /// Create an instance and make it the master insert, displacing a CLAP one
    /// if that is what was there.
    SetMasterEffect { module_path: PathBuf, cid: String },
    /// Put a **built-in** source at the head of a track that currently holds a
    /// hosted VST3 plugin, and destroy that plugin.
    SetTrackBuiltIn { track: usize, source_id: String },
    /// Empty the master insert and destroy its instance.
    ClearMasterEffect,
    /// Open a plugin's editor in its own native window.
    OpenGui(Vst3Target),
    /// Close it. Not an error when nothing is open.
    CloseGui(Vst3Target),
    /// Serialise a plugin's component state.
    SaveState(Vst3Target),
    /// Restore a plugin from a blob produced by [`Vst3Command::SaveState`].
    LoadState { target: Vst3Target, state: Vec<u8> },
    /// Append a track and rebuild the graph, re-attaching every hosted plugin of
    /// **both** formats.
    AddTrack {
        source_id: String,
        name: Option<String>,
    },
    /// Remove a track and rebuild, likewise.
    RemoveTrack { track: usize },
    /// Install a graph from the project as it now stands and re-attach every
    /// hosted plugin of both formats. See
    /// [`ClapCommand::Rebuild`](crate::ClapCommand::Rebuild).
    Rebuild,
}

/// What the main thread answers with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Vst3Answer {
    /// The operation succeeded and has nothing to report.
    Done,
    /// The editor's state after an open or close.
    Gui(Vst3GuiStatusDto),
    /// A saved state blob. Empty for a plugin that has none to give, which is a
    /// normal answer.
    State(Vec<u8>),
}

impl Vst3Answer {
    /// The editor status, for a caller that asked for one.
    ///
    /// # Errors
    /// A sentence, when the main thread answered something else. Unreachable
    /// unless this module and `vst3_host` disagree, which is a bug rather than a
    /// condition -- but a `String` beats a panic on the main thread's caller.
    pub fn into_gui(self) -> Result<Vst3GuiStatusDto, String> {
        match self {
            Self::Gui(status) => Ok(status),
            other => Err(format!("expected an editor status, got {other:?}")),
        }
    }

    /// The saved blob, for a caller that asked for one.
    ///
    /// # Errors
    /// See [`into_gui`](Self::into_gui).
    pub fn into_state(self) -> Result<Vec<u8>, String> {
        match self {
            Self::State(bytes) => Ok(bytes),
            other => Err(format!("expected a state blob, got {other:?}")),
        }
    }
}

/// State of one VST3 plugin's editor, as the webview sees it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Vst3GuiStatusDto {
    /// Whether an editor window is open.
    pub open: bool,
    /// Whether the plugin has an editor at all. False means the button should
    /// not be offered, rather than offered and always failing.
    pub has_editor: bool,
    /// Class id (hex) of the plugin whose editor this is, when one is placed.
    pub cid: Option<String>,
    /// Editor size in physical pixels; `0 x 0` when closed.
    pub width: u32,
    pub height: u32,
}

/// One placed VST3 plugin, as the mixer draws it.
///
/// The CLAP counterpart is `ClapSlotDto`. Kept separate because the identifying
/// fields genuinely differ -- `cid` is 32 hex characters, not a reverse-DNS
/// string -- and a shared struct would have had to call one of them by the
/// other's name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Vst3SlotDto {
    /// The host's opaque instance handle. Stable across track renumbering.
    pub instance: u64,
    /// Class id, as hex.
    pub cid: String,
    /// Display name; falls back to the class id for a module that declares none.
    pub name: String,
    pub vendor: String,
    /// Absolute path of the `.vst3` module it was loaded from.
    pub module_path: String,
    pub has_editor: bool,
    pub gui_open: bool,
}

/// Which VST3 plugin is where, published by the main thread.
#[derive(Debug, Default)]
pub struct Vst3Board {
    /// One entry per track index; `None` where the track is not VST3-headed.
    tracks: Mutex<Vec<Option<Vst3SlotDto>>>,
    master: Mutex<Option<Vst3SlotDto>>,
}

impl Vst3Board {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the whole picture. Called by the main thread after every change.
    pub fn publish(&self, tracks: Vec<Option<Vst3SlotDto>>, master: Option<Vst3SlotDto>) {
        *self.tracks.lock() = tracks;
        *self.master.lock() = master;
    }

    /// The plugin on `track`, if it is VST3-headed.
    pub fn track(&self, track: usize) -> Option<Vst3SlotDto> {
        self.tracks.lock().get(track).cloned().flatten()
    }

    /// The master insert, if a VST3 plugin is placed there.
    pub fn master(&self) -> Option<Vst3SlotDto> {
        self.master.lock().clone()
    }

    /// Whether any hosted VST3 plugin is placed at all.
    ///
    /// This is what decides whether a layout edit has to go through the VST3
    /// host (which rebuilds and re-attaches *both* formats) or can take the
    /// CLAP-only path.
    pub fn is_empty(&self) -> bool {
        self.master.lock().is_none() && self.tracks.lock().iter().all(Option::is_none)
    }

    /// How many plugins are placed. Diagnostics and tests.
    pub fn len(&self) -> usize {
        self.tracks
            .lock()
            .iter()
            .filter(|slot| slot.is_some())
            .count()
            + usize::from(self.master.lock().is_some())
    }
}

/// One queued request and the channel its answer goes back on.
type Pending = (Vst3Command, SyncSender<Result<Vst3Answer, String>>);

/// `Send + Sync` handle to the main thread's VST3 host.
pub struct Vst3HostBridge {
    queue: Mutex<VecDeque<Pending>>,
    /// Whether a main thread is actually serving this bridge. Without it every
    /// VST3 command in a headless build would block for
    /// [`REQUEST_TIMEOUT`](crate::clap_bridge::REQUEST_TIMEOUT) before failing.
    served: AtomicBool,
    board: Vst3Board,
    waker: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl Default for Vst3HostBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl Vst3HostBridge {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            served: AtomicBool::new(false),
            board: Vst3Board::new(),
            waker: Mutex::new(None),
        }
    }

    /// Record that a main thread is serving this bridge.
    pub fn set_served(&self, served: bool) {
        self.served.store(served, Ordering::Release);
    }

    /// Whether hosted VST3 plugins can be used in this session at all.
    pub fn is_served(&self) -> bool {
        self.served.load(Ordering::Acquire)
    }

    /// Which plugin is where.
    pub fn board(&self) -> &Vst3Board {
        &self.board
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
    pub fn request(&self, command: Vst3Command) -> Result<Vst3Answer, String> {
        if !self.is_served() {
            return Err(NO_MAIN_THREAD.to_owned());
        }
        let (reply, answer) = sync_channel(1);
        self.queue.lock().push_back((command, reply));
        if let Some(waker) = self.waker.lock().as_ref() {
            waker();
        }

        answer.recv_timeout(REQUEST_TIMEOUT).unwrap_or_else(|_| {
            Err(format!(
                "the VST3 host did not respond within {} s; is the main window still open?",
                REQUEST_TIMEOUT.as_secs()
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

impl std::fmt::Debug for Vst3HostBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vst3HostBridge")
            .field("served", &self.is_served())
            .field("pending", &self.pending())
            .field("placed", &self.board.len())
            .field("has_waker", &self.waker.lock().is_some())
            .finish()
    }
}

/// What a VST3 command answers when no event loop is running.
pub const NO_MAIN_THREAD: &str =
    "hosted VST3 plugins need MYDAW's main window, which is not running in this session";

/// Answer one request, ignoring a caller that has already given up.
pub fn reply(channel: &SyncSender<Result<Vst3Answer, String>>, answer: Result<Vst3Answer, String>) {
    let _ = channel.try_send(answer);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn slot(cid: &str) -> Vst3SlotDto {
        Vst3SlotDto {
            instance: 1,
            cid: cid.to_owned(),
            name: "Thing".to_owned(),
            vendor: "Acme".to_owned(),
            module_path: r"C:\plugins\Thing.vst3".to_owned(),
            has_editor: true,
            gui_open: false,
        }
    }

    #[test]
    fn a_request_with_no_main_thread_fails_immediately_rather_than_timing_out() {
        let bridge = Vst3HostBridge::new();
        assert!(!bridge.is_served());
        let started = Instant::now();
        let err = bridge.request(Vst3Command::ClearMasterEffect).unwrap_err();
        assert_eq!(err, NO_MAIN_THREAD);
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(bridge.pending(), 0, "nothing may be queued");
    }

    #[test]
    fn a_request_is_queued_answered_and_woken() {
        let bridge = Arc::new(Vst3HostBridge::new());
        bridge.set_served(true);
        let woken = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&woken);
        bridge.set_waker(Box::new(move || flag.store(true, Ordering::Release)));

        let main_thread = Arc::clone(&bridge);
        let loop_thread = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if let Some((command, channel)) = main_thread.drain().into_iter().next() {
                    assert_eq!(command, Vst3Command::OpenGui(Vst3Target::Master));
                    reply(
                        &channel,
                        Ok(Vst3Answer::Gui(Vst3GuiStatusDto {
                            open: true,
                            has_editor: true,
                            cid: Some("ABCD".to_owned()),
                            width: 800,
                            height: 600,
                        })),
                    );
                    return;
                }
                assert!(Instant::now() < deadline, "the request never arrived");
                std::thread::sleep(Duration::from_millis(5));
            }
        });

        let status = bridge
            .request(Vst3Command::OpenGui(Vst3Target::Master))
            .expect("answered")
            .into_gui()
            .expect("an editor status");
        assert!(status.open);
        assert_eq!(status.width, 800);
        assert!(woken.load(Ordering::Acquire), "the loop must be poked");
        assert_eq!(bridge.pending(), 0);
        loop_thread.join().unwrap();
    }

    #[test]
    fn requests_are_drained_in_the_order_they_were_made() {
        // "make this track a synth" then "open its editor" must not be reordered.
        let bridge = Vst3HostBridge::new();
        let commands = [
            Vst3Command::SetTrackSource {
                track: 0,
                module_path: PathBuf::from("a.vst3"),
                cid: "AA".to_owned(),
            },
            Vst3Command::OpenGui(Vst3Target::Track { index: 0 }),
            Vst3Command::CloseGui(Vst3Target::Track { index: 0 }),
        ];
        for command in &commands {
            let (channel, _answer) = sync_channel(1);
            bridge.queue.lock().push_back((command.clone(), channel));
        }
        let drained: Vec<Vst3Command> = bridge
            .drain()
            .into_iter()
            .map(|(command, _)| command)
            .collect();
        assert_eq!(drained, commands.to_vec());
        assert_eq!(bridge.pending(), 0);
    }

    #[test]
    fn replying_to_a_caller_that_gave_up_is_not_a_panic() {
        let bridge = Vst3HostBridge::new();
        let (channel, answer) = sync_channel(1);
        bridge
            .queue
            .lock()
            .push_back((Vst3Command::ClearMasterEffect, channel));
        drop(answer);

        let drained = bridge.drain();
        assert_eq!(drained.len(), 1);
        reply(&drained[0].1, Ok(Vst3Answer::Done));
    }

    #[test]
    fn the_board_answers_which_plugin_is_where_with_no_round_trip() {
        let board = Vst3Board::new();
        assert!(board.is_empty());
        assert_eq!(board.len(), 0);

        board.publish(vec![None, Some(slot("1111")), None], Some(slot("2222")));
        assert!(!board.is_empty());
        assert_eq!(board.len(), 2);
        assert!(board.track(0).is_none());
        assert_eq!(board.track(1).unwrap().cid, "1111");
        assert!(board.track(99).is_none(), "an index past the end is empty");
        assert_eq!(board.master().unwrap().cid, "2222");

        // A whole-picture republish cannot leave a stale entry behind.
        board.publish(vec![None], None);
        assert!(board.is_empty());
        assert!(board.track(1).is_none());
    }

    #[test]
    fn the_wire_shapes_are_what_the_webview_reads() {
        let json = serde_json::to_string(&slot("ABCD")).unwrap();
        assert!(json.contains("\"cid\""), "{json}");
        assert!(json.contains("\"guiOpen\""), "{json}");
        assert!(json.contains("\"modulePath\""), "{json}");

        let status = Vst3GuiStatusDto {
            has_editor: true,
            ..Vst3GuiStatusDto::default()
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"hasEditor\""), "{json}");
    }

    #[test]
    fn a_vst3_target_is_the_same_slot_a_clap_target_names() {
        // Load-bearing: the master insert holds one plugin of either format, so
        // both hosts have to be able to say "the master insert" in the same
        // words.
        let target: Vst3Target = ClapTarget::Master;
        assert_eq!(
            serde_json::to_string(&target).unwrap(),
            r#"{"slot":"master"}"#
        );
        assert_eq!(ClapTarget::Track { index: 2 }.to_string(), "track 2");
    }

    #[test]
    fn an_answer_of_the_wrong_shape_is_reported_rather_than_unwrapped() {
        assert!(Vst3Answer::Done.into_gui().is_err());
        assert!(Vst3Answer::Done.into_state().is_err());
        assert_eq!(
            Vst3Answer::State(vec![1, 2]).into_state().unwrap(),
            vec![1, 2]
        );
    }
}
