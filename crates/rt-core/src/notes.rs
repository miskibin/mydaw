//! Note events: the sample-accurate control path from the UI into instruments.
//!
//! Audio flows through [`AudioBuffers`](crate::AudioBuffers); *notes* flow
//! alongside it, as a per-block slice of [`NoteEvent`] hanging off
//! [`ProcessCtx`](crate::ProcessCtx). The shapes here exist to make that slice
//! cheap to build and impossible to get wrong on the audio thread:
//!
//! ```text
//!   UI thread                 audio thread (one block)
//!   ---------                 ------------------------
//!   LiveNote --note_channel--> drain_notes -> NoteEventBuffer -> ProcessCtx.events
//!            (rtrb, wait-free)  (frame_offset 0)   (never grows)      (&[NoteEvent])
//! ```
//!
//! # Why two types
//! A [`LiveNote`] is a note with **no position**: a key the user is pressing
//! right now, which the engine can only honour at the start of the next block.
//! A [`NoteEvent`] is a note **placed in a block**, at `frame_offset` frames from
//! its first sample. The queue carries the first; the buffer holds the second.
//! Keeping them distinct means a live note cannot silently claim a sample
//! accuracy it never had, and the sequencer (which produces real offsets) has
//! somewhere to put them without a second event type.
//!
//! # RT safety
//! [`NoteEventBuffer`] allocates exactly once, in [`NoteEventBuffer::new`]. Every
//! other method -- `clear`, `push`, `as_slice` -- is arithmetic and memory moves
//! inside that allocation. A `push` past capacity returns `false` instead of
//! growing; the caller counts it. Dropping notes is bad; allocating on the audio
//! thread is worse.

use rtrb::{Consumer, Producer};

/// Capacity used for the engine's live note queue and per-block event buffer.
///
/// 1024 events in one block is far beyond anything a human (or a dense MIDI
/// file) produces at 48 kHz / 512 frames; the number exists so that overflow is
/// a theoretical branch rather than a design constraint.
pub const DEFAULT_NOTE_CAPACITY: usize = 1024;

/// Highest key number MIDI and CLAP address. Keys are `0..=127`.
pub const MAX_KEY: u8 = 127;

/// What a note event does.
///
/// `#[non_exhaustive]`: CLAP has choke, note-end and per-note expression events,
/// and Phase 2 will want at least the first two. Adding a variant here must not
/// break downstream crates, so matches outside this crate carry a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum NoteKind {
    /// A key was pressed. A velocity of `0.0` is a legal note-on in CLAP and is
    /// *not* a note-off -- unlike MIDI 1.0, where the two are conflated.
    NoteOn {
        /// MIDI key number: 60 is middle C, 69 is A440.
        key: u8,
        /// Normalised velocity, `0.0..=1.0`.
        velocity: f32,
    },
    /// A key was released.
    NoteOff {
        /// MIDI key number, matching the note-on that started the voice.
        key: u8,
    },
}

impl NoteKind {
    /// The key this event addresses, if it addresses one.
    ///
    /// `Option` because a future variant (a choke targeting a whole channel, an
    /// expression aimed at a note id) need not name a key.
    pub const fn key(&self) -> Option<u8> {
        match self {
            Self::NoteOn { key, .. } | Self::NoteOff { key } => Some(*key),
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }

    /// Whether this event starts a voice.
    pub const fn is_note_on(&self) -> bool {
        matches!(self, Self::NoteOn { .. })
    }
}

/// A note event placed inside one processing block.
///
/// `Copy` and allocation-free, like everything else that crosses the RT wall.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoteEvent {
    /// Frames from the start of the block at which this event takes effect.
    ///
    /// Always `< ctx.frames`. Live input uses `0` (see [`LiveNote`]); the
    /// sequencer will use real offsets.
    pub frame_offset: u32,
    /// Note channel, `0..=15`, mirroring MIDI 1.0 channels.
    pub channel: u8,
    pub kind: NoteKind,
}

impl NoteEvent {
    /// Note-on at `frame_offset` on channel 0.
    pub const fn note_on(frame_offset: u32, key: u8, velocity: f32) -> Self {
        Self {
            frame_offset,
            channel: 0,
            kind: NoteKind::NoteOn { key, velocity },
        }
    }

    /// Note-off at `frame_offset` on channel 0.
    pub const fn note_off(frame_offset: u32, key: u8) -> Self {
        Self {
            frame_offset,
            channel: 0,
            kind: NoteKind::NoteOff { key },
        }
    }

    /// Builder-style channel override.
    #[must_use]
    pub const fn on_channel(mut self, channel: u8) -> Self {
        self.channel = channel;
        self
    }
}

/// A note with no position: what a keyboard, a pad or the UI produces.
///
/// The engine stamps it with a [`frame_offset`](NoteEvent::frame_offset) when it
/// arrives, which for live input is always `0` -- the start of the first block
/// after the key was pressed. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LiveNote {
    /// Note channel, `0..=15`.
    pub channel: u8,
    pub kind: NoteKind,
}

impl LiveNote {
    /// Note-on on channel 0.
    pub const fn note_on(key: u8, velocity: f32) -> Self {
        Self {
            channel: 0,
            kind: NoteKind::NoteOn { key, velocity },
        }
    }

    /// Note-off on channel 0.
    pub const fn note_off(key: u8) -> Self {
        Self {
            channel: 0,
            kind: NoteKind::NoteOff { key },
        }
    }

    /// Builder-style channel override.
    #[must_use]
    pub const fn on_channel(mut self, channel: u8) -> Self {
        self.channel = channel;
        self
    }

    /// Place this note inside a block.
    pub const fn at(self, frame_offset: u32) -> NoteEvent {
        NoteEvent {
            frame_offset,
            channel: self.channel,
            kind: self.kind,
        }
    }
}

/// Producer half of the live note queue; lives on the non-RT side.
pub type NoteProducer = Producer<LiveNote>;
/// Consumer half of the live note queue; lives on the audio thread.
pub type NoteConsumer = Consumer<LiveNote>;

/// Create the live note queue.
///
/// Separate from [`rt_channel::<RtCommand>`](crate::rt_channel) on purpose: a
/// held chord is a burst of dozens of events, and letting that share a queue
/// with Play/Stop would mean sizing the transport queue for the worst musical
/// case (or dropping a Stop because somebody leaned on the keyboard).
///
/// Allocates its storage here, once, on the calling (non-RT) thread.
pub fn note_channel(capacity: usize) -> (NoteProducer, NoteConsumer) {
    crate::rt_channel(capacity)
}

/// The block's note events, in a buffer that never grows.
///
/// Kept sorted by [`frame_offset`](NoteEvent::frame_offset), with insertion
/// order preserved among events sharing an offset -- so a note-off pushed before
/// a note-on for the same key at the same frame still reaches the plugin in that
/// order, which is the difference between a re-trigger and a stuck note.
///
/// # RT safety
/// One allocation, in [`new`](Self::new). [`push`](Self::push) is an ordered
/// insert into spare capacity (a `memmove`, never a `realloc`) and returns
/// `false` rather than growing when the buffer is full.
#[derive(Debug)]
pub struct NoteEventBuffer {
    events: Vec<NoteEvent>,
}

impl NoteEventBuffer {
    /// Allocate room for `capacity` events. **Non-RT.**
    pub fn new(capacity: usize) -> Self {
        Self {
            events: Vec::with_capacity(capacity),
        }
    }

    /// Events this buffer can hold in one block.
    pub fn capacity(&self) -> usize {
        self.events.capacity()
    }

    /// Events currently in the buffer.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Empty the buffer, keeping its allocation. Call once per block.
    pub fn clear(&mut self) {
        self.events.clear();
    }

    /// Insert `event` in `frame_offset` order.
    ///
    /// Returns `false` when the buffer is full; the event is dropped and the
    /// caller is expected to count it. RT-safe either way.
    pub fn push(&mut self, event: NoteEvent) -> bool {
        if self.events.len() == self.events.capacity() {
            return false;
        }
        // Scan from the end: the common case (events arriving in time order,
        // and every live event at offset 0) appends after one comparison.
        let mut index = self.events.len();
        while index > 0 && self.events[index - 1].frame_offset > event.frame_offset {
            index -= 1;
        }
        // `len < capacity`, so this shifts elements inside the existing
        // allocation and cannot reallocate.
        self.events.insert(index, event);
        true
    }

    /// The block's events, sorted by `frame_offset`.
    pub fn as_slice(&self) -> &[NoteEvent] {
        &self.events
    }

    /// Whether the sorted invariant holds. Diagnostics and tests.
    pub fn is_sorted(&self) -> bool {
        self.events
            .windows(2)
            .all(|pair| pair[0].frame_offset <= pair[1].frame_offset)
    }
}

/// Move every queued live note into `buffer`, stamped with `frame_offset`.
///
/// Returns how many events were dropped because the buffer was full. The queue
/// is drained either way: leaving events behind would make the *next* block play
/// notes the user pressed two blocks ago, which is worse than losing them.
///
/// RT-safe: bounded by the queue's capacity, no allocation, no locks.
pub fn drain_notes(
    rx: &mut NoteConsumer,
    buffer: &mut NoteEventBuffer,
    frame_offset: u32,
) -> usize {
    let mut dropped = 0;
    while let Ok(note) = rx.pop() {
        if !buffer.push(note.at(frame_offset)) {
            dropped += 1;
        }
    }
    dropped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_live_note_becomes_a_placed_event() {
        let note = LiveNote::note_on(69, 0.8).on_channel(3);
        let event = note.at(128);
        assert_eq!(event.frame_offset, 128);
        assert_eq!(event.channel, 3);
        assert_eq!(
            event.kind,
            NoteKind::NoteOn {
                key: 69,
                velocity: 0.8
            }
        );
        assert_eq!(event.kind.key(), Some(69));
        assert!(event.kind.is_note_on());

        let off = LiveNote::note_off(69).at(0);
        assert_eq!(off.kind, NoteKind::NoteOff { key: 69 });
        assert!(!off.kind.is_note_on());
        assert_eq!(off.channel, 0);
    }

    #[test]
    fn a_zero_velocity_note_on_is_not_a_note_off() {
        // CLAP is explicit about this, and a host that conflates the two makes
        // every plugin that uses velocity 0 for anything sound wrong.
        let quiet = NoteEvent::note_on(0, 60, 0.0);
        assert!(quiet.kind.is_note_on());
        assert_ne!(quiet.kind, NoteKind::NoteOff { key: 60 });
    }

    #[test]
    fn the_note_queue_round_trips_and_drains() {
        let (mut tx, mut rx) = note_channel(8);
        tx.push(LiveNote::note_on(60, 1.0)).unwrap();
        tx.push(LiveNote::note_off(60)).unwrap();

        let mut buffer = NoteEventBuffer::new(16);
        assert_eq!(drain_notes(&mut rx, &mut buffer, 0), 0);
        assert_eq!(
            buffer.as_slice(),
            &[NoteEvent::note_on(0, 60, 1.0), NoteEvent::note_off(0, 60)],
            "live events land at offset 0, in the order they were sent"
        );
        assert!(buffer.is_sorted());

        // Drained: a second pass adds nothing.
        buffer.clear();
        assert_eq!(drain_notes(&mut rx, &mut buffer, 0), 0);
        assert!(buffer.is_empty());
    }

    #[test]
    fn a_full_note_queue_reports_the_error_instead_of_growing() {
        let (mut tx, _rx) = note_channel(1);
        tx.push(LiveNote::note_on(60, 1.0)).unwrap();
        assert!(tx.push(LiveNote::note_on(61, 1.0)).is_err());
    }

    #[test]
    fn the_buffer_keeps_events_sorted_by_offset_and_stable_within_one() {
        let mut buffer = NoteEventBuffer::new(8);
        // Deliberately out of order, with a repeated offset.
        assert!(buffer.push(NoteEvent::note_on(64, 60, 1.0)));
        assert!(buffer.push(NoteEvent::note_off(0, 48)));
        assert!(buffer.push(NoteEvent::note_on(0, 48, 0.5)));
        assert!(buffer.push(NoteEvent::note_on(32, 55, 1.0)));

        let offsets: Vec<u32> = buffer.as_slice().iter().map(|e| e.frame_offset).collect();
        assert_eq!(offsets, vec![0, 0, 32, 64]);
        assert!(buffer.is_sorted());
        // Stability at the same offset: the note-off still precedes the note-on,
        // so a retrigger does not become a stuck note.
        assert_eq!(buffer.as_slice()[0].kind, NoteKind::NoteOff { key: 48 });
        assert_eq!(
            buffer.as_slice()[1].kind,
            NoteKind::NoteOn {
                key: 48,
                velocity: 0.5
            }
        );
    }

    #[test]
    fn the_buffer_drops_past_capacity_instead_of_reallocating() {
        let mut buffer = NoteEventBuffer::new(2);
        let capacity = buffer.capacity();
        assert!(buffer.push(NoteEvent::note_on(0, 60, 1.0)));
        assert!(buffer.push(NoteEvent::note_on(0, 61, 1.0)));
        assert!(!buffer.push(NoteEvent::note_on(0, 62, 1.0)), "full");
        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer.capacity(), capacity, "the buffer must never grow");

        buffer.clear();
        assert!(buffer.is_empty());
        assert_eq!(buffer.capacity(), capacity, "clear keeps the allocation");
        assert!(buffer.push(NoteEvent::note_on(0, 62, 1.0)));
    }

    #[test]
    fn overflowing_the_block_buffer_is_counted_and_the_queue_still_drains() {
        let (mut tx, mut rx) = note_channel(8);
        for key in 60..=64 {
            tx.push(LiveNote::note_on(key, 1.0)).unwrap();
        }

        let mut buffer = NoteEventBuffer::new(2);
        assert_eq!(
            drain_notes(&mut rx, &mut buffer, 0),
            3,
            "five events into a two-slot buffer drops three"
        );
        assert_eq!(buffer.len(), 2);
        assert!(rx.pop().is_err(), "the queue must be drained even so");
    }

    #[test]
    fn a_sequencer_style_drain_stamps_the_offset_it_is_given() {
        let (mut tx, mut rx) = note_channel(4);
        tx.push(LiveNote::note_on(72, 0.25)).unwrap();
        let mut buffer = NoteEventBuffer::new(4);
        drain_notes(&mut rx, &mut buffer, 77);
        assert_eq!(buffer.as_slice()[0].frame_offset, 77);
    }

    #[test]
    fn the_default_capacity_is_generous_enough_to_be_theoretical() {
        const { assert!(DEFAULT_NOTE_CAPACITY >= 256) };
        assert_eq!(MAX_KEY, 127);
    }
}
