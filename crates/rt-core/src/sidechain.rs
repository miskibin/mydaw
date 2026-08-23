//! The key bus: every strip's post-insert signal, readable by any compressor.
//!
//! ```text
//!   track k:  source -> insert.. --┬-> [strip] -> sum
//!                                  │
//!                                  └─ copy ──▶ SidechainBus slot k  (bank A)
//!
//!   track n:  source -> insert..[compressor] -> [strip] -> sum
//!                                     ▲
//!                                     └── reads slot k (bank B, last block)
//! ```
//!
//! # Why a bus and not a graph edge
//! The compiled graph is a mixer of **strict per-track serial chains**: a track
//! renders into its own scratch, its strip mixes that into the master, and no
//! node ever sees another track's buffer. Sidechain is the one routing a user
//! genuinely wants that this shape has no edge for -- duck the bass under the
//! kick -- so rather than teach [`CompiledGraph`](crate::CompiledGraph) about
//! cross-track dependencies (a scheduler, a topological sort over a graph that
//! can contain cycles, and a rebuild on every routing change), the graph
//! *publishes* each strip's key signal into a fixed set of pre-allocated slots
//! and a compressor *subscribes* to one by index.
//!
//! # The tap is post-insert, pre-fader
//! Post-insert, because the key a user hears is the processed kick: an EQ or a
//! transient shaper on the kick is part of what makes it a good key, and a
//! detector fed the raw sample would disagree with the sound in the room.
//!
//! Pre-fader, because the key must not move when the source's **fader, pan or
//! mute** does. Riding the kick fader, or muting it to audition something, would
//! otherwise change how hard the bass ducks -- silently, and in a place the user
//! is not looking. This is the same argument a desk makes for wiring a key input
//! from a pre-fader send, and it is the one case where "the send follows the
//! fader" is wrong.
//!
//! # One block of latency, on purpose
//! Tracks run in index order, so a compressor on track 2 keyed off track 7 would
//! read a slot that has not been written yet this block, while the same
//! compressor keyed off track 0 would read a fresh one. Order-dependent DSP is
//! the worst kind: the project would sound different after a track reorder, with
//! nothing on screen to explain it.
//!
//! So the slots are **double-buffered**: the graph writes bank `generation & 1`
//! and every reader reads the *other* bank, which is always exactly one block
//! old regardless of who runs first. One block is 2.7-10 ms at the usual device
//! sizes -- the same order as the lookahead a hardware key input costs, and well
//! inside the 0-5 ms attack a ducking patch wants anyway. It is documented in
//! the editor's tooltip rather than hidden.
//!
//! # RT safety
//! Every buffer is allocated in [`SidechainBus::new`], off the audio thread.
//! [`write`](SidechainBus::write) and [`read`](SidechainBus::read) are relaxed
//! atomic loads and stores over those buffers -- no allocation, no locking, no
//! syscalls. Relaxed is right for the same reason it is right for a parameter:
//! writer and readers are the **same thread** (the audio callback), and the
//! atomics are there to satisfy shared-reference mutation, not to synchronise
//! two threads.
//!
//! # Only what somebody asked for is copied
//! A slot nobody keys off costs one store per block. A compressor with a source
//! selected calls [`request`](SidechainBus::request) each block, which sets a bit
//! in a mask the next [`begin_block`](SidechainBus::begin_block) promotes; the
//! graph copies only the strips in that mask. So the common project -- no
//! sidechain anywhere -- pays nothing per sample.

use std::sync::atomic::{AtomicU32, Ordering};

use crate::mixer::{StripId, MAX_BUSES, MAX_TRACKS};
use crate::params::AtomicF32;
use crate::AudioBuffers;

/// Slots the key bus carries: one per track, then one per bus.
pub const MAX_KEY_SLOTS: usize = MAX_TRACKS + MAX_BUSES;

/// The selector value meaning "no external key; use the insert's own input".
pub const KEY_NONE: i32 = -1;

/// Which strip a compressor keys off.
///
/// The same two words the send matrix and the insert commands use, so `"track"`
/// and `"bus"` mean one thing across the whole surface. Deliberately *not*
/// `master`: keying off the master from an insert on the master is a loop, and
/// keying off it from a track is a mix-wide pump nobody asks for by name.
pub type KeySource = StripId;

/// Slot index for a strip, or `None` for one this build has no slot for.
pub fn key_slot(source: KeySource) -> Option<usize> {
    match source {
        StripId::Track(index) if index < MAX_TRACKS => Some(index),
        StripId::Bus(index) if index < MAX_BUSES => Some(MAX_TRACKS + index),
        _ => None,
    }
}

/// The strip a slot index names, inverse of [`key_slot`].
pub fn key_source(slot: usize) -> Option<KeySource> {
    if slot < MAX_TRACKS {
        Some(StripId::Track(slot))
    } else if slot < MAX_KEY_SLOTS {
        Some(StripId::Bus(slot - MAX_TRACKS))
    } else {
        None
    }
}

/// A strip as a selector value: the slot index, or [`KEY_NONE`].
pub fn encode_key(source: Option<KeySource>) -> i32 {
    match source.and_then(key_slot) {
        Some(slot) => slot as i32,
        None => KEY_NONE,
    }
}

/// A selector value as a strip. `None` for [`KEY_NONE`] and for anything out of
/// range -- a dangling selector is silence, never a different track.
pub fn decode_key(value: i32) -> Option<KeySource> {
    if value < 0 {
        return None;
    }
    key_source(value as usize)
}

/// One strip's published key signal, in two banks.
struct KeySlot {
    /// Interleaved L/R, one array per bank. Sized to `max_block` frames.
    banks: [Box<[AtomicF32]>; 2],
    /// Frames valid in each bank. Zeroed for the write bank at block start, so a
    /// strip that stopped being copied reads as silence rather than as whatever
    /// it last held.
    frames: [AtomicU32; 2],
}

impl KeySlot {
    fn new(max_block: usize) -> Self {
        let bank = || {
            (0..max_block * 2)
                .map(|_| AtomicF32::new(0.0))
                .collect::<Vec<_>>()
                .into_boxed_slice()
        };
        Self {
            banks: [bank(), bank()],
            frames: [AtomicU32::new(0), AtomicU32::new(0)],
        }
    }
}

/// Every strip's key signal, shared by the graph and by the compressors.
///
/// Held by `Arc` for the lifetime of the session -- like
/// [`MixerParams`](crate::MixerParams) -- so a graph rebuild does not drop a
/// compressor's subscription on the floor.
pub struct SidechainBus {
    slots: Box<[KeySlot]>,
    max_block: usize,
    /// Incremented once per block by the graph. Its low bit picks the write bank.
    generation: AtomicU32,
    /// Slots the graph should copy this block.
    armed: AtomicU32,
    /// Slots somebody asked for during this block, promoted to `armed` at the
    /// next [`begin_block`](Self::begin_block).
    requested: AtomicU32,
}

impl SidechainBus {
    /// **Non-RT**: allocates two `max_block`-frame stereo banks per slot.
    pub fn new(max_block: usize) -> Self {
        let max_block = max_block.max(1);
        Self {
            slots: (0..MAX_KEY_SLOTS)
                .map(|_| KeySlot::new(max_block))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            max_block,
            generation: AtomicU32::new(0),
            armed: AtomicU32::new(0),
            requested: AtomicU32::new(0),
        }
    }

    /// Frames one bank can hold.
    pub fn max_block(&self) -> usize {
        self.max_block
    }

    /// Bank the graph writes this block.
    fn write_bank(&self) -> usize {
        (self.generation.load(Ordering::Relaxed) & 1) as usize
    }

    /// Bank a reader reads: the one written last block.
    fn read_bank(&self) -> usize {
        self.write_bank() ^ 1
    }

    /// Start a block: flip the banks and promote the pending subscriptions.
    ///
    /// RT-safe. Called once, at the top of
    /// [`CompiledGraph::process`](crate::CompiledGraph::process), before any
    /// strip runs.
    pub fn begin_block(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.armed
            .store(self.requested.swap(0, Ordering::Relaxed), Ordering::Relaxed);
        // Everything starts the block invalid; only a strip actually copied
        // below publishes a length. A source that was removed, muted out of the
        // graph or simply not reached therefore reads as silence next block
        // rather than as a frozen loop of its last buffer.
        let bank = self.write_bank();
        for slot in self.slots.iter() {
            slot.frames[bank].store(0, Ordering::Relaxed);
        }
    }

    /// Ask for `slot` to be copied from the next block onwards. RT-safe.
    ///
    /// Idempotent, and re-asserted every block by every reader that wants it, so
    /// a compressor that is removed, bypassed or repointed stops the copy on its
    /// own with nothing to unregister.
    pub fn request(&self, slot: usize) {
        if slot < MAX_KEY_SLOTS {
            self.requested.fetch_or(1 << slot, Ordering::Relaxed);
        }
    }

    /// Whether the graph should copy `slot` this block. RT-safe.
    pub fn is_armed(&self, slot: usize) -> bool {
        slot < MAX_KEY_SLOTS && self.armed.load(Ordering::Relaxed) & (1 << slot) != 0
    }

    /// Whether anything at all is armed -- the branch that keeps a project with
    /// no sidechain free.
    pub fn any_armed(&self) -> bool {
        self.armed.load(Ordering::Relaxed) != 0
    }

    /// Publish one strip's post-insert block into its slot. RT-safe.
    ///
    /// A no-op for a slot nobody requested, which is the overwhelmingly common
    /// case and the reason this can be called unconditionally per strip.
    pub fn write(&self, slot: usize, buffers: &AudioBuffers) {
        if !self.is_armed(slot) {
            return;
        }
        let Some(cell) = self.slots.get(slot) else {
            return;
        };
        let bank = self.write_bank();
        let [left, right] = buffers.as_slices();
        let frames = left.len().min(right.len()).min(self.max_block);
        let store = &cell.banks[bank];
        for frame in 0..frames {
            store[frame * 2].set(left[frame]);
            store[frame * 2 + 1].set(right[frame]);
        }
        cell.frames[bank].store(frames as u32, Ordering::Relaxed);
    }

    /// Frames of key signal available in `slot` this block. RT-safe.
    ///
    /// Zero when nothing was published -- a source that does not exist, a
    /// subscription made this block, or the first block after a rebuild.
    pub fn frames(&self, slot: usize) -> usize {
        self.slots
            .get(slot)
            .map(|cell| cell.frames[self.read_bank()].load(Ordering::Relaxed) as usize)
            .unwrap_or(0)
    }

    /// One frame of the key signal, or silence past its end. RT-safe.
    pub fn read(&self, slot: usize, frame: usize) -> [f32; 2] {
        let Some(cell) = self.slots.get(slot) else {
            return [0.0, 0.0];
        };
        let bank = self.read_bank();
        let frames = cell.frames[bank].load(Ordering::Relaxed) as usize;
        if frame >= frames {
            return [0.0, 0.0];
        }
        let store = &cell.banks[bank];
        [store[frame * 2].get(), store[frame * 2 + 1].get()]
    }
}

impl std::fmt::Debug for SidechainBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SidechainBus")
            .field("slots", &self.slots.len())
            .field("max_block", &self.max_block)
            .field("armed", &self.armed.load(Ordering::Relaxed))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(value: f32, frames: usize) -> AudioBuffers {
        let mut buffers = AudioBuffers::new(frames);
        for channel in buffers.as_mut_slices() {
            channel.fill(value);
        }
        buffers
    }

    #[test]
    fn a_slot_index_and_a_strip_are_the_same_thing_both_ways() {
        assert_eq!(key_slot(StripId::Track(0)), Some(0));
        assert_eq!(key_slot(StripId::Bus(0)), Some(MAX_TRACKS));
        assert_eq!(key_slot(StripId::Track(MAX_TRACKS)), None);
        assert_eq!(key_source(0), Some(StripId::Track(0)));
        assert_eq!(key_source(MAX_TRACKS), Some(StripId::Bus(0)));
        assert_eq!(key_source(MAX_KEY_SLOTS), None);
    }

    #[test]
    fn a_dangling_selector_is_none_rather_than_a_different_track() {
        // The whole point of decoding rather than indexing: a project saved on a
        // build with more tracks must not silently key off somebody else.
        assert_eq!(decode_key(KEY_NONE), None);
        assert_eq!(decode_key(-7), None);
        assert_eq!(decode_key(MAX_KEY_SLOTS as i32), None);
        assert_eq!(decode_key(0), Some(StripId::Track(0)));
        assert_eq!(encode_key(Some(StripId::Bus(2))), (MAX_TRACKS + 2) as i32);
        assert_eq!(encode_key(None), KEY_NONE);
    }

    #[test]
    fn nothing_is_copied_until_somebody_asks_and_the_ask_takes_one_block() {
        let bus = SidechainBus::new(8);
        bus.begin_block();
        assert!(!bus.any_armed());
        bus.write(0, &block(0.5, 8));
        assert_eq!(bus.frames(0), 0, "an unrequested slot is never copied");

        bus.request(0);
        // Still not armed *this* block -- the request is promoted at the next.
        bus.write(0, &block(0.5, 8));
        bus.begin_block();
        assert!(bus.is_armed(0));
        bus.write(0, &block(0.5, 8));
        bus.begin_block();
        assert_eq!(bus.frames(0), 8);
        assert_eq!(bus.read(0, 0), [0.5, 0.5]);
    }

    #[test]
    fn a_reader_always_sees_exactly_the_previous_block() {
        // The determinism guarantee: what a compressor reads must not depend on
        // whether its key source's track runs before or after it.
        let bus = SidechainBus::new(4);
        bus.request(3);
        bus.begin_block();
        bus.write(3, &block(1.0, 4));
        // Same block, after the write: still the *previous* (empty) bank.
        assert_eq!(bus.read(3, 0), [0.0, 0.0]);

        bus.request(3);
        bus.begin_block();
        assert_eq!(bus.read(3, 0), [1.0, 1.0]);
        bus.write(3, &block(0.25, 4));
        assert_eq!(bus.read(3, 0), [1.0, 1.0], "still last block's");

        bus.begin_block();
        assert_eq!(bus.read(3, 0), [0.25, 0.25]);
    }

    #[test]
    fn a_source_that_stops_being_written_goes_silent_rather_than_looping() {
        let bus = SidechainBus::new(4);
        bus.request(1);
        bus.begin_block();
        bus.write(1, &block(0.8, 4));
        bus.request(1);
        bus.begin_block();
        assert_eq!(bus.read(1, 0), [0.8, 0.8]);

        // Two blocks with no write at all: the reader must fall to silence, not
        // hold the last buffer for ever.
        bus.begin_block();
        bus.begin_block();
        assert_eq!(bus.frames(1), 0);
        assert_eq!(bus.read(1, 0), [0.0, 0.0]);
    }

    #[test]
    fn reading_past_the_block_or_off_the_end_is_silence_not_a_panic() {
        let bus = SidechainBus::new(4);
        bus.request(0);
        bus.begin_block();
        bus.write(0, &block(0.9, 2));
        bus.begin_block();
        assert_eq!(bus.read(0, 1), [0.9, 0.9]);
        assert_eq!(bus.read(0, 2), [0.0, 0.0], "past the frames written");
        assert_eq!(bus.read(MAX_KEY_SLOTS, 0), [0.0, 0.0], "no such slot");
        assert_eq!(bus.frames(MAX_KEY_SLOTS), 0);
    }

    #[test]
    fn a_block_longer_than_the_bus_was_built_for_is_truncated_not_grown() {
        let bus = SidechainBus::new(2);
        bus.request(0);
        bus.begin_block();
        bus.write(0, &block(0.3, 8));
        bus.begin_block();
        assert_eq!(bus.frames(0), 2);
    }
}
