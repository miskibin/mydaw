//! `mydaw-fixture-clap-gain` -- a real CLAP plugin, built for the test suite.
//!
//! Spike 2 hosts CLAP plugins. Testing that against whatever the developer
//! happens to have installed is not a test, it is a coincidence: the assertions
//! would depend on a third party's DSP, the machine, and the day. So the
//! workspace builds its own plugin instead.
//!
//! The DSP is the most boring thing that is still observable end to end:
//! **stereo in, stereo out, every sample multiplied by [`GAIN`] (0.5, -6.02 dB)**.
//! No parameters, no GUI, no state, no events, no latency, no randomness --
//! `output == input * 0.5`, sample for sample, forever. That makes the host-side
//! assertions exact rather than approximate.
//!
//! # Why `crate-type = ["cdylib", "rlib"]`
//! The `cdylib` is the artifact under test: `cargo build` writes
//! `target/<profile>/mydaw_fixture_clap_gain.dll`, which the host-side tests copy
//! to a temp directory as `mydaw-gain.clap` and load through the real OS loader,
//! exactly like a vendor plugin. The `rlib` exists so that [`PLUGIN_ID`] and
//! [`GAIN`] are compile-time constants shared with the tests instead of string
//! literals duplicated on both sides of the FFI boundary.
//!
//! This crate is a test fixture. It is never shipped.

pub mod gui;

use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
};
use clack_extensions::gui::{
    GuiConfiguration, GuiResizeHints, GuiSize, PluginGui, PluginGuiImpl, Window,
};
use clack_extensions::timer::{PluginTimer, PluginTimerImpl, TimerId};
use clack_plugin::plugin::PluginError;
use clack_plugin::prelude::*;

use crate::gui::FixtureGui;

/// CLAP id of the fixture. Hosts address the plugin by this string.
pub const PLUGIN_ID: &str = "dev.mydaw.fixture.gain";

/// Display name reported in the plugin descriptor.
pub const PLUGIN_NAME: &str = "MYDAW Fixture Gain";

/// Vendor reported in the plugin descriptor.
pub const PLUGIN_VENDOR: &str = "MYDAW";

/// Version reported in the plugin descriptor.
pub const PLUGIN_VERSION: &str = "0.1.0";

/// The one and only thing this plugin does: -6.02 dB, exactly.
///
/// 0.5 is chosen because it is exact in binary floating point, so a host test can
/// assert `out == in * 0.5` bit-for-bit if it wants to, with no epsilon fudge.
pub const GAIN: f32 = 0.5;

/// Channel count of both the input and the output port.
pub const CHANNELS: u32 = 2;

/// One stereo port in each direction.
pub const PORT_COUNT: u32 = 1;

/// Environment variable that removes the `gui` and `timer-support` extensions
/// from this plugin (`MYDAW_FIXTURE_GUI=0`).
///
/// A test affordance, and one that only exists because this is a test fixture: it
/// is how the host's "this plugin has no editor" path
/// (`ClapGuiError::NoGuiSupport`) is exercised end to end without a second
/// fixture plugin. Read when the host queries extensions, which is before any
/// editor exists.
pub const GUI_ENV: &str = "MYDAW_FIXTURE_GUI";

/// Whether this instance should advertise an editor.
fn gui_enabled() -> bool {
    !matches!(
        std::env::var(GUI_ENV).ok().as_deref(),
        Some("0") | Some("off") | Some("false")
    )
}

/// The plugin type. Stateless -- everything it needs is a constant.
pub struct GainPlugin;

impl Plugin for GainPlugin {
    type AudioProcessor<'a> = GainProcessor;
    type Shared<'a> = ();
    type MainThread<'a> = GainMainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&()>) {
        // Without this extension a CLAP plugin formally has *no* audio ports, and
        // a correct host would refuse to send it audio. Declaring one stereo in
        // and one stereo out is what makes this fixture a realistic subject for
        // the host's port negotiation, not just for its process loop.
        builder.register::<PluginAudioPorts>();

        if gui_enabled() {
            // Spike 3's subject: a real embedded Win32 editor, animated only by
            // host timers. `timer-support` is not an optional extra here --
            // without it the editor visibly freezes, which is the point.
            builder.register::<PluginGui>();
            builder.register::<PluginTimer>();
        }
    }
}

impl DefaultPluginFactory for GainPlugin {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(PLUGIN_ID, PLUGIN_NAME)
            .with_vendor(PLUGIN_VENDOR)
            .with_version(PLUGIN_VERSION)
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<(), PluginError> {
        Ok(())
    }

    fn new_main_thread<'a>(
        host: HostMainThreadHandle<'a>,
        _shared: &'a (),
    ) -> Result<GainMainThread<'a>, PluginError> {
        Ok(GainMainThread {
            host,
            gui: FixtureGui::new(),
        })
    }
}

/// Main-thread half: the audio-ports description, and the editor.
///
/// It holds the host handle because the editor needs it -- `clap.timer-support`
/// is a *host* service, and registering a timer is a main-thread call.
pub struct GainMainThread<'a> {
    host: HostMainThreadHandle<'a>,
    gui: FixtureGui,
}

impl<'a> PluginMainThread<'a, ()> for GainMainThread<'a> {}

impl PluginAudioPortsImpl for GainMainThread<'_> {
    fn count(&mut self, _is_input: bool) -> u32 {
        PORT_COUNT
    }

    fn get(&mut self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        if index != 0 {
            return;
        }
        writer.set(&AudioPortInfo {
            id: ClapId::new(0),
            name: if is_input {
                b"Stereo In"
            } else {
                b"Stereo Out"
            },
            channel_count: CHANNELS,
            flags: AudioPortFlags::IS_MAIN,
            port_type: Some(AudioPortType::STEREO),
            // No in-place pair: the host must hand over distinct input and output
            // buffers, which is the layout MYDAW's `ClapNode` actually uses.
            in_place_pair: None,
        });
    }
}

/// Audio-thread half.
///
/// Stateless on purpose: with no filter state or phase accumulator, two identical
/// input blocks produce two identical output blocks, so a test can call `process`
/// repeatedly and compare results without modelling any history.
pub struct GainProcessor;

impl<'a> PluginAudioProcessor<'a, (), GainMainThread<'a>> for GainProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut GainMainThread<'a>,
        _shared: &'a (),
        _audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        Ok(Self)
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        _events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        for mut port_pair in &mut audio {
            // 32-bit only. A host asking for f64 gets silence rather than a
            // wrong answer, and the fixture never advertises 64-bit support.
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else {
                continue;
            };

            for channel_pair in channel_pairs {
                match channel_pair {
                    // Input with no matching output: nothing to write.
                    ChannelPair::InputOnly(_) => {}
                    // Output with no matching input: define it, do not leave the
                    // host's uninitialised scratch to leak through as noise.
                    ChannelPair::OutputOnly(output) => output.fill(0.0),
                    ChannelPair::InputOutput(input, output) => {
                        for (input, output) in input.iter().zip(output) {
                            *output = *input * GAIN;
                        }
                    }
                    ChannelPair::InPlace(buffer) => {
                        for sample in buffer {
                            *sample *= GAIN;
                        }
                    }
                }
            }
        }

        // Always `Continue`: a `Sleep` would let a host skip process() calls, and
        // this fixture exists to make host behaviour predictable, not to be
        // clever about idling.
        Ok(ProcessStatus::Continue)
    }
}

/// The editor. Every method is a thin adapter over [`FixtureGui`]; the
/// interesting behaviour (size negotiation, the animation, the painted state)
/// is documented in [`gui`].
impl PluginGuiImpl for GainMainThread<'_> {
    fn is_api_supported(&mut self, configuration: GuiConfiguration) -> bool {
        self.gui.is_api_supported(configuration)
    }

    fn get_preferred_api(&mut self) -> Option<GuiConfiguration<'_>> {
        self.gui.preferred_api()
    }

    fn create(&mut self, configuration: GuiConfiguration) -> Result<(), PluginError> {
        self.gui.create(&mut self.host, configuration)
    }

    fn destroy(&mut self) {
        self.gui.destroy(&mut self.host);
    }

    fn set_scale(&mut self, scale: f64) -> Result<(), PluginError> {
        self.gui.set_scale(scale)
    }

    fn get_size(&mut self) -> Option<GuiSize> {
        Some(self.gui.size())
    }

    fn can_resize(&mut self) -> bool {
        self.gui.can_resize()
    }

    fn get_resize_hints(&mut self) -> Option<GuiResizeHints> {
        Some(self.gui.resize_hints())
    }

    fn adjust_size(&mut self, size: GuiSize) -> Option<GuiSize> {
        Some(gui::adjust(size))
    }

    fn set_size(&mut self, size: GuiSize) -> Result<(), PluginError> {
        self.gui.set_size(size)
    }

    fn set_parent(&mut self, window: Window) -> Result<(), PluginError> {
        self.gui.set_parent(window)
    }

    /// Embedded only: a fixture that accepted a transient window would let a host
    /// bug (opening the editor floating) pass unnoticed.
    fn set_transient(&mut self, _window: Window) -> Result<(), PluginError> {
        Err(PluginError::Message(
            "the fixture editor is embedded, never floating",
        ))
    }

    fn show(&mut self) -> Result<(), PluginError> {
        self.gui.show()
    }

    fn hide(&mut self) -> Result<(), PluginError> {
        self.gui.hide()
    }
}

/// The only thing in this plugin that advances the editor's animation.
impl PluginTimerImpl for GainMainThread<'_> {
    fn on_timer(&mut self, _timer_id: TimerId) {
        self.gui.on_timer();
    }
}

clack_export_entry!(SinglePluginEntry<GainPlugin>);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gain_is_exactly_minus_six_db_and_representable() {
        assert_eq!(GAIN, 0.5);
        // Exact in binary floating point: 0.2 * 0.5 is the same number a host
        // computes when it predicts the expected output.
        assert_eq!(0.2_f32 * GAIN, 0.1_f32);
        let db = 20.0 * f64::from(GAIN).log10();
        assert!(
            (db + 6.0206).abs() < 1e-3,
            "gain should be -6.02 dB, got {db}"
        );
    }

    #[test]
    fn the_descriptor_reports_the_advertised_identity() {
        let descriptor = GainPlugin::get_descriptor();
        assert_eq!(descriptor.id().unwrap().to_str().unwrap(), PLUGIN_ID);
        assert_eq!(descriptor.name().unwrap().to_str().unwrap(), PLUGIN_NAME);
        assert_eq!(
            descriptor.vendor().unwrap().to_str().unwrap(),
            PLUGIN_VENDOR
        );
        assert_eq!(
            descriptor.version().unwrap().to_str().unwrap(),
            PLUGIN_VERSION
        );
    }

    #[test]
    fn one_stereo_port_is_declared_in_each_direction() {
        // `GainMainThread` now needs a live host handle, which only a real host
        // can produce, so the port shape is asserted through the constants the
        // implementation returns. `plugins/host-clap` checks the same thing
        // through the actual FFI, against a loaded instance.
        assert_eq!(PORT_COUNT, 1);
        assert_eq!(CHANNELS, 2);
    }

    #[test]
    fn the_editor_is_advertised_unless_the_environment_switches_it_off() {
        // No variable is set in this process, so the editor is declared.
        assert!(gui_enabled());
        assert_eq!(GUI_ENV, "MYDAW_FIXTURE_GUI");
    }
}
