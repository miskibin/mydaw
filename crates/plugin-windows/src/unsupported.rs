//! Backend for platforms with no plugin-window implementation (everything but
//! Windows). Everything above this file compiles and is tested anywhere; only
//! these calls are missing. macOS (`NSView`) and X11 backends slot in here.

use std::rc::Rc;
use std::sync::atomic::AtomicU64;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use crate::service::{WindowError, WindowEvent, WindowId, WindowOptions, WindowSize};

pub(crate) fn create_window(
    _options: &WindowOptions,
    _size: WindowSize,
    _events: Sender<WindowEvent>,
    _ticks: Arc<AtomicU64>,
) -> Result<isize, WindowError> {
    Err(WindowError::Unsupported)
}

pub(crate) fn destroy_window(_hwnd: isize) {}

pub(crate) fn show_window(_hwnd: isize) {}

pub(crate) fn request_close(_hwnd: isize) {}

pub(crate) fn set_tick_handler(_hwnd: isize, _handler: Option<Rc<dyn Fn()>>) {}

pub(crate) fn dpi(_hwnd: isize) -> u32 {
    96
}

/// "Cannot tell" rather than "no".
pub(crate) fn has_child_window(_hwnd: isize) -> Result<bool, WindowError> {
    Err(WindowError::Unsupported)
}

pub(crate) fn client_size(_id: WindowId, _hwnd: isize) -> Result<WindowSize, WindowError> {
    Err(WindowError::Unsupported)
}

pub(crate) fn set_client_size(
    _id: WindowId,
    _hwnd: isize,
    _size: WindowSize,
) -> Result<(), WindowError> {
    Err(WindowError::Unsupported)
}
