//! Saving and restoring a plugin's state (`clap.state`).
//!
//! A DAW that cannot reopen a session the way the user left it is a toy. This
//! module is the host half of the CLAP extension that makes that possible:
//! `clap_plugin_state->save` writes an opaque blob, `->load` puts it back.
//! MYDAW does not interpret a byte of it -- that is the point of the extension,
//! and the reason a project file can store a preset for a plugin it knows
//! nothing about.
//!
//! Step 6 uses it for two things:
//!
//! 1. **Groundwork for step 8's project file.** [`save_state`] is what a save
//!    will call per instance; [`load_state`] is what a load will call after
//!    re-creating one.
//! 2. **Moving an instance between graphs.** Step 6 keeps the *same*
//!    [`LoadedClapPlugin`] across a graph rebuild (see
//!    [`crate::instances::ClapInstanceHost`]), so a rebuild does not actually
//!    need a state round trip -- but *replacing* an instance does, and a project
//!    load is exactly that.
//!
//! [`save_state`]: LoadedClapPlugin::save_state
//! [`load_state`]: LoadedClapPlugin::load_state
//!
//! # Threading
//! `clap_plugin_state` is `[main-thread]` in both directions. Every function
//! here takes `&mut LoadedClapPlugin`, which is `!Send`, so there is no way to
//! call one from the wrong thread by accident.
//!
//! # A plugin with no `state` extension
//! Per the CLAP specification a plugin that does not implement `clap.state` has
//! no state to persist. That is a normal answer, not a failure:
//! [`save_state`] returns an **empty** blob and [`load_state`] is a **no-op**
//! that reports success. The workspace's own gain fixture is deliberately such a
//! plugin, so the path is exercised rather than assumed.
//!
//! # Hostile input
//! A plugin controls how many bytes it writes. `save` therefore writes through a
//! cap ([`MAX_STATE_BYTES`]): a plugin that streams forever produces a typed
//! error instead of an out-of-memory abort in the DAW. Every call is wrapped in
//! [`catch_unwind`], like every other place vendor code is entered.

use std::io::{self, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::Ordering;

use clack_extensions::state::PluginState;

use crate::hosting::LoadedClapPlugin;

/// Largest state blob MYDAW will accept from, or hand to, a plugin.
///
/// 64 MiB is far more than any sane plugin needs (a sampler that embeds its
/// samples is the realistic worst case) and far less than "until the machine
/// dies". A plugin that exceeds it gets [`ClapStateError::TooLarge`], and the
/// partial blob is discarded rather than saved half-written.
pub const MAX_STATE_BYTES: usize = 64 * 1024 * 1024;

/// Why a state round trip failed.
///
/// Every variant is reachable from plugin-controlled behaviour, and none of
/// them is fatal to the DAW: a plugin that will not save its state is a plugin
/// whose settings are lost, not a session that has to end.
#[derive(Debug, thiserror::Error)]
pub enum ClapStateError {
    /// `clap_plugin_state->save` returned false.
    #[error("CLAP plugin '{id}' refused to save its state")]
    Save { id: String },

    /// `clap_plugin_state->load` returned false -- typically a blob from a
    /// different plugin, or from a version that no longer understands it.
    #[error("CLAP plugin '{id}' refused to load the state it was given")]
    Load { id: String },

    /// The plugin wrote more than [`MAX_STATE_BYTES`].
    #[error("CLAP plugin '{id}' tried to write more than {MAX_STATE_BYTES} bytes of state")]
    TooLarge { id: String },

    /// The blob handed to [`LoadedClapPlugin::load_state`] is itself too large.
    /// Reachable from a corrupted or hostile project file.
    #[error("a state blob of {bytes} bytes is larger than the {MAX_STATE_BYTES} byte limit")]
    BlobTooLarge { bytes: usize },

    /// Plugin code panicked through the FFI boundary.
    #[error("CLAP plugin '{id}' panicked while {stage} its state")]
    Panicked { id: String, stage: &'static str },
}

/// A `Write` sink that refuses to grow past `limit`.
///
/// `clap_ostream->write` is a loop the *plugin* drives, so the only way a host
/// bounds it is by failing a write. Returning an error is exactly what the
/// stream contract says to do, and a well-behaved plugin turns it into a failed
/// `save` rather than a crash.
struct CappedWriter {
    buffer: Vec<u8>,
    limit: usize,
    /// Set when a write was refused, so the caller can tell "the plugin said no"
    /// from "we said no".
    overflowed: bool,
}

impl CappedWriter {
    const fn new(limit: usize) -> Self {
        Self {
            buffer: Vec::new(),
            limit,
            overflowed: false,
        }
    }
}

impl Write for CappedWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.buffer.len().saturating_add(data.len()) > self.limit {
            self.overflowed = true;
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "plugin state exceeds the host's size limit",
            ));
        }
        self.buffer.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl LoadedClapPlugin {
    /// The plugin's `clap.state` extension, if it implements one.
    ///
    /// `[main-thread]`, like every extension query.
    pub fn state_extension(&mut self) -> Option<PluginState> {
        self.plugin_handle().get_extension::<PluginState>()
    }

    /// Whether this plugin has any state to persist at all.
    ///
    /// False is a normal answer: plenty of plugins (the workspace's gain
    /// fixture among them) have no parameters and therefore nothing to save.
    pub fn has_state(&mut self) -> bool {
        self.state_extension().is_some()
    }

    /// Take the "my state changed since you last saved it" flag, clearing it.
    ///
    /// Set by `clap_host_state->mark_dirty`, which a plugin calls when the user
    /// turns a knob in its editor. Step 8 turns this into a modified-project
    /// marker; step 6 only records it so the plumbing is proven.
    pub fn take_state_dirty(&self) -> bool {
        self.state_dirty().swap(false, Ordering::AcqRel)
    }

    /// Whether the plugin has reported a state change that has not been read.
    pub fn is_state_dirty(&self) -> bool {
        self.state_dirty().load(Ordering::Acquire)
    }

    /// Ask the plugin to serialise itself.
    ///
    /// Returns an **empty** blob for a plugin with no `state` extension -- see
    /// the module docs; that is what "this plugin has no state" looks like, and
    /// it round-trips through [`load_state`](Self::load_state) as a no-op.
    ///
    /// # Errors
    /// [`ClapStateError::Save`] when the plugin returns failure,
    /// [`ClapStateError::TooLarge`] when it writes past [`MAX_STATE_BYTES`], and
    /// [`ClapStateError::Panicked`] when it panics through the FFI boundary.
    pub fn save_state(&mut self) -> Result<Vec<u8>, ClapStateError> {
        let Some(state) = self.state_extension() else {
            return Ok(Vec::new());
        };
        let id = self.info().id.clone();

        let mut writer = CappedWriter::new(MAX_STATE_BYTES);
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            state.save(&mut self.plugin_handle(), &mut writer)
        }))
        .map_err(|_| ClapStateError::Panicked {
            id: id.clone(),
            stage: "saving",
        })?;

        if writer.overflowed {
            return Err(ClapStateError::TooLarge { id });
        }
        outcome.map_err(|_| ClapStateError::Save { id })?;
        Ok(writer.buffer)
    }

    /// Hand a blob from [`save_state`](Self::save_state) back to the plugin.
    ///
    /// An **empty** blob is a no-op that reports success, and so is any blob
    /// given to a plugin with no `state` extension: both mean "there was nothing
    /// to restore", which is a state, not an error.
    ///
    /// # Errors
    /// [`ClapStateError::Load`] when the plugin rejects the blob (a blob from a
    /// different plugin, or one it no longer understands),
    /// [`ClapStateError::BlobTooLarge`] for a blob past [`MAX_STATE_BYTES`], and
    /// [`ClapStateError::Panicked`] when it panics through the FFI boundary.
    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), ClapStateError> {
        if bytes.len() > MAX_STATE_BYTES {
            return Err(ClapStateError::BlobTooLarge { bytes: bytes.len() });
        }
        if bytes.is_empty() {
            return Ok(());
        }
        let Some(state) = self.state_extension() else {
            log::debug!(
                "CLAP plugin '{}' has no state extension; {} byte(s) of saved state are ignored",
                self.info().id,
                bytes.len()
            );
            return Ok(());
        };
        let id = self.info().id.clone();

        let mut reader = io::Cursor::new(bytes);
        catch_unwind(AssertUnwindSafe(|| {
            state.load(&mut self.plugin_handle(), &mut reader)
        }))
        .map_err(|_| ClapStateError::Panicked {
            id: id.clone(),
            stage: "loading",
        })?
        .map_err(|_| ClapStateError::Load { id })?;

        // A plugin that has just been told what it is cannot also be dirty
        // relative to that: `load` legitimately fires `mark_dirty` in some
        // implementations, and carrying that into the session would mark a
        // freshly-opened project as modified.
        self.state_dirty().store(false, Ordering::Release);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_capped_writer_accepts_everything_up_to_the_limit() {
        let mut writer = CappedWriter::new(8);
        writer.write_all(&[1, 2, 3, 4]).expect("under the limit");
        writer.write_all(&[5, 6, 7, 8]).expect("exactly the limit");
        assert!(!writer.overflowed);
        assert_eq!(writer.buffer, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn the_capped_writer_refuses_the_write_that_would_cross_the_limit() {
        // A plugin that streams forever must produce an error, not an
        // out-of-memory abort in the DAW.
        let mut writer = CappedWriter::new(4);
        writer.write_all(&[0; 3]).expect("under the limit");
        let err = writer.write_all(&[0; 2]).expect_err("would cross it");
        assert_eq!(err.kind(), io::ErrorKind::WriteZero);
        assert!(writer.overflowed);
        assert_eq!(
            writer.buffer.len(),
            3,
            "the refused write must not be partially applied"
        );
    }

    #[test]
    fn a_single_oversized_write_is_refused_rather_than_wrapping() {
        // `len + data.len()` is computed with `saturating_add` so a hostile
        // length cannot wrap around the limit check.
        let mut writer = CappedWriter::new(16);
        assert!(writer.write(&[0; 17]).is_err());
        assert!(writer.overflowed);
        assert!(writer.buffer.is_empty());
    }

    #[test]
    fn the_size_limit_is_generous_but_finite() {
        assert_eq!(MAX_STATE_BYTES, 64 * 1024 * 1024);
    }

    #[test]
    fn every_error_names_the_plugin_or_the_size_that_caused_it() {
        // These messages reach a status bar; "state error" would tell a user
        // nothing they can act on.
        let save = ClapStateError::Save {
            id: "com.acme.thing".to_owned(),
        };
        assert!(save.to_string().contains("com.acme.thing"), "{save}");
        let big = ClapStateError::BlobTooLarge { bytes: 999 };
        assert!(big.to_string().contains("999"), "{big}");
        let panicked = ClapStateError::Panicked {
            id: "com.acme.thing".to_owned(),
            stage: "saving",
        };
        assert!(panicked.to_string().contains("saving"), "{panicked}");
    }
}
