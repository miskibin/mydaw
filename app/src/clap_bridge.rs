//! The one mailbox between the Tauri commands and the main thread's CLAP host.
//!
//! # Why a mailbox and not a method call
//!
//! Everything about a hosted CLAP plugin is `[main-thread]` work on a `!Send`
//! value: creating an instance, activating it, opening its editor, saving its
//! state, destroying it. Tauri commands run on a worker pool and Tauri's managed
//! state must be `Send + Sync`, so a command cannot reach any of it. What a
//! command *can* do is leave a request somewhere and wait for the main thread's
//! answer -- which is what this is.
//!
//! ```text
//!   webview                 command (worker pool)         main thread (tao loop)
//!   -------                 ---------------------         ----------------------
//!   setTrackSourceClap  ->  bridge.request(..) ---------> drain() in MainEventsCleared
//!                             blocks on a reply channel     ClapHost creates the instance,
//!                                                           activates it, swaps the node in
//!                           <----------------------------- reply(Done)
//!   mixer state         <-  Ok(())                          board.publish(..)
//! ```
//!
//! # One bridge, not two
//!
//! Spike 3 had `ClapGuiBridge` and step 5 added `ClapSourceBridge`: two mailboxes
//! with the same shape, drained at two points in the same event-loop pass, and
//! therefore two independent orderings for operations that are emphatically not
//! independent -- "make this track a synth" and "open that synth's editor" must
//! happen in the order they were asked for, or the second addresses a plugin
//! that does not exist yet. One queue makes that ordering automatic instead of
//! something the run loop has to get right by hand.
//!
//! Every command names its [`ClapTarget`], because with many instances the
//! question "which plugin" is no longer rhetorical.
//!
//! # The wait is bounded
//! [`REQUEST_TIMEOUT`]: a UI that hangs because the main loop is busy is worse
//! than one that says so. The [`waker`](ClapHostBridge::set_waker) exists
//! because the loop may be idle inside `GetMessage` with nothing to wake it;
//! Tauri's `run_on_main_thread` posts an event, which is enough to make the loop
//! turn once and drain the queue.
//!
//! # The status board
//! Answers describe one operation. The *state* -- which track holds which
//! plugin, whether its editor is open -- is published to [`ClapBoard`] by the
//! main thread after every change, so a mixer refresh reads it with no round
//! trip at all. That matters because `get_tracks` is called after every fader
//! move.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

/// How long a Tauri command waits for the main thread before giving up.
///
/// Generous: creating an instance loads a bundle from disk and runs a vendor's
/// initialisation; opening an editor runs more of it. Short enough that a wedged
/// main loop shows up as an error message instead of a frozen window.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Where a hosted plugin sits.
///
/// A track index rather than a track id, because every other command in the
/// mixer surface is indexed and mixing the two would be a bug waiting for a
/// track removal. The main thread renumbers its own placements when the layout
/// changes, so the two never disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "slot")]
pub enum ClapTarget {
    /// The instrument at the head of a track.
    Track { index: usize },
    /// The insert on the master bus.
    Master,
}

impl std::fmt::Display for ClapTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Track { index } => write!(f, "track {index}"),
            Self::Master => f.write_str("the master insert"),
        }
    }
}

/// What the UI can ask the main thread to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClapCommand {
    /// Create an instance of `plugin_id` from `bundle_path` and make it the
    /// source of `track`, destroying whatever was there.
    SetTrackSource {
        track: usize,
        bundle_path: PathBuf,
        plugin_id: String,
    },
    /// Create an instance and make it the master insert.
    SetMasterEffect {
        bundle_path: PathBuf,
        plugin_id: String,
    },
    /// Put a **built-in** source at the head of a track that currently holds a
    /// hosted plugin, and destroy that plugin.
    ///
    /// The swap itself needs no main thread -- a built-in node is built from a
    /// `Send + Sync` factory -- but destroying the instance it displaces does,
    /// and leaving one hosted with nothing pointing at it is a leak nobody can
    /// see. So the pair travels together.
    SetTrackBuiltIn { track: usize, source_id: String },
    /// Empty the master insert and destroy its instance.
    ClearMasterEffect,
    /// Open a plugin's editor in its own native window.
    OpenGui(ClapTarget),
    /// Close it. Not an error when nothing is open.
    CloseGui(ClapTarget),
    /// Serialise a plugin. Groundwork for the project file (step 8).
    SaveState(ClapTarget),
    /// Restore a plugin from a blob produced by [`ClapCommand::SaveState`].
    LoadState { target: ClapTarget, state: Vec<u8> },
    /// Append a track and rebuild the graph, re-attaching every hosted plugin.
    AddTrack {
        source_id: String,
        name: Option<String>,
    },
    /// Remove a track and rebuild the graph, re-attaching every hosted plugin
    /// and renumbering the placements after it.
    RemoveTrack { track: usize },
}

/// What the main thread answers with.
///
/// Deliberately small: the *state* the UI draws comes from [`ClapBoard`] and the
/// mixer DTO, which every mixer command returns anyway. An answer only has to
/// carry what is specific to the operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClapAnswer {
    /// The operation succeeded and has nothing to report.
    Done,
    /// The editor's state after an open or close.
    Gui(ClapGuiStatusDto),
    /// A saved state blob. Empty for a plugin with no `state` extension, which
    /// is a normal answer -- see `mydaw_plugin_host_clap::state`.
    State(Vec<u8>),
}

impl ClapAnswer {
    /// The editor status, for a caller that asked for one.
    ///
    /// # Errors
    /// A sentence, when the main thread answered something else. Unreachable
    /// unless this module and `clap_host` disagree, which is a bug rather than a
    /// condition -- but a `String` beats a panic on the main thread's caller.
    pub fn into_gui(self) -> Result<ClapGuiStatusDto, String> {
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

/// State of one plugin's editor, as the webview sees it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClapGuiStatusDto {
    /// Whether an editor window is open.
    pub open: bool,
    /// Whether the plugin has an editor at all. False means the button should
    /// not be offered, rather than offered and always failing.
    pub has_editor: bool,
    /// CLAP id of the plugin whose editor this is, when one is placed there.
    pub plugin_id: Option<String>,
    /// Editor size in physical pixels; `0 x 0` when closed.
    pub width: u32,
    pub height: u32,
}

impl ClapGuiStatusDto {
    /// Nothing open, and (optionally) a plugin that could be opened.
    pub fn closed(plugin_id: Option<String>, has_editor: bool) -> Self {
        Self {
            open: false,
            has_editor,
            plugin_id,
            width: 0,
            height: 0,
        }
    }
}

/// One placed plugin, as the mixer draws it.
///
/// Published by the main thread after every change; read by `get_tracks`, which
/// runs after every fader move and must not make a round trip to get it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClapSlotDto {
    /// The host's opaque instance handle. Stable across track renumbering, so a
    /// UI can tell "the same plugin moved" from "a different plugin".
    pub instance: u64,
    pub plugin_id: String,
    /// Display name; falls back to the id for a bundle that declares none.
    pub name: String,
    pub bundle_path: String,
    pub has_editor: bool,
    pub gui_open: bool,
}

/// Which plugin is where, published by the main thread.
///
/// `Send + Sync` and read from command threads. Kept separate from the answer
/// channel because it is a *snapshot*, not an event: the UI wants to know the
/// current arrangement whenever it draws, not only when it changed something.
#[derive(Debug, Default)]
pub struct ClapBoard {
    /// One entry per track index; `None` where the track is not CLAP-headed.
    /// Truncated and re-published on every layout change, so a stale entry
    /// cannot outlive the track it described.
    tracks: Mutex<Vec<Option<ClapSlotDto>>>,
    master: Mutex<Option<ClapSlotDto>>,
}

impl ClapBoard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the whole picture. Called by the main thread after every change.
    ///
    /// Whole-picture rather than per-slot, for the same reason every mixer
    /// command answers with the whole `MixerState`: a partial update has an
    /// ordering problem and a merge to get wrong, and this is a handful of short
    /// strings.
    pub fn publish(&self, tracks: Vec<Option<ClapSlotDto>>, master: Option<ClapSlotDto>) {
        *self.tracks.lock() = tracks;
        *self.master.lock() = master;
    }

    /// The plugin on `track`, if it is CLAP-headed.
    pub fn track(&self, track: usize) -> Option<ClapSlotDto> {
        self.tracks.lock().get(track).cloned().flatten()
    }

    /// The master insert, if one is placed.
    pub fn master(&self) -> Option<ClapSlotDto> {
        self.master.lock().clone()
    }

    /// Whether any hosted plugin is placed at all.
    ///
    /// This is what decides whether a layout edit has to go through the main
    /// thread or can be done inline on the calling thread -- see
    /// [`AppState::add_track`](crate::AppState::add_track).
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
type Pending = (ClapCommand, SyncSender<Result<ClapAnswer, String>>);

/// `Send + Sync` handle to the main thread's CLAP host.
pub struct ClapHostBridge {
    queue: Mutex<VecDeque<Pending>>,
    /// Whether a main thread is actually serving this bridge.
    ///
    /// Set once by the shell before the event loop starts. Without it every
    /// CLAP command in a headless build -- a test, a capture run, a machine
    /// whose window failed to open -- would block for [`REQUEST_TIMEOUT`]
    /// before failing, five seconds at a time.
    served: AtomicBool,
    board: ClapBoard,
    waker: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl Default for ClapHostBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl ClapHostBridge {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            served: AtomicBool::new(false),
            board: ClapBoard::new(),
            waker: Mutex::new(None),
        }
    }

    /// Record that a main thread is serving this bridge. Called once by the
    /// shell, immediately before it enters the event loop.
    pub fn set_served(&self, served: bool) {
        self.served.store(served, Ordering::Release);
    }

    /// Whether hosted CLAP plugins can be used in this session at all.
    pub fn is_served(&self) -> bool {
        self.served.load(Ordering::Acquire)
    }

    /// Which plugin is where.
    pub fn board(&self) -> &ClapBoard {
        &self.board
    }

    /// Install the callback that wakes the event loop.
    ///
    /// Set once, after the Tauri app exists. Until then requests still queue;
    /// they are just served whenever the loop next turns.
    pub fn set_waker(&self, waker: Box<dyn Fn() + Send + Sync>) {
        *self.waker.lock() = Some(waker);
    }

    /// Queue `command` and wait for the main thread's answer.
    ///
    /// # Errors
    /// [`NO_MAIN_THREAD`] without a round trip when nothing is serving the
    /// bridge, whatever the main thread reports, or a timeout message when the
    /// loop never answered.
    pub fn request(&self, command: ClapCommand) -> Result<ClapAnswer, String> {
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
                "the CLAP host did not respond within {} s; is the main window still open?",
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

impl std::fmt::Debug for ClapHostBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClapHostBridge")
            .field("served", &self.is_served())
            .field("pending", &self.pending())
            .field("placed", &self.board.len())
            .field("has_waker", &self.waker.lock().is_some())
            .finish()
    }
}

/// What a CLAP command answers when no event loop is running.
///
/// The honest sentence for a headless build (a test, a `MYDAW_DEBUG_CAPTURE`
/// run, a window that failed to open): hosted plugins need a main thread, and
/// there is not one.
pub const NO_MAIN_THREAD: &str =
    "hosted CLAP plugins need MYDAW's main window, which is not running in this session";

// ---------------------------------------------------------------------------
// Base64
// ---------------------------------------------------------------------------
//
// A plugin state blob is opaque binary. It crosses the IPC boundary as JSON, and
// a JSON array of numbers costs about four characters per byte; base64 costs
// four per three. Forty lines of arithmetic, written here rather than pulled in
// as a dependency, because that is all it is and because a project file will
// want exactly the same encoding.

/// The standard alphabet (RFC 4648), with padding.
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encode a state blob for the wire.
pub fn to_base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(BASE64[(n >> 18) as usize & 63] as char);
        out.push(BASE64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            BASE64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            BASE64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Decode a state blob from the wire.
///
/// # Errors
/// A sentence, for a string that is not base64. Reachable from a corrupted or
/// hand-edited project file, so it is reported rather than assumed away.
pub fn from_base64(text: &str) -> Result<Vec<u8>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let symbols: &[u8] = text.as_bytes();
    if !symbols.len().is_multiple_of(4) {
        return Err(
            "the state blob is not valid base64 (its length is not a multiple of 4)".into(),
        );
    }

    let value = |symbol: u8| -> Result<u32, String> {
        BASE64
            .iter()
            .position(|candidate| *candidate == symbol)
            .map(|index| index as u32)
            .ok_or_else(|| {
                format!(
                    "the state blob contains '{}', which is not base64",
                    symbol as char
                )
            })
    };

    let mut out = Vec::with_capacity(symbols.len() / 4 * 3);
    for quad in symbols.chunks(4) {
        let padding = quad.iter().rev().take_while(|s| **s == b'=').count();
        if padding > 2 || quad[..4 - padding].contains(&b'=') {
            return Err("the state blob has misplaced base64 padding".into());
        }
        let mut n = 0_u32;
        for (index, symbol) in quad.iter().enumerate() {
            let bits = if *symbol == b'=' { 0 } else { value(*symbol)? };
            n |= bits << (18 - 6 * index);
        }
        out.push((n >> 16) as u8);
        if padding < 2 {
            out.push((n >> 8) as u8);
        }
        if padding < 1 {
            out.push(n as u8);
        }
    }
    Ok(out)
}

/// Answer one request, ignoring a caller that has already given up.
///
/// A command that timed out has dropped its receiver; the main thread must not
/// care, and must certainly not panic.
pub fn reply(channel: &SyncSender<Result<ClapAnswer, String>>, answer: Result<ClapAnswer, String>) {
    let _ = channel.try_send(answer);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Instant;

    fn slot(id: &str) -> ClapSlotDto {
        ClapSlotDto {
            instance: 1,
            plugin_id: id.to_owned(),
            name: "Thing".to_owned(),
            bundle_path: r"C:\plugins\thing.clap".to_owned(),
            has_editor: true,
            gui_open: false,
        }
    }

    #[test]
    fn a_request_with_no_main_thread_fails_immediately_rather_than_timing_out() {
        // Five seconds per command in a headless test run would be intolerable,
        // and the answer is knowable without asking anyone.
        let bridge = ClapHostBridge::new();
        assert!(!bridge.is_served());
        let started = Instant::now();
        let err = bridge.request(ClapCommand::ClearMasterEffect).unwrap_err();
        assert_eq!(err, NO_MAIN_THREAD);
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(bridge.pending(), 0, "nothing may be queued");
    }

    #[test]
    fn a_request_is_queued_answered_and_woken() {
        let bridge = Arc::new(ClapHostBridge::new());
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
                        ClapCommand::OpenGui(ClapTarget::Track { index: 2 }),
                        "the command names the target it is for"
                    );
                    reply(
                        &channel,
                        Ok(ClapAnswer::Gui(ClapGuiStatusDto {
                            open: true,
                            has_editor: true,
                            plugin_id: Some("com.acme.thing".to_owned()),
                            width: 400,
                            height: 250,
                        })),
                    );
                    return;
                }
                assert!(Instant::now() < deadline, "the request never arrived");
                std::thread::sleep(Duration::from_millis(5));
            }
        });

        let status = bridge
            .request(ClapCommand::OpenGui(ClapTarget::Track { index: 2 }))
            .expect("answered")
            .into_gui()
            .expect("an editor status");
        assert!(status.open);
        assert_eq!(status.width, 400);
        assert!(woken.load(Ordering::Acquire), "the loop must be poked");
        assert_eq!(bridge.pending(), 0);
        loop_thread.join().unwrap();
    }

    #[test]
    fn an_error_from_the_main_thread_reaches_the_caller_verbatim() {
        let bridge = Arc::new(ClapHostBridge::new());
        bridge.set_served(true);
        let main_thread = Arc::clone(&bridge);
        let loop_thread = std::thread::spawn(move || loop {
            if let Some((_, channel)) = main_thread.drain().into_iter().next() {
                reply(&channel, Err("this plugin has no editor".to_owned()));
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        });

        let err = bridge
            .request(ClapCommand::OpenGui(ClapTarget::Master))
            .unwrap_err();
        assert_eq!(err, "this plugin has no editor");
        loop_thread.join().unwrap();
    }

    #[test]
    fn requests_are_drained_in_the_order_they_were_made() {
        // The whole reason there is one bridge rather than two: "make this track
        // a synth" then "open its editor" must not be reordered.
        let bridge = ClapHostBridge::new();
        let commands = [
            ClapCommand::SetTrackSource {
                track: 0,
                bundle_path: PathBuf::from("a.clap"),
                plugin_id: "a".to_owned(),
            },
            ClapCommand::OpenGui(ClapTarget::Track { index: 0 }),
            ClapCommand::CloseGui(ClapTarget::Track { index: 0 }),
        ];
        for command in &commands {
            let (channel, _answer) = sync_channel(1);
            bridge.queue.lock().push_back((command.clone(), channel));
        }
        let drained: Vec<ClapCommand> = bridge
            .drain()
            .into_iter()
            .map(|(command, _)| command)
            .collect();
        assert_eq!(drained, commands.to_vec());
        assert_eq!(bridge.pending(), 0);
    }

    #[test]
    fn replying_to_a_caller_that_gave_up_is_not_a_panic() {
        let bridge = ClapHostBridge::new();
        let (channel, answer) = sync_channel(1);
        bridge
            .queue
            .lock()
            .push_back((ClapCommand::ClearMasterEffect, channel));
        drop(answer);

        let drained = bridge.drain();
        assert_eq!(drained.len(), 1);
        reply(&drained[0].1, Ok(ClapAnswer::Done));
    }

    #[test]
    fn the_board_answers_which_plugin_is_where_with_no_round_trip() {
        let board = ClapBoard::new();
        assert!(board.is_empty());
        assert_eq!(board.len(), 0);

        board.publish(
            vec![None, Some(slot("com.acme.synth")), None],
            Some(slot("com.acme.reverb")),
        );
        assert!(!board.is_empty());
        assert_eq!(board.len(), 2);
        assert!(board.track(0).is_none());
        assert_eq!(board.track(1).unwrap().plugin_id, "com.acme.synth");
        assert!(board.track(2).is_none());
        assert!(board.track(99).is_none(), "an index past the end is empty");
        assert_eq!(board.master().unwrap().plugin_id, "com.acme.reverb");

        // A whole-picture republish cannot leave a stale entry behind.
        board.publish(vec![None], None);
        assert!(board.is_empty());
        assert!(board.track(1).is_none());
    }

    #[test]
    fn the_wire_shapes_are_what_the_webview_reads() {
        let json = serde_json::to_string(&slot("com.acme.thing")).unwrap();
        assert!(json.contains("\"pluginId\""), "{json}");
        assert!(json.contains("\"guiOpen\""), "{json}");
        assert!(json.contains("\"bundlePath\""), "{json}");

        let status = ClapGuiStatusDto::closed(Some("com.acme.thing".to_owned()), true);
        assert!(!status.open && status.has_editor);
        assert_eq!((status.width, status.height), (0, 0));
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"hasEditor\""), "{json}");

        // The target is tagged, so TypeScript reads a discriminated union.
        let track = serde_json::to_string(&ClapTarget::Track { index: 3 }).unwrap();
        assert_eq!(track, r#"{"slot":"track","index":3}"#);
        assert_eq!(
            serde_json::to_string(&ClapTarget::Master).unwrap(),
            r#"{"slot":"master"}"#
        );
        assert_eq!(
            serde_json::from_str::<ClapTarget>(r#"{"slot":"track","index":3}"#).unwrap(),
            ClapTarget::Track { index: 3 }
        );
        assert_eq!(ClapTarget::Master.to_string(), "the master insert");
        assert_eq!(ClapTarget::Track { index: 3 }.to_string(), "track 3");
    }

    #[test]
    fn base64_round_trips_every_blob_length_including_the_empty_one() {
        // A plugin's state is opaque binary of any length, and the three
        // residues mod 3 are the whole of base64's behaviour.
        for length in 0..32_usize {
            let blob: Vec<u8> = (0..length).map(|i| (i as u8).wrapping_mul(37)).collect();
            let text = to_base64(&blob);
            assert_eq!(text.len() % 4, 0, "length {length} is not padded");
            assert_eq!(from_base64(&text).unwrap(), blob, "length {length}");
        }
        assert_eq!(to_base64(&[]), "");
        assert!(from_base64("").unwrap().is_empty());
    }

    #[test]
    fn base64_matches_the_standard_alphabet_on_known_vectors() {
        // Pinned against RFC 4648's own examples, so a rewrite cannot quietly
        // invent a dialect no other tool can read.
        assert_eq!(to_base64(b"f"), "Zg==");
        assert_eq!(to_base64(b"fo"), "Zm8=");
        assert_eq!(to_base64(b"foo"), "Zm9v");
        assert_eq!(to_base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(from_base64("Zm9vYmFy").unwrap(), b"foobar");
        assert_eq!(to_base64(&[0xff, 0xef, 0xfe]), "/+/+");
    }

    #[test]
    fn a_state_blob_that_is_not_base64_is_reported_rather_than_guessed_at() {
        // Reachable from a hand-edited or corrupted project file.
        for bad in ["Zm9", "Zm9v!===", "====", "Z=9v"] {
            assert!(from_base64(bad).is_err(), "{bad} should be rejected");
        }
        // Whitespace around the payload is a JSON reality, not corruption.
        assert_eq!(from_base64("  Zm9v \n").unwrap(), b"foo");
    }

    #[test]
    fn an_answer_of_the_wrong_shape_is_reported_rather_than_unwrapped() {
        assert!(ClapAnswer::Done.into_gui().is_err());
        assert!(ClapAnswer::Done.into_state().is_err());
        assert_eq!(
            ClapAnswer::State(vec![1, 2]).into_state().unwrap(),
            vec![1, 2]
        );
    }
}
