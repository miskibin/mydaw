//! The graceful path: a plugin with no editor at all.
//!
//! Plenty of real CLAP effects ship without a GUI, so "no editor" is a normal
//! answer and must reach the user as a clear message rather than as a failure of
//! anything. The fixture drops its `gui` and `timer-support` extensions when
//! `MYDAW_FIXTURE_GUI=0`, which is what makes this testable without a second
//! fixture plugin.
//!
//! **This file is its own test binary on purpose.** The environment variable is
//! process-wide and is read when the plugin's extensions are queried, so a test
//! that sets it cannot share a process with tests that expect an editor.

#![cfg(windows)]

use std::path::PathBuf;

use mydaw_fixture_clap_gain::{GUI_ENV, PLUGIN_ID};
use mydaw_plugin_host_clap::gui::{ClapGuiError, ClapGuiSession};
use mydaw_plugin_host_clap::hosting::LoadedClapPlugin;
use mydaw_plugin_host_clap::test_util::{missing_fixture_message, stage_fixture_bundle};
use mydaw_plugin_windows::PluginWindowService;

#[test]
fn a_plugin_without_an_editor_is_a_typed_error() {
    // Set before anything is loaded, and this binary has exactly one test, so
    // nothing else can observe the change.
    std::env::set_var(GUI_ENV, "0");

    let directory: PathBuf = std::env::temp_dir().join(format!(
        "mydaw-clap-nogui-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&directory).expect("scratch dir");

    let Some(bundle) = stage_fixture_bundle(&directory) else {
        eprintln!("{}", missing_fixture_message());
        let _ = std::fs::remove_dir_all(&directory);
        return;
    };

    let mut plugin = LoadedClapPlugin::load(&bundle, PLUGIN_ID).expect("the fixture must load");
    let windows = PluginWindowService::new();

    let err = ClapGuiSession::open(&mut plugin, &windows, "MYDAW Fixture Gain")
        .expect_err("a plugin without the gui extension cannot open an editor");
    assert!(
        matches!(err, ClapGuiError::NoGuiSupport),
        "expected NoGuiSupport, got {err:?}"
    );
    assert!(
        err.to_string().contains("no editor"),
        "this message is shown to the user as-is: {err}"
    );
    assert_eq!(
        windows.open_count(),
        0,
        "a refused editor must not leave a window behind"
    );

    // The plugin is untouched by the refusal: it still processes audio.
    assert!(plugin.create_node(48_000.0, 256).is_ok());

    let _ = std::fs::remove_dir_all(&directory);
}
