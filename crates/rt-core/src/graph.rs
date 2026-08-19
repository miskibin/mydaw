//! Audio graph: built off-thread, handed to the audio thread lock-free.
//!
//! # Shape
//! From step 5 the graph is a mixer, not a chain:
//!
//! ```text
//!   track 0:  source -> insert.. -> [strip: gain, pan, mute] --\
//!   track 1:  source -> insert.. -> [strip: gain, pan, mute] ---+--> sum
//!   ...                                                        /      |
//!   track n:  source -> insert.. -> [strip: gain, pan, mute] --/       v
//!                                                        master insert..
//!                                                                     |
//!                                                        [master strip: gain]
//! ```
//!
//! Each track renders into its **own** pre-allocated stereo scratch buffer, and
//! its strip scales that buffer into the shared bus. `ctx.buffers` -- the bus the
//! audio callback hands in -- is the *master*: it is cleared at the top of the
//! block, summed into, and is what the device, the meters and the debug tap see.
//!
//! Tracks are processed one after another on the audio thread. Rendering them in
//! parallel is a real win once tracks carry heavyweight plugins, and it is
//! deliberately not attempted here: it needs a worker pool with real-time
//! priority, a dependency-ordered schedule and a wait that cannot block the
//! callback, none of which pay for themselves at two tracks of synthesis. The
//! seam is [`CompiledGraph::process`]'s loop, and nothing outside this file
//! depends on it being serial.
//!
//! # Note routing
//! A [`NoteEvent`]'s `channel` **is** the track index. Each track is handed only
//! the events addressed to it, filtered into a per-track buffer that was
//! allocated at compile time. So a scheduler row on channel 1 plays track 1's
//! instrument and nothing else, and a key pressed on the UI's piano goes to
//! whichever track the user selected.
//!
//! # Why not `ArcSwap<CompiledGraph>`
//! The obvious "publish a new graph atomically" answer is `ArcSwap`, but
//! [`RtNode::process`] needs `&mut self`: nodes hold filter state, oscillator
//! phase, delay lines. An `Arc` only ever yields shared access, so the audio
//! thread could never call into the graph without a lock or interior mutability.
//!
//! So ownership moves instead of being shared: the audio thread **owns** the
//! current graph, and new graphs arrive by value through an `rtrb` queue as
//! [`basedrop::Owned`]. When the audio thread drops the superseded graph, the
//! `Owned` wrapper hands the allocation to a [`basedrop::Collector`], which frees
//! it later on the GC thread -- so no `free()` ever runs in the callback.
//!
//! ```text
//!  non-RT thread                       audio thread
//!  ------------                        ------------
//!  GraphSpec --compile--> Owned<CompiledGraph>
//!                    -- rtrb push --> rtrb pop --> RtGraphSlot::current
//!                                                    |  (old graph dropped)
//!  GcThread <-- basedrop collector queue <------------+
//! ```
//!
//! # What survives a rebuild
//! [`MixerParams`] is shared by `Arc` and handed to every graph compiled from a
//! spec that names it, so adding a track does not reset anybody's fader. Each
//! strip is constructed *already sitting at* its target, so the new graph picks
//! the mixer up exactly where the old one left it rather than fading in.

use std::sync::Arc;

use basedrop::{Handle, Owned};
use rtrb::{Consumer, Producer};

use crate::mixer::{MasterStrip, MixerParams, TrackPeak, TrackPeaks, TrackStrip, MAX_TRACKS};
use crate::{AudioBuffers, NoteEvent, ProcessCtx, RtNode};

/// Block size a graph's scratch buffers are sized to when the builder does not
/// say. Matches `AudioIoConfig::max_block`; the audio service overrides it with
/// the real one when the graph is installed into a running stream.
pub const DEFAULT_MAX_BLOCK: usize = 4096;

/// Note events one track's per-block buffer can hold when the builder does not
/// say. Matches [`DEFAULT_NOTE_CAPACITY`](crate::DEFAULT_NOTE_CAPACITY).
pub const DEFAULT_MAX_EVENTS: usize = crate::DEFAULT_NOTE_CAPACITY;

/// Sample rate a graph's parameter ramps assume when the builder does not say.
pub const DEFAULT_SAMPLE_RATE: f64 = 48_000.0;

/// The sizes a compiled graph has to allocate for, fixed before it crosses the
/// RT wall.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GraphLimits {
    /// Ramp lengths are derived from this; see [`crate::mixer`].
    pub sample_rate: f64,
    /// Frames each track's scratch buffer is allocated for.
    pub max_block: usize,
    /// Note events one track's per-block filter buffer can hold.
    pub max_events: usize,
}

impl Default for GraphLimits {
    fn default() -> Self {
        Self {
            sample_rate: DEFAULT_SAMPLE_RATE,
            max_block: DEFAULT_MAX_BLOCK,
            max_events: DEFAULT_MAX_EVENTS,
        }
    }
}

/// One track, as a non-RT thread describes it.
pub struct TrackSpec {
    /// What generates the track's audio. Runs first, into a silent buffer.
    pub source: Box<dyn RtNode>,
    /// Effects, in order, after the source.
    pub inserts: Vec<Box<dyn RtNode>>,
    /// Note channel this track answers to. Defaults to the track's index.
    pub channel: Option<u8>,
}

impl TrackSpec {
    /// A track with `source` at its head and nothing else.
    pub fn new(source: Box<dyn RtNode>) -> Self {
        Self {
            source,
            inserts: Vec::new(),
            channel: None,
        }
    }

    /// Builder-style insert append.
    #[must_use]
    pub fn with_insert(mut self, node: Box<dyn RtNode>) -> Self {
        self.inserts.push(node);
        self
    }

    /// Address this track on a note channel other than its index.
    #[must_use]
    pub const fn on_channel(mut self, channel: u8) -> Self {
        self.channel = Some(channel);
        self
    }

    /// Nodes in this track: the source plus its inserts.
    pub fn node_count(&self) -> usize {
        1 + self.inserts.len()
    }
}

/// A graph description assembled on a non-RT thread.
///
/// Two ways to build one, and they are the same builder:
///
/// - [`push`](Self::push) / [`with`](Self::with) append to a **single** track,
///   which is the shape everything before step 5 used and every offline render
///   still wants: `spec.with(source).with(effect)` is one track whose source is
///   `source`.
/// - [`push_track`](Self::push_track) / [`with_track`](Self::with_track) add
///   whole tracks, which is what the shell's project builder uses.
#[derive(Default)]
pub struct GraphSpec {
    tracks: Vec<TrackSpec>,
    master_inserts: Vec<Box<dyn RtNode>>,
    params: Option<Arc<MixerParams>>,
    limits: Option<GraphLimits>,
}

impl GraphSpec {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a node to the last track, starting one if the spec is empty.
    ///
    /// The first node pushed becomes a track's source; every node after it is an
    /// insert on that same track. That makes a bare `GraphSpec` a serial chain,
    /// which is exactly what it was before the mixer existed.
    pub fn push(&mut self, node: Box<dyn RtNode>) -> &mut Self {
        match self.tracks.last_mut() {
            Some(track) => track.inserts.push(node),
            None => self.tracks.push(TrackSpec::new(node)),
        }
        self
    }

    /// Builder-style [`push`](Self::push).
    #[must_use]
    pub fn with(mut self, node: Box<dyn RtNode>) -> Self {
        self.push(node);
        self
    }

    /// Add a whole track.
    pub fn push_track(&mut self, track: TrackSpec) -> &mut Self {
        self.tracks.push(track);
        self
    }

    /// Builder-style [`push_track`](Self::push_track).
    #[must_use]
    pub fn with_track(mut self, track: TrackSpec) -> Self {
        self.tracks.push(track);
        self
    }

    /// Append an effect to the master bus, after the sum and before the fader.
    pub fn push_master_insert(&mut self, node: Box<dyn RtNode>) -> &mut Self {
        self.master_inserts.push(node);
        self
    }

    /// Builder-style [`push_master_insert`](Self::push_master_insert).
    #[must_use]
    pub fn with_master_insert(mut self, node: Box<dyn RtNode>) -> Self {
        self.master_inserts.push(node);
        self
    }

    /// Use `params` as the mixer's parameter block.
    ///
    /// Shared by `Arc` on purpose: pass the same block to every graph and the
    /// faders survive a rebuild. Without this the compiled graph gets a fresh
    /// block at unity gain and centre pan, which is what every test and every
    /// offline render wants.
    #[must_use]
    pub fn with_params(mut self, params: Arc<MixerParams>) -> Self {
        self.params = Some(params);
        self
    }

    /// Pin the sizes the compiled graph allocates for.
    ///
    /// Left unset, [`AudioIoService::install_graph`] fills them in from the open
    /// stream, which is what the shell relies on -- a graph builder should not
    /// have to know the device's block size to be correct.
    ///
    /// [`AudioIoService::install_graph`]: ../../mydaw_plugin_audio_io/service/struct.AudioIoService.html#method.install_graph
    #[must_use]
    pub fn with_limits(mut self, limits: GraphLimits) -> Self {
        self.limits = Some(limits);
        self
    }

    /// Set the limits only if the builder did not pin them. Returns whether they
    /// were applied.
    pub fn apply_default_limits(&mut self, limits: GraphLimits) -> bool {
        if self.limits.is_some() {
            return false;
        }
        self.limits = Some(limits);
        true
    }

    /// The limits this spec will compile with.
    pub fn limits(&self) -> GraphLimits {
        self.limits.unwrap_or_default()
    }

    /// The mixer parameters this spec will compile with, if it was given any.
    pub fn params(&self) -> Option<&Arc<MixerParams>> {
        self.params.as_ref()
    }

    /// Total nodes across every track and the master bus.
    pub fn len(&self) -> usize {
        self.tracks.iter().map(TrackSpec::node_count).sum::<usize>() + self.master_inserts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Tracks in this spec.
    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// Freeze the spec into something the audio thread can run.
    ///
    /// Non-RT: this is the last point at which allocation is allowed. Every
    /// scratch buffer, every per-track event buffer and every strip is built
    /// here.
    ///
    /// Tracks past [`MAX_TRACKS`] are dropped rather than allocated for -- the
    /// RT-side arrays are that size, and a builder that produced more has a bug
    /// its caller should have caught.
    pub fn compile(mut self) -> CompiledGraph {
        let limits = self.limits.unwrap_or_default();
        let params = self.params.unwrap_or_else(|| Arc::new(MixerParams::new()));
        self.tracks.truncate(MAX_TRACKS);

        let tracks: Vec<TrackChain> = self
            .tracks
            .into_iter()
            .enumerate()
            .map(|(index, spec)| TrackChain::new(index, spec, &params, &limits))
            .collect();

        let master = MasterStrip::new(limits.sample_rate, params.master_gain());
        CompiledGraph {
            peaks: TrackPeaks::empty(tracks.len()),
            tracks,
            master_inserts: self.master_inserts,
            master,
            params,
        }
    }
}

/// One track as the audio thread runs it.
struct TrackChain {
    source: Box<dyn RtNode>,
    inserts: Vec<Box<dyn RtNode>>,
    /// Note channel this track answers to.
    channel: u8,
    strip: TrackStrip,
    /// This track's private stereo bus. Allocated once, reused every block.
    scratch: AudioBuffers,
    /// The subset of the block's events addressed to this track. Allocated to
    /// its full capacity here and only ever `clear`ed and `push`ed after that.
    events: Vec<NoteEvent>,
}

impl TrackChain {
    fn new(index: usize, spec: TrackSpec, params: &MixerParams, limits: &GraphLimits) -> Self {
        Self {
            source: spec.source,
            inserts: spec.inserts,
            channel: spec.channel.unwrap_or(index as u8),
            strip: TrackStrip::new(limits.sample_rate, params.track_target(index)),
            scratch: AudioBuffers::new(limits.max_block.max(1)),
            events: Vec::with_capacity(limits.max_events),
        }
    }

    fn node_count(&self) -> usize {
        1 + self.inserts.len()
    }
}

/// An executable graph. Owned by the audio thread while it is current.
pub struct CompiledGraph {
    tracks: Vec<TrackChain>,
    master_inserts: Vec<Box<dyn RtNode>>,
    master: MasterStrip,
    params: Arc<MixerParams>,
    /// Last block's post-strip peak per track, for the meter path.
    peaks: TrackPeaks,
}

impl CompiledGraph {
    /// Render one block: every track into its own buffer, then the sum.
    ///
    /// RT-safe as long as each node honours the zero-alloc rule; this function
    /// itself only iterates pre-allocated `Vec`s and writes pre-allocated
    /// buffers.
    pub fn process(&mut self, ctx: &mut ProcessCtx) {
        // Destructured so the parameter block can be read while the tracks are
        // borrowed mutably -- the alternative is an `Arc::clone` per block.
        let Self {
            tracks,
            master_inserts,
            master,
            params,
            peaks,
        } = self;

        // The bus starts silent: it is a *sum*, and whatever the previous block
        // left in it is not part of this one.
        ctx.buffers.silence();
        *peaks = TrackPeaks::empty(tracks.len());

        for (index, track) in tracks.iter_mut().enumerate() {
            let TrackChain {
                source,
                inserts,
                channel,
                strip,
                scratch,
                events,
            } = track;

            // Only this track's notes. `ctx.events` is sorted by frame offset
            // and the filter preserves order, so the subset is sorted too.
            events.clear();
            if !ctx.events.is_empty() {
                let capacity = events.capacity();
                for event in ctx.events {
                    if event.channel == *channel && events.len() < capacity {
                        events.push(*event);
                    }
                }
            }

            let frames = {
                let mut track_ctx = ProcessCtx::with_events(
                    scratch,
                    ctx.sample_rate,
                    ctx.frames,
                    ctx.transport,
                    events,
                );
                // A track's input is silence; its source generates into it.
                track_ctx.buffers.silence();
                source.process(&mut track_ctx);
                for node in inserts.iter_mut() {
                    node.process(&mut track_ctx);
                }
                track_ctx.frames
            };
            debug_assert!(frames <= ctx.frames);

            let peak = strip.mix_into(scratch, ctx.buffers, params.track_target(index));
            peaks.set(
                index,
                TrackPeak {
                    peak_l: peak[0],
                    peak_r: peak[1],
                },
            );
        }

        for node in master_inserts.iter_mut() {
            node.process(ctx);
        }
        master.apply(ctx.buffers, params.master_gain());
    }

    /// Total nodes across every track and the master bus.
    pub fn node_count(&self) -> usize {
        self.tracks
            .iter()
            .map(TrackChain::node_count)
            .sum::<usize>()
            + self.master_inserts.len()
    }

    /// Tracks this graph runs.
    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// Note channel `track` answers to.
    pub fn track_channel(&self, track: usize) -> Option<u8> {
        self.tracks.get(track).map(|t| t.channel)
    }

    /// Per-track peaks from the last block processed.
    pub fn track_peaks(&self) -> TrackPeaks {
        self.peaks
    }

    /// The mixer parameters this graph reads.
    pub fn params(&self) -> &Arc<MixerParams> {
        &self.params
    }
}

/// Failure modes of [`GraphHandle::install`].
#[derive(Debug, thiserror::Error)]
pub enum GraphInstallError {
    /// The audio thread has not consumed previous graphs yet. The caller should
    /// retry; the rejected graph is dropped here, on the non-RT thread.
    #[error("graph queue is full; the audio thread has not picked up previous graphs")]
    QueueFull,
    /// The audio thread went away (queue consumer dropped).
    #[error("audio thread is gone; cannot install graph")]
    Disconnected,
}

/// Non-RT side of the graph channel.
pub struct GraphHandle {
    tx: Producer<Owned<CompiledGraph>>,
    gc: Handle,
}

impl GraphHandle {
    /// Compile `spec` and hand the result to the audio thread.
    ///
    /// All allocation (node construction, scratch buffers, the `Owned`
    /// allocation) happens here, on the calling non-RT thread.
    pub fn install(&mut self, spec: GraphSpec) -> Result<(), GraphInstallError> {
        let graph = Owned::new(&self.gc, spec.compile());
        // On failure the rejected graph is returned inside the error and dropped
        // right here -- deliberately on this thread, never on the audio thread.
        self.tx.push(graph).map_err(|_| {
            if self.tx.is_abandoned() {
                GraphInstallError::Disconnected
            } else {
                GraphInstallError::QueueFull
            }
        })
    }

    /// Collector handle, for code that needs to allocate other deferred-drop data.
    pub fn gc_handle(&self) -> Handle {
        self.gc.clone()
    }
}

/// Audio-thread side of the graph channel: owns the current graph.
pub struct RtGraphSlot {
    rx: Consumer<Owned<CompiledGraph>>,
    current: Option<Owned<CompiledGraph>>,
}

impl RtGraphSlot {
    /// Pick up the newest pending graph, if any. Wait-free; call once per block
    /// *before* [`process`](Self::process).
    ///
    /// Drains the whole queue and keeps only the last graph, so a burst of edits
    /// costs one block, not one block each. Superseded graphs are dropped here,
    /// which only enqueues them on the collector -- no `free()` on this thread.
    ///
    /// Returns `true` if the current graph was replaced.
    pub fn poll_swap(&mut self) -> bool {
        let mut swapped = false;
        while let Ok(next) = self.rx.pop() {
            // Assigning drops the previous `Owned`, which pushes its allocation
            // onto the collector's queue (an atomic list push -- no dealloc).
            self.current = Some(next);
            swapped = true;
        }
        swapped
    }

    /// Run the current graph. Outputs silence when no graph is installed yet.
    pub fn process(&mut self, ctx: &mut ProcessCtx) {
        match self.current.as_mut() {
            Some(graph) => graph.process(ctx),
            None => ctx.buffers.silence(),
        }
    }

    pub fn has_graph(&self) -> bool {
        self.current.is_some()
    }

    /// Node count of the current graph. Diagnostics/tests.
    pub fn node_count(&self) -> usize {
        self.current.as_ref().map_or(0, |g| g.node_count())
    }

    /// Tracks the current graph runs.
    pub fn track_count(&self) -> usize {
        self.current.as_ref().map_or(0, |g| g.track_count())
    }

    /// Per-track peaks from the last block. Silent when no graph is installed.
    pub fn track_peaks(&self) -> TrackPeaks {
        self.current
            .as_ref()
            .map_or(TrackPeaks::SILENT, |g| g.track_peaks())
    }
}

/// Create a graph channel. `capacity` is the number of in-flight graphs.
///
/// `gc` comes from a [`basedrop::Collector`] -- in the app, from
/// [`GcThread::handle`](crate::GcThread::handle).
pub fn graph_channel(capacity: usize, gc: Handle) -> (GraphHandle, RtGraphSlot) {
    let (tx, rx) = crate::rt_channel::<Owned<CompiledGraph>>(capacity);
    (GraphHandle { tx, gc }, RtGraphSlot { rx, current: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mixer::{pan_gains, MAX_TRACKS};
    use crate::{AudioBuffers, NoteKind, TransportSnapshot};
    use basedrop::Collector;

    const SR: f64 = 48_000.0;

    fn limits(max_block: usize) -> GraphLimits {
        GraphLimits {
            sample_rate: SR,
            max_block,
            max_events: 64,
        }
    }

    /// Writes a constant into both channels so tests can tell graphs apart.
    struct ConstNode(f32);
    impl RtNode for ConstNode {
        fn process(&mut self, ctx: &mut ProcessCtx) {
            for ch in ctx.buffers.as_mut_slices() {
                ch.fill(self.0);
            }
        }
    }

    /// Adds a constant, to verify serial ordering / accumulation.
    struct AddNode(f32);
    impl RtNode for AddNode {
        fn process(&mut self, ctx: &mut ProcessCtx) {
            for ch in ctx.buffers.as_mut_slices() {
                for s in ch.iter_mut() {
                    *s += self.0;
                }
            }
        }
    }

    /// Writes `value` only while it has seen a note-on, so a test can tell which
    /// track an event reached.
    #[derive(Default)]
    struct NoteGatedNode {
        value: f32,
        keys: Vec<u8>,
        open: bool,
    }

    impl NoteGatedNode {
        fn new(value: f32) -> Self {
            Self {
                value,
                keys: Vec::with_capacity(64),
                open: false,
            }
        }
    }

    impl RtNode for NoteGatedNode {
        fn process(&mut self, ctx: &mut ProcessCtx) {
            for event in ctx.events {
                if let NoteKind::NoteOn { key, .. } = event.kind {
                    self.keys.push(key);
                    self.open = true;
                }
            }
            let value = if self.open { self.value } else { 0.0 };
            for ch in ctx.buffers.as_mut_slices() {
                ch.fill(value);
            }
        }
    }

    fn render(slot: &mut RtGraphSlot, buffers: &mut AudioBuffers, frames: usize) -> f32 {
        let mut ctx = ProcessCtx::new(buffers, SR, frames, TransportSnapshot::default());
        slot.process(&mut ctx);
        buffers.as_slices()[0][0]
    }

    #[test]
    fn silence_before_any_graph_is_installed() {
        let collector = Collector::new();
        let (_h, mut slot) = graph_channel(2, collector.handle());
        let mut buffers = AudioBuffers::new(16);
        for ch in buffers.as_mut_slices() {
            ch.fill(0.5);
        }
        assert!(!slot.has_graph());
        assert_eq!(render(&mut slot, &mut buffers, 16), 0.0);
        assert_eq!(slot.track_peaks(), TrackPeaks::SILENT);
        assert_eq!(slot.track_count(), 0);
    }

    #[test]
    fn graph_swap_delivers_new_graph_to_rt_side() {
        let collector = Collector::new();
        let (mut handle, mut slot) = graph_channel(4, collector.handle());
        let mut buffers = AudioBuffers::new(16);

        handle
            .install(GraphSpec::new().with(Box::new(ConstNode(0.25))))
            .unwrap();

        // Simulated audio thread: poll, then process.
        assert!(slot.poll_swap());
        assert!(slot.has_graph());
        assert_eq!(render(&mut slot, &mut buffers, 16), 0.25);

        // Nothing pending -> no swap, previous graph keeps running.
        assert!(!slot.poll_swap());
        assert_eq!(render(&mut slot, &mut buffers, 16), 0.25);

        handle
            .install(GraphSpec::new().with(Box::new(ConstNode(-1.0))))
            .unwrap();
        assert!(slot.poll_swap());
        assert_eq!(render(&mut slot, &mut buffers, 16), -1.0);
    }

    #[test]
    fn serial_chain_runs_in_order() {
        let collector = Collector::new();
        let (mut handle, mut slot) = graph_channel(2, collector.handle());
        let mut buffers = AudioBuffers::new(8);

        let mut spec = GraphSpec::new();
        spec.push(Box::new(ConstNode(1.0)));
        spec.push(Box::new(AddNode(0.5)));
        spec.push(Box::new(AddNode(0.25)));
        assert_eq!(spec.len(), 3);
        assert_eq!(spec.track_count(), 1, "a bare chain is one track");
        handle.install(spec).unwrap();

        slot.poll_swap();
        assert_eq!(slot.node_count(), 3);
        assert_eq!(render(&mut slot, &mut buffers, 8), 1.75);
    }

    #[test]
    fn a_default_mixer_is_transparent() {
        // The property the whole compatibility story rests on: one track, unity
        // gain, centre pan, unity master -- bit-identical to what the source
        // produced. See the pan-law normalisation note in `crate::mixer`.
        let collector = Collector::new();
        let (mut handle, mut slot) = graph_channel(2, collector.handle());
        let mut buffers = AudioBuffers::new(8);
        handle
            .install(GraphSpec::new().with(Box::new(ConstNode(0.123_456_79))))
            .unwrap();
        slot.poll_swap();
        assert_eq!(render(&mut slot, &mut buffers, 8), 0.123_456_79);
    }

    #[test]
    fn two_tracks_sum_into_the_master_bus() {
        let collector = Collector::new();
        let (mut handle, mut slot) = graph_channel(2, collector.handle());
        let mut buffers = AudioBuffers::new(8);

        let spec = GraphSpec::new()
            .with_limits(limits(8))
            .with_track(TrackSpec::new(Box::new(ConstNode(0.25))))
            .with_track(TrackSpec::new(Box::new(ConstNode(0.5))));
        assert_eq!(spec.track_count(), 2);
        assert_eq!(spec.len(), 2);
        handle.install(spec).unwrap();
        slot.poll_swap();

        assert_eq!(render(&mut slot, &mut buffers, 8), 0.75);
        assert_eq!(slot.track_count(), 2);

        let peaks = slot.track_peaks();
        assert_eq!(peaks.count(), 2);
        assert_eq!(peaks.get(0).peak_l, 0.25);
        assert_eq!(peaks.get(1).peak_l, 0.5);
    }

    #[test]
    fn a_tracks_gain_and_pan_reach_the_sum_and_its_meter() {
        let collector = Collector::new();
        let (mut handle, mut slot) = graph_channel(2, collector.handle());
        let mut buffers = AudioBuffers::new(64);
        let params = Arc::new(MixerParams::new());

        params.set_track_gain(0, 0.5);
        params.set_track_pan(1, -1.0);
        handle
            .install(
                GraphSpec::new()
                    .with_limits(limits(64))
                    .with_params(Arc::clone(&params))
                    .with_track(TrackSpec::new(Box::new(ConstNode(1.0))))
                    .with_track(TrackSpec::new(Box::new(ConstNode(0.25)))),
            )
            .unwrap();
        slot.poll_swap();

        let mut ctx = ProcessCtx::new(&mut buffers, SR, 64, TransportSnapshot::default());
        slot.process(&mut ctx);
        let [left, right] = buffers.as_slices();

        let hard_left = pan_gains(-1.0)[0];
        assert!(
            (left[0] - (0.5 + 0.25 * hard_left)).abs() < 1e-6,
            "{}",
            left[0]
        );
        assert!(
            (right[0] - 0.5).abs() < 1e-6,
            "track 1 is hard left: {}",
            right[0]
        );

        // Strips start at their target, so nothing ramps on the first block.
        let peaks = slot.track_peaks();
        assert!((peaks.get(0).peak_l - 0.5).abs() < 1e-6);
        assert!((peaks.get(1).peak_r).abs() < 1e-9, "no right-channel leak");
    }

    #[test]
    fn a_muted_track_contributes_nothing_once_its_ramp_has_settled() {
        let collector = Collector::new();
        let (mut handle, mut slot) = graph_channel(2, collector.handle());
        let mut buffers = AudioBuffers::new(1024);
        let params = Arc::new(MixerParams::new());
        params.set_track_muted(1, true);

        handle
            .install(
                GraphSpec::new()
                    .with_limits(limits(1024))
                    .with_params(Arc::clone(&params))
                    .with_track(TrackSpec::new(Box::new(ConstNode(0.25))))
                    .with_track(TrackSpec::new(Box::new(ConstNode(0.5)))),
            )
            .unwrap();
        slot.poll_swap();
        // Muted before the graph was built, so the strip starts at zero.
        assert_eq!(render(&mut slot, &mut buffers, 1024), 0.25);
        assert_eq!(slot.track_peaks().get(1), TrackPeak::SILENT);

        // Unmuting ramps up rather than stepping.
        params.set_track_muted(1, false);
        let mut ctx = ProcessCtx::new(&mut buffers, SR, 1024, TransportSnapshot::default());
        slot.process(&mut ctx);
        let left = buffers.as_slices()[0];
        assert!(left[0] < 0.26, "the unmute must not step: {}", left[0]);
        assert!((left[1023] - 0.75).abs() < 1e-6, "and it must arrive");
    }

    #[test]
    fn the_master_fader_scales_the_sum_after_every_track() {
        let collector = Collector::new();
        let (mut handle, mut slot) = graph_channel(2, collector.handle());
        let mut buffers = AudioBuffers::new(1024);
        let params = Arc::new(MixerParams::new());
        params.set_master_gain(0.5);

        handle
            .install(
                GraphSpec::new()
                    .with_limits(limits(1024))
                    .with_params(Arc::clone(&params))
                    .with_track(TrackSpec::new(Box::new(ConstNode(1.0))))
                    .with_track(TrackSpec::new(Box::new(ConstNode(1.0)))),
            )
            .unwrap();
        slot.poll_swap();
        assert_eq!(render(&mut slot, &mut buffers, 1024), 1.0, "2.0 * 0.5");

        // The per-track meters are pre-master, which is what a channel strip
        // shows: the master fader must not move them.
        assert_eq!(slot.track_peaks().get(0).peak_l, 1.0);
    }

    #[test]
    fn a_master_insert_runs_on_the_sum_before_the_fader() {
        let collector = Collector::new();
        let (mut handle, mut slot) = graph_channel(2, collector.handle());
        let mut buffers = AudioBuffers::new(16);

        handle
            .install(
                GraphSpec::new()
                    .with_limits(limits(16))
                    .with_track(TrackSpec::new(Box::new(ConstNode(0.25))))
                    .with_track(TrackSpec::new(Box::new(ConstNode(0.25))))
                    .with_master_insert(Box::new(AddNode(1.0))),
            )
            .unwrap();
        slot.poll_swap();
        assert_eq!(slot.node_count(), 3, "two sources and the master insert");
        assert_eq!(render(&mut slot, &mut buffers, 16), 1.5);
    }

    #[test]
    fn each_track_only_sees_the_events_addressed_to_it() {
        let collector = Collector::new();
        let (mut handle, mut slot) = graph_channel(2, collector.handle());
        let mut buffers = AudioBuffers::new(16);

        handle
            .install(
                GraphSpec::new()
                    .with_limits(limits(16))
                    .with_track(TrackSpec::new(Box::new(NoteGatedNode::new(0.25))))
                    .with_track(TrackSpec::new(Box::new(NoteGatedNode::new(0.5)))),
            )
            .unwrap();
        slot.poll_swap();

        // One note on channel 1: only track 1 may open.
        let events = [NoteEvent::note_on(0, 60, 1.0).on_channel(1)];
        let mut ctx =
            ProcessCtx::with_events(&mut buffers, SR, 16, TransportSnapshot::default(), &events);
        slot.process(&mut ctx);
        assert_eq!(buffers.as_slices()[0][0], 0.5, "track 1 alone");

        let peaks = slot.track_peaks();
        assert_eq!(peaks.get(0), TrackPeak::SILENT);
        assert_eq!(peaks.get(1).peak_l, 0.5);
    }

    #[test]
    fn a_tracks_channel_defaults_to_its_index_and_can_be_overridden() {
        let collector = Collector::new();
        let (mut handle, mut slot) = graph_channel(2, collector.handle());
        handle
            .install(
                GraphSpec::new()
                    .with_track(TrackSpec::new(Box::new(ConstNode(0.0))))
                    .with_track(TrackSpec::new(Box::new(ConstNode(0.0))).on_channel(9)),
            )
            .unwrap();
        slot.poll_swap();
        let graph_channels: Vec<Option<u8>> = (0..3)
            .map(|i| slot.current.as_ref().unwrap().track_channel(i))
            .collect();
        assert_eq!(graph_channels, vec![Some(0), Some(9), None]);
    }

    #[test]
    fn a_per_track_event_buffer_drops_rather_than_growing() {
        let collector = Collector::new();
        let (mut handle, mut slot) = graph_channel(2, collector.handle());
        let mut buffers = AudioBuffers::new(8);

        handle
            .install(
                GraphSpec::new()
                    .with_limits(GraphLimits {
                        sample_rate: SR,
                        max_block: 8,
                        max_events: 2,
                    })
                    .with_track(TrackSpec::new(Box::new(NoteGatedNode::new(1.0)))),
            )
            .unwrap();
        slot.poll_swap();

        let events: Vec<NoteEvent> = (60..66).map(|k| NoteEvent::note_on(0, k, 1.0)).collect();
        let mut ctx =
            ProcessCtx::with_events(&mut buffers, SR, 8, TransportSnapshot::default(), &events);
        slot.process(&mut ctx);
        // The node saw only what fitted, and nothing reallocated.
        assert_eq!(buffers.as_slices()[0][0], 1.0);
    }

    #[test]
    fn tracks_past_the_fixed_limit_are_dropped_at_compile_time() {
        let mut spec = GraphSpec::new().with_limits(limits(8));
        for _ in 0..MAX_TRACKS + 4 {
            spec = spec.with_track(TrackSpec::new(Box::new(ConstNode(0.1))));
        }
        let graph = spec.compile();
        assert_eq!(graph.track_count(), MAX_TRACKS);
    }

    #[test]
    fn parameters_survive_a_rebuild_and_the_new_graph_does_not_fade_in() {
        let collector = Collector::new();
        let (mut handle, mut slot) = graph_channel(4, collector.handle());
        let mut buffers = AudioBuffers::new(64);
        let params = Arc::new(MixerParams::new());
        params.set_track_gain(0, 0.5);

        let build = |params: &Arc<MixerParams>, extra: bool| {
            let mut spec = GraphSpec::new()
                .with_limits(limits(64))
                .with_params(Arc::clone(params))
                .with_track(TrackSpec::new(Box::new(ConstNode(1.0))));
            if extra {
                spec = spec.with_track(TrackSpec::new(Box::new(ConstNode(0.25))));
            }
            spec
        };

        handle.install(build(&params, false)).unwrap();
        slot.poll_swap();
        assert_eq!(render(&mut slot, &mut buffers, 64), 0.5);

        // Add a track mid-play: the first sample of the new graph is already at
        // the settled level, so there is no dropout at the swap.
        handle.install(build(&params, true)).unwrap();
        slot.poll_swap();
        assert_eq!(render(&mut slot, &mut buffers, 64), 0.75);
        assert_eq!(slot.track_count(), 2);
    }

    /// dBFS of a linear amplitude, floored so `0.0` is a number.
    fn dbfs(amplitude: f32) -> f32 {
        20.0 * amplitude.max(1e-12).log10()
    }

    /// The zipper test, at graph level: a mute arriving mid-play must reach
    /// silence without a single step the ear could hear as a click.
    #[test]
    fn a_mute_applied_mid_play_is_a_fade_and_never_a_step() {
        const BLOCK: usize = 256;
        let collector = Collector::new();
        let (mut handle, mut slot) = graph_channel(2, collector.handle());
        let mut buffers = AudioBuffers::new(BLOCK);
        let params = Arc::new(MixerParams::new());

        handle
            .install(
                GraphSpec::new()
                    .with_limits(limits(BLOCK))
                    .with_params(Arc::clone(&params))
                    // DC, so the output *is* the strip's coefficient and any
                    // discontinuity in it is a discontinuity in the samples.
                    .with_track(TrackSpec::new(Box::new(ConstNode(1.0)))),
            )
            .unwrap();
        slot.poll_swap();

        let mut rendered: Vec<f32> = Vec::new();
        for block in 0..8 {
            if block == 2 {
                params.set_track_muted(0, true);
            }
            let mut ctx = ProcessCtx::new(&mut buffers, SR, BLOCK, TransportSnapshot::default());
            slot.process(&mut ctx);
            rendered.extend_from_slice(buffers.as_slices()[0]);
        }

        // 10 ms at 48 kHz is 480 frames, so one full-scale mute can move at
        // most 1/480 per sample -- block boundaries included.
        let limit = 1.0 / 480.0 + 1e-6;
        let worst = rendered
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0_f32, f32::max);
        assert!(
            worst <= limit,
            "the mute stepped by {worst} (limit {limit})"
        );

        assert_eq!(&rendered[..BLOCK * 2], &vec![1.0_f32; BLOCK * 2][..]);
        let tail = &rendered[BLOCK * 4..];
        assert!(
            tail.iter().all(|s| *s == 0.0),
            "the mute must actually arrive, not asymptote"
        );

        // ...and un-muting comes back the same way.
        params.set_track_muted(0, false);
        let mut ctx = ProcessCtx::new(&mut buffers, SR, BLOCK, TransportSnapshot::default());
        slot.process(&mut ctx);
        let up = buffers.as_slices()[0];
        assert!(up[0] > 0.0 && up[0] <= limit, "unmute stepped: {}", up[0]);
    }

    #[test]
    fn a_hard_panned_track_leaves_the_opposite_channel_below_minus_eighty_dbfs() {
        const BLOCK: usize = 1024;
        let collector = Collector::new();
        let (mut handle, mut slot) = graph_channel(2, collector.handle());
        let mut buffers = AudioBuffers::new(BLOCK);
        let params = Arc::new(MixerParams::new());

        handle
            .install(
                GraphSpec::new()
                    .with_limits(limits(BLOCK))
                    .with_params(Arc::clone(&params))
                    .with_track(TrackSpec::new(Box::new(ConstNode(1.0)))),
            )
            .unwrap();
        slot.poll_swap();

        for (pan, quiet, loud) in [(-1.0_f32, 1_usize, 0_usize), (1.0, 0, 1)] {
            params.set_track_pan(0, pan);
            // Two blocks: the first covers the 480-frame ramp, the second is
            // the settled state the assertion is about.
            for _ in 0..2 {
                let mut ctx =
                    ProcessCtx::new(&mut buffers, SR, BLOCK, TransportSnapshot::default());
                slot.process(&mut ctx);
            }
            let channels = buffers.as_slices();
            let residual = channels[quiet]
                .iter()
                .fold(0.0_f32, |peak, s| peak.max(s.abs()));
            assert!(
                dbfs(residual) <= -80.0,
                "pan {pan}: {residual} left in the quiet channel ({} dBFS)",
                dbfs(residual)
            );
            let kept = channels[loud]
                .iter()
                .fold(0.0_f32, |peak, s| peak.max(s.abs()));
            assert!(
                (kept - std::f32::consts::SQRT_2).abs() < 1e-6,
                "the loud channel should carry the whole signal, got {kept}"
            );
        }
    }

    #[test]
    fn two_identical_runs_of_the_same_graph_are_bit_identical() {
        // Determinism is what makes every spectral assertion elsewhere a number
        // rather than a tolerance. Nothing in the mixer may introduce state that
        // survives between runs of the same construction.
        const BLOCK: usize = 128;
        let render_once = || {
            let collector = Collector::new();
            let (mut handle, mut slot) = graph_channel(2, collector.handle());
            let params = Arc::new(MixerParams::new());
            params.set_track_gain(0, 0.3);
            params.set_track_pan(1, 0.6);
            handle
                .install(
                    GraphSpec::new()
                        .with_limits(limits(BLOCK))
                        .with_params(Arc::clone(&params))
                        .with_track(TrackSpec::new(Box::new(ConstNode(0.7))))
                        .with_track(TrackSpec::new(Box::new(AddNode(0.11)))),
                )
                .unwrap();
            slot.poll_swap();

            let mut buffers = AudioBuffers::new(BLOCK);
            let mut out = Vec::new();
            for block in 0..6 {
                if block == 3 {
                    params.set_track_gain(0, 0.9);
                }
                let mut ctx =
                    ProcessCtx::new(&mut buffers, SR, BLOCK, TransportSnapshot::default());
                slot.process(&mut ctx);
                let [left, right] = buffers.as_slices();
                out.extend_from_slice(left);
                out.extend_from_slice(right);
            }
            out
        };
        assert_eq!(render_once(), render_once());
    }

    #[test]
    fn a_rebuild_mid_play_leaves_the_untouched_track_continuous() {
        // Adding a track compiles and installs a whole new graph. The tracks
        // that did not change must not notice: no gap, no fade, no click.
        const BLOCK: usize = 64;
        let collector = Collector::new();
        let (mut handle, mut slot) = graph_channel(4, collector.handle());
        let mut buffers = AudioBuffers::new(BLOCK);
        let params = Arc::new(MixerParams::new());
        params.set_track_gain(0, 0.5);

        let spec = |tracks: usize| {
            let mut spec = GraphSpec::new()
                .with_limits(limits(BLOCK))
                .with_params(Arc::clone(&params))
                .with_track(TrackSpec::new(Box::new(ConstNode(1.0))));
            for _ in 1..tracks {
                spec = spec.with_track(TrackSpec::new(Box::new(ConstNode(0.0))));
            }
            spec
        };

        handle.install(spec(1)).unwrap();
        slot.poll_swap();
        let mut rendered = Vec::new();
        for block in 0..8 {
            if block == 4 {
                handle.install(spec(4)).unwrap();
            }
            slot.poll_swap();
            let mut ctx = ProcessCtx::new(&mut buffers, SR, BLOCK, TransportSnapshot::default());
            slot.process(&mut ctx);
            rendered.extend_from_slice(buffers.as_slices()[0]);
        }

        assert_eq!(slot.track_count(), 4);
        assert!(
            rendered.iter().all(|s| *s == 0.5),
            "the surviving track's level must be continuous across the swap"
        );
    }

    #[test]
    fn limits_are_pinned_by_the_builder_or_filled_in_by_the_caller() {
        let mut spec = GraphSpec::new();
        assert_eq!(spec.limits(), GraphLimits::default());
        assert!(spec.apply_default_limits(limits(128)));
        assert_eq!(spec.limits().max_block, 128);

        let mut pinned = GraphSpec::new().with_limits(limits(32));
        assert!(
            !pinned.apply_default_limits(limits(4096)),
            "a builder that said what it wanted must not be overridden"
        );
        assert_eq!(pinned.limits().max_block, 32);
    }

    #[test]
    fn burst_of_installs_collapses_to_the_newest_graph() {
        let collector = Collector::new();
        let (mut handle, mut slot) = graph_channel(8, collector.handle());
        let mut buffers = AudioBuffers::new(8);
        for v in [0.1_f32, 0.2, 0.3] {
            handle
                .install(GraphSpec::new().with(Box::new(ConstNode(v))))
                .unwrap();
        }
        assert!(slot.poll_swap());
        assert_eq!(render(&mut slot, &mut buffers, 8), 0.3);
    }

    #[test]
    fn full_queue_is_reported_not_silently_dropped() {
        let collector = Collector::new();
        let (mut handle, _slot) = graph_channel(1, collector.handle());
        handle.install(GraphSpec::new()).unwrap();
        let err = handle.install(GraphSpec::new()).unwrap_err();
        assert!(matches!(err, GraphInstallError::QueueFull), "got {err:?}");
    }

    /// The point of `basedrop`: dropping a graph on the "audio thread" must not
    /// free anything -- the allocation stays alive until the collector runs.
    #[test]
    fn superseded_graph_is_freed_by_the_collector_not_the_rt_thread() {
        let mut collector = Collector::new();
        let (mut handle, mut slot) = graph_channel(4, collector.handle());

        handle
            .install(GraphSpec::new().with(Box::new(ConstNode(1.0))))
            .unwrap();
        handle
            .install(GraphSpec::new().with(Box::new(ConstNode(2.0))))
            .unwrap();
        assert_eq!(collector.alloc_count(), 2, "two graphs allocated");

        // Audio thread picks up both; the first is dropped -> handed to collector.
        slot.poll_swap();
        assert_eq!(
            collector.alloc_count(),
            2,
            "dropping on the RT thread must NOT deallocate"
        );

        collector.collect();
        assert_eq!(collector.alloc_count(), 1, "collector freed the old graph");

        // Dropping the RT slot releases the last graph, freed on the next collect.
        drop(slot);
        collector.collect();
        assert_eq!(collector.alloc_count(), 0);
    }
}
