//! Lock-free command plumbing from non-RT threads to the audio thread.

use rtrb::{Consumer, Producer, RingBuffer};

use crate::PlayMode;

/// Commands the audio thread applies at a block boundary.
///
/// Deliberately `Copy` and allocation-free: pushing one of these never touches
/// the allocator, and popping it on the audio thread never runs a destructor.
/// Anything that needs heap data (a new graph, a pattern, a sample buffer) goes
/// through [`GraphHandle`](crate::GraphHandle) / `basedrop` instead, never
/// through here.
///
/// Not `Eq`: [`SetBpm`](Self::SetBpm) carries an `f64`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RtCommand {
    /// Start or stop the transport.
    SetPlaying(bool),
    /// Jump to an absolute sample position.
    Seek(u64),
    /// Change the tempo. Clamped by
    /// [`TransportClock::set_bpm`](crate::TransportClock::set_bpm), so an
    /// out-of-range value here changes the tempo to the nearest legal one
    /// rather than corrupting the sequencer's arithmetic.
    SetBpm(f64),
    /// Play the active pattern, or play the arrangement.
    ///
    /// A transport command rather than a song edit: switching mode must take
    /// effect on the *next block*, at the same latency as pressing play, and it
    /// must not wait for a song recompile that has nothing to do with it.
    SetPlayMode(PlayMode),
}

/// Producer half, lives on the non-RT side.
pub type RtCommandProducer = Producer<RtCommand>;
/// Consumer half, lives on the audio thread.
pub type RtCommandConsumer = Consumer<RtCommand>;

/// Create a single-producer/single-consumer queue of `capacity` items.
///
/// The ring buffer allocates its storage here, once, on the calling (non-RT)
/// thread. Afterwards `push`/`pop` are wait-free.
pub fn rt_channel<T>(capacity: usize) -> (Producer<T>, Consumer<T>) {
    RingBuffer::new(capacity)
}

/// Apply every pending command to `transport`, draining the queue.
///
/// Convenience for the audio thread: RT-safe (bounded by queue capacity, no
/// allocation, no locks).
pub fn drain_commands(rx: &mut RtCommandConsumer, transport: &crate::TransportClock) {
    while let Ok(cmd) = rx.pop() {
        match cmd {
            RtCommand::SetPlaying(p) => transport.set_playing(p),
            RtCommand::Seek(pos) => transport.seek(pos),
            RtCommand::SetBpm(bpm) => {
                transport.set_bpm(bpm);
            }
            RtCommand::SetPlayMode(mode) => transport.set_play_mode(mode),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TransportClock;

    #[test]
    fn command_roundtrip() {
        let (mut tx, mut rx) = rt_channel::<RtCommand>(8);
        tx.push(RtCommand::SetPlaying(true)).unwrap();
        tx.push(RtCommand::Seek(4096)).unwrap();
        assert_eq!(rx.pop().unwrap(), RtCommand::SetPlaying(true));
        assert_eq!(rx.pop().unwrap(), RtCommand::Seek(4096));
        assert!(rx.pop().is_err(), "queue should be drained");
    }

    #[test]
    fn full_queue_reports_error_instead_of_growing() {
        let (mut tx, _rx) = rt_channel::<RtCommand>(1);
        tx.push(RtCommand::Seek(0)).unwrap();
        assert!(tx.push(RtCommand::Seek(1)).is_err());
    }

    #[test]
    fn drain_applies_commands_to_transport() {
        let (mut tx, mut rx) = rt_channel::<RtCommand>(8);
        let transport = TransportClock::new(48_000.0);
        tx.push(RtCommand::SetPlaying(true)).unwrap();
        tx.push(RtCommand::Seek(1024)).unwrap();
        tx.push(RtCommand::SetBpm(174.3)).unwrap();
        tx.push(RtCommand::SetPlayMode(PlayMode::Song)).unwrap();
        drain_commands(&mut rx, &transport);
        let s = transport.snapshot();
        assert!(s.playing);
        assert_eq!(s.sample_pos, 1024);
        assert_eq!(s.bpm, 174.3);
        assert_eq!(s.mode, PlayMode::Song);
    }

    #[test]
    fn a_tempo_command_is_clamped_where_it_is_applied() {
        // The queue carries whatever it was given; the clock is what refuses to
        // hold a tempo the sequencer cannot divide by.
        let (mut tx, mut rx) = rt_channel::<RtCommand>(8);
        let transport = TransportClock::new(48_000.0);
        tx.push(RtCommand::SetBpm(0.0)).unwrap();
        drain_commands(&mut rx, &transport);
        assert_eq!(transport.bpm(), crate::MIN_BPM);
    }
}
