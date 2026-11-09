//! `wave-engine` — everything WAVE does with sound, independent of any window or device.

pub mod catalog;
pub mod midi;
pub mod comp;
pub mod content;
pub mod demo;
pub mod model;
pub mod music;
pub mod ops;
pub mod pitch;
pub mod processor;
pub mod reference;
pub mod singer;

pub use model::*;
pub use ops::{Dirty, Op, OpError};
pub use processor::{BandParams, ParamBlock, ProcessCtx, Processor};
pub mod analysis;
pub mod denoise;
pub mod dsp;
pub mod engine;
pub mod rt;
pub mod sources;
pub mod synth;
pub mod telemetry;
pub mod tune_render;
pub mod vocal;

pub use engine::{Engine, EngineError, NoPlugins, PluginFactory, StateDto, TransportDto};
pub use rt::Rt;
