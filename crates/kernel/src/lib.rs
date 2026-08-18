//! `mydaw-kernel` -- the MYDAW microkernel.
//!
//! The kernel knows nothing about audio, MIDI, plugins formats or UI. It provides
//! four things and stops:
//!
//! 1. [`Plugin`] lifecycle with dependency ordering ([`Kernel`]).
//! 2. A type-keyed [`ServiceRegistry`] for dependency injection between plugins.
//! 3. A typed, synchronous [`EventBus`] -- **non-RT only**.
//! 4. Contribution collectors on [`KernelCtx`]: RT node factories and UI panels.
//!
//! Everything else -- transport, mixer, CLAP/VST3 hosting, audio I/O, panels -- is
//! a statically linked plugin registered by hand in `app/main.rs`.
//!
//! ```
//! use mydaw_kernel::{Kernel, KernelCtx, PanelManifest, Plugin, PluginManifest};
//!
//! struct Hello;
//! impl Plugin for Hello {
//!     fn manifest(&self) -> PluginManifest {
//!         PluginManifest::new("mydaw.hello", "Hello")
//!     }
//!     fn register(&mut self, ctx: &mut KernelCtx) {
//!         ctx.contribute_panel(PanelManifest::new(
//!             "mydaw.panel.hello",
//!             "Hello",
//!             "HelloPanel",
//!         ));
//!     }
//!     fn start(&mut self, _ctx: &KernelCtx) {}
//!     fn stop(&mut self) {}
//! }
//!
//! let mut kernel = Kernel::new();
//! kernel.add_plugin(Box::new(Hello)).unwrap();
//! kernel.boot().unwrap();
//! assert_eq!(kernel.ui_panels().len(), 1);
//! kernel.shutdown();
//! ```
//!
//! # RT safety
//! Nothing in this crate is real-time safe: it locks, allocates and logs. The
//! audio thread talks to the rest of the app exclusively through `mydaw-rt-core`
//! primitives (`rtrb` queues, atomics, pre-published graphs).

mod bus;
mod ctx;
mod error;
mod kernel;
mod plugin;
mod services;

pub use bus::{Event, EventBus};
pub use ctx::{KernelCtx, RtNodeFactoryEntry};
pub use error::{KernelError, KernelResult};
pub use kernel::Kernel;
pub use plugin::{PanelManifest, Plugin, PluginManifest, PluginState, PluginStatus};
pub use services::ServiceRegistry;

/// Re-exported from `mydaw-rt-core` so plugins can contribute node factories with
/// only a `mydaw-kernel` dependency.
pub use mydaw_rt_core::{ProcessCtx, RtNode, RtNodeFactory};
