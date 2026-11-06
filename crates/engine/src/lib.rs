//! `wave-engine` — everything WAVE does with sound, independent of any window or device.

pub mod catalog;
pub mod demo;
pub mod model;
pub mod ops;
pub mod processor;

pub use model::*;
pub use ops::{Dirty, Op, OpError};
pub use processor::{BandParams, ParamBlock, ProcessCtx, Processor};
pub mod analysis;
pub mod dsp;
pub mod engine;
pub mod rt;
pub mod sources;
pub mod synth;
pub mod telemetry;

pub use engine::{Engine, EngineError, NoPlugins, PluginFactory, StateDto, TransportDto};
pub use rt::Rt;
