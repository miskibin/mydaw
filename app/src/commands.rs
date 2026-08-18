//! The `#[tauri::command]` surface. Contract: `ui/README.md` + `ui/src/api/backend.ts`.
//!
//! Every command is a one-liner over [`AppState`]: the shell keeps its logic in
//! testable modules and this file only adapts it to Tauri's calling convention
//! (managed state in, `Result<_, String>` out -- `AudioIoError` is not
//! `Serialize`, and the UI shows the message as-is in the status bar).

use mydaw_ipc_bridge::UiSnapshot;
use tauri::ipc::Channel;
use tauri::State;

use crate::dto::MeterFrameDto;
use crate::state::AppState;

/// Plugins and panels, as of boot. Called once by the UI on startup.
#[tauri::command]
pub fn get_ui_snapshot(state: State<'_, AppState>) -> UiSnapshot {
    state.snapshot()
}

/// Start the transport. The position is wherever it was left.
#[tauri::command]
pub fn transport_play(state: State<'_, AppState>) -> Result<(), String> {
    state.set_playing(true).map_err(|err| err.to_string())
}

/// Stop the transport, keeping the playhead where it is (no implicit rewind --
/// the UI decides what Stop means visually).
#[tauri::command]
pub fn transport_stop(state: State<'_, AppState>) -> Result<(), String> {
    state.set_playing(false).map_err(|err| err.to_string())
}

/// Stream throttled meter frames into `channel` until the webview drops it.
///
/// One subscription is live at a time; a second call (a page reload) replaces the
/// first. `Channel::send` failing is how the pump learns the receiver is gone.
#[tauri::command]
pub fn subscribe_meters(state: State<'_, AppState>, channel: Channel<MeterFrameDto>) {
    state
        .meters()
        .subscribe(Box::new(move |frame| channel.send(frame).is_ok()));
}
