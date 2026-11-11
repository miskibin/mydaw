//! The binary frame the UI draws from. Layout is documented in docs/architecture.md.

use crate::analysis::FxAnalysis;
use crate::model::Id;
use crate::rt::{Shared, MAX_INSERTS};

pub const MAGIC: u32 = 0x5456_4157;

struct W(Vec<u8>);
impl W {
    fn u8(&mut self, v: u8) {
        self.0.push(v)
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes())
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes())
    }
    fn f32(&mut self, v: f32) {
        self.0.extend_from_slice(&v.to_le_bytes())
    }
    fn f64(&mut self, v: f64) {
        self.0.extend_from_slice(&v.to_le_bytes())
    }
    fn f32s(&mut self, v: &[f32]) {
        for &x in v {
            self.f32(x)
        }
    }
}

/// `rec` = (track, clipStart, seconds, peakOffset, new peaks).
pub fn frame(
    seq: u32,
    shared: &Shared,
    insert_ids: &[Id],
    fx: Option<&[FxAnalysis]>,
    rec: Option<&(u32, f64, f32, u32, Vec<f32>)>,
) -> Vec<u8> {
    let nfx = fx.map(|f| f.len()).unwrap_or(0);
    let mut w = W(Vec::with_capacity(
        64 + insert_ids.len() * 12
            + nfx * (144 + 4 * (128 * 2 + 256 * 2))
            + rec.map(|r| r.4.len() * 4 + 32).unwrap_or(4),
    ));
    w.u32(MAGIC);
    w.u32(seq);
    // the playhead shows what is heard, not what the mixer is rendering (playback latency)
    w.f64(shared.heard_beat());
    let playing = shared.playing.load(std::sync::atomic::Ordering::Relaxed);
    let recording = shared.recording.load(std::sync::atomic::Ordering::Relaxed);
    let previewing = shared.preview_pos() >= 0.0;
    w.u8(playing as u8 | (recording as u8) << 1 | (previewing as u8) << 2);
    w.u8(0);
    w.u16(0);
    w.f32(shared.take_in_level());
    w.f32(shared.preview_pos());
    w.f32(shared.cpu());
    let n = insert_ids.len().min(MAX_INSERTS);
    w.u32(n as u32);
    for (i, id) in insert_ids.iter().take(n).enumerate() {
        let (l, r) = shared.take_meter(i);
        w.u32(*id as u32);
        w.f32(l);
        w.f32(r);
    }
    w.u32(nfx as u32);
    for a in fx.unwrap_or(&[]) {
        w.u32(a.id as u32);
        w.f32(a.in_peak);
        w.f32(a.out_peak);
        w.f32(a.gr);
        w.f32s(&a.meters);
        w.f32s(&a.spec_in);
        w.f32s(&a.spec_out);
        w.f32s(&a.scope_in);
        w.f32s(&a.scope_out);
    }
    match rec {
        None => w.u32(0),
        Some((track, start, secs, off, peaks)) => {
            w.u32(1);
            w.u32(*track);
            w.f64(*start);
            w.f32(*secs);
            w.u32(*off);
            w.u32((peaks.len() / 2) as u32);
            w.f32s(peaks);
        }
    }
    // trailing: the pattern loop preview's position (beats into the pattern, −1 = stopped)
    w.f64(shared.pat_beat());
    w.0
}
