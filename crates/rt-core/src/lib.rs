//! `mydaw-rt-core` -- real-time audio primitives for MYDAW.
//!
//! This crate is the *inside* of the real-time wall. Everything here is designed
//! so that the audio callback can run without allocating, locking, or making a
//! syscall.
//!
//! # The RT wall
//!
//! ```text
//!            non-RT threads                 |          audio thread
//!   (UI, IPC, plugin hosts, GC)             |
//!   ---------------------------------------- | --------------------------------
//!   GraphSpec -> GraphHandle::install  ------> RtGraphSlot::poll_swap / process
//!   RtCommand -> rt_channel producer   ------> drain_commands
//!   LiveNote  -> note_channel producer ------> drain_notes -> NoteEventBuffer
//!   MixerParams::set_* (atomics)       ------> TrackStrip / MasterStrip
//!   ParamBlock::set (atomics)          ------> SmoothedParams (per-node knobs)
//!   TransportClock (read for UI)       <-----> TransportClock::advance
//!   GcThread (frees old graphs)        <------ Owned<..> dropped, never freed
//! ```
//!
//! Rules for anything running inside [`RtNode::process`]:
//! **no allocation, no locks, no syscalls, no logging, no panics.**
//! See the [`RtNode`] docs for the full list -- it is a hard invariant, not a
//! style preference.
//!
//! Allocation happens exclusively on the non-RT side: [`RtNodeFactory::create`],
//! [`GraphSpec::compile`], [`AudioBuffers::new`], [`rt_channel`].

mod buffers;
mod command;
mod denormals;
mod gc;
mod graph;
pub mod mixer;
mod node;
mod notes;
pub mod params;
mod sidechain;
mod spectrum;
mod transport;

pub use buffers::{AudioBuffers, CHANNELS};
pub use command::{drain_commands, rt_channel, RtCommand, RtCommandConsumer, RtCommandProducer};
pub use denormals::disable_denormals;
#[cfg(target_arch = "x86_64")]
pub use denormals::mxcsr;
pub use gc::{GcThread, DEFAULT_GC_INTERVAL};
pub use graph::{
    bus_order, graph_channel, BusSpec, CompiledGraph, GraphHandle, GraphInstallError, GraphLimits,
    GraphSpec, RtGraphSlot, SendSpec, TrackSpec, DEFAULT_MAX_BLOCK, DEFAULT_MAX_EVENTS,
    DEFAULT_SAMPLE_RATE,
};
pub use mixer::{
    db_to_gain, gain_to_db, max_send_db, pan_gains, param_ramp_frames, BusPeaks, MasterStrip,
    MixerParams, PeakBlock, SendParams, StripId, TrackParams, TrackPeak, TrackPeaks, TrackStrip,
    MAX_BUSES, MAX_GAIN_DB, MAX_SEND_LEVEL, MAX_SEND_SOURCES, MAX_TRACKS, PARAM_RAMP_MIN_FRAMES,
    PARAM_RAMP_SECONDS, SILENCE_DB,
};
pub use node::{ProcessCtx, RtNode, RtNodeFactory};
pub use notes::{
    drain_notes, note_channel, LiveNote, NoteConsumer, NoteEvent, NoteEventBuffer, NoteKind,
    NoteProducer, DEFAULT_NOTE_CAPACITY, MAX_KEY,
};
pub use params::{
    smoothing_frames, AtomicF32, ParamBlock, ParamCurve, ParamDef, ParamNodeFactory, ParamSmoother,
    ReadoutDef, SmoothedParams, DEFAULT_SMOOTHING_MS,
};
pub use sidechain::{
    decode_key, encode_key, key_slot, key_source, KeySource, SidechainBus, KEY_NONE, MAX_KEY_SLOTS,
};
pub use spectrum::{
    process_inserts, spectrum_channel, SpectrumHop, SpectrumKind, SpectrumLoc, SpectrumTapRt,
    SpectrumTarget, SpectrumWatch, SPECTRUM_HOP,
};
pub use transport::{
    clamp_bpm, PlayMode, TransportClock, TransportSnapshot, DEFAULT_BPM, DEFAULT_LOOPING, MAX_BPM,
    MIN_BPM,
};

/// Re-exported so downstream crates can name `basedrop` types (`Handle`, `Owned`)
/// without depending on the exact `basedrop` version themselves.
pub use basedrop;
