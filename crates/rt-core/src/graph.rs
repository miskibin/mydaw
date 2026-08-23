//! Audio graph: built off-thread, handed to the audio thread lock-free.
//!
//! # Shape
//! From step 5 the graph is a mixer, not a chain; from the routing step it is a
//! mixer with buses:
//!
//! ```text
//!   track 0:  source -> insert.. -> [strip: gain, pan, mute] --\
//!   track 1:  source -> insert.. -> [strip: gain, pan, mute] ---+--> sum
//!   ...                    |  \                                /      |
//!   track n:  source -> insert.. -> [strip: gain, pan, mute] --/       |
//!                          |   \                                      |
//!                    (pre tap)  (post tap)                            |
//!                          v     v                                    |
//!                     bus b:  Σ -> insert.. -> [strip] ---------------/|
//!                                     |   \                           |
//!                               (pre tap)  (post tap) -> bus c (later) |
//!                                                                      v
//!                                                        master insert..
//!                                                                      |
//!                                                        [master strip: gain]
//! ```
//!
//! Each track renders into its **own** pre-allocated stereo scratch buffer, and
//! its strip scales that buffer into the shared bus. `ctx.buffers` -- the bus the
//! audio callback hands in -- is the *master*: it is cleared at the top of the
//! block, summed into, and is what the device, the meters and the debug tap see.
//!
//! # Routing: one main out, any number of taps
//! Every strip -- track or bus -- has **one implicit main output, into the
//! master**, and any number of additional *send taps* into buses. There is no
//! command to reroute a main out, and that is the design rather than an
//! omission: a mixer where every strip's main path is settled is one where the
//! signal flow can be read off the strip list, and the thing users actually want
//! (a reverb, a parallel compressor, a drum group) is a *tap*, not a reroute.
//! "Wet only" is a pre-fader tap with the fader down, which is exactly how it is
//! done on a desk.
//!
//! **Pre-fader means after the inserts and before the strip** -- so a pre-fader
//! tap is unaffected by the source's fader, pan *and* mute, and a track pulled to
//! silence still feeds its reverb. **Post-fader means after the strip**, so the
//! tap follows the fader, which is what keeps a reverb in proportion while a
//! fader is ridden.
//!
//! Both taps for one send are compiled into the graph, and the pre/post flag
//! chooses which of the two carries the level -- see
//! [`MixerParams::send_targets`]. Flipping the flag therefore ramps one tap down
//! while the other ramps up rather than switching a route on a block boundary,
//! which is the difference between a routing change and a click.
//!
//! # Bus order, and why a cycle cannot reach the audio thread
//! A bus's inputs must all have been computed before it runs, so the buses are
//! processed in a **topological order** of the bus-to-bus send edges, computed
//! once at compile time by [`bus_order`]. A cycle has no such order; the shell
//! refuses to create one (see the routing spec), and if one arrives anyway the
//! compiler breaks it by **dropping the edges that would run backwards** rather
//! than by producing a graph whose output depends on buffer staleness. The audio
//! thread never sees a cycle and never has to check for one.
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

use crate::mixer::{
    BusPeaks, MasterStrip, MixerParams, StripId, TrackPeak, TrackPeaks, TrackStrip, MAX_BUSES,
    MAX_TRACKS,
};
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

/// One bus, as a non-RT thread describes it.
///
/// A bus has no source: its input is whatever sends into it. Everything else
/// about it -- the insert chain, the strip, the main out into the master -- is a
/// track's, minus the generator.
#[derive(Default)]
pub struct BusSpec {
    /// Effects, in order, on the summed input.
    pub inserts: Vec<Box<dyn RtNode>>,
}

impl BusSpec {
    /// A bus with an empty chain.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder-style insert append.
    #[must_use]
    pub fn with_insert(mut self, node: Box<dyn RtNode>) -> Self {
        self.inserts.push(node);
        self
    }

    /// Nodes in this bus.
    pub fn node_count(&self) -> usize {
        self.inserts.len()
    }
}

/// One send tap, as a non-RT thread describes it.
///
/// Structure only: *that* a tap exists, and between which two strips. Its level
/// and its pre/post flag live in [`MixerParams`] and are read per block, which
/// is what makes moving a send knob free and adding one a rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendSpec {
    /// Where the signal is taken from.
    pub source: StripId,
    /// Which bus it is added into.
    pub dest: usize,
}

impl SendSpec {
    pub const fn new(source: StripId, dest: usize) -> Self {
        Self { source, dest }
    }
}

/// A processing order for `bus_count` buses that respects every bus-to-bus send.
///
/// Kahn's algorithm over the edges `send.source == Bus(a) -> send.dest == b`.
/// Buses caught in a cycle -- which the shell refuses to create -- are appended
/// in index order after everything that *can* be ordered, so the answer is
/// always a permutation of `0..bus_count` and is always deterministic.
///
/// The second half of the answer is what the caller must then do: a send whose
/// destination is **not later** in this order has to be dropped, because running
/// it would add into a buffer that has already been consumed. See
/// [`GraphSpec::compile`].
pub fn bus_order(bus_count: usize, sends: &[SendSpec]) -> Vec<usize> {
    let count = bus_count.min(MAX_BUSES);
    let mut indegree = vec![0_usize; count];
    // Edges, deduplicated by construction: a pair may only appear once, and a
    // self-edge is not an ordering constraint (it is a cycle of one, dropped).
    let mut edges: Vec<(usize, usize)> = Vec::new();
    for send in sends {
        let StripId::Bus(from) = send.source else {
            continue;
        };
        if from >= count || send.dest >= count || from == send.dest {
            continue;
        }
        if edges.contains(&(from, send.dest)) {
            continue;
        }
        edges.push((from, send.dest));
        indegree[send.dest] += 1;
    }

    let mut order: Vec<usize> = Vec::with_capacity(count);
    let mut ready: Vec<usize> = (0..count).filter(|bus| indegree[*bus] == 0).collect();
    // Popped from the front so ties break by index, which makes the order stable
    // across rebuilds and therefore makes a compiled graph reproducible.
    let mut cursor = 0;
    while cursor < ready.len() {
        let bus = ready[cursor];
        cursor += 1;
        order.push(bus);
        for (from, to) in &edges {
            if *from != bus {
                continue;
            }
            indegree[*to] -= 1;
            if indegree[*to] == 0 {
                ready.push(*to);
            }
        }
    }
    // Whatever is left is in a cycle. Appending it keeps the answer total.
    for bus in 0..count {
        if !order.contains(&bus) {
            order.push(bus);
        }
    }
    order
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
    buses: Vec<BusSpec>,
    sends: Vec<SendSpec>,
    master_inserts: Vec<Box<dyn RtNode>>,
    params: Option<Arc<MixerParams>>,
    sidechain: Option<Arc<crate::SidechainBus>>,
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

    /// Add a bus. Its index is its position, and that is what a
    /// [`SendSpec`] names.
    pub fn push_bus(&mut self, bus: BusSpec) -> &mut Self {
        self.buses.push(bus);
        self
    }

    /// Builder-style [`push_bus`](Self::push_bus).
    #[must_use]
    pub fn with_bus(mut self, bus: BusSpec) -> Self {
        self.buses.push(bus);
        self
    }

    /// Add a send tap.
    ///
    /// Structure only: the level and the pre/post flag are read from
    /// [`MixerParams`] every block, so nothing here has to be rebuilt when
    /// either moves. A tap naming a strip or a bus this spec does not have is
    /// dropped at [`compile`](Self::compile) time rather than refused, because
    /// the spec is assembled from a project that has already been validated and
    /// a mismatch here is a wiring bug, not a user error.
    pub fn push_send(&mut self, send: SendSpec) -> &mut Self {
        self.sends.push(send);
        self
    }

    /// Builder-style [`push_send`](Self::push_send).
    #[must_use]
    pub fn with_send(mut self, send: SendSpec) -> Self {
        self.sends.push(send);
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

    /// Publish every strip's post-insert signal into `bus`, for the
    /// compressors that key off one.
    ///
    /// Shared by `Arc` for the same reason [`with_params`](Self::with_params)
    /// is: the compressors hold the same bus, and a rebuild must not drop the
    /// subscription between them. Left unset -- which is every offline render
    /// and every test that is not about sidechain -- nothing is published and a
    /// compressor keyed off a strip simply hears silence.
    #[must_use]
    pub fn with_sidechain(mut self, bus: Arc<crate::SidechainBus>) -> Self {
        self.sidechain = Some(bus);
        self
    }

    /// The key bus this spec will compile with, if it was given one.
    pub fn sidechain(&self) -> Option<&Arc<crate::SidechainBus>> {
        self.sidechain.as_ref()
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

    /// Total nodes across every track, every bus and the master bus.
    pub fn len(&self) -> usize {
        self.tracks.iter().map(TrackSpec::node_count).sum::<usize>()
            + self.buses.iter().map(BusSpec::node_count).sum::<usize>()
            + self.master_inserts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Tracks in this spec.
    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// Buses in this spec.
    pub fn bus_count(&self) -> usize {
        self.buses.len()
    }

    /// Send taps in this spec, before compilation drops any.
    pub fn send_count(&self) -> usize {
        self.sends.len()
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
        self.buses.truncate(MAX_BUSES);

        let track_count = self.tracks.len();
        let bus_count = self.buses.len();
        let order = bus_order(bus_count, &self.sends);
        // Where each bus sits in the processing order, so a send can be checked
        // for "does my destination run after me" in constant time.
        let mut position = [0_usize; MAX_BUSES];
        for (at, bus) in order.iter().enumerate() {
            position[*bus] = at;
        }

        // Keep only the taps this graph can actually run: a source that exists,
        // a destination that exists, and -- for a bus-to-bus tap -- a
        // destination that has not been consumed by the time the source runs.
        let keep = |send: &SendSpec| -> bool {
            if send.dest >= bus_count {
                return false;
            }
            match send.source {
                StripId::Track(index) => index < track_count,
                StripId::Bus(index) => index < bus_count && position[send.dest] > position[index],
            }
        };
        let sends: Vec<SendSpec> = self.sends.iter().copied().filter(keep).collect();

        let taps_for = |source: StripId| -> Vec<SendTap> {
            sends
                .iter()
                .filter(|send| send.source == source)
                .map(|send| SendTap::new(send.dest, source, &params, &limits))
                .collect()
        };

        let tracks: Vec<TrackChain> = self
            .tracks
            .into_iter()
            .enumerate()
            .map(|(index, spec)| {
                TrackChain::new(
                    index,
                    spec,
                    taps_for(StripId::Track(index)),
                    &params,
                    &limits,
                )
            })
            .collect();

        let buses: Vec<BusChain> = self
            .buses
            .into_iter()
            .enumerate()
            .map(|(index, spec)| {
                BusChain::new(index, spec, taps_for(StripId::Bus(index)), &params, &limits)
            })
            .collect();

        let master = MasterStrip::new(limits.sample_rate, params.master_gain());
        CompiledGraph {
            peaks: TrackPeaks::empty(tracks.len()),
            bus_peaks: BusPeaks::empty(buses.len()),
            tracks,
            buses,
            bus_order: order,
            // One shared post-fader scratch, reused by every strip in turn: a
            // strip's post-fader signal is produced, handed to the master and to
            // its taps, and finished with before the next strip starts.
            post: AudioBuffers::new(limits.max_block.max(1)),
            master_inserts: self.master_inserts,
            master,
            params,
            sidechain: self.sidechain,
        }
    }
}

/// One send tap as the audio thread runs it.
///
/// Two strips rather than one, because the pre/post flag is a **crossfade**:
/// [`MixerParams::send_targets`] hands the level to exactly one of them and zero
/// to the other, and both ramp. A flag flipped mid-play therefore fades between
/// two taps of the same signal rather than switching a route on a block
/// boundary.
///
/// The strips are `TrackStrip`s with the same coefficient in both channels, so a
/// send never moves the image of what it is tapping.
struct SendTap {
    dest: usize,
    source: StripId,
    pre: TrackStrip,
    post: TrackStrip,
}

impl SendTap {
    fn new(dest: usize, source: StripId, params: &MixerParams, limits: &GraphLimits) -> Self {
        let (pre, post) = params.send_targets(source, dest);
        Self {
            dest,
            source,
            // Built already sitting at their targets, for the same reason a
            // strip is: a rebuild must not fade a reverb send in.
            pre: TrackStrip::new(limits.sample_rate, [pre, pre]),
            post: TrackStrip::new(limits.sample_rate, [post, post]),
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
    /// This track's send taps, in destination order.
    sends: Vec<SendTap>,
}

impl TrackChain {
    fn new(
        index: usize,
        spec: TrackSpec,
        sends: Vec<SendTap>,
        params: &MixerParams,
        limits: &GraphLimits,
    ) -> Self {
        Self {
            source: spec.source,
            inserts: spec.inserts,
            channel: spec.channel.unwrap_or(index as u8),
            strip: TrackStrip::new(limits.sample_rate, params.track_target(index)),
            scratch: AudioBuffers::new(limits.max_block.max(1)),
            events: Vec::with_capacity(limits.max_events),
            sends,
        }
    }

    fn node_count(&self) -> usize {
        1 + self.inserts.len()
    }
}

/// One bus as the audio thread runs it.
///
/// A track chain without the source and without the note filtering: its input
/// arrives in [`input`](Self::input) from whatever tapped into it earlier in the
/// block.
struct BusChain {
    inserts: Vec<Box<dyn RtNode>>,
    strip: TrackStrip,
    /// Where this bus's inputs accumulate, and where its inserts run. Cleared at
    /// the top of every block, for the same reason the master bus is: it is a
    /// sum, and last block's is not part of this one.
    input: AudioBuffers,
    sends: Vec<SendTap>,
}

impl BusChain {
    fn new(
        index: usize,
        spec: BusSpec,
        sends: Vec<SendTap>,
        params: &MixerParams,
        limits: &GraphLimits,
    ) -> Self {
        Self {
            inserts: spec.inserts,
            strip: TrackStrip::new(limits.sample_rate, params.bus_target(index)),
            input: AudioBuffers::new(limits.max_block.max(1)),
            sends,
        }
    }

    fn node_count(&self) -> usize {
        self.inserts.len()
    }
}

/// Add `src` into `dst`, frame for frame over the shorter of the two.
///
/// The second half of a strip that has taps to feed: the post-fader signal is
/// produced into a scratch buffer once and this is what carries it to the master
/// sum. RT-safe -- arithmetic over pre-allocated slices.
fn add_into(src: &AudioBuffers, dst: &mut AudioBuffers) {
    let source = src.as_slices();
    let mut destination = dst.as_mut_slices();
    for channel in 0..crate::CHANNELS {
        let input = source[channel];
        let output = &mut destination[channel];
        let frames = input.len().min(output.len());
        for frame in 0..frames {
            output[frame] += input[frame];
        }
    }
}

/// Two distinct elements of a slice, both mutable.
///
/// The buses live in one `Vec` and a bus-to-bus tap needs to read one and write
/// another, which is exactly the borrow `split_at_mut` exists for. `a == b` is
/// unreachable: a self-send is dropped at compile time (see
/// [`GraphSpec::compile`]), because a bus cannot run after itself.
fn pair_mut<T>(items: &mut [T], a: usize, b: usize) -> (&mut T, &mut T) {
    debug_assert_ne!(a, b, "a strip cannot send to itself");
    if a < b {
        let (left, right) = items.split_at_mut(b);
        (&mut left[a], &mut right[0])
    } else {
        let (left, right) = items.split_at_mut(a);
        (&mut right[0], &mut left[b])
    }
}

/// An executable graph. Owned by the audio thread while it is current.
pub struct CompiledGraph {
    tracks: Vec<TrackChain>,
    buses: Vec<BusChain>,
    /// The order the buses are processed in: a topological sort of the
    /// bus-to-bus send edges, fixed at compile time. See [`bus_order`].
    bus_order: Vec<usize>,
    /// Scratch for one strip's post-fader signal, reused strip by strip. Only
    /// touched by a strip that has at least one send tap.
    post: AudioBuffers,
    master_inserts: Vec<Box<dyn RtNode>>,
    master: MasterStrip,
    params: Arc<MixerParams>,
    /// Where each strip's post-insert, pre-fader block is published for the
    /// compressors keyed off it. See [`crate::SidechainBus`].
    sidechain: Option<Arc<crate::SidechainBus>>,
    /// Last block's post-strip peak per track, for the meter path.
    peaks: TrackPeaks,
    /// Last block's post-strip peak per bus.
    bus_peaks: BusPeaks,
}

impl CompiledGraph {
    /// Render one block: every track into its own buffer, then the sum.
    ///
    /// RT-safe as long as each node honours the zero-alloc rule; this function
    /// itself only iterates pre-allocated `Vec`s and writes pre-allocated
    /// buffers.
    /// Render one block without an analysis tap.
    pub fn process(&mut self, ctx: &mut ProcessCtx) {
        self.process_with_spectrum(ctx, None);
    }

    /// Render one block, optionally copying a hop around the watched insert.
    pub fn process_with_spectrum(
        &mut self,
        ctx: &mut ProcessCtx,
        mut tap: Option<&mut crate::SpectrumTapRt>,
    ) {
        // Destructured so the parameter block can be read while the tracks are
        // borrowed mutably -- the alternative is an `Arc::clone` per block.
        let Self {
            tracks,
            buses,
            bus_order,
            post,
            master_inserts,
            master,
            params,
            sidechain,
            peaks,
            bus_peaks,
        } = self;

        // Flip the key bus's banks before anything renders, so every reader in
        // this block sees exactly the previous block's signal whatever order the
        // strips run in. See [`crate::SidechainBus`].
        if let Some(keys) = sidechain.as_deref() {
            keys.begin_block();
        }

        // The bus starts silent: it is a *sum*, and whatever the previous block
        // left in it is not part of this one.
        ctx.buffers.silence();
        post.set_frames(ctx.frames);
        *peaks = TrackPeaks::empty(tracks.len());
        *bus_peaks = BusPeaks::empty(buses.len());
        for bus in buses.iter_mut() {
            bus.input.set_frames(ctx.frames);
            bus.input.silence();
        }

        for (index, track) in tracks.iter_mut().enumerate() {
            let TrackChain {
                source,
                inserts,
                channel,
                strip,
                scratch,
                events,
                sends,
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
                if tap
                    .as_ref()
                    .is_some_and(|tap| tap.watching_track_source(index))
                {
                    if let Some(tap) = tap.as_mut() {
                        tap.capture_pre(track_ctx.buffers);
                        tap.capture_post(track_ctx.buffers);
                    }
                }
                crate::process_inserts(
                    inserts,
                    &mut track_ctx,
                    tap.as_deref_mut(),
                    crate::SpectrumLoc::Track(index),
                );
                track_ctx.frames
            };
            debug_assert!(frames <= ctx.frames);

            // The key tap: post-insert, pre-fader, for the reasons in
            // [`crate::SidechainBus`]. A no-op unless a compressor asked for
            // this strip.
            if let Some(keys) = sidechain.as_deref() {
                keys.write(index, scratch);
            }

            // Pre-fader taps read the chain's output *before* the strip, so a
            // track pulled to silence still feeds its reverb.
            for tap in sends.iter_mut() {
                let (pre, _) = params.send_targets(tap.source, tap.dest);
                tap.pre
                    .mix_into(scratch, &mut buses[tap.dest].input, [pre, pre]);
            }

            let peak = if sends.is_empty() {
                // The overwhelmingly common shape, and the one every mixer test
                // before routing existed measures: multiply and accumulate in
                // one pass, straight into the master sum.
                strip.mix_into(scratch, ctx.buffers, params.track_target(index))
            } else {
                // With taps to feed, the post-fader signal has more than one
                // consumer, so it is produced once into the shared scratch and
                // handed to each. Bit-identical to the branch above.
                let peak = strip.apply_into(scratch, post, params.track_target(index));
                add_into(post, ctx.buffers);
                for tap in sends.iter_mut() {
                    let (_, level) = params.send_targets(tap.source, tap.dest);
                    tap.post
                        .mix_into(post, &mut buses[tap.dest].input, [level, level]);
                }
                peak
            };
            peaks.set(
                index,
                TrackPeak {
                    peak_l: peak[0],
                    peak_r: peak[1],
                },
            );
        }

        // Buses, in an order that guarantees every input is already summed.
        for &index in bus_order.iter() {
            {
                let BusChain { input, inserts, .. } = &mut buses[index];
                let mut bus_ctx =
                    ProcessCtx::with_events(input, ctx.sample_rate, ctx.frames, ctx.transport, &[]);
                crate::process_inserts(
                    inserts,
                    &mut bus_ctx,
                    tap.as_deref_mut(),
                    crate::SpectrumLoc::Bus(index),
                );
            }

            if let Some(keys) = sidechain.as_deref() {
                if let Some(slot) = crate::key_slot(StripId::Bus(index)) {
                    keys.write(slot, &buses[index].input);
                }
            }

            let tap_count = buses[index].sends.len();
            for tap in 0..tap_count {
                let dest = buses[index].sends[tap].dest;
                let source = buses[index].sends[tap].source;
                let (pre, _) = params.send_targets(source, dest);
                let (from, to) = pair_mut(buses, index, dest);
                let BusChain { input, sends, .. } = from;
                sends[tap].pre.mix_into(input, &mut to.input, [pre, pre]);
            }

            let target = params.bus_target(index);
            let peak = if tap_count == 0 {
                let BusChain { input, strip, .. } = &mut buses[index];
                strip.mix_into(input, ctx.buffers, target)
            } else {
                let peak = {
                    let BusChain { input, strip, .. } = &mut buses[index];
                    strip.apply_into(input, post, target)
                };
                add_into(post, ctx.buffers);
                for tap in 0..tap_count {
                    let dest = buses[index].sends[tap].dest;
                    let source = buses[index].sends[tap].source;
                    let (_, level) = params.send_targets(source, dest);
                    let (from, to) = pair_mut(buses, index, dest);
                    from.sends[tap]
                        .post
                        .mix_into(post, &mut to.input, [level, level]);
                }
                peak
            };
            bus_peaks.set(
                index,
                TrackPeak {
                    peak_l: peak[0],
                    peak_r: peak[1],
                },
            );
        }

        crate::process_inserts(master_inserts, ctx, tap, crate::SpectrumLoc::Master);
        master.apply(ctx.buffers, params.master_gain());
    }

    /// Total nodes across every track, every bus and the master bus.
    pub fn node_count(&self) -> usize {
        self.tracks
            .iter()
            .map(TrackChain::node_count)
            .sum::<usize>()
            + self.buses.iter().map(BusChain::node_count).sum::<usize>()
            + self.master_inserts.len()
    }

    /// Tracks this graph runs.
    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// Buses this graph runs.
    pub fn bus_count(&self) -> usize {
        self.buses.len()
    }

    /// Send taps this graph runs, after compilation dropped any it could not.
    pub fn send_count(&self) -> usize {
        self.tracks.iter().map(|t| t.sends.len()).sum::<usize>()
            + self.buses.iter().map(|b| b.sends.len()).sum::<usize>()
    }

    /// The order the buses run in. Diagnostics and tests.
    pub fn bus_order(&self) -> &[usize] {
        &self.bus_order
    }

    /// Per-bus peaks from the last block processed.
    pub fn bus_peaks(&self) -> BusPeaks {
        self.bus_peaks
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
        self.process_with_spectrum(ctx, None);
    }

    /// Run the current graph, optionally copying a hop around the watched insert.
    pub fn process_with_spectrum(
        &mut self,
        ctx: &mut ProcessCtx,
        tap: Option<&mut crate::SpectrumTapRt>,
    ) {
        match self.current.as_mut() {
            Some(graph) => graph.process_with_spectrum(ctx, tap),
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

    /// Buses the current graph runs.
    pub fn bus_count(&self) -> usize {
        self.current.as_ref().map_or(0, |g| g.bus_count())
    }

    /// Per-bus peaks from the last block. Silent when no graph is installed.
    pub fn bus_peaks(&self) -> BusPeaks {
        self.current
            .as_ref()
            .map_or(BusPeaks::SILENT, |g| g.bus_peaks())
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

    /// Scales what is already on the bus, so a test can tell an insert's output
    /// from its input.
    struct GainNode(f32);
    impl RtNode for GainNode {
        fn process(&mut self, ctx: &mut ProcessCtx) {
            for ch in ctx.buffers.as_mut_slices() {
                for s in ch.iter_mut() {
                    *s *= self.0;
                }
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

    /// A node that reads one key slot and records what it saw, so a test can
    /// ask what a compressor *would* have keyed off at that point in the graph.
    struct KeyReader {
        bus: Arc<crate::SidechainBus>,
        slot: usize,
        seen: Arc<std::sync::atomic::AtomicU32>,
    }

    impl RtNode for KeyReader {
        fn process(&mut self, _ctx: &mut ProcessCtx) {
            self.bus.request(self.slot);
            let mut peak = 0.0_f32;
            for frame in 0..self.bus.frames(self.slot) {
                let [left, right] = self.bus.read(self.slot, frame);
                peak = peak.max(left.abs().max(right.abs()));
            }
            self.seen
                .store(peak.to_bits(), std::sync::atomic::Ordering::Relaxed);
        }
    }

    #[test]
    fn a_strip_publishes_its_key_after_its_inserts_and_before_its_fader() {
        // The tap's contract, in one graph: track 0 makes 0.5 and doubles it to
        // 1.0 in an insert, then its fader is pulled right down. What a keyed
        // compressor on track 1 sees must be the insert's output and not the
        // fader's, or muting the kick would stop the bass ducking.
        let collector = Collector::new();
        let bus = Arc::new(crate::SidechainBus::new(16));
        let seen = Arc::new(std::sync::atomic::AtomicU32::new(0));

        let params = Arc::new(MixerParams::new());
        params.set_track_gain(0, 0.0);

        let mut spec = GraphSpec::new()
            .with_params(Arc::clone(&params))
            .with_sidechain(Arc::clone(&bus))
            .with_limits(limits(16));
        spec.push_track(
            TrackSpec::new(Box::new(ConstNode(0.5))).with_insert(Box::new(GainNode(2.0))),
        );
        spec.push_track(
            TrackSpec::new(Box::new(ConstNode(0.0))).with_insert(Box::new(KeyReader {
                bus: Arc::clone(&bus),
                slot: crate::key_slot(StripId::Track(0)).expect("a slot"),
                seen: Arc::clone(&seen),
            })),
        );

        let (mut handle, mut slot) = graph_channel(2, collector.handle());
        handle.install(spec).expect("installs");
        let mut buffers = AudioBuffers::new(16);
        slot.poll_swap();

        // Block 1 arms the subscription, block 2 publishes, block 3 reads it.
        for _ in 0..3 {
            render(&mut slot, &mut buffers, 16);
        }
        let peak = f32::from_bits(seen.load(std::sync::atomic::Ordering::Relaxed));
        assert!(
            (peak - 1.0).abs() < 1.0e-6,
            "post-insert and pre-fader: expected 1.0, saw {peak}"
        );
    }

    #[test]
    fn a_graph_with_no_key_bus_runs_exactly_as_it_did_before_one_existed() {
        // Every offline render and every test that predates sidechain compiles
        // without a bus, and must be untouched by its existence.
        let collector = Collector::new();
        let mut spec = GraphSpec::new().with_limits(limits(8));
        spec.push_track(TrackSpec::new(Box::new(ConstNode(0.25))));
        assert!(spec.sidechain().is_none());
        let (mut handle, mut slot) = graph_channel(2, collector.handle());
        handle.install(spec).unwrap();
        let mut buffers = AudioBuffers::new(8);
        slot.poll_swap();
        assert_eq!(render(&mut slot, &mut buffers, 8), 0.25);
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

    // ------------------------------------------------------------ routing

    /// Doubles the bus it runs on, so a test can tell "the bus chain ran" from
    /// "the signal reached the master some other way".
    struct DoubleNode;
    impl RtNode for DoubleNode {
        fn process(&mut self, ctx: &mut ProcessCtx) {
            for ch in ctx.buffers.as_mut_slices() {
                for s in ch.iter_mut() {
                    *s *= 2.0;
                }
            }
        }
    }

    /// Render `blocks` blocks of `frames` and return the whole left channel, so
    /// a ramp can be measured rather than only its endpoint.
    fn render_blocks(
        slot: &mut RtGraphSlot,
        buffers: &mut AudioBuffers,
        frames: usize,
        blocks: usize,
    ) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames * blocks);
        for _ in 0..blocks {
            let mut ctx = ProcessCtx::new(buffers, SR, frames, TransportSnapshot::default());
            slot.process(&mut ctx);
            out.extend_from_slice(buffers.as_slices()[0]);
        }
        out
    }

    #[test]
    fn a_send_tap_adds_the_source_into_a_bus_and_the_bus_reaches_the_master() {
        let collector = Collector::new();
        let (mut handle, mut slot) = graph_channel(2, collector.handle());
        let mut buffers = AudioBuffers::new(64);
        let params = Arc::new(MixerParams::new());
        params.set_send_level(StripId::Track(0), 0, 0.5);

        handle
            .install(
                GraphSpec::new()
                    .with_limits(limits(64))
                    .with_params(Arc::clone(&params))
                    .with_track(TrackSpec::new(Box::new(ConstNode(1.0))))
                    // The bus doubles, so its contribution is distinguishable
                    // from the track's own main out.
                    .with_bus(BusSpec::new().with_insert(Box::new(DoubleNode)))
                    .with_send(SendSpec::new(StripId::Track(0), 0)),
            )
            .unwrap();
        slot.poll_swap();

        // main out (1.0) + bus (1.0 * 0.5 tap, doubled) = 2.0
        let rendered = render_blocks(&mut slot, &mut buffers, 64, 2);
        assert!((rendered[127] - 2.0).abs() < 1e-6, "{}", rendered[127]);
        assert_eq!(slot.bus_count(), 1);
        assert!((slot.bus_peaks().get(0).peak_l - 1.0).abs() < 1e-6);
        assert!((slot.track_peaks().get(0).peak_l - 1.0).abs() < 1e-6);
    }

    /// The property the whole pre/post distinction exists for, stated as a
    /// measurement: with the source's fader all the way down, a pre-fader tap
    /// still feeds its bus and a post-fader one is silent.
    #[test]
    fn a_fader_at_silence_stops_a_post_fader_send_and_not_a_pre_fader_one() {
        const BLOCK: usize = 1024;
        let measure = |pre: bool| -> f32 {
            let collector = Collector::new();
            let (mut handle, mut slot) = graph_channel(2, collector.handle());
            let mut buffers = AudioBuffers::new(BLOCK);
            let params = Arc::new(MixerParams::new());
            // Fader down *before* the graph is built, so the strip starts at
            // silence and nothing is measuring a ramp.
            params.set_track_gain(0, 0.0);
            params.set_send_level(StripId::Track(0), 0, 1.0);
            params.set_send_pre(StripId::Track(0), 0, pre);

            handle
                .install(
                    GraphSpec::new()
                        .with_limits(limits(BLOCK))
                        .with_params(Arc::clone(&params))
                        .with_track(TrackSpec::new(Box::new(ConstNode(0.5))))
                        .with_bus(BusSpec::new())
                        .with_send(SendSpec::new(StripId::Track(0), 0)),
                )
                .unwrap();
            slot.poll_swap();
            let rendered = render_blocks(&mut slot, &mut buffers, BLOCK, 2);
            rendered[rendered.len() - 1]
        };

        let pre = measure(true);
        let post = measure(false);
        assert!(
            (pre - 0.5).abs() < 1e-6,
            "a pre-fader send ignores the fader: {pre}"
        );
        assert_eq!(post, 0.0, "a post-fader send follows it");
        assert!(
            dbfs(post) < dbfs(pre) - 80.0,
            "pre {} dBFS vs post {} dBFS",
            dbfs(pre),
            dbfs(post)
        );
    }

    #[test]
    fn a_mute_stops_a_post_fader_send_and_not_a_pre_fader_one() {
        // The same argument as the fader, for the control users actually reach
        // for when they want "wet only".
        const BLOCK: usize = 1024;
        let collector = Collector::new();
        let (mut handle, mut slot) = graph_channel(2, collector.handle());
        let mut buffers = AudioBuffers::new(BLOCK);
        let params = Arc::new(MixerParams::new());
        params.set_track_muted(0, true);
        params.set_send_level(StripId::Track(0), 0, 1.0);
        params.set_send_pre(StripId::Track(0), 0, true);

        handle
            .install(
                GraphSpec::new()
                    .with_limits(limits(BLOCK))
                    .with_params(Arc::clone(&params))
                    .with_track(TrackSpec::new(Box::new(ConstNode(0.25))))
                    .with_bus(BusSpec::new())
                    .with_send(SendSpec::new(StripId::Track(0), 0)),
            )
            .unwrap();
        slot.poll_swap();
        let rendered = render_blocks(&mut slot, &mut buffers, BLOCK, 2);
        assert!((rendered[rendered.len() - 1] - 0.25).abs() < 1e-6);
    }

    #[test]
    fn a_send_level_change_ramps_instead_of_stepping() {
        const BLOCK: usize = 256;
        let collector = Collector::new();
        let (mut handle, mut slot) = graph_channel(2, collector.handle());
        let mut buffers = AudioBuffers::new(BLOCK);
        let params = Arc::new(MixerParams::new());
        params.set_track_muted(0, true); // Only the send path is audible.
        params.set_send_level(StripId::Track(0), 0, 0.0);
        params.set_send_pre(StripId::Track(0), 0, true);

        handle
            .install(
                GraphSpec::new()
                    .with_limits(limits(BLOCK))
                    .with_params(Arc::clone(&params))
                    .with_track(TrackSpec::new(Box::new(ConstNode(1.0))))
                    .with_bus(BusSpec::new())
                    .with_send(SendSpec::new(StripId::Track(0), 0)),
            )
            .unwrap();
        slot.poll_swap();

        let mut rendered = Vec::new();
        for block in 0..8 {
            if block == 2 {
                params.set_send_level(StripId::Track(0), 0, 1.0);
            }
            let mut ctx = ProcessCtx::new(&mut buffers, SR, BLOCK, TransportSnapshot::default());
            slot.process(&mut ctx);
            rendered.extend_from_slice(buffers.as_slices()[0]);
        }

        let limit = 1.0 / 480.0 + 1e-6;
        let worst = rendered
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0_f32, f32::max);
        assert!(worst <= limit, "the send knob stepped by {worst}");
        assert!(
            (rendered[rendered.len() - 1] - 1.0).abs() < 1e-6,
            "and arrived"
        );
    }

    #[test]
    fn flipping_a_send_between_pre_and_post_crossfades_rather_than_switching() {
        // Both taps are in the graph and the flag decides which carries the
        // level, so the change is two ramps rather than a reroute -- which is
        // what keeps it from clicking. With the fader at unity the two taps
        // carry the same signal, so the sum stays constant throughout.
        const BLOCK: usize = 256;
        let collector = Collector::new();
        let (mut handle, mut slot) = graph_channel(2, collector.handle());
        let mut buffers = AudioBuffers::new(BLOCK);
        let params = Arc::new(MixerParams::new());
        params.set_track_muted(0, true);
        params.set_send_level(StripId::Track(0), 0, 1.0);
        params.set_send_pre(StripId::Track(0), 0, true);

        handle
            .install(
                GraphSpec::new()
                    .with_limits(limits(BLOCK))
                    .with_params(Arc::clone(&params))
                    .with_track(TrackSpec::new(Box::new(ConstNode(1.0))))
                    .with_bus(BusSpec::new())
                    .with_send(SendSpec::new(StripId::Track(0), 0)),
            )
            .unwrap();
        slot.poll_swap();
        // Unmute so the post tap carries the same level as the pre one.
        params.set_track_muted(0, false);

        let mut rendered = Vec::new();
        for block in 0..8 {
            if block == 3 {
                params.set_send_pre(StripId::Track(0), 0, false);
            }
            let mut ctx = ProcessCtx::new(&mut buffers, SR, BLOCK, TransportSnapshot::default());
            slot.process(&mut ctx);
            // The bus contribution alone: the master carries the track's own
            // main out as well, and that is not what this measures.
            rendered.push(slot.bus_peaks().get(0).peak_l);
        }
        assert!(
            rendered.iter().all(|level| (*level - 1.0).abs() < 1e-3),
            "the bus level dipped during the flip: {rendered:?}"
        );
    }

    #[test]
    fn buses_run_in_an_order_that_respects_every_bus_to_bus_send() {
        // 0 -> 2, 2 -> 1: the only valid order is 0, 2, 1.
        let sends = [
            SendSpec::new(StripId::Bus(0), 2),
            SendSpec::new(StripId::Bus(2), 1),
        ];
        assert_eq!(bus_order(3, &sends), vec![0, 2, 1]);
        // Nothing to order: index order, so a rebuild is reproducible.
        assert_eq!(bus_order(3, &[]), vec![0, 1, 2]);
        assert_eq!(bus_order(0, &sends), Vec::<usize>::new());
    }

    #[test]
    fn a_bus_to_bus_send_reaches_the_destination_in_the_same_block() {
        let collector = Collector::new();
        let (mut handle, mut slot) = graph_channel(2, collector.handle());
        let mut buffers = AudioBuffers::new(64);
        let params = Arc::new(MixerParams::new());
        params.set_track_muted(0, true);
        params.set_send_level(StripId::Track(0), 1, 1.0);
        params.set_send_pre(StripId::Track(0), 1, true);
        params.set_send_level(StripId::Bus(1), 0, 1.0);

        handle
            .install(
                GraphSpec::new()
                    .with_limits(limits(64))
                    .with_params(Arc::clone(&params))
                    .with_track(TrackSpec::new(Box::new(ConstNode(0.25))))
                    // Bus 0 is declared first but must run *second*.
                    .with_bus(BusSpec::new())
                    .with_bus(BusSpec::new())
                    .with_send(SendSpec::new(StripId::Track(0), 1))
                    .with_send(SendSpec::new(StripId::Bus(1), 0)),
            )
            .unwrap();
        slot.poll_swap();
        render_blocks(&mut slot, &mut buffers, 64, 2);

        assert_eq!(slot.current.as_ref().unwrap().bus_order(), &[1, 0]);
        let peaks = slot.bus_peaks();
        assert!(
            (peaks.get(1).peak_l - 0.25).abs() < 1e-6,
            "bus 1 got the track"
        );
        assert!(
            (peaks.get(0).peak_l - 0.25).abs() < 1e-6,
            "and bus 0 got bus 1, in the same block"
        );
    }

    #[test]
    fn a_cycle_is_broken_at_compile_time_rather_than_reaching_the_audio_thread() {
        // 0 -> 1 and 1 -> 0. Whatever order is chosen, one of the two edges runs
        // backwards; that edge is dropped, and the other survives.
        let spec = GraphSpec::new()
            .with_limits(limits(8))
            .with_bus(BusSpec::new())
            .with_bus(BusSpec::new())
            .with_send(SendSpec::new(StripId::Bus(0), 1))
            .with_send(SendSpec::new(StripId::Bus(1), 0));
        assert_eq!(spec.send_count(), 2);
        let graph = spec.compile();
        assert_eq!(graph.bus_count(), 2);
        assert_eq!(
            graph.send_count(),
            1,
            "exactly the edges that would run backwards are dropped"
        );

        // A self-send is a cycle of one and is dropped entirely.
        let graph = GraphSpec::new()
            .with_limits(limits(8))
            .with_bus(BusSpec::new())
            .with_send(SendSpec::new(StripId::Bus(0), 0))
            .compile();
        assert_eq!(graph.send_count(), 0);
    }

    #[test]
    fn a_send_naming_a_strip_the_graph_does_not_have_is_dropped() {
        let graph = GraphSpec::new()
            .with_limits(limits(8))
            .with_track(TrackSpec::new(Box::new(ConstNode(1.0))))
            .with_bus(BusSpec::new())
            .with_send(SendSpec::new(StripId::Track(0), 0))
            .with_send(SendSpec::new(StripId::Track(9), 0))
            .with_send(SendSpec::new(StripId::Track(0), 5))
            .compile();
        assert_eq!(graph.send_count(), 1);
    }

    #[test]
    fn buses_past_the_fixed_limit_are_dropped_at_compile_time() {
        let mut spec = GraphSpec::new().with_limits(limits(8));
        for _ in 0..MAX_BUSES + 3 {
            spec = spec.with_bus(BusSpec::new());
        }
        assert_eq!(spec.compile().bus_count(), MAX_BUSES);
    }

    #[test]
    fn a_bus_fader_and_pan_reach_the_master_sum_and_the_bus_meter() {
        const BLOCK: usize = 1024;
        let collector = Collector::new();
        let (mut handle, mut slot) = graph_channel(2, collector.handle());
        let mut buffers = AudioBuffers::new(BLOCK);
        let params = Arc::new(MixerParams::new());
        params.set_track_muted(0, true);
        params.set_send_level(StripId::Track(0), 0, 1.0);
        params.set_send_pre(StripId::Track(0), 0, true);
        params.set_bus_gain(0, 0.5);
        params.set_bus_pan(0, -1.0);

        handle
            .install(
                GraphSpec::new()
                    .with_limits(limits(BLOCK))
                    .with_params(Arc::clone(&params))
                    .with_track(TrackSpec::new(Box::new(ConstNode(1.0))))
                    .with_bus(BusSpec::new())
                    .with_send(SendSpec::new(StripId::Track(0), 0)),
            )
            .unwrap();
        slot.poll_swap();

        let mut ctx = ProcessCtx::new(&mut buffers, SR, BLOCK, TransportSnapshot::default());
        slot.process(&mut ctx);
        let [left, right] = buffers.as_slices();
        assert!(
            (left[0] - 0.5 * pan_gains(-1.0)[0]).abs() < 1e-6,
            "{}",
            left[0]
        );
        assert!(
            right[0].abs() < 1e-9,
            "hard left leaves nothing on the right"
        );
        assert!(slot.bus_peaks().get(0).peak_r.abs() < 1e-9);

        params.set_bus_muted(0, true);
        for _ in 0..2 {
            let mut ctx = ProcessCtx::new(&mut buffers, SR, BLOCK, TransportSnapshot::default());
            slot.process(&mut ctx);
        }
        assert_eq!(slot.bus_peaks().get(0), TrackPeak::SILENT);
    }

    #[test]
    fn a_graph_with_no_sends_is_bit_identical_to_one_compiled_before_buses_existed() {
        // The compatibility claim behind adding all of this: a project with no
        // routing must produce exactly the samples it produced before, which is
        // why the no-tap path still goes through `mix_into` rather than through
        // the post-fader scratch.
        const BLOCK: usize = 128;
        let render = |with_empty_bus: bool| -> Vec<f32> {
            let collector = Collector::new();
            let (mut handle, mut slot) = graph_channel(2, collector.handle());
            let params = Arc::new(MixerParams::new());
            params.set_track_gain(0, 0.3);
            let mut spec = GraphSpec::new()
                .with_limits(limits(BLOCK))
                .with_params(Arc::clone(&params))
                .with_track(TrackSpec::new(Box::new(ConstNode(0.7))))
                .with_track(TrackSpec::new(Box::new(AddNode(0.11))));
            if with_empty_bus {
                spec = spec.with_bus(BusSpec::new());
            }
            handle.install(spec).unwrap();
            slot.poll_swap();
            let mut buffers = AudioBuffers::new(BLOCK);
            render_blocks(&mut slot, &mut buffers, BLOCK, 4)
        };
        assert_eq!(
            render(false),
            render(true),
            "a bus nothing sends into must be inaudible"
        );
    }

    #[test]
    fn send_parameters_survive_a_rebuild_and_the_new_graph_does_not_fade_in() {
        // The step-6 property, at send granularity: a structural edit (adding a
        // bus, adding an insert) recompiles the graph, and the reverb send must
        // not sweep back up over 10 ms every time.
        const BLOCK: usize = 64;
        let collector = Collector::new();
        let (mut handle, mut slot) = graph_channel(4, collector.handle());
        let mut buffers = AudioBuffers::new(BLOCK);
        let params = Arc::new(MixerParams::new());
        params.set_track_muted(0, true);
        params.set_send_level(StripId::Track(0), 0, 0.75);
        params.set_send_pre(StripId::Track(0), 0, true);

        let build = |extra_bus: bool| {
            let mut spec = GraphSpec::new()
                .with_limits(limits(BLOCK))
                .with_params(Arc::clone(&params))
                .with_track(TrackSpec::new(Box::new(ConstNode(1.0))))
                .with_bus(BusSpec::new())
                .with_send(SendSpec::new(StripId::Track(0), 0));
            if extra_bus {
                spec = spec.with_bus(BusSpec::new());
            }
            spec
        };

        handle.install(build(false)).unwrap();
        slot.poll_swap();
        let mut rendered = render_blocks(&mut slot, &mut buffers, BLOCK, 2);
        handle.install(build(true)).unwrap();
        slot.poll_swap();
        rendered.extend(render_blocks(&mut slot, &mut buffers, BLOCK, 2));

        assert!(
            rendered.iter().all(|s| (*s - 0.75).abs() < 1e-6),
            "the send level must be continuous across the swap"
        );
        assert_eq!(slot.bus_count(), 2);
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

    #[test]
    fn the_spectrum_tap_copies_one_insert_pre_and_post() {
        let mut graph = GraphSpec::new()
            .with_limits(limits(256))
            .with(Box::new(ConstNode(0.25)))
            .with(Box::new(AddNode(0.1)))
            .compile();
        let (tx, mut rx) = crate::spectrum_channel(4);
        let watch = std::sync::Arc::new(crate::SpectrumWatch::new());
        watch.set(crate::SpectrumTarget::track(0, 0));
        let mut tap = crate::SpectrumTapRt::new(tx, watch, SR);
        let mut buffers = AudioBuffers::new(256);
        for _ in 0..(crate::SPECTRUM_HOP / 256) {
            let mut ctx = ProcessCtx::new(&mut buffers, SR, 256, TransportSnapshot::default());
            graph.process_with_spectrum(&mut ctx, Some(&mut tap));
        }
        let hop = rx.pop().expect("a full hop should have been pushed");
        assert!(
            (hop.pre_l[10] - 0.25).abs() < 1e-5,
            "pre should be the source, got {}",
            hop.pre_l[10]
        );
        assert!(
            (hop.post_l[10] - 0.35).abs() < 1e-5,
            "post should be after the insert, got {}",
            hop.post_l[10]
        );
    }

    #[test]
    fn the_spectrum_tap_can_copy_the_track_source_without_an_insert() {
        let mut graph = GraphSpec::new()
            .with_limits(limits(256))
            .with(Box::new(ConstNode(0.4)))
            .compile();
        let (tx, mut rx) = crate::spectrum_channel(4);
        let watch = std::sync::Arc::new(crate::SpectrumWatch::new());
        watch.set(crate::SpectrumTarget::track_source(0));
        let mut tap = crate::SpectrumTapRt::new(tx, watch, SR);
        let mut buffers = AudioBuffers::new(256);
        for _ in 0..(crate::SPECTRUM_HOP / 256) {
            let mut ctx = ProcessCtx::new(&mut buffers, SR, 256, TransportSnapshot::default());
            graph.process_with_spectrum(&mut ctx, Some(&mut tap));
        }
        let hop = rx.pop().expect("a full hop should have been pushed");
        assert!(
            (hop.pre_l[10] - 0.4).abs() < 1e-5,
            "source tap pre should be the instrument, got {}",
            hop.pre_l[10]
        );
        assert!(
            (hop.post_l[10] - 0.4).abs() < 1e-5,
            "source tap post matches pre when there is no insert, got {}",
            hop.post_l[10]
        );
    }
}
