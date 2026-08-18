//! Kernel error types.
//!
//! Boot problems are programmer errors (a plugin declared a dependency that was
//! never registered, two plugins claim the same id, the dependency graph has a
//! cycle). Per the design spec they fail fast and loud with a precise message
//! rather than degrading into a half-booted app.

/// Anything that can go wrong in the kernel.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum KernelError {
    /// A plugin declared a dependency on an id no registered plugin provides.
    #[error("plugin '{plugin}' depends on '{dependency}', which is not registered")]
    MissingDependency { plugin: String, dependency: String },

    /// The dependency graph is cyclic. `plugins` lists the members that could not
    /// be ordered (sorted, so the message is deterministic).
    #[error("dependency cycle between plugins: {}", plugins.join(", "))]
    DependencyCycle { plugins: Vec<String> },

    /// Two plugins declared the same manifest id.
    #[error("duplicate plugin id '{0}'")]
    DuplicatePluginId(String),

    /// A service of this type was already provided. Services are keyed by type,
    /// so a second provider would silently shadow the first.
    #[error("service of type '{0}' was already provided")]
    DuplicateService(&'static str),

    /// `boot()` called twice.
    #[error("kernel has already booted")]
    AlreadyBooted,

    /// A plugin was added after `boot()`.
    #[error("cannot add plugin '{0}' after boot")]
    AddAfterBoot(String),
}

pub type KernelResult<T> = Result<T, KernelError>;
