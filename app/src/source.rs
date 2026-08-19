//! Choosing what sits at the head of the chain, while the engine is running.
//!
//! Step 1 built the chain at startup and left it there. Step 3 has four things
//! that can generate audio -- the test tone, two built-in instruments and a
//! hosted CLAP instrument -- and a user who wants to switch between them by
//! clicking, without a click.
//!
//! # Why a slot, and not a new graph each time
//!
//! ```text
//!   install once:   SourceSlot -> [CLAP effect] -> TransportGate
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
//! the graph means building a new node for the effect too, and the old effect
//! node is not retired until the *old graph* is dropped -- which cannot happen
//! until the new graph is installed. The two requirements are circular, and the
//! ways out (install a graph without the effect, wait for the collector, then
//! install it again) drop the insert for a few milliseconds every time the user
//! changes instrument.
//!
//! Swapping at node granularity has no such problem. The graph -- and the
//! effect node inside it -- is built once and never touched again; only the
//! head of the chain moves. It is also what a DAW actually does: changing the
//! instrument on a track does not rebuild the mixer.
//!
//! The mechanism is [`GraphHandle`](mydaw_rt_core::GraphHandle)'s, one level
//! down: the new node crosses the RT wall by value through an `rtrb` queue as a
//! [`basedrop::Owned`], and the superseded one is dropped on the audio thread,
//! which under `basedrop` means *queued for the collector*, not freed.
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
//! node for it. So [`ClapSourceBridge`] is a mailbox in the shape of
//! [`ClapGuiBridge`](crate::gui_bridge::ClapGuiBridge): the command queues a
//! request, the run loop serves it, the command gets the answer or a timeout.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mydaw_plugin_audio_io::AudioIoError;
use mydaw_plugin_host_clap::LoadedClapPlugin;
use mydaw_rt_core::basedrop::{Handle, Owned};
use mydaw_rt_core::{ProcessCtx, RtNode};
use parking_lot::Mutex;
use rtrb::{Consumer, Producer};
use serde::Serialize;

use crate::graph::{clap_instrument_node, CLAP_INSTRUMENT_ENV};

/// In-flight source swaps. Four is a user clicking faster than the audio thread
/// runs a block, which does not happen; the queue exists so that overflow is a
/// reported error rather than a possibility nobody thought about.
pub const SOURCE_QUEUE_CAPACITY: usize = 4;

/// How long a Tauri command waits for the main thread to switch to the CLAP
/// instrument. Matches the plugin-GUI bridge: the work runs vendor code.
pub const CLAP_SOURCE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the main thread waits for a previously installed CLAP node to be
/// retired by the collector before giving up on re-activating the plugin.
///
/// Switching *away* from the CLAP instrument hands its node to `basedrop`, and
/// `clap_plugin->activate()` cannot be called again until the collector has
/// actually dropped it. That is a wait of one GC interval, so this is a couple
/// of them; longer would freeze the window for no benefit, because at that
/// point something is genuinely wrong rather than merely slow.
pub const CLAP_RETIRE_WAIT: Duration = Duration::from_millis(500);

/// What generates audio at the head of the chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    /// The step-1 sine generator. Free-running, so it is gated on the transport.
    TestTone,
    /// The built-in [`poly-synth`](mydaw_plugin_instruments::PolySynthNode).
    PolySynth,
    /// The built-in [`drum-kit`](mydaw_plugin_instruments::DrumKitNode).
    DrumKit,
    /// The CLAP instrument named by [`CLAP_INSTRUMENT_ENV`], if one is loaded.
    Clap,
}

impl Source {
    /// Every source, in the order the UI offers them.
    pub const ALL: [Self; 4] = [Self::PolySynth, Self::DrumKit, Self::TestTone, Self::Clap];

    /// The source a fresh session starts on when no CLAP instrument is loaded.
    ///
    /// The poly synth rather than the test tone: a DAW that greets you with a
    /// playable instrument is telling the truth about what it is, and a
    /// continuous sine is a diagnostic, not a welcome. The tone is still one
    /// click away, and is still what the hardware tests use.
    pub const DEFAULT: Self = Self::PolySynth;

    /// Stable string id, as it crosses the IPC boundary and appears in the UI.
    pub const fn id(self) -> &'static str {
        match self {
            Self::TestTone => "test-tone",
            Self::PolySynth => "poly-synth",
            Self::DrumKit => "drum-kit",
            Self::Clap => "clap",
        }
    }

    /// Name for a menu.
    pub const fn label(self) -> &'static str {
        match self {
            Self::TestTone => "Test Tone",
            Self::PolySynth => "Poly Synth",
            Self::DrumKit => "Drum Kit",
            // Short, because these are segmented buttons in a transport bar
            // that also has to fit the transport. The description carries the
            // rest.
            Self::Clap => "CLAP",
        }
    }

    /// One line for a tooltip.
    pub const fn description(self) -> &'static str {
        match self {
            Self::TestTone => "440 Hz sine, gated on the transport",
            Self::PolySynth => "Eight-voice saw + sub synth, played from the keyboard",
            Self::DrumKit => "Synthesised kick, snare and hi-hats on GM keys 36/38/42/46",
            Self::Clap => "The CLAP instrument named by MYDAW_CLAP_INSTRUMENT",
        }
    }

    /// Whether this source has to be silenced while the transport is stopped.
    ///
    /// True for the test tone only. See the module docs and [`crate::graph`].
    pub const fn gated(self) -> bool {
        matches!(self, Self::TestTone)
    }

    /// Whether this source plays note events.
    pub const fn takes_notes(self) -> bool {
        !matches!(self, Self::TestTone)
    }

    /// The `type_id` of the built-in instrument backing this source, if any.
    pub const fn instrument_type_id(self) -> Option<&'static str> {
        match self {
            Self::PolySynth => Some(mydaw_plugin_instruments::poly_synth::NODE_TYPE_ID),
            Self::DrumKit => Some(mydaw_plugin_instruments::drum_kit::NODE_TYPE_ID),
            _ => None,
        }
    }

    /// Parse an id that came from the webview.
    ///
    /// # Errors
    /// [`SourceError::Unknown`], carrying the string, so the status bar can say
    /// what was asked for rather than "invalid input".
    pub fn parse(id: &str) -> Result<Self, SourceError> {
        Self::ALL
            .into_iter()
            .find(|source| source.id() == id)
            .ok_or_else(|| SourceError::Unknown(id.to_owned()))
    }
}

/// Why a source could not be selected.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("unknown source '{0}'; expected one of test-tone, poly-synth, drum-kit or clap")]
    Unknown(String),
    /// Asked for `clap` on a session that has no CLAP instrument loaded. A
    /// normal answer, not a malfunction: the environment variable is a step
    /// affordance and most sessions do not set it.
    #[error(
        "no CLAP instrument is loaded; set {CLAP_INSTRUMENT_ENV}=<bundle>;<plugin id> \
         before starting MYDAW"
    )]
    ClapNotConfigured,
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
    /// Whatever the main thread reported while activating the CLAP instrument,
    /// or a timeout when the run loop never answered.
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
/// Created once at boot and reused across stream restarts: `attach` gives it
/// the sender for the current stream, `detach` takes it back. Holding it in
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
        let mut guard = self.sender.lock();
        let sender = guard.as_mut().ok_or(AudioIoError::NotRunning)?;
        // Allocated here, on this non-RT thread, which is the whole contract.
        let owned = Owned::new(&sender.gc, SourceNode::new(node, source.gated()));
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
        *self.current.lock()
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

/// One source, as the webview sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceDto {
    pub id: String,
    pub label: String,
    pub description: String,
    /// Whether this source can be selected right now. Only `clap` is ever
    /// unavailable, and only because no bundle was named at startup.
    pub available: bool,
    /// Whether this source is the one currently playing.
    pub selected: bool,
    /// Whether the computer-keyboard piano does anything for this source.
    pub takes_notes: bool,
}

impl SourceDto {
    /// Describe `source` in the context of the running session.
    pub fn new(source: Source, current: Source, clap_available: bool) -> Self {
        Self {
            id: source.id().to_owned(),
            label: source.label().to_owned(),
            description: source.description().to_owned(),
            available: source != Source::Clap || clap_available,
            selected: source == current,
            takes_notes: source.takes_notes(),
        }
    }
}

/// One queued request: the track it is for, and the channel its answer goes
/// back on.
type PendingClapSource = (usize, SyncSender<Result<(), String>>);

/// Mailbox between a Tauri command and the main thread's CLAP instrument.
///
/// See the module docs. Same shape as
/// [`ClapGuiBridge`](crate::gui_bridge::ClapGuiBridge), and for the same
/// reason: the thing being asked for is `!Send` and thread-bound.
pub struct ClapSourceBridge {
    queue: Mutex<VecDeque<PendingClapSource>>,
    /// Whether a CLAP instrument was loaded at startup. Published by the main
    /// thread so a command can reject `clap` without a round trip -- and, more
    /// importantly, so the UI can grey the option out instead of offering a
    /// button that always fails.
    available: AtomicBool,
    waker: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl Default for ClapSourceBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl ClapSourceBridge {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            available: AtomicBool::new(false),
            waker: Mutex::new(None),
        }
    }

    /// Record whether a CLAP instrument is loaded. Called once by the shell.
    pub fn set_available(&self, available: bool) {
        self.available.store(available, Ordering::Release);
    }

    /// Whether `clap` is a selectable source in this session.
    pub fn is_available(&self) -> bool {
        self.available.load(Ordering::Acquire)
    }

    /// Install the callback that wakes the event loop.
    pub fn set_waker(&self, waker: Box<dyn Fn() + Send + Sync>) {
        *self.waker.lock() = Some(waker);
    }

    /// Ask the main thread to make the CLAP instrument `track`'s source, and
    /// wait.
    ///
    /// # Errors
    /// [`SourceError::ClapNotConfigured`] without a round trip when no bundle
    /// was loaded, or [`SourceError::MainThread`] carrying whatever the main
    /// thread said -- including a timeout, which is what a build with no window
    /// (a headless capture, a test) produces.
    pub fn request(&self, track: usize) -> Result<(), SourceError> {
        if !self.is_available() {
            return Err(SourceError::ClapNotConfigured);
        }
        let (reply, answer) = sync_channel(1);
        self.queue.lock().push_back((track, reply));
        if let Some(waker) = self.waker.lock().as_ref() {
            waker();
        }

        match answer.recv_timeout(CLAP_SOURCE_TIMEOUT) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(message)) => Err(SourceError::MainThread(message)),
            Err(_) => Err(SourceError::MainThread(format!(
                "the main thread did not answer within {} s; is the window still open?",
                CLAP_SOURCE_TIMEOUT.as_secs()
            ))),
        }
    }

    /// Take every queued request. Called from the main thread.
    pub fn drain(&self) -> Vec<PendingClapSource> {
        self.queue.lock().drain(..).collect()
    }

    /// How many requests are waiting. Diagnostics and tests.
    pub fn pending(&self) -> usize {
        self.queue.lock().len()
    }
}

impl std::fmt::Debug for ClapSourceBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClapSourceBridge")
            .field("available", &self.is_available())
            .field("pending", &self.pending())
            .finish()
    }
}

/// Serve every queued CLAP-source request. **Main thread only.**
///
/// Called from the run loop's `MainEventsCleared`, next to the plugin-GUI host,
/// because it needs the same `!Send` [`LoadedClapPlugin`] the editor does.
///
/// Waits (up to [`CLAP_RETIRE_WAIT`]) for a previously installed node to come
/// back from the collector before re-activating: a plugin can only be active
/// once, so switching away and back is exactly the case that needs the wait.
/// The wait only happens on that path, and only after the user asked for it.
///
/// There is one plugin instance, so a burst of requests naming *different*
/// tracks cannot all be honoured: the first queued one wins and the rest are
/// told why. Only a user clicking two mixer strips inside one frame produces
/// that, and saying so beats silently activating the wrong track.
pub fn serve_clap_source_requests(
    bridge: &ClapSourceBridge,
    host: &crate::tracks::TrackHost,
    instrument: Option<&mut LoadedClapPlugin>,
    stream: Option<(f64, usize)>,
) {
    let mut pending = bridge.drain().into_iter();
    let Some((track, reply)) = pending.next() else {
        return;
    };
    let answer = build_clap_source(host, track, instrument, stream);
    // A command that timed out has dropped its receiver; that is its business,
    // and it must not panic this thread.
    let _ = reply.try_send(answer);
    for (_, superseded) in pending {
        let _ = superseded.try_send(Err(
            "another track claimed the CLAP instrument first; there is only one instance"
                .to_owned(),
        ));
    }
}

/// The body of [`serve_clap_source_requests`], separated so the error strings
/// are written once.
fn build_clap_source(
    host: &crate::tracks::TrackHost,
    track: usize,
    instrument: Option<&mut LoadedClapPlugin>,
    stream: Option<(f64, usize)>,
) -> Result<(), String> {
    let Some(plugin) = instrument else {
        return Err(SourceError::ClapNotConfigured.to_string());
    };
    let Some((sample_rate, max_block)) = stream else {
        return Err(AudioIoError::NotRunning.to_string());
    };

    // The previous node is retired by the `basedrop` collector some time after
    // the slot swapped it out, and `activate()` cannot run until it is.
    let deadline = Instant::now() + CLAP_RETIRE_WAIT;
    while plugin.is_active() && Instant::now() < deadline {
        if plugin.collect_retired() > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    let node = clap_instrument_node(plugin, sample_rate, max_block).ok_or_else(|| {
        format!(
            "the CLAP instrument '{}' could not be activated; see the log",
            plugin.info().id
        )
    })?;
    host.install_source(track, Source::Clap, node)
        .map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_rt_core::{AudioBuffers, TransportSnapshot};
    use std::thread::sleep;

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

    #[test]
    fn every_source_has_a_stable_id_that_round_trips() {
        for source in Source::ALL {
            assert_eq!(Source::parse(source.id()).unwrap(), source);
            assert!(!source.label().is_empty());
            assert!(!source.description().is_empty());
        }
        // The ids are persisted by the UI and will be persisted by project
        // files; changing one is a breaking change, so they are pinned here.
        assert_eq!(
            Source::ALL.map(Source::id),
            ["poly-synth", "drum-kit", "test-tone", "clap"]
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
    fn only_the_test_tone_is_gated_and_only_it_ignores_notes() {
        assert!(Source::TestTone.gated());
        assert!(!Source::TestTone.takes_notes());
        for source in [Source::PolySynth, Source::DrumKit, Source::Clap] {
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
        assert_eq!(Source::Clap.instrument_type_id(), None);
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

    /// A host on the default two-track project, for the tests that need one.
    fn host() -> crate::tracks::TrackHost {
        crate::tracks::TrackHost::new(
            Arc::new(mydaw_plugin_test_tone::TestToneFactory::new()),
            Arc::new(mydaw_plugin_instruments::InstrumentRegistry::new()),
        )
    }

    #[test]
    fn the_clap_bridge_refuses_without_a_round_trip_when_nothing_is_loaded() {
        let bridge = ClapSourceBridge::new();
        assert!(!bridge.is_available());
        assert!(matches!(
            bridge.request(0),
            Err(SourceError::ClapNotConfigured)
        ));
        assert_eq!(
            bridge.pending(),
            0,
            "nothing may be queued for the main thread"
        );
    }

    #[test]
    fn a_clap_request_is_queued_answered_and_woken() {
        let bridge = Arc::new(ClapSourceBridge::new());
        bridge.set_available(true);
        let woken = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&woken);
        bridge.set_waker(Box::new(move || flag.store(true, Ordering::Release)));

        let main_thread = Arc::clone(&bridge);
        let loop_thread = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                let pending = main_thread.drain();
                if let Some((track, reply)) = pending.into_iter().next() {
                    assert_eq!(track, 1, "the request names the track it is for");
                    let _ = reply.try_send(Ok(()));
                    return;
                }
                assert!(Instant::now() < deadline, "the request never arrived");
                sleep(Duration::from_millis(5));
            }
        });

        bridge.request(1).expect("the main thread answered");
        assert!(woken.load(Ordering::Acquire), "the loop must be poked");
        assert_eq!(bridge.pending(), 0);
        loop_thread.join().unwrap();
    }

    #[test]
    fn an_error_from_the_main_thread_reaches_the_caller_verbatim() {
        let bridge = Arc::new(ClapSourceBridge::new());
        bridge.set_available(true);
        let main_thread = Arc::clone(&bridge);
        let loop_thread = std::thread::spawn(move || loop {
            if let Some((_, reply)) = main_thread.drain().into_iter().next() {
                let _ = reply.try_send(Err("this plugin will not activate".to_owned()));
                return;
            }
            sleep(Duration::from_millis(5));
        });

        let err = bridge.request(0).unwrap_err();
        assert_eq!(err.to_string(), "this plugin will not activate");
        loop_thread.join().unwrap();
    }

    #[test]
    fn serving_a_request_with_no_plugin_answers_rather_than_hanging() {
        let bridge = ClapSourceBridge::new();
        bridge.set_available(true);
        let host = host();
        let (reply, answer) = sync_channel(1);
        bridge.queue.lock().push_back((0, reply));

        serve_clap_source_requests(&bridge, &host, None, Some((48_000.0, 512)));
        let outcome = answer.try_recv().expect("answered immediately");
        assert!(outcome.is_err());
        assert_eq!(bridge.pending(), 0);
    }

    #[test]
    fn a_second_request_in_the_same_frame_is_told_it_lost_rather_than_ignored() {
        let bridge = ClapSourceBridge::new();
        bridge.set_available(true);
        let host = host();
        let (first, first_answer) = sync_channel(1);
        let (second, second_answer) = sync_channel(1);
        bridge.queue.lock().push_back((0, first));
        bridge.queue.lock().push_back((1, second));

        serve_clap_source_requests(&bridge, &host, None, Some((48_000.0, 512)));
        assert!(first_answer.try_recv().expect("answered").is_err());
        let loser = second_answer
            .try_recv()
            .expect("also answered")
            .unwrap_err();
        assert!(loser.contains("only one instance"), "{loser}");
        assert_eq!(bridge.pending(), 0);
    }

    #[test]
    fn serving_with_no_pending_request_does_nothing_at_all() {
        let bridge = ClapSourceBridge::new();
        let host = host();
        serve_clap_source_requests(&bridge, &host, None, None);
        assert_eq!(bridge.pending(), 0);
        assert_eq!(host.selected_source(), Source::DrumKit);
    }

    #[test]
    fn the_dto_marks_availability_and_selection_for_the_webview() {
        let listed: Vec<SourceDto> = Source::ALL
            .into_iter()
            .map(|source| SourceDto::new(source, Source::DrumKit, false))
            .collect();

        assert_eq!(listed.len(), 4);
        assert!(listed.iter().filter(|dto| dto.selected).count() == 1);
        assert!(
            listed
                .iter()
                .find(|dto| dto.id == "drum-kit")
                .unwrap()
                .selected
        );
        let clap = listed.iter().find(|dto| dto.id == "clap").unwrap();
        assert!(!clap.available, "no bundle was loaded");
        assert!(listed
            .iter()
            .filter(|dto| dto.id != "clap")
            .all(|dto| dto.available));
        assert!(
            !listed
                .iter()
                .find(|dto| dto.id == "test-tone")
                .unwrap()
                .takes_notes
        );

        // The webview reads camelCase.
        let json = serde_json::to_string(&listed[0]).unwrap();
        assert!(json.contains("\"takesNotes\""), "{json}");

        let with_clap = SourceDto::new(Source::Clap, Source::Clap, true);
        assert!(with_clap.available && with_clap.selected);
    }
}
