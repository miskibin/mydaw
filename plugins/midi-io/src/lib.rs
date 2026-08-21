//! `mydaw-plugin-midi-io` — MIDI input ports, pairing, live notes.
//!
//! A sibling of `audio-io`: that crate owns the output device, this one owns
//! MIDI inputs. Parsed note-on / note-off events feed the same live-note
//! queue the typing keyboard already uses. The UI never sees raw MIDI bytes.
//!
//! ```text
//!   USB controller  -->  midir (WinMM)  -->  MidiParser  -->  HeldNotes
//!                                                              |
//!                                                              v
//!                                                    MidiNoteSink (shell)
//!                                                              |
//!                                                              v
//!                                                    AudioIoService::send_note
//! ```
//!
//! Boot does not open ports. [`MidiIoService::attach_store`] loads `midi.json`
//! and starts the hot-plug poller; [`MidiIoService::set_sink`] is what makes
//! notes audible.

pub mod held;
pub mod pairing;
pub mod parser;
pub mod ports;
pub mod service;

pub use pairing::{MidiDeviceSettings, MidiPortDto, MidiSettings, MidiStoreError};
pub use parser::{MidiEvent, MidiParser};
pub use ports::{FakeMidiPorts, MidiPorts, MidirPorts};
pub use service::{MidiIoService, MidiNoteSink, MidiStatusDto};

use std::sync::Arc;

use mydaw_kernel::{KernelCtx, Plugin, PluginManifest};

/// Kernel id of this plugin.
pub const PLUGIN_ID: &str = "midi-io";

const DEPS: &[&str] = &["audio-io"];

/// Owns the [`MidiIoService`].
pub struct MidiIoPlugin {
    service: Option<Arc<MidiIoService>>,
}

impl Default for MidiIoPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl MidiIoPlugin {
    pub fn new() -> Self {
        Self { service: None }
    }

    pub fn service(&self) -> Option<&Arc<MidiIoService>> {
        self.service.as_ref()
    }
}

impl Plugin for MidiIoPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest::with_deps(PLUGIN_ID, "MIDI I/O", DEPS)
    }

    fn register(&mut self, ctx: &mut KernelCtx) {
        let service = Arc::new(MidiIoService::with_midir());
        ctx.services
            .provide(Arc::clone(&service))
            .expect("MidiIoService provided twice: midi-io registered more than once");
        self.service = Some(service);
    }

    fn start(&mut self, _ctx: &KernelCtx) {
        // Ports open when the shell attaches midi.json. Starting here would
        // spawn a poller in every `boot_kernel` test.
    }

    fn stop(&mut self) {
        if let Some(service) = self.service.as_ref() {
            service.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mydaw_kernel::Kernel;

    #[test]
    fn midi_io_depends_on_audio_io() {
        assert_eq!(MidiIoPlugin::new().manifest().deps, &["audio-io"]);
    }

    #[test]
    fn missing_audio_io_dependency_refuses_to_boot() {
        let mut kernel = Kernel::new();
        kernel.add_plugin(Box::new(MidiIoPlugin::new())).unwrap();
        assert!(kernel.boot().is_err());
    }
}
