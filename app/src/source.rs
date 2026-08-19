//! Choosing what sits at the head of a track, while the engine is running.
//!
//! Step 1 built the chain at startup and left it there. Step 3 made the head of
//! the chain swappable. Step 5 gave every track its own head. Step 6 makes the
//! *set* of things that can go there open-ended: two built-in instruments, the
//! test tone, and **any CLAP instrument installed on the machine**, chosen from
//! a picker rather than named by an environment variable.
//!
//! # Why a slot, and not a new graph each time
//!
//! ```text
//!   install once:   SourceSlot -> [insert..] -> TransportGate
//!                        ^                              ^
//!                        |                              |
//!            new source node, by value           reads the same flag
//!            (rtrb + basedrop, like GraphHandle)  the slot publishes
//! ```
//!
//! The obvious implementation of `set_source` is "build a new `GraphSpec` and
//! install it", the way [`TrackHost::install_graph`](crate::tracks::TrackHost::install_graph)
//! does at startup. It does not work, for a reason that is worth writing down:
//! **a hosted CLAP plugin can only be activated once at a time.** Rebuilding
//! the graph means building a new node for every hosted plugin too, and the old
//! nodes are not retired until the *old graph* is dropped -- which cannot happen
//! until the new graph is installed. The two requirements are circular.
//!
//! Swapping at node granularity has no such problem. The graph is built once and
//! only the head of a chain moves. It is also what a DAW actually does: changing
//! the instrument on a track does not rebuild the mixer.
//!
//! Step 6 does finally allow the *layout* to change with CLAP plugins loaded,
//! and it breaks the circle by conceding a few milliseconds of silence rather
//! than by pretending the constraint is not there -- see
//! [`crate::clap_host::ClapHost::rebuild`].
//!
//! # The gate travels with the source
//! `TransportGate` exists because the test tone is a free-running oscillator;
//! an instrument must sound with the transport stopped (see [`crate::graph`]).
//! That is a property of the *source*, so the source publishes it: the slot
//! stores the incoming node's `gated` flag into a shared atomic as it picks the
//! node up, and the gate -- still the last node in the chain, so Stop still
//! silences an effect's tail -- reads it. Both happen inside the same block, in
//! chain order, so the flag can never describe the previous source.
//!
//! # The CLAP source needs the main thread
//! Everything here is `Send + Sync` and callable from a Tauri command thread,
//! with one exception: a [`LoadedClapPlugin`](mydaw_plugin_host_clap::LoadedClapPlugin)
//! is `!Send`, lives on the main thread, and is the only thing that can build a
//! node for it. So a CLAP source change goes through
//! [`ClapHostBridge`](crate::clap_bridge::ClapHostBridge): the command queues a
//! request, the run loop serves it, the command gets the answer or a timeout.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use mydaw_plugin_audio_io::AudioIoError;
use mydaw_plugin_host_clap::ClapPluginInfo;
use mydaw_plugin_host_vst3::{ClassId, Vst3PluginInfo};
use mydaw_rt_core::basedrop::{Handle, Owned};
use mydaw_rt_core::{ProcessCtx, RtNode};
use parking_lot::Mutex;
use rtrb::{Consumer, Producer};
use serde::Serialize;

/// In-flight source swaps. Four is a user clicking faster than the audio thread
/// runs a block, which does not happen; the queue exists so that overflow is a
/// reported error rather than a possibility nobody thought about.
pub const SOURCE_QUEUE_CAPACITY: usize = 4;

/// Prefix of a CLAP source's wire id, so `poly-synth` and a plugin that happens
/// to call itself `poly-synth` can never collide.
pub const CLAP_SOURCE_PREFIX: &str = "clap:";

/// Prefix of a VST3 source's wire id.
///
/// Distinct from [`CLAP_SOURCE_PREFIX`] so an id records *which format* a
/// track was headed by, not only which plugin: the two hosts are separate, and
/// an id that did not say would be ambiguous the moment a vendor ships the same
/// plugin in both formats -- which most of them do.
pub const VST3_SOURCE_PREFIX: &str = "vst3:";

/// A hosted CLAP instrument, identified the way a project file records one.
///
/// Bundle path **and** plugin id, because neither alone is enough: one bundle
/// commonly exports several plugins (the workspace's own fixture exports two),
/// and the same plugin id can be installed in two places.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClapSourceRef {
    /// Absolute path of the `.clap` bundle.
    pub bundle_path: PathBuf,
    /// CLAP plugin id inside that bundle, e.g. `com.u-he.diva`.
    pub plugin_id: String,
    /// Display name from the scan. May be empty -- CLAP only requires an id --
    /// in which case the id is shown instead.
    pub name: String,
}

impl ClapSourceRef {
    /// A reference to `plugin_id` inside `bundle_path`, with no display name.
    pub fn new(bundle_path: impl Into<PathBuf>, plugin_id: impl Into<String>) -> Self {
        Self {
            bundle_path: bundle_path.into(),
            plugin_id: plugin_id.into(),
            name: String::new(),
        }
    }

    /// The same reference carrying a display name.
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// What to put on a mixer strip.
    pub fn label(&self) -> &str {
        if self.name.is_empty() {
            &self.plugin_id
        } else {
            &self.name
        }
    }

    /// Wire id: `clap:<plugin id>`.
    ///
    /// The bundle path is deliberately *not* part of it. An id crosses the IPC
    /// boundary and lands in a UI list; a Windows path in it would be a
    /// separator minefield. The path travels as its own argument on the one
    /// command that needs it
    /// ([`set_track_source_clap`](crate::commands::set_track_source_clap)).
    pub fn wire_id(&self) -> String {
        format!("{CLAP_SOURCE_PREFIX}{}", self.plugin_id)
    }
}

impl From<&ClapPluginInfo> for ClapSourceRef {
    fn from(info: &ClapPluginInfo) -> Self {
        Self {
            bundle_path: info.path.clone(),
            plugin_id: info.id.clone(),
            name: info.name.clone(),
        }
    }
}

/// A hosted VST3 instrument, identified the way a project file records one.
///
/// Module path **and** class id, for the same reason a CLAP source carries
/// bundle and plugin id: one module commonly exports several classes, and the
/// same class can be installed in two places.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Vst3SourceRef {
    /// Absolute path of the `.vst3` module (bundle directory or flat DLL).
    pub module_path: PathBuf,
    /// The plugin class inside that module.
    pub cid: ClassId,
    /// Display name from the scan. May be empty, in which case the class id is
    /// shown instead -- a name is not something the format guarantees.
    pub name: String,
}

impl Vst3SourceRef {
    /// A reference to `cid` inside `module_path`, with no display name.
    pub fn new(module_path: impl Into<PathBuf>, cid: ClassId) -> Self {
        Self {
            module_path: module_path.into(),
            cid,
            name: String::new(),
        }
    }

    /// The same reference carrying a display name.
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// What to put on a mixer strip.
    pub fn label(&self) -> String {
        if self.name.is_empty() {
            self.cid.to_hex()
        } else {
            self.name.clone()
        }
    }

    /// Wire id: `vst3:<class id as hex>`.
    ///
    /// The module path is deliberately *not* part of it, exactly as for CLAP: an
    /// id crosses the IPC boundary and lands in a UI list, and a Windows path in
    /// it would be a separator minefield. The path travels as its own argument on
    /// [`set_track_source_vst3`](crate::commands::set_track_source_vst3).
    pub fn wire_id(&self) -> String {
        format!("{VST3_SOURCE_PREFIX}{}", self.cid.to_hex())
    }
}

impl From<&Vst3PluginInfo> for Vst3SourceRef {
    fn from(info: &Vst3PluginInfo) -> Self {
        Self {
            module_path: info.path.clone(),
            cid: info.cid,
            name: info.name.clone(),
        }
    }
}

/// What generates audio at the head of a track.
///
/// Not `Copy` since step 6: a CLAP source carries the bundle path and plugin id
/// that identify it, which is exactly what a project file has to record.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Source {
    /// The step-1 sine generator. Free-running, so it is gated on the transport.
    TestTone,
    /// The built-in [`poly-synth`](mydaw_plugin_instruments::PolySynthNode).
    PolySynth,
    /// The built-in [`drum-kit`](mydaw_plugin_instruments::DrumKitNode).
    DrumKit,
    /// A hosted CLAP instrument, chosen from the plugin picker.
    Clap(ClapSourceRef),
    /// A hosted VST3 instrument, chosen from the same picker.
    Vst3(Vst3SourceRef),
}

impl Source {
    /// The built-in sources, in the order the UI offers them.
    ///
    /// CLAP instruments are not here: what is available depends on the machine,
    /// and the list comes from the scan
    /// ([`AppState::sources`](crate::AppState::sources)).
    pub const BUILT_IN: [Self; 3] = [Self::PolySynth, Self::DrumKit, Self::TestTone];

    /// The source a fresh session's synth track starts on.
    ///
    /// The poly synth rather than the test tone: a DAW that greets you with a
    /// playable instrument is telling the truth about what it is, and a
    /// continuous sine is a diagnostic, not a welcome. The tone is still one
    /// click away, and is still what the hardware tests use.
    pub const DEFAULT: Self = Self::PolySynth;

    /// Stable string id, as it crosses the IPC boundary and appears in the UI.
    pub fn id(&self) -> String {
        match self {
            Self::TestTone => "test-tone".to_owned(),
            Self::PolySynth => "poly-synth".to_owned(),
            Self::DrumKit => "drum-kit".to_owned(),
            Self::Clap(reference) => reference.wire_id(),
            Self::Vst3(reference) => reference.wire_id(),
        }
    }

    /// Name for a menu.
    pub fn label(&self) -> String {
        match self {
            Self::TestTone => "Test Tone".to_owned(),
            Self::PolySynth => "Poly Synth".to_owned(),
            Self::DrumKit => "Drum Kit".to_owned(),
            Self::Clap(reference) => reference.label().to_owned(),
            Self::Vst3(reference) => reference.label(),
        }
    }

    /// One line for a tooltip.
    pub fn description(&self) -> String {
        match self {
            Self::TestTone => "440 Hz sine, gated on the transport".to_owned(),
            Self::PolySynth => "Eight-voice saw + sub synth, played from the keyboard".to_owned(),
            Self::DrumKit => {
                "Synthesised kick, snare and hi-hats on GM keys 36/38/42/46".to_owned()
            }
            Self::Clap(reference) => format!(
                "Hosted CLAP instrument {} from {}",
                reference.plugin_id,
                reference.bundle_path.display()
            ),
            Self::Vst3(reference) => format!(
                "Hosted VST3 instrument {} from {}",
                reference.cid,
                reference.module_path.display()
            ),
        }
    }

    /// Whether this source has to be silenced while the transport is stopped.
    ///
    /// True for the test tone only. See the module docs and [`crate::graph`].
    pub const fn gated(&self) -> bool {
        matches!(self, Self::TestTone)
    }

    /// Whether this source plays note events.
    pub const fn takes_notes(&self) -> bool {
        !matches!(self, Self::TestTone)
    }

    /// Whether this source is a hosted CLAP plugin, and therefore needs the
    /// main thread to build its node.
    pub const fn is_clap(&self) -> bool {
        matches!(self, Self::Clap(_))
    }

    /// Whether this source is a hosted VST3 plugin.
    pub const fn is_vst3(&self) -> bool {
        matches!(self, Self::Vst3(_))
    }

    /// Whether this source is hosted at all, in either format.
    ///
    /// The question the *graph* asks. A hosted source of either kind renders
    /// [`SilentNode`] until its own host's main thread swaps the real node in,
    /// and neither can be built from a `Send + Sync` factory -- which is the one
    /// property the graph builder cares about.
    pub const fn is_hosted(&self) -> bool {
        self.is_clap() || self.is_vst3()
    }

    /// The bundle path and plugin id, for a CLAP source.
    pub const fn clap_ref(&self) -> Option<&ClapSourceRef> {
        match self {
            Self::Clap(reference) => Some(reference),
            _ => None,
        }
    }

    /// The module path and class id, for a VST3 source.
    pub const fn vst3_ref(&self) -> Option<&Vst3SourceRef> {
        match self {
            Self::Vst3(reference) => Some(reference),
            _ => None,
        }
    }

    /// The `type_id` of the built-in instrument backing this source, if any.
    pub const fn instrument_type_id(&self) -> Option<&'static str> {
        match self {
            Self::PolySynth => Some(mydaw_plugin_instruments::poly_synth::NODE_TYPE_ID),
            Self::DrumKit => Some(mydaw_plugin_instruments::drum_kit::NODE_TYPE_ID),
            _ => None,
        }
    }

    /// Parse a **built-in** id that came from the webview.
    ///
    /// A CLAP source is deliberately not parseable from an id alone: the id
    /// carries no bundle path, and a host that guessed one from a previous scan
    /// would silently load a different copy of the plugin. The webview uses
    /// [`set_track_source_clap`](crate::commands::set_track_source_clap), which
    /// carries both halves.
    ///
    /// # Errors
    /// [`SourceError::ClapNeedsBundle`] for a `clap:` id, and
    /// [`SourceError::Unknown`] otherwise -- carrying the string, so the status
    /// bar can say what was asked for rather than "invalid input".
    pub fn parse(id: &str) -> Result<Self, SourceError> {
        if let Some(plugin_id) = id.strip_prefix(CLAP_SOURCE_PREFIX) {
            return Err(SourceError::ClapNeedsBundle {
                plugin_id: plugin_id.to_owned(),
            });
        }
        if let Some(cid) = id.strip_prefix(VST3_SOURCE_PREFIX) {
            return Err(SourceError::Vst3NeedsModule {
                cid: cid.to_owned(),
            });
        }
        Self::BUILT_IN
            .into_iter()
            .find(|source| source.id() == id)
            .ok_or_else(|| SourceError::Unknown(id.to_owned()))
    }
}

/// Why a source could not be selected.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("unknown source '{0}'; expected one of test-tone, poly-synth or drum-kit")]
    Unknown(String),
    /// A `clap:` id arrived where only a built-in was expected. Always a UI bug:
    /// the webview has the bundle path from the scan and must send it.
    #[error(
        "the CLAP plugin '{plugin_id}' must be chosen with its bundle path; \
         use set_track_source_clap"
    )]
    ClapNeedsBundle { plugin_id: String },
    /// A `vst3:` id arrived where only a built-in was expected. Always a UI bug,
    /// for the same reason as [`ClapNeedsBundle`](Self::ClapNeedsBundle).
    #[error(
        "the VST3 plugin '{cid}' must be chosen with its module path; \
         use set_track_source_vst3"
    )]
    Vst3NeedsModule { cid: String },
    /// The instruments plugin did not contribute the node this source names.
    /// Always a wiring bug, never a runtime condition.
    #[error("the built-in instrument '{0}' is not registered")]
    MissingInstrument(&'static str),
    /// No stream is open, so there is no slot to install into.
    #[error(transparent)]
    Audio(#[from] AudioIoError),
    /// The audio thread has not picked up previous swaps. Retryable.
    #[error("the source queue is full; the audio thread has not picked up previous swaps")]
    QueueFull,
    /// Whatever the main thread reported while creating the CLAP instance, or a
    /// timeout when the run loop never answered.
    #[error("{0}")]
    MainThread(String),
}

/// A source node together with the gating decision that belongs to it.
///
/// The pair travels as one value so that the flag can never be published
/// separately from the node it describes.
pub struct SourceNode {
    node: Box<dyn RtNode>,
    gated: bool,
}

impl SourceNode {
    pub fn new(node: Box<dyn RtNode>, gated: bool) -> Self {
        Self { node, gated }
    }
}

/// A source that produces nothing at all.
///
/// The placeholder a CLAP-headed track renders while its instance is being
/// attached: at startup, and for the few milliseconds of a graph rebuild.
/// Silence rather than a fallback instrument on purpose -- a track the user set
/// to a synthesiser must not answer with a sine wave because the host was
/// briefly busy, and a *momentary gap* is a far smaller lie than a *wrong
/// sound*.
pub struct SilentNode;

impl RtNode for SilentNode {
    fn process(&mut self, ctx: &mut ProcessCtx) {
        ctx.buffers.silence();
    }
}

/// Non-RT end of the source channel, for one open stream.
struct SourceSender {
    tx: Producer<Owned<SourceNode>>,
    gc: Handle,
}

/// Audio-thread end: the head of the chain.
///
/// Picks up a pending node once per block (before rendering, so a swap takes
/// effect on the block it arrives for), publishes its gating flag, and runs it.
/// Silence when nothing is installed, exactly like
/// [`RtGraphSlot`](mydaw_rt_core::RtGraphSlot).
pub struct SourceSlot {
    rx: Consumer<Owned<SourceNode>>,
    current: Option<Owned<SourceNode>>,
    gate: Arc<AtomicBool>,
}

impl SourceSlot {
    /// Pick up the newest pending source, keeping only the last of a burst.
    ///
    /// Returns `true` if the source was replaced. Wait-free; the superseded
    /// node is dropped here, which under `basedrop` enqueues it on the
    /// collector rather than freeing it.
    fn poll_swap(&mut self) -> bool {
        let mut swapped = false;
        while let Ok(next) = self.rx.pop() {
            self.current = Some(next);
            swapped = true;
        }
        if swapped {
            let gated = self.current.as_ref().is_some_and(|source| source.gated);
            // Release: the gate, later in the same chain, must see this write.
            self.gate.store(gated, Ordering::Release);
        }
        swapped
    }
}

impl RtNode for SourceSlot {
    fn process(&mut self, ctx: &mut ProcessCtx) {
        self.poll_swap();
        match self.current.as_mut() {
            Some(source) => source.node.process(ctx),
            // A stream whose source has not arrived yet outputs silence rather
            // than whatever was in the buffers.
            None => ctx.buffers.silence(),
        }
    }
}

/// Create the source channel for one stream.
///
/// `initial` is installed directly rather than pushed, so the very first block
/// already renders the right source; it is wrapped in [`Owned`] here, on the
/// calling (non-RT) thread, because the audio thread will one day drop it.
fn source_channel(
    initial: SourceNode,
    gc: Handle,
    gate: Arc<AtomicBool>,
) -> (SourceSender, SourceSlot) {
    let (tx, rx) = mydaw_rt_core::rt_channel::<Owned<SourceNode>>(SOURCE_QUEUE_CAPACITY);
    gate.store(initial.gated, Ordering::Release);
    let current = Some(Owned::new(&gc, initial));
    (SourceSender { tx, gc }, SourceSlot { rx, current, gate })
}

/// The shell's handle on the head of the chain. `Send + Sync`.
///
/// Created once at boot and reused across stream restarts: `open` gives it the
/// sender for the current stream, `close` takes it back. Holding it in
/// [`AppState`](crate::AppState) means a Tauri command can change the
/// instrument without going anywhere near the main thread -- which is most of
/// the point of doing the swap at node granularity.
pub struct SourceHandle {
    sender: Mutex<Option<SourceSender>>,
    current: Mutex<Source>,
}

impl SourceHandle {
    /// A detached handle reporting `initial` as its source.
    pub fn new(initial: Source) -> Self {
        Self {
            sender: Mutex::new(None),
            current: Mutex::new(initial),
        }
    }

    /// Build the channel for a freshly opened stream and take its non-RT end.
    ///
    /// Returns the RT end for the caller to put in the graph. Any previous
    /// sender is dropped, which is right: it belonged to a stream that is gone.
    pub fn open(
        &self,
        initial: Box<dyn RtNode>,
        source: Source,
        gc: Handle,
        gate: Arc<AtomicBool>,
    ) -> SourceSlot {
        let (sender, slot) = source_channel(SourceNode::new(initial, source.gated()), gc, gate);
        *self.sender.lock() = Some(sender);
        *self.current.lock() = source;
        slot
    }

    /// Forget the current stream's channel. Called when audio stops.
    pub fn close(&self) {
        *self.sender.lock() = None;
    }

    /// Publish a new source node to the audio thread.
    ///
    /// # Errors
    /// [`AudioIoError::NotRunning`] when no stream is open, or
    /// [`SourceError::QueueFull`] when the audio thread is behind (retryable).
    pub fn install(&self, node: Box<dyn RtNode>, source: Source) -> Result<(), SourceError> {
        let gated = source.gated();
        let mut guard = self.sender.lock();
        let sender = guard.as_mut().ok_or(AudioIoError::NotRunning)?;
        // Allocated here, on this non-RT thread, which is the whole contract.
        let owned = Owned::new(&sender.gc, SourceNode::new(node, gated));
        // On failure the rejected node is returned inside the error and dropped
        // right here, deliberately not on the audio thread. An *abandoned*
        // queue is a stream that has stopped since this sender was attached,
        // which is a different fact from a full one and deserves the error that
        // says so.
        sender.tx.push(owned).map_err(|_| {
            if sender.tx.is_abandoned() {
                SourceError::Audio(AudioIoError::NotRunning)
            } else {
                SourceError::QueueFull
            }
        })?;
        drop(guard);
        *self.current.lock() = source;
        Ok(())
    }

    /// The source last installed.
    pub fn current(&self) -> Source {
        self.current.lock().clone()
    }

    /// Whether a stream's channel is attached. Diagnostics and tests.
    pub fn is_open(&self) -> bool {
        self.sender.lock().is_some()
    }
}

impl std::fmt::Debug for SourceHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceHandle")
            .field("current", &self.current().id())
            .field("open", &self.is_open())
            .finish()
    }
}

/// What a source *is*, for a picker that groups them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKindDto {
    /// A generator that ships with MYDAW.
    Internal,
    /// A hosted CLAP instrument found by the scan.
    Clap,
    /// A hosted VST3 instrument found by the scan.
    Vst3,
}

/// One source, as the webview sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceDto {
    pub id: String,
    pub label: String,
    pub description: String,
    /// Whether this is a built-in or a scanned CLAP plugin, so the picker can
    /// group them under headings rather than mixing them into one list.
    pub kind: SourceKindDto,
    /// Vendor, for a CLAP plugin. Empty for a built-in and for a bundle that
    /// declares none.
    pub vendor: String,
    /// Bundle path, for a CLAP plugin. The webview sends it straight back to
    /// [`set_track_source_clap`](crate::commands::set_track_source_clap), so it
    /// never has to be re-derived from an id.
    pub bundle_path: Option<String>,
    /// CLAP plugin id, for a CLAP plugin.
    pub plugin_id: Option<String>,
    /// Whether this source is the one the selected track is playing.
    pub selected: bool,
    /// Whether the computer-keyboard piano does anything for this source.
    pub takes_notes: bool,
}

impl SourceDto {
    /// Describe a built-in source in the context of the running session.
    pub fn built_in(source: &Source, current: &Source) -> Self {
        Self {
            id: source.id(),
            label: source.label(),
            description: source.description(),
            kind: SourceKindDto::Internal,
            vendor: String::new(),
            bundle_path: None,
            plugin_id: None,
            selected: source == current,
            takes_notes: source.takes_notes(),
        }
    }

    /// Describe a scanned VST3 instrument.
    ///
    /// `selected` compares the **class id and module path**, for the same reason
    /// [`clap`](Self::clap) compares plugin id and bundle path.
    pub fn vst3(info: &Vst3PluginInfo, current: &Source) -> Self {
        let reference = Vst3SourceRef::from(info);
        let selected = current
            .vst3_ref()
            .is_some_and(|active| active.cid == info.cid && active.module_path == info.path);
        Self {
            id: reference.wire_id(),
            label: reference.label(),
            description: if info.vendor.is_empty() {
                format!("VST3 — {}", info.cid)
            } else {
                format!("{} — VST3", info.vendor)
            },
            kind: SourceKindDto::Vst3,
            vendor: info.vendor.clone(),
            bundle_path: Some(info.path.display().to_string()),
            plugin_id: Some(info.cid.to_hex()),
            selected,
            takes_notes: true,
        }
    }

    /// Describe a scanned CLAP instrument.
    ///
    /// `selected` compares the **plugin id and bundle path**, not the display
    /// name: two builds of the same plugin can share a name, and a picker that
    /// ticked the wrong one would be worse than one that ticked none.
    pub fn clap(info: &ClapPluginInfo, current: &Source) -> Self {
        let reference = ClapSourceRef::from(info);
        let selected = current
            .clap_ref()
            .is_some_and(|active| active.plugin_id == info.id && active.bundle_path == info.path);
        Self {
            id: reference.wire_id(),
            label: reference.label().to_owned(),
            description: if info.vendor.is_empty() {
                info.id.clone()
            } else {
                format!("{} — {}", info.vendor, info.id)
            },
            kind: SourceKindDto::Clap,
            vendor: info.vendor.clone(),
            bundle_path: Some(info.path.display().to_string()),
            plugin_id: Some(info.id.clone()),
            selected,
            takes_notes: true,
        }
    }
}

/// A bundle path that arrived from the webview, checked before it reaches a
/// loader.
///
/// # Errors
/// [`SourceError::MainThread`] with a sentence naming the path, for an empty
/// string or one that is not a file. Rejecting here rather than in the loader
/// means the user is told "there is no such bundle" instead of the OS loader's
/// error number.
pub fn check_bundle_path(path: &str) -> Result<PathBuf, SourceError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(SourceError::MainThread(
            "a CLAP bundle path cannot be empty".to_owned(),
        ));
    }
    let path = Path::new(trimmed);
    if !path.exists() {
        return Err(SourceError::MainThread(format!(
            "there is no CLAP bundle at {}",
            path.display()
        )));
    }
    Ok(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_rt_core::{AudioBuffers, TransportSnapshot};

    /// Writes a constant, so a test can tell one source from another.
    struct ConstNode(f32);
    impl RtNode for ConstNode {
        fn process(&mut self, ctx: &mut ProcessCtx) {
            for channel in ctx.buffers.as_mut_slices() {
                channel.fill(self.0);
            }
        }
    }

    fn render(slot: &mut SourceSlot, buffers: &mut AudioBuffers) -> f32 {
        let frames = buffers.max_block();
        let mut ctx = ProcessCtx::new(buffers, 48_000.0, frames, TransportSnapshot::default());
        slot.process(&mut ctx);
        buffers.as_slices()[0][0]
    }

    fn fixture_ref() -> ClapSourceRef {
        ClapSourceRef::new(r"C:\plugins\thing.clap", "com.acme.thing").named("Acme Thing")
    }

    #[test]
    fn every_built_in_source_has_a_stable_id_that_round_trips() {
        for source in Source::BUILT_IN {
            assert_eq!(Source::parse(&source.id()).unwrap(), source);
            assert!(!source.label().is_empty());
            assert!(!source.description().is_empty());
        }
        // The ids are persisted by the UI and will be persisted by project
        // files; changing one is a breaking change, so they are pinned here.
        assert_eq!(
            Source::BUILT_IN.map(|source| source.id()),
            ["poly-synth", "drum-kit", "test-tone"]
        );
        assert_eq!(Source::DEFAULT, Source::PolySynth);
    }

    #[test]
    fn an_unknown_source_names_itself_in_the_error() {
        let err = Source::parse("moog").unwrap_err();
        assert!(matches!(err, SourceError::Unknown(ref id) if id == "moog"));
        assert!(err.to_string().contains("moog"), "{err}");
    }

    #[test]
    fn a_clap_id_alone_is_refused_because_it_names_no_bundle() {
        // Two copies of the same plugin can be installed; guessing which one a
        // bare id meant is exactly the sort of silent wrongness this refuses.
        let err = Source::parse("clap:com.acme.thing").unwrap_err();
        assert!(
            matches!(err, SourceError::ClapNeedsBundle { ref plugin_id } if plugin_id == "com.acme.thing")
        );
        assert!(err.to_string().contains("set_track_source_clap"), "{err}");
    }

    #[test]
    fn a_clap_source_carries_everything_a_project_file_needs_to_reload_it() {
        let source = Source::Clap(fixture_ref());
        assert!(source.is_clap());
        assert!(source.takes_notes());
        assert!(!source.gated());
        assert_eq!(source.id(), "clap:com.acme.thing");
        assert_eq!(source.label(), "Acme Thing");

        let reference = source.clap_ref().expect("a CLAP source has one");
        assert_eq!(reference.plugin_id, "com.acme.thing");
        assert_eq!(
            reference.bundle_path,
            PathBuf::from(r"C:\plugins\thing.clap")
        );
        assert_eq!(source.instrument_type_id(), None);
    }

    #[test]
    fn a_clap_plugin_with_no_display_name_falls_back_to_its_id() {
        // CLAP only requires an id; a bundle that declares no name must still
        // produce a strip label a user can read.
        let anonymous = ClapSourceRef::new("x.clap", "com.acme.anonymous");
        assert_eq!(anonymous.label(), "com.acme.anonymous");
        assert_eq!(anonymous.wire_id(), "clap:com.acme.anonymous");
    }

    #[test]
    fn only_the_test_tone_is_gated_and_only_it_ignores_notes() {
        assert!(Source::TestTone.gated());
        assert!(!Source::TestTone.takes_notes());
        for source in [
            Source::PolySynth,
            Source::DrumKit,
            Source::Clap(fixture_ref()),
        ] {
            assert!(
                !source.gated(),
                "{} must sound with Stop pressed",
                source.id()
            );
            assert!(source.takes_notes());
        }
    }

    #[test]
    fn the_built_in_sources_name_the_instrument_factories_that_exist() {
        let registry = mydaw_plugin_instruments::InstrumentRegistry::new();
        for source in [Source::PolySynth, Source::DrumKit] {
            let type_id = source.instrument_type_id().expect("built-in");
            assert!(
                registry.contains(type_id),
                "'{type_id}' is not a registered instrument"
            );
        }
        assert_eq!(Source::TestTone.instrument_type_id(), None);
    }

    #[test]
    fn a_handle_with_no_stream_reports_it_instead_of_pretending() {
        let handle = SourceHandle::new(Source::TestTone);
        assert!(!handle.is_open());
        assert_eq!(handle.current(), Source::TestTone);
        assert!(matches!(
            handle.install(Box::new(ConstNode(1.0)), Source::PolySynth),
            Err(SourceError::Audio(AudioIoError::NotRunning))
        ));
        assert_eq!(
            handle.current(),
            Source::TestTone,
            "a failed install must not change what the UI is told is playing"
        );
    }

    #[test]
    fn the_slot_starts_on_its_initial_source_and_swaps_on_the_next_block() {
        let collector = mydaw_rt_core::basedrop::Collector::new();
        let handle = SourceHandle::new(Source::TestTone);
        let gate = Arc::new(AtomicBool::new(false));
        let mut slot = handle.open(
            Box::new(ConstNode(0.25)),
            Source::TestTone,
            collector.handle(),
            Arc::clone(&gate),
        );
        let mut buffers = AudioBuffers::new(16);

        assert!(handle.is_open());
        assert_eq!(render(&mut slot, &mut buffers), 0.25, "the initial source");
        assert!(gate.load(Ordering::Acquire), "the tone is gated");

        handle
            .install(Box::new(ConstNode(-0.5)), Source::PolySynth)
            .expect("installs");
        assert_eq!(handle.current(), Source::PolySynth);
        assert_eq!(render(&mut slot, &mut buffers), -0.5, "the new source");
        assert!(
            !gate.load(Ordering::Acquire),
            "an instrument must not be gated on the transport"
        );
    }

    #[test]
    fn a_burst_of_swaps_collapses_to_the_last_one() {
        let collector = mydaw_rt_core::basedrop::Collector::new();
        let handle = SourceHandle::new(Source::TestTone);
        let mut slot = handle.open(
            Box::new(ConstNode(0.0)),
            Source::TestTone,
            collector.handle(),
            Arc::new(AtomicBool::new(false)),
        );
        let mut buffers = AudioBuffers::new(8);

        for value in [0.1_f32, 0.2, 0.3] {
            handle
                .install(Box::new(ConstNode(value)), Source::PolySynth)
                .unwrap();
        }
        assert_eq!(render(&mut slot, &mut buffers), 0.3);
    }

    #[test]
    fn a_full_queue_is_reported_rather_than_dropped_silently() {
        let collector = mydaw_rt_core::basedrop::Collector::new();
        let handle = SourceHandle::new(Source::TestTone);
        // The slot is never polled, so the queue fills and stays full.
        let _slot = handle.open(
            Box::new(ConstNode(0.0)),
            Source::TestTone,
            collector.handle(),
            Arc::new(AtomicBool::new(false)),
        );
        for _ in 0..SOURCE_QUEUE_CAPACITY {
            handle
                .install(Box::new(ConstNode(1.0)), Source::PolySynth)
                .unwrap();
        }
        assert!(matches!(
            handle.install(Box::new(ConstNode(1.0)), Source::PolySynth),
            Err(SourceError::QueueFull)
        ));
    }

    /// The point of `basedrop`: a superseded source is dropped on the audio
    /// thread, which must not free it.
    #[test]
    fn a_superseded_source_is_freed_by_the_collector_not_the_audio_thread() {
        let mut collector = mydaw_rt_core::basedrop::Collector::new();
        let handle = SourceHandle::new(Source::TestTone);
        let mut slot = handle.open(
            Box::new(ConstNode(1.0)),
            Source::TestTone,
            collector.handle(),
            Arc::new(AtomicBool::new(false)),
        );
        let mut buffers = AudioBuffers::new(8);
        assert_eq!(collector.alloc_count(), 1);

        handle
            .install(Box::new(ConstNode(2.0)), Source::DrumKit)
            .unwrap();
        assert_eq!(collector.alloc_count(), 2);

        render(&mut slot, &mut buffers);
        assert_eq!(
            collector.alloc_count(),
            2,
            "dropping on the audio thread must not deallocate"
        );
        collector.collect();
        assert_eq!(
            collector.alloc_count(),
            1,
            "the collector freed the old source"
        );

        handle.close();
        drop(slot);
        collector.collect();
        assert_eq!(collector.alloc_count(), 0);
    }

    #[test]
    fn a_slot_with_nothing_installed_outputs_silence() {
        // Reachable only if the sender is dropped before the first block; the
        // contract is still "silence", not "whatever was in the buffer".
        let collector = mydaw_rt_core::basedrop::Collector::new();
        let handle = SourceHandle::new(Source::TestTone);
        let mut slot = handle.open(
            Box::new(ConstNode(0.5)),
            Source::TestTone,
            collector.handle(),
            Arc::new(AtomicBool::new(false)),
        );
        slot.current = None;
        let mut buffers = AudioBuffers::new(8);
        for channel in buffers.as_mut_slices() {
            channel.fill(0.9);
        }
        assert_eq!(render(&mut slot, &mut buffers), 0.0);
    }

    #[test]
    fn the_placeholder_for_an_unattached_clap_track_is_silence_not_a_tone() {
        // A track set to a synthesiser must not answer with a sine wave because
        // the host was briefly busy attaching the plugin.
        let mut buffers = AudioBuffers::new(8);
        for channel in buffers.as_mut_slices() {
            channel.fill(0.7);
        }
        let mut node = SilentNode;
        {
            let mut ctx = ProcessCtx::new(&mut buffers, 48_000.0, 8, TransportSnapshot::default());
            node.process(&mut ctx);
        }
        assert!(buffers.as_slices()[0].iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn the_dto_separates_built_ins_from_scanned_plugins_for_the_picker() {
        let info = ClapPluginInfo {
            id: "com.acme.thing".to_owned(),
            name: "Acme Thing".to_owned(),
            vendor: "Acme".to_owned(),
            version: "1.0".to_owned(),
            path: PathBuf::from(r"C:\plugins\thing.clap"),
            features: vec!["instrument".to_owned()],
        };
        let current = Source::Clap(ClapSourceRef::from(&info));

        let built_in: Vec<SourceDto> = Source::BUILT_IN
            .iter()
            .map(|source| SourceDto::built_in(source, &current))
            .collect();
        assert_eq!(built_in.len(), 3);
        assert!(built_in
            .iter()
            .all(|dto| dto.kind == SourceKindDto::Internal));
        assert!(
            built_in.iter().all(|dto| !dto.selected),
            "a CLAP source is playing, so no built-in is"
        );
        assert!(built_in.iter().all(|dto| dto.bundle_path.is_none()));

        let hosted = SourceDto::clap(&info, &current);
        assert_eq!(hosted.kind, SourceKindDto::Clap);
        assert!(hosted.selected);
        assert_eq!(hosted.id, "clap:com.acme.thing");
        assert_eq!(hosted.vendor, "Acme");
        assert!(hosted.bundle_path.is_some());
        assert!(hosted.takes_notes);

        // The same plugin id from a different bundle is *not* the one playing.
        let elsewhere = ClapPluginInfo {
            path: PathBuf::from(r"D:\other\thing.clap"),
            ..info.clone()
        };
        assert!(!SourceDto::clap(&elsewhere, &current).selected);

        // The webview reads camelCase.
        let json = serde_json::to_string(&hosted).unwrap();
        assert!(json.contains("\"takesNotes\""), "{json}");
        assert!(json.contains("\"bundlePath\""), "{json}");
        assert!(json.contains("\"kind\":\"clap\""), "{json}");
    }

    #[test]
    fn a_bundle_path_from_the_webview_is_checked_before_it_reaches_a_loader() {
        assert!(check_bundle_path("").is_err());
        assert!(check_bundle_path("   ").is_err());
        let missing = std::env::temp_dir().join("mydaw-no-such-bundle-5e21.clap");
        let err = check_bundle_path(&missing.display().to_string()).unwrap_err();
        assert!(err.to_string().contains("no CLAP bundle"), "{err}");

        // A path that does exist is accepted and trimmed.
        let existing = std::env::temp_dir();
        let padded = format!("  {}  ", existing.display());
        assert_eq!(check_bundle_path(&padded).unwrap(), existing);
    }
}
