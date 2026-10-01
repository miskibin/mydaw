//! The COM objects MYDAW implements *for* a VST3 plugin.
//!
//! Hosting VST3 is not only calling into a plugin; a plugin calls back, and it
//! does so through interfaces the host has to provide. There are five, and each
//! one exists because leaving it null makes some real plugin misbehave:
//!
//! | Object | Given to | Why it cannot be null |
//! | --- | --- | --- |
//! | [`Vst3HostContext`] | `IComponent::initialize`, `IEditController::initialize` | a plugin asks the context for the host's name, and many refuse to initialise without one |
//! | [`HostEventList`] | `ProcessData::inputEvents` | the only way to play an instrument a note |
//! | [`NoParameterChanges`] | `ProcessData::inputParameterChanges` | the field is nominally optional, and plugins dereference it anyway |
//! | [`PlugFrame`] | `IPlugView::setFrame` | how an editor asks its window to change size |
//! | [`MemoryStream`] | `IComponent::getState` / `setState` | there is no other way to move a state blob |
//!
//! # Implementing an interface with these bindings
//!
//! The `vst3` crate has no macro for this: a Rust type declares which interfaces
//! it exposes through [`Class::Interfaces`], implements the matching `…Trait`,
//! and is handed out through [`ComWrapper`], which owns the refcount. Every trait
//! method takes `&self`, so anything mutable needs interior mutability -- which is
//! where the RT-safety argument for [`HostEventList`] lives.
//!
//! # Threads
//!
//! [`Vst3HostContext`], [`PlugFrame`] and [`MemoryStream`] are main-thread
//! objects. [`HostEventList`] and [`NoParameterChanges`] are owned by the RT node
//! and touched only by the thread that owns it -- see [`HostEventList`]'s safety
//! note, which is the one place in this crate where `Send`/`Sync` is asserted by
//! hand.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};

use vst3::Steinberg::Vst::{
    Event, IComponentHandler, IComponentHandlerTrait, IEventList, IEventListTrait,
    IHostApplication, IHostApplicationTrait, ParamID, ParamValue, String128,
};
use vst3::Steinberg::{
    int32, int64, FUnknown, IBStream, IBStreamTrait, IPlugFrame, IPlugFrameTrait, IPlugView,
    ViewRect, TUID,
};
use vst3::Steinberg::{kInvalidArgument, kNotImplemented, kResultOk};
use vst3::{Class, ComWrapper};

use crate::module::write_utf16;

/// Host identity handed to every plugin this host instantiates.
pub const HOST_NAME: &str = "MYDAW";

/// Events one block may carry into a plugin.
///
/// Matches the engine's own per-block note buffer, so anything the graph can
/// hold, the node can forward.
pub const MAX_BLOCK_EVENTS: usize = mydaw_rt_core::DEFAULT_NOTE_CAPACITY;

// ---------------------------------------------------------------------------
// What the plugin has asked the host for
// ---------------------------------------------------------------------------

/// `IComponentHandler` callbacks, latched as atomics.
///
/// A plugin may call these from its own threads (an editor thread turning a
/// knob, a worker deciding its latency changed), so the implementations must not
/// lock or allocate. The main thread reads and clears them, exactly as the CLAP
/// host does with `clap_host->request_*`.
#[derive(Debug, Default)]
pub struct Vst3HostRequests {
    pub(crate) parameters: OnceLock<Arc<mydaw_plugin_params::ParameterBank>>,
    /// OR of every `restartComponent` flag seen since the last read.
    restart_flags: AtomicI32,
    /// Parameter edit gestures in flight. Counted rather than acted on: MYDAW
    /// has no automation to record into yet.
    edits_begun: AtomicU32,
    edits_performed: AtomicU32,
    /// Set by `performEdit`, which is a plugin telling the host its state is no
    /// longer what was last saved.
    dirty: AtomicBool,
}

impl Vst3HostRequests {
    /// Take the accumulated `restartComponent` flags, clearing them.
    ///
    /// Zero means the plugin asked for nothing. The flags themselves are VST3's
    /// `RestartFlags` bitmask; step 7 records them without acting, because
    /// re-activating an instance is the shell's business.
    pub fn take_restart_flags(&self) -> i32 {
        self.restart_flags.swap(0, Ordering::AcqRel)
    }

    /// Whether the plugin has edited a parameter since this was last read.
    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }

    /// How many `beginEdit` gestures have been seen. Diagnostics and tests.
    pub fn edits_begun(&self) -> u32 {
        self.edits_begun.load(Ordering::Relaxed)
    }

    /// How many `performEdit` calls have been seen. Diagnostics and tests.
    pub fn edits_performed(&self) -> u32 {
        self.edits_performed.load(Ordering::Relaxed)
    }
}

/// What an editor has asked its window to do, latched as atomics.
///
/// `IPlugFrame::resizeView` may arrive from a plugin's own thread and means
/// touching a window, which is main-thread work. So the callback records and the
/// main thread acts -- the same shape as `ClapGuiRequests`.
#[derive(Debug, Default)]
pub struct Vst3GuiRequests {
    width: AtomicI32,
    height: AtomicI32,
    resize_pending: AtomicBool,
}

impl Vst3GuiRequests {
    /// Take the size the plugin asked its window to become, if any.
    pub fn take_resize(&self) -> Option<(i32, i32)> {
        if self.resize_pending.swap(false, Ordering::AcqRel) {
            Some((
                self.width.load(Ordering::Acquire),
                self.height.load(Ordering::Acquire),
            ))
        } else {
            None
        }
    }

    /// Forget everything, so a stale request cannot be applied to the next
    /// editor session.
    pub fn clear(&self) {
        self.resize_pending.store(false, Ordering::Release);
    }
}

// ---------------------------------------------------------------------------
// IHostApplication + IComponentHandler
// ---------------------------------------------------------------------------

/// The host context every plugin is initialised with.
///
/// One object exposing both interfaces, because that is what a plugin expects to
/// find: it is handed an `FUnknown` and queries it for whatever it wants.
///
/// # `createInstance` is not implemented
/// VST3's host context can be asked to create `IMessage` and `IAttributeList`
/// objects, which a *dual-component* plugin uses to talk between its processor
/// and its controller over `IConnectionPoint`. MYDAW answers `kNotImplemented`,
/// which is legal and which every plugin must tolerate -- but a plugin that
/// relies on messaging will have an editor that does not follow its processor.
/// See the step 7 spec's known gaps; implementing it is a contained piece of
/// work, not a redesign.
pub struct Vst3HostContext {
    requests: Arc<Vst3HostRequests>,
}

impl Vst3HostContext {
    /// A context latching its callbacks into `requests`.
    pub fn new(requests: Arc<Vst3HostRequests>) -> ComWrapper<Self> {
        ComWrapper::new(Self { requests })
    }
}

impl Class for Vst3HostContext {
    type Interfaces = (IHostApplication, IComponentHandler);
}

impl IHostApplicationTrait for Vst3HostContext {
    unsafe fn getName(&self, name: *mut String128) -> vst3::Steinberg::tresult {
        if name.is_null() {
            return kInvalidArgument;
        }
        write_utf16(HOST_NAME, &mut *name);
        kResultOk
    }

    /// See the type docs: MYDAW provides no `IMessage`/`IAttributeList` yet.
    unsafe fn createInstance(
        &self,
        _cid: *mut TUID,
        _iid: *mut TUID,
        _obj: *mut *mut c_void,
    ) -> vst3::Steinberg::tresult {
        kNotImplemented
    }
}

impl IComponentHandlerTrait for Vst3HostContext {
    unsafe fn beginEdit(&self, _id: ParamID) -> vst3::Steinberg::tresult {
        self.requests.edits_begun.fetch_add(1, Ordering::Relaxed);
        kResultOk
    }

    unsafe fn performEdit(
        &self,
        id: ParamID,
        value_normalized: ParamValue,
    ) -> vst3::Steinberg::tresult {
        self.requests
            .edits_performed
            .fetch_add(1, Ordering::Relaxed);
        // A knob moved in the editor: the instance's state is no longer what was
        // last saved. Recorded, not acted on -- a host that saved on every
        // gesture would write a megabyte per drag.
        self.requests.dirty.store(true, Ordering::Release);
        if let Some(parameters) = self.requests.parameters.get() {
            parameters.publish(id, value_normalized);
        }
        kResultOk
    }

    unsafe fn endEdit(&self, _id: ParamID) -> vst3::Steinberg::tresult {
        kResultOk
    }

    unsafe fn restartComponent(&self, flags: int32) -> vst3::Steinberg::tresult {
        self.requests
            .restart_flags
            .fetch_or(flags, Ordering::AcqRel);
        kResultOk
    }
}

// ---------------------------------------------------------------------------
// IPlugFrame
// ---------------------------------------------------------------------------

/// The frame an editor is given, so it can ask for a different size.
///
/// Answering `kResultOk` means "acknowledged", not "already done": the main
/// thread applies the size on its next pump pass, which is the only thread
/// allowed to touch a window.
pub struct PlugFrame {
    requests: Arc<Vst3GuiRequests>,
}

impl PlugFrame {
    /// A frame latching resize requests into `requests`.
    pub fn new(requests: Arc<Vst3GuiRequests>) -> ComWrapper<Self> {
        ComWrapper::new(Self { requests })
    }
}

impl Class for PlugFrame {
    type Interfaces = (IPlugFrame,);
}

impl IPlugFrameTrait for PlugFrame {
    unsafe fn resizeView(
        &self,
        _view: *mut IPlugView,
        new_size: *mut ViewRect,
    ) -> vst3::Steinberg::tresult {
        if new_size.is_null() {
            return kInvalidArgument;
        }
        let rect = *new_size;
        self.requests
            .width
            .store(rect.right - rect.left, Ordering::Release);
        self.requests
            .height
            .store(rect.bottom - rect.top, Ordering::Release);
        self.requests.resize_pending.store(true, Ordering::Release);
        kResultOk
    }
}

// ---------------------------------------------------------------------------
// IEventList
// ---------------------------------------------------------------------------

/// This block's note events, in VST3's own wire format.
///
/// # RT safety, and the `Send`/`Sync` assertion
///
/// The event storage is a [`Vec`] grown to [`MAX_BLOCK_EVENTS`] once, at node
/// creation, and only ever cleared and refilled afterwards -- so `process` does
/// no allocation. `IEventListTrait` takes `&self`, so the storage is behind a
/// [`RefCell`]; a `RefCell` borrow is a flag check, not a lock, which is why it
/// is admissible inside the RT wall where a `Mutex` would not be.
///
/// [`ComWrapper`] is `Send` only for a `Send + Sync` payload, and `RefCell` is
/// not `Sync`. The `unsafe impl` below asserts what is actually true of this
/// object rather than what the type system can see: it is created by the main
/// thread, moved *wholesale* into one [`Vst3Node`](crate::Vst3Node), and from
/// then on touched only by the thread that owns that node -- by us when we fill
/// it, and by the plugin when it reads it, both inside the same `process` call
/// on the same thread. No second handle to it exists.
pub struct HostEventList {
    events: RefCell<Vec<Event>>,
    /// Events dropped because the block carried more than [`MAX_BLOCK_EVENTS`].
    /// A [`Cell`] rather than an atomic: only the owning thread touches it.
    overflowed: Cell<u32>,
}

// SAFETY: see the type docs. The object is never shared between threads; this
// asserts that fact so `Vst3Node` -- which the graph moves to the audio thread --
// can be `Send`.
unsafe impl Send for HostEventList {}
unsafe impl Sync for HostEventList {}

impl HostEventList {
    /// A list pre-grown to [`MAX_BLOCK_EVENTS`], and empty.
    ///
    /// Allocates here, on a non-RT thread, so that `process` never does.
    pub fn new() -> ComWrapper<Self> {
        let mut events = Vec::new();
        events.reserve_exact(MAX_BLOCK_EVENTS);
        ComWrapper::new(Self {
            events: RefCell::new(events),
            overflowed: Cell::new(0),
        })
    }

    /// Empty the list, keeping its capacity. RT-safe.
    pub fn reset(&self) {
        self.events.borrow_mut().clear();
    }

    /// Append one event, if there is room. Returns whether it was stored.
    ///
    /// RT-safe: the push writes into capacity reserved by [`new`](Self::new) and
    /// the length check is what guarantees it never reallocates.
    pub fn push(&self, event: Event) -> bool {
        let mut events = self.events.borrow_mut();
        if events.len() >= MAX_BLOCK_EVENTS {
            self.overflowed.set(self.overflowed.get().saturating_add(1));
            return false;
        }
        events.push(event);
        true
    }

    /// How many events this block carries.
    pub fn len(&self) -> usize {
        self.events.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Events dropped for want of room since the node was created.
    pub fn overflowed(&self) -> u32 {
        self.overflowed.get()
    }
}

impl Class for HostEventList {
    type Interfaces = (IEventList,);
}

impl IEventListTrait for HostEventList {
    unsafe fn getEventCount(&self) -> int32 {
        // Saturating: a count that wrapped negative would send a plugin into a
        // loop it never leaves.
        int32::try_from(self.events.borrow().len()).unwrap_or(int32::MAX)
    }

    unsafe fn getEvent(&self, index: int32, out: *mut Event) -> vst3::Steinberg::tresult {
        if out.is_null() || index < 0 {
            return kInvalidArgument;
        }
        let events = self.events.borrow();
        match events.get(index as usize) {
            Some(event) => {
                *out = *event;
                kResultOk
            }
            None => kInvalidArgument,
        }
    }

    /// A plugin adding to the host's *input* list is not something MYDAW has a
    /// use for, and silently accepting it would let a plugin grow a buffer the
    /// RT path assumes is bounded.
    unsafe fn addEvent(&self, _event: *mut Event) -> vst3::Steinberg::tresult {
        kNotImplemented
    }
}

// ---------------------------------------------------------------------------
// IParameterChanges
// ---------------------------------------------------------------------------

/// An empty parameter-change list.
///
/// `ProcessData::inputParameterChanges` is documented as optional, and passing
/// null is legal -- but enough shipping plugins dereference it without checking
/// that a host which passes null is a host that crashes on somebody's machine.
/// An object that answers "no parameters" costs one allocation per node and
/// removes the entire class of problem.
///
/// Step 7 automates no parameters; when it does, this becomes a real queue.
pub use crate::parameters::ParameterChanges as NoParameterChanges;

// ---------------------------------------------------------------------------
// IBStream
// ---------------------------------------------------------------------------

/// Seek origins, from `IBStream::IStreamSeekMode`.
const SEEK_SET: int32 = 0;
const SEEK_CUR: int32 = 1;
const SEEK_END: int32 = 2;

/// Largest state blob this host will accept from, or hand to, a plugin.
///
/// A guard against a plugin that streams without end (a sampler writing its
/// whole library into a preset, a bug that writes in a loop). 64 MiB is far past
/// any sane preset and far short of exhausting memory.
pub const MAX_STATE_BYTES: usize = 64 * 1024 * 1024;

/// An in-memory `IBStream`, which is how a state blob crosses the boundary.
///
/// VST3 has no "give me your state as bytes" call: `IComponent::getState` writes
/// into a stream the *host* provides. This is that stream.
///
/// Main-thread only, so plain [`RefCell`] with no `Send` assertion.
pub struct MemoryStream {
    buffer: RefCell<Vec<u8>>,
    cursor: Cell<usize>,
}

impl MemoryStream {
    /// An empty stream, for a plugin to write its state into.
    pub fn empty() -> ComWrapper<Self> {
        ComWrapper::new(Self {
            buffer: RefCell::new(Vec::new()),
            cursor: Cell::new(0),
        })
    }

    /// A stream holding `bytes`, positioned at the start, for a plugin to read.
    pub fn reading(bytes: &[u8]) -> ComWrapper<Self> {
        ComWrapper::new(Self {
            buffer: RefCell::new(bytes.to_vec()),
            cursor: Cell::new(0),
        })
    }

    /// Everything written to this stream so far.
    pub fn contents(&self) -> Vec<u8> {
        self.buffer.borrow().clone()
    }

    /// How many bytes the stream holds.
    pub fn len(&self) -> usize {
        self.buffer.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Class for MemoryStream {
    type Interfaces = (IBStream,);
}

impl IBStreamTrait for MemoryStream {
    unsafe fn read(
        &self,
        buffer: *mut c_void,
        num_bytes: int32,
        num_bytes_read: *mut int32,
    ) -> vst3::Steinberg::tresult {
        if buffer.is_null() || num_bytes < 0 {
            return kInvalidArgument;
        }
        let source = self.buffer.borrow();
        let start = self.cursor.get().min(source.len());
        let wanted = num_bytes as usize;
        let available = (source.len() - start).min(wanted);
        if available > 0 {
            std::ptr::copy_nonoverlapping(
                source.as_ptr().add(start),
                buffer.cast::<u8>(),
                available,
            );
        }
        self.cursor.set(start + available);
        if !num_bytes_read.is_null() {
            *num_bytes_read = int32::try_from(available).unwrap_or(int32::MAX);
        }
        kResultOk
    }

    unsafe fn write(
        &self,
        buffer: *mut c_void,
        num_bytes: int32,
        num_bytes_written: *mut int32,
    ) -> vst3::Steinberg::tresult {
        if buffer.is_null() || num_bytes < 0 {
            return kInvalidArgument;
        }
        let wanted = num_bytes as usize;
        let mut sink = self.buffer.borrow_mut();
        let start = self.cursor.get();
        if start.saturating_add(wanted) > MAX_STATE_BYTES {
            // Refusing beats growing until the machine swaps. The caller sees a
            // short write, which is what `IBStream` failure looks like.
            if !num_bytes_written.is_null() {
                *num_bytes_written = 0;
            }
            return kInvalidArgument;
        }
        if sink.len() < start + wanted {
            sink.resize(start + wanted, 0);
        }
        std::ptr::copy_nonoverlapping(buffer.cast::<u8>(), sink.as_mut_ptr().add(start), wanted);
        self.cursor.set(start + wanted);
        if !num_bytes_written.is_null() {
            *num_bytes_written = num_bytes;
        }
        kResultOk
    }

    unsafe fn seek(&self, pos: int64, mode: int32, result: *mut int64) -> vst3::Steinberg::tresult {
        let len = self.buffer.borrow().len() as i64;
        let base = match mode {
            SEEK_SET => 0,
            SEEK_CUR => self.cursor.get() as i64,
            SEEK_END => len,
            _ => return kInvalidArgument,
        };
        let Some(target) = base.checked_add(pos) else {
            return kInvalidArgument;
        };
        if target < 0 {
            return kInvalidArgument;
        }
        // Seeking past the end is legal and is how a plugin sizes its stream;
        // the gap fills with zeroes on the next write.
        self.cursor.set(target as usize);
        if !result.is_null() {
            *result = target;
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut int64) -> vst3::Steinberg::tresult {
        if pos.is_null() {
            return kInvalidArgument;
        }
        *pos = self.cursor.get() as i64;
        kResultOk
    }
}

/// The `FUnknown` a plugin's `initialize` is given.
///
/// A thin helper so the two call sites (component and controller) cannot get the
/// cast wrong.
pub fn host_context_ptr(context: &ComWrapper<Vst3HostContext>) -> *mut FUnknown {
    context
        .as_com_ref::<IHostApplication>()
        .map_or(std::ptr::null_mut(), |host| {
            host.as_ptr().cast::<FUnknown>()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vst3::ComPtr;
    use vst3::Steinberg::Vst::IEventList;
    use vst3::Steinberg::Vst::{IParameterChanges, IParameterChangesTrait};

    #[test]
    fn the_host_reports_its_name_as_utf16() {
        let requests = Arc::new(Vst3HostRequests::default());
        let context = Vst3HostContext::new(Arc::clone(&requests));
        let host: ComPtr<IHostApplication> = context.to_com_ptr().expect("IHostApplication");

        let mut name: String128 = [0xffff; 128];
        // SAFETY: `name` is a live, fully initialised String128.
        let result = unsafe { host.getName(&mut name) };
        assert_eq!(result, kResultOk);
        assert_eq!(crate::module::utf16_to_string(&name), HOST_NAME);
    }

    #[test]
    fn a_null_name_buffer_is_refused_rather_than_written_through() {
        let context = Vst3HostContext::new(Arc::new(Vst3HostRequests::default()));
        let host: ComPtr<IHostApplication> = context.to_com_ptr().unwrap();
        // SAFETY: passing null is exactly what is under test.
        assert_eq!(
            unsafe { host.getName(std::ptr::null_mut()) },
            kInvalidArgument
        );
    }

    #[test]
    fn component_handler_callbacks_latch_until_the_main_thread_reads_them() {
        let requests = Arc::new(Vst3HostRequests::default());
        let context = Vst3HostContext::new(Arc::clone(&requests));
        let handler: ComPtr<IComponentHandler> = context.to_com_ptr().expect("IComponentHandler");

        assert_eq!(requests.take_restart_flags(), 0);
        assert!(!requests.take_dirty());

        // SAFETY: the handler is live and the arguments are plain scalars.
        unsafe {
            assert_eq!(handler.beginEdit(7), kResultOk);
            assert_eq!(handler.performEdit(7, 0.5), kResultOk);
            assert_eq!(handler.endEdit(7), kResultOk);
            // Flags accumulate: two restarts before one read must not lose one.
            assert_eq!(handler.restartComponent(2), kResultOk);
            assert_eq!(handler.restartComponent(8), kResultOk);
        }

        assert_eq!(requests.edits_begun(), 1);
        assert_eq!(requests.edits_performed(), 1);
        assert!(requests.take_dirty(), "an edit marks the instance dirty");
        assert!(!requests.take_dirty(), "reading clears it");
        assert_eq!(requests.take_restart_flags(), 2 | 8);
        assert_eq!(requests.take_restart_flags(), 0, "reading clears them");
    }

    #[test]
    fn a_plug_frame_records_the_size_the_editor_asked_for() {
        let requests = Arc::new(Vst3GuiRequests::default());
        let frame = PlugFrame::new(Arc::clone(&requests));
        let plug_frame: ComPtr<IPlugFrame> = frame.to_com_ptr().expect("IPlugFrame");

        assert!(requests.take_resize().is_none());
        let mut rect = ViewRect {
            left: 10,
            top: 20,
            right: 810,
            bottom: 620,
        };
        // SAFETY: `rect` is live for the call; the view pointer is unused.
        let result = unsafe { plug_frame.resizeView(std::ptr::null_mut(), &mut rect) };
        assert_eq!(result, kResultOk);
        assert_eq!(
            requests.take_resize(),
            Some((800, 600)),
            "the frame reports width and height, not corners"
        );
        assert!(requests.take_resize().is_none(), "reading clears it");

        // A stale request must not survive into the next editor session.
        let _ = unsafe { plug_frame.resizeView(std::ptr::null_mut(), &mut rect) };
        requests.clear();
        assert!(requests.take_resize().is_none());
    }

    #[test]
    fn a_null_rect_is_refused_rather_than_dereferenced() {
        let requests = Arc::new(Vst3GuiRequests::default());
        let frame = PlugFrame::new(Arc::clone(&requests));
        let plug_frame: ComPtr<IPlugFrame> = frame.to_com_ptr().unwrap();
        // SAFETY: passing null is exactly what is under test.
        let result = unsafe { plug_frame.resizeView(std::ptr::null_mut(), std::ptr::null_mut()) };
        assert_eq!(result, kInvalidArgument);
        assert!(requests.take_resize().is_none());
    }

    fn note_on(pitch: i16, offset: int32) -> Event {
        let mut event: Event = unsafe { std::mem::zeroed() };
        event.sampleOffset = offset;
        event.r#type = vst3::Steinberg::Vst::Event_::EventTypes_::kNoteOnEvent as u16;
        event.__field0.noteOn = vst3::Steinberg::Vst::NoteOnEvent {
            channel: 0,
            pitch,
            tuning: 0.0,
            velocity: 0.8,
            length: 0,
            noteId: -1,
        };
        event
    }

    #[test]
    fn an_event_list_hands_the_plugin_back_what_the_host_put_in() {
        let list = HostEventList::new();
        assert!(list.is_empty());
        assert!(list.push(note_on(60, 0)));
        assert!(list.push(note_on(64, 16)));
        assert_eq!(list.len(), 2);

        let events: ComPtr<IEventList> = list.to_com_ptr().expect("IEventList");
        // SAFETY: the list is live and every index below is in range.
        unsafe {
            assert_eq!(events.getEventCount(), 2);
            let mut out: Event = std::mem::zeroed();
            assert_eq!(events.getEvent(1, &mut out), kResultOk);
            assert_eq!(out.sampleOffset, 16);
            assert_eq!(out.__field0.noteOn.pitch, 64);
        }

        list.reset();
        assert!(list.is_empty());
        // SAFETY: as above.
        unsafe {
            assert_eq!(events.getEventCount(), 0);
        }
    }

    #[test]
    fn reading_past_the_end_of_an_event_list_is_refused_not_undefined() {
        let list = HostEventList::new();
        list.push(note_on(60, 0));
        let events: ComPtr<IEventList> = list.to_com_ptr().unwrap();
        // SAFETY: out-of-range indices and a null out-pointer are the point.
        unsafe {
            let mut out: Event = std::mem::zeroed();
            assert_eq!(events.getEvent(1, &mut out), kInvalidArgument);
            assert_eq!(events.getEvent(-1, &mut out), kInvalidArgument);
            assert_eq!(events.getEvent(0, std::ptr::null_mut()), kInvalidArgument);
            // A plugin must not be able to grow the host's RT-sized buffer.
            let mut event = note_on(1, 0);
            assert_eq!(events.addEvent(&mut event), kNotImplemented);
        }
    }

    #[test]
    fn an_event_list_never_grows_past_its_reserved_capacity() {
        // The whole RT-safety argument: `process` must not allocate, so a block
        // with more events than the buffer holds drops the excess and counts it.
        let list = HostEventList::new();
        for index in 0..MAX_BLOCK_EVENTS {
            assert!(list.push(note_on(index as i16 % 128, 0)), "at {index}");
        }
        assert_eq!(list.overflowed(), 0);
        assert!(!list.push(note_on(60, 0)), "the buffer is full");
        assert_eq!(list.len(), MAX_BLOCK_EVENTS);
        assert_eq!(list.overflowed(), 1);
    }

    #[test]
    fn the_empty_parameter_list_answers_rather_than_being_null() {
        let params = NoParameterChanges::new();
        let changes: ComPtr<IParameterChanges> = params.to_com_ptr().expect("IParameterChanges");
        // SAFETY: the object is live; every call below takes plain scalars.
        unsafe {
            assert_eq!(changes.getParameterCount(), 0);
            assert!(changes.getParameterData(0).is_null());
            let id: ParamID = 3;
            let mut index: int32 = 0;
            assert!(!changes.addParameterData(&id, &mut index).is_null());
            assert_eq!(changes.getParameterCount(), 1);
        }
    }

    #[test]
    fn a_memory_stream_round_trips_a_state_blob() {
        let blob: Vec<u8> = (0..300_u32).map(|i| (i % 251) as u8).collect();

        // The write side, as `IComponent::getState` drives it.
        let sink = MemoryStream::empty();
        let stream: ComPtr<IBStream> = sink.to_com_ptr().expect("IBStream");
        let mut written: int32 = 0;
        // SAFETY: `blob` outlives the call and `written` is a live out-param.
        unsafe {
            assert_eq!(
                stream.write(
                    blob.as_ptr() as *mut c_void,
                    blob.len() as int32,
                    &mut written
                ),
                kResultOk
            );
        }
        assert_eq!(written as usize, blob.len());
        assert_eq!(sink.contents(), blob);

        // The read side, as `IComponent::setState` drives it.
        let source = MemoryStream::reading(&blob);
        let stream: ComPtr<IBStream> = source.to_com_ptr().unwrap();
        let mut out = vec![0_u8; blob.len() + 16];
        let mut read: int32 = 0;
        // SAFETY: `out` is large enough for anything the stream can return.
        unsafe {
            assert_eq!(
                stream.read(
                    out.as_mut_ptr().cast::<c_void>(),
                    out.len() as int32,
                    &mut read
                ),
                kResultOk
            );
        }
        assert_eq!(
            read as usize,
            blob.len(),
            "a short read at the end is normal"
        );
        assert_eq!(&out[..blob.len()], &blob[..]);
    }

    #[test]
    fn a_memory_stream_seeks_from_all_three_origins() {
        let source = MemoryStream::reading(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let stream: ComPtr<IBStream> = source.to_com_ptr().unwrap();
        let mut where_now: int64 = -1;

        // SAFETY: every out-pointer below is a live local.
        unsafe {
            assert_eq!(stream.seek(4, SEEK_SET, &mut where_now), kResultOk);
            assert_eq!(where_now, 4);
            assert_eq!(stream.seek(2, SEEK_CUR, &mut where_now), kResultOk);
            assert_eq!(where_now, 6);
            assert_eq!(stream.seek(-8, SEEK_END, &mut where_now), kResultOk);
            assert_eq!(where_now, 0);
            assert_eq!(stream.tell(&mut where_now), kResultOk);
            assert_eq!(where_now, 0);

            // A negative absolute position, and an unknown mode, are refused.
            assert_eq!(stream.seek(-1, SEEK_SET, &mut where_now), kInvalidArgument);
            assert_eq!(stream.seek(0, 99, &mut where_now), kInvalidArgument);
            assert_eq!(stream.tell(std::ptr::null_mut()), kInvalidArgument);
        }
    }

    #[test]
    fn a_stream_write_past_the_size_limit_is_refused_rather_than_allocated() {
        // A plugin that streams without end must not be able to exhaust memory.
        let sink = MemoryStream::empty();
        let stream: ComPtr<IBStream> = sink.to_com_ptr().unwrap();
        let mut where_now: int64 = 0;
        let chunk = [0_u8; 16];
        let mut written: int32 = 0;
        // SAFETY: the buffer and out-params are live locals.
        unsafe {
            assert_eq!(
                stream.seek(MAX_STATE_BYTES as i64, SEEK_SET, &mut where_now),
                kResultOk
            );
            assert_eq!(
                stream.write(chunk.as_ptr() as *mut c_void, 16, &mut written),
                kInvalidArgument
            );
        }
        assert_eq!(written, 0);
        assert!(sink.is_empty(), "nothing may have been allocated");
    }
}
