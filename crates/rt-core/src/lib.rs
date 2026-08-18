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
mod node;
mod transport;

pub use buffers::{AudioBuffers, CHANNELS};
pub use command::{drain_commands, rt_channel, RtCommand, RtCommandConsumer, RtCommandProducer};
pub use denormals::disable_denormals;
#[cfg(target_arch = "x86_64")]
pub use denormals::mxcsr;
pub use gc::{GcThread, DEFAULT_GC_INTERVAL};
pub use graph::{
    graph_channel, CompiledGraph, GraphHandle, GraphInstallError, GraphSpec, RtGraphSlot,
};
pub use node::{ProcessCtx, RtNode, RtNodeFactory};
pub use transport::{TransportClock, TransportSnapshot};

/// Re-exported so downstream crates can name `basedrop` types (`Handle`, `Owned`)
/// without depending on the exact `basedrop` version themselves.
pub use basedrop;
