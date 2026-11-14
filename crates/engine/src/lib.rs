//! `wave-engine` — everything WAVE does with sound, independent of any window or device.

pub mod autovocal;
pub mod catalog;
mod clipmix;
pub mod comp;
pub mod content;
pub mod demo;
pub mod factory;
pub mod melody;
pub mod midi;
pub mod model;
pub mod music;
pub mod ops;
pub mod pitch;
pub mod poly;
pub mod processor;
pub mod reference;
pub mod rider;
pub mod singer;
pub mod transform;

pub use model::*;
pub use ops::{Dirty, Op, OpError};
pub use processor::{BandParams, FxData, ParamBlock, ProcessCtx, Processor, MAX_PARAMS};
pub mod analysis;
pub mod denoise;
pub mod dsp;
pub mod engine;
pub mod keys;
pub mod pitch_api;
mod project_zip;
pub mod rt;
pub mod sources;
pub mod synth;
pub mod telemetry;
pub mod tune_edit;
pub mod tune_render;
pub mod vocal;

pub use engine::{Engine, EngineError, MadeUnique, NoPlugins, PluginFactory, RenderTail, StateDto, TransportDto};
pub use rt::Rt;
