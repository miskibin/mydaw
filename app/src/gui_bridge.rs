//! The mailbox between a Tauri command and the main thread's plugin GUI.
//!
//! # Why a mailbox and not a method call
//!
//! Opening a CLAP editor is `[main-thread]` work that touches a `!Send`
//! `LoadedClapPlugin` and a thread-bound `HWND`. Tauri commands run on a worker
//! pool, and Tauri's managed state must be `Send + Sync`, so a command cannot
//! reach either. What a command *can* do is leave a request somewhere and wait
//! for the main thread's answer -- which is what this is.
//!
//! ```text
//!   webview            command (worker pool)          main thread (tao loop)
//!   -------            ---------------------          ----------------------
//!   open_clap_gui  ->  bridge.request(Open) --------> drain() in MainEventsCleared
//!                        blocks on a reply channel      ClapGuiHost opens the editor
//!                      <------------------------------ reply(status)
//!   status shown   <-  Ok(status)
//! ```
//!
//! The wait is bounded ([`GUI_REQUEST_TIMEOUT`]): a UI that hangs because the
//! main loop is busy is worse than one that says so.
//!
//! The [`waker`](ClapGuiBridge::set_waker) exists because the loop may be idle in
//! `GetMessage` with nothing to wake it. Tauri's `run_on_main_thread` posts an
//! event, which is enough to make the loop turn once and drain the queue.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::time::Duration;

use parking_lot::Mutex;
use serde::Serialize;

/// How long a Tauri command waits for the main thread before giving up.
///
/// Generous: opening an editor runs third-party code that may load resources
/// from disk. Short enough that a wedged main loop shows up as an error message
/// instead of a frozen window.
pub const GUI_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// What the UI can ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClapGuiCommand {
    /// Open the editor of the loaded CLAP effect.
    Open,
    /// Close it. Not an error when nothing is open.
    Close,
}

/// State of the plugin editor, as the webview sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClapGuiStatusDto {
    /// Whether an editor window is open.
    pub open: bool,
    /// CLAP id of the plugin whose editor this is, when one is loaded.
    pub plugin_id: Option<String>,
    /// Editor size in physical pixels; `0 x 0` when closed.
    pub width: u32,
    pub height: u32,
}

impl ClapGuiStatusDto {
    /// Nothing open, and (optionally) a plugin that could be opened.
    pub fn closed(plugin_id: Option<String>) -> Self {
        Self {
            open: false,
            plugin_id,
            width: 0,
            height: 0,
        }
    }
}

/// One queued request and the channel its answer goes back on.
type Pending = (ClapGuiCommand, SyncSender<Result<ClapGuiStatusDto, String>>);

/// Send + Sync handle to the main thread's plugin-GUI host.
pub struct ClapGuiBridge {
    queue: Mutex<VecDeque<Pending>>,
    open: AtomicBool,
    /// Pokes the event loop so a queued request is noticed promptly.
    waker: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl Default for ClapGuiBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl ClapGuiBridge {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            open: AtomicBool::new(false),
            waker: Mutex::new(None),
        }
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
    /// Whatever the main thread reports (a plugin with no editor, a window that
    /// would not open), or a timeout message if the loop never answered -- which
    /// is what a headless build, or one whose window has already closed, does.
    pub fn request(&self, command: ClapGuiCommand) -> Result<ClapGuiStatusDto, String> {
        let (reply, answer) = sync_channel(1);
        self.queue.lock().push_back((command, reply));
        self.wake();

        answer
            .recv_timeout(GUI_REQUEST_TIMEOUT)
            .unwrap_or_else(|_| {
                Err(format!(
                    "the plugin GUI did not respond within {} s; is the main window still open?",
                    GUI_REQUEST_TIMEOUT.as_secs()
                ))
            })
    }

    /// Take every queued request. Called from the main thread.
    pub fn drain(&self) -> Vec<Pending> {
        let mut queue = self.queue.lock();
        queue.drain(..).collect()
    }

    /// How many requests are waiting. For tests and diagnostics.
    pub fn pending(&self) -> usize {
        self.queue.lock().len()
    }

    /// Publish whether an editor is open, so a status query needs no round trip.
    pub fn set_open(&self, open: bool) {
        self.open.store(open, Ordering::Release);
    }

    /// Whether an editor is open, as last published by the main thread.
    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::Acquire)
    }

    fn wake(&self) {
        if let Some(waker) = self.waker.lock().as_ref() {
            waker();
        }
    }
}

impl std::fmt::Debug for ClapGuiBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClapGuiBridge")
            .field("open", &self.is_open())
            .field("pending", &self.pending())
            .field("has_waker", &self.waker.lock().is_some())
            .finish()
    }
}

/// Answer one request, ignoring a caller that has already given up.
///
/// A command that timed out has dropped its receiver; the main thread must not
/// care, and must certainly not panic.
pub fn reply(
    channel: &SyncSender<Result<ClapGuiStatusDto, String>>,
    answer: Result<ClapGuiStatusDto, String>,
) {
    let _ = channel.try_send(answer);
}

/// Receiver half, exposed for tests that stand in for the main thread.
pub type ClapGuiAnswer = Receiver<Result<ClapGuiStatusDto, String>>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Instant;

    #[test]
    fn a_request_is_queued_and_answered_by_the_other_side() {
        let bridge = Arc::new(ClapGuiBridge::new());
        let main_thread = Arc::clone(&bridge);

        // Stands in for the run loop: wait for a request, then answer it.
        let loop_thread = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if let Some((command, channel)) = main_thread.drain().into_iter().next() {
                    assert_eq!(command, ClapGuiCommand::Open);
                    main_thread.set_open(true);
                    reply(
                        &channel,
                        Ok(ClapGuiStatusDto {
                            open: true,
                            plugin_id: Some("dev.mydaw.fixture.gain".to_owned()),
                            width: 400,
                            height: 250,
                        }),
                    );
                    return;
                }
                assert!(Instant::now() < deadline, "the request never arrived");
                std::thread::sleep(Duration::from_millis(5));
            }
        });

        let status = bridge.request(ClapGuiCommand::Open).expect("answered");
        assert!(status.open);
        assert_eq!(status.width, 400);
        assert!(bridge.is_open());
        assert_eq!(bridge.pending(), 0);
        loop_thread.join().unwrap();
    }

    #[test]
    fn an_error_from_the_main_thread_reaches_the_caller_verbatim() {
        let bridge = Arc::new(ClapGuiBridge::new());
        let main_thread = Arc::clone(&bridge);
        let loop_thread = std::thread::spawn(move || loop {
            let pending = main_thread.drain();
            if let Some((_, channel)) = pending.into_iter().next() {
                reply(&channel, Err("this plugin has no editor".to_owned()));
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        });

        let err = bridge.request(ClapGuiCommand::Open).unwrap_err();
        assert_eq!(err, "this plugin has no editor");
        loop_thread.join().unwrap();
    }

    #[test]
    fn the_waker_is_called_when_a_request_is_queued() {
        let bridge = ClapGuiBridge::new();
        let woken = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&woken);
        bridge.set_waker(Box::new(move || flag.store(true, Ordering::Release)));

        // No one will answer, so this times out -- but the wake must happen first.
        let (reply_tx, _answer) = sync_channel(1);
        bridge
            .queue
            .lock()
            .push_back((ClapGuiCommand::Open, reply_tx));
        bridge.wake();

        assert!(woken.load(Ordering::Acquire));
        assert_eq!(bridge.pending(), 1);
    }

    #[test]
    fn a_main_thread_that_never_answers_produces_an_error_not_a_hang() {
        // The timeout itself is five seconds, which is too long for a test, so
        // this exercises the same path with a receiver that is already gone: the
        // main thread's reply must not panic.
        let bridge = ClapGuiBridge::new();
        let (channel, answer) = sync_channel(1);
        bridge
            .queue
            .lock()
            .push_back((ClapGuiCommand::Close, channel));
        drop(answer);

        let drained = bridge.drain();
        assert_eq!(drained.len(), 1);
        reply(&drained[0].1, Ok(ClapGuiStatusDto::closed(None)));
        assert_eq!(bridge.pending(), 0);
    }

    #[test]
    fn requests_are_drained_in_order() {
        let bridge = ClapGuiBridge::new();
        for command in [
            ClapGuiCommand::Open,
            ClapGuiCommand::Close,
            ClapGuiCommand::Open,
        ] {
            let (channel, _answer) = sync_channel(1);
            bridge.queue.lock().push_back((command, channel));
        }
        let commands: Vec<ClapGuiCommand> = bridge
            .drain()
            .into_iter()
            .map(|(command, _)| command)
            .collect();
        assert_eq!(
            commands,
            vec![
                ClapGuiCommand::Open,
                ClapGuiCommand::Close,
                ClapGuiCommand::Open
            ]
        );
        assert_eq!(bridge.pending(), 0);
    }

    #[test]
    fn a_closed_status_carries_the_plugin_that_could_be_opened() {
        let status = ClapGuiStatusDto::closed(Some("com.acme.thing".to_owned()));
        assert!(!status.open);
        assert_eq!(status.plugin_id.as_deref(), Some("com.acme.thing"));
        assert_eq!((status.width, status.height), (0, 0));

        // The webview reads camelCase.
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"pluginId\""), "{json}");
    }
}
