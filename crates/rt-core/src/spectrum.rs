//! Analysis hop from one focused insert to a non-RT FFT.
//!
//! The audio thread never transforms. It copies a short stereo pre/post window
//! of the insert the UI is looking at and pushes that window across an `rtrb`.
//! The shell drains the queue on its poll and runs the FFT there.
//!
//! Only one insert is armed at a time. A relaxed atomic names it; a missed hop
//! is a stale analyser, never a blocked callback.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use rtrb::{Consumer, Producer};

use crate::AudioBuffers;

/// Frames in one hop. ~21 ms at 48 kHz; 1024 is a convenient FFT size.
pub const SPECTRUM_HOP: usize = 1024;

/// Packed watch value meaning "nobody is looking".
const WATCH_NONE: u32 = 0;
const VALID: u32 = 1 << 31;
const KIND_SHIFT: u32 = 24;
const STRIP_SHIFT: u32 = 16;
const KIND_MASK: u32 = 0x3;
const BYTE_MASK: u32 = 0xFF;

/// Which chain the tap is sitting on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpectrumKind {
    Track = 0,
    Bus = 1,
    Master = 2,
}

/// One insert in the running graph, as the tap names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpectrumTarget {
    pub kind: SpectrumKind,
    pub strip: u8,
    pub insert: u8,
}

impl SpectrumTarget {
    pub const fn track(strip: usize, insert: usize) -> Option<Self> {
        Self::pack(SpectrumKind::Track, strip, insert)
    }

    pub const fn bus(strip: usize, insert: usize) -> Option<Self> {
        Self::pack(SpectrumKind::Bus, strip, insert)
    }

    pub const fn master(insert: usize) -> Option<Self> {
        Self::pack(SpectrumKind::Master, 0, insert)
    }

    const fn pack(kind: SpectrumKind, strip: usize, insert: usize) -> Option<Self> {
        if strip > 255 || insert > 255 {
            return None;
        }
        Some(Self {
            kind,
            strip: strip as u8,
            insert: insert as u8,
        })
    }

    fn encode(self) -> u32 {
        VALID
            | ((self.kind as u32) << KIND_SHIFT)
            | ((self.strip as u32) << STRIP_SHIFT)
            | u32::from(self.insert)
    }

    fn decode(raw: u32) -> Option<Self> {
        if raw & VALID == 0 {
            return None;
        }
        let kind = match (raw >> KIND_SHIFT) & KIND_MASK {
            0 => SpectrumKind::Track,
            1 => SpectrumKind::Bus,
            2 => SpectrumKind::Master,
            _ => return None,
        };
        Some(Self {
            kind,
            strip: ((raw >> STRIP_SHIFT) & BYTE_MASK) as u8,
            insert: (raw & BYTE_MASK) as u8,
        })
    }
}

/// Where the graph is right now, used to decide whether to copy this insert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpectrumLoc {
    Track(usize),
    Bus(usize),
    Master,
}

/// Shared arming word. The UI stores; the callback loads.
#[derive(Debug)]
pub struct SpectrumWatch {
    raw: AtomicU32,
}

impl SpectrumWatch {
    pub fn new() -> Self {
        Self {
            raw: AtomicU32::new(WATCH_NONE),
        }
    }

    /// Arm the tap on `target`, or silence it with `None`.
    pub fn set(&self, target: Option<SpectrumTarget>) {
        self.raw.store(
            target.map(SpectrumTarget::encode).unwrap_or(WATCH_NONE),
            Ordering::Relaxed,
        );
    }

    /// What the callback should copy this block, if anything.
    pub fn get(&self) -> Option<SpectrumTarget> {
        SpectrumTarget::decode(self.raw.load(Ordering::Relaxed))
    }

    fn load_raw(&self) -> u32 {
        self.raw.load(Ordering::Relaxed)
    }
}

impl Default for SpectrumWatch {
    fn default() -> Self {
        Self::new()
    }
}

/// One filled hop. Plain data so the queue can move it without allocating.
#[derive(Clone, Copy)]
pub struct SpectrumHop {
    pub pre_l: [f32; SPECTRUM_HOP],
    pub pre_r: [f32; SPECTRUM_HOP],
    pub post_l: [f32; SPECTRUM_HOP],
    pub post_r: [f32; SPECTRUM_HOP],
    pub sample_rate: f32,
}

impl SpectrumHop {
    pub const SILENT: Self = Self {
        pre_l: [0.0; SPECTRUM_HOP],
        pre_r: [0.0; SPECTRUM_HOP],
        post_l: [0.0; SPECTRUM_HOP],
        post_r: [0.0; SPECTRUM_HOP],
        sample_rate: 0.0,
    };
}

/// Audio-thread accumulator. Lives in [`crate`] RT state, not in the graph:
/// a graph rebuild must not swallow the producer.
pub struct SpectrumTapRt {
    watch: Arc<SpectrumWatch>,
    tx: Producer<SpectrumHop>,
    pre_l: [f32; SPECTRUM_HOP],
    pre_r: [f32; SPECTRUM_HOP],
    post_l: [f32; SPECTRUM_HOP],
    post_r: [f32; SPECTRUM_HOP],
    filled: usize,
    /// Frames copied on the matching `capture_pre` so post stays aligned.
    pending: usize,
    armed: u32,
    sample_rate: f32,
}

impl SpectrumTapRt {
    /// **Non-RT**: the producer and the watch are handed in from the metering
    /// plugin; the arrays are this struct's storage, not a heap bump later.
    pub fn new(tx: Producer<SpectrumHop>, watch: Arc<SpectrumWatch>, sample_rate: f64) -> Self {
        Self {
            watch,
            tx,
            pre_l: [0.0; SPECTRUM_HOP],
            pre_r: [0.0; SPECTRUM_HOP],
            post_l: [0.0; SPECTRUM_HOP],
            post_r: [0.0; SPECTRUM_HOP],
            filled: 0,
            pending: 0,
            armed: WATCH_NONE,
            sample_rate: if sample_rate.is_finite() && sample_rate > 0.0 {
                sample_rate as f32
            } else {
                48_000.0
            },
        }
    }

    /// Insert index to wrap, if this location is the one being watched.
    pub fn watched_insert(&self, loc: SpectrumLoc) -> Option<usize> {
        let target = self.watch.get()?;
        let matches = match (target.kind, loc) {
            (SpectrumKind::Track, SpectrumLoc::Track(index)) => index == usize::from(target.strip),
            (SpectrumKind::Bus, SpectrumLoc::Bus(index)) => index == usize::from(target.strip),
            (SpectrumKind::Master, SpectrumLoc::Master) => true,
            _ => false,
        };
        matches.then_some(usize::from(target.insert))
    }

    /// Copy the buffer *before* the watched insert runs.
    pub fn capture_pre(&mut self, buffers: &AudioBuffers) {
        let raw = self.watch.load_raw();
        if raw != self.armed {
            self.filled = 0;
            self.armed = raw;
        }
        if raw == WATCH_NONE {
            self.pending = 0;
            return;
        }
        let room = SPECTRUM_HOP.saturating_sub(self.filled);
        let n = buffers.frames().min(room);
        if n == 0 {
            self.pending = 0;
            return;
        }
        let [left, right] = buffers.as_slices();
        self.pre_l[self.filled..self.filled + n].copy_from_slice(&left[..n]);
        self.pre_r[self.filled..self.filled + n].copy_from_slice(&right[..n]);
        self.pending = n;
    }

    /// Copy the buffer *after* the watched insert runs, and push when full.
    pub fn capture_post(&mut self, buffers: &AudioBuffers) {
        let n = self.pending.min(buffers.frames());
        self.pending = 0;
        if n == 0 {
            return;
        }
        let [left, right] = buffers.as_slices();
        self.post_l[self.filled..self.filled + n].copy_from_slice(&left[..n]);
        self.post_r[self.filled..self.filled + n].copy_from_slice(&right[..n]);
        self.filled += n;
        if self.filled >= SPECTRUM_HOP {
            let hop = SpectrumHop {
                pre_l: self.pre_l,
                pre_r: self.pre_r,
                post_l: self.post_l,
                post_r: self.post_r,
                sample_rate: self.sample_rate,
            };
            let _ = self.tx.push(hop);
            self.filled = 0;
        }
    }

    /// Give the producer back so a later stream can reuse it.
    pub fn take_producer(self) -> Producer<SpectrumHop> {
        self.tx
    }
}

/// Create the hop queue. Allocates on the calling (non-RT) thread.
pub fn spectrum_channel(capacity: usize) -> (Producer<SpectrumHop>, Consumer<SpectrumHop>) {
    rtrb::RingBuffer::new(capacity.max(1))
}

/// Drive one insert chain, copying pre/post around the watched slot.
pub fn process_inserts(
    inserts: &mut [Box<dyn crate::RtNode>],
    ctx: &mut crate::ProcessCtx,
    mut tap: Option<&mut SpectrumTapRt>,
    loc: SpectrumLoc,
) {
    let watched = tap.as_ref().and_then(|tap| tap.watched_insert(loc));
    for (index, node) in inserts.iter_mut().enumerate() {
        if watched == Some(index) {
            if let Some(tap) = tap.as_mut() {
                tap.capture_pre(ctx.buffers);
            }
        }
        node.process(ctx);
        if watched == Some(index) {
            if let Some(tap) = tap.as_mut() {
                tap.capture_post(ctx.buffers);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_is_not_track_zero_insert_zero() {
        let watch = SpectrumWatch::new();
        assert_eq!(watch.get(), None);
        watch.set(SpectrumTarget::track(0, 0));
        assert_eq!(watch.get(), SpectrumTarget::track(0, 0));
        watch.set(None);
        assert_eq!(watch.get(), None);
    }

    #[test]
    fn a_hop_fills_from_successive_blocks_and_keeps_pre_post_aligned() {
        let (tx, mut rx) = spectrum_channel(2);
        let watch = Arc::new(SpectrumWatch::new());
        watch.set(SpectrumTarget::track(0, 0));
        let mut tap = SpectrumTapRt::new(tx, Arc::clone(&watch), 48_000.0);
        let mut buffers = AudioBuffers::new(256);

        for block in 0..(SPECTRUM_HOP / 256) {
            buffers.set_frames(256);
            let [left, right] = buffers.as_mut_slices();
            for (i, sample) in left.iter_mut().enumerate() {
                *sample = (block * 256 + i) as f32;
            }
            right.copy_from_slice(left);
            tap.capture_pre(&buffers);
            let [left, right] = buffers.as_mut_slices();
            for sample in left.iter_mut().chain(right.iter_mut()) {
                *sample += 1.0;
            }
            tap.capture_post(&buffers);
        }

        let hop = rx.pop().expect("a full hop should have been pushed");
        assert_eq!(hop.pre_l[0], 0.0);
        assert_eq!(hop.pre_l[1023], 1023.0);
        assert_eq!(hop.post_l[0], 1.0);
        assert_eq!(hop.post_l[1023], 1024.0);
        assert_eq!(hop.sample_rate, 48_000.0);
    }
}
