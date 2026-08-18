//! Backend for platforms that have no plugin-window implementation yet.
//!
//! MYDAW is Windows-first but the code stays cross-platform-clean, as the
//! scaffold spec requires: everything above this file compiles and is tested on
//! any target, and only the four Win32 calls are missing. macOS (`NSView` +
//! `CLAP_WINDOW_API_COCOA`) and X11 backends slot in here.

use std::sync::atomic::AtomicU64;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use crate::service::{
    PluginWindowError, PluginWindowEvent, PluginWindowId, WindowOptions, WindowSize,
};

pub(crate) fn create_window(
    _options: &WindowOptions,
    _size: WindowSize,
    _events: Sender<PluginWindowEvent>,
    _ticks: Arc<AtomicU64>,
) -> Result<isize, PluginWindowError> {
    Err(PluginWindowError::Unsupported)
}

/// Unreachable in practice: no window can have been created to destroy.
pub(crate) fn destroy_window(_hwnd: isize) {}

pub(crate) fn show_window(_hwnd: isize) {}

pub(crate) fn dpi(_hwnd: isize) -> u32 {
    96
}

/// "Cannot tell" rather than "no": the caller must not conclude anything from a
/// platform that has no windows at all.
pub(crate) fn has_child_window(_hwnd: isize) -> Result<bool, PluginWindowError> {
    Err(PluginWindowError::Unsupported)
}

pub(crate) fn client_size(
    _id: PluginWindowId,
    _hwnd: isize,
) -> Result<WindowSize, PluginWindowError> {
    Err(PluginWindowError::Unsupported)
}

pub(crate) fn set_client_size(
    _id: PluginWindowId,
    _hwnd: isize,
    _size: WindowSize,
) -> Result<(), PluginWindowError> {
    Err(PluginWindowError::Unsupported)
}
