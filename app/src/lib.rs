//! `mydaw-app` -- the shell.
//!
//! Everything above the microkernel is a plugin; this crate is what turns that
//! pile of plugins into an application:
//!
//! 1. build a [`Kernel`](mydaw_kernel::Kernel) and register the Phase 0 plugin
//!    set (metering, test-tone, audio-io, host-clap, host-vst3, plugin-windows),
//! 2. boot it -- dependency order and lifecycle are the kernel's problem,
//! 3. snapshot plugins and contributed panels for the webview,
//! 4. open the audio device and publish the test-tone graph,
//! 5. run a Tauri v2 window over the SolidJS UI in `ui/`, exposing four commands
//!    and one meter stream,
//! 6. stop audio and shut the kernel down when the window closes.
//!
//! The kernel deliberately does *not* live in Tauri's managed state: plugins are
//! not `Send`. It stays on the main thread, owned by the run loop; the UI-facing
//! state is a snapshot plus `Arc`s to services (see [`state::AppState`]).
//!
//! # Running it
//! ```text
//! cargo tauri dev                                  # if the Tauri CLI is installed
//! pnpm --dir ui dev  &&  cargo run -p mydaw-app    # same thing, by hand
//! pnpm --dir ui build && cargo run -p mydaw-app --features custom-protocol
//! ```

mod commands;
mod dto;
mod graph;
mod meters;
mod state;

pub use dto::{ClapPluginInfoDto, MeterFrameDto};
pub use graph::{
    clap_effect_node, clap_fx_spec_from_env, install_tone_graph, load_env_clap_effect,
    parse_clap_fx_spec, tone_graph, ClapEffectSpec, ClapFxEnvError, TransportGate, CLAP_FX_ENV,
};
pub use meters::{FrameSink, MeterPump, POLL_INTERVAL};
pub use state::{boot_kernel, ui_snapshot, AppState, StartupError, KERNEL_STATUS_PANEL};

use std::process::ExitCode;

/// Boot the kernel, open the window, run until the user closes it.
///
/// Returns [`ExitCode::FAILURE`] on a startup failure that leaves nothing worth
/// showing: a kernel boot error (dependency cycle, missing plugin service) or a
/// window that cannot be created. A missing audio device is *not* one of those --
/// the UI still boots and says so.
pub fn run() -> ExitCode {
    init_logging();

    let mut kernel = match boot_kernel() {
        Ok(kernel) => kernel,
        Err(err) => {
            eprintln!("MYDAW: kernel boot failed: {err}");
            return ExitCode::FAILURE;
        }
    };
    log::info!("kernel booted: {}", kernel.boot_order().join(" -> "));

    let state = match AppState::from_kernel(&kernel) {
        Ok(state) => state,
        Err(err) => {
            eprintln!("MYDAW: {err}");
            kernel.shutdown();
            return ExitCode::FAILURE;
        }
    };

    // Loaded before the device opens, activated after: the plugin needs the
    // negotiated sample rate. Owned here, on the main thread, because a
    // `LoadedClapPlugin` is `!Send` and must outlive the node it hands the graph.
    let mut clap_fx = load_env_clap_effect(state.clap());

    match state.start_audio_with_effect(clap_fx.as_mut()) {
        Ok(info) => log::info!(
            "audio output: {} @ {} Hz, {} ch (max block {}); test-tone graph installed",
            info.device_name,
            info.sample_rate,
            info.channels,
            info.max_block
        ),
        // Headless machines, RDP sessions and CI runners land here. The window,
        // the panels and the plugin list all still work; only sound is missing.
        Err(err) => log::warn!("audio output unavailable ({err}); running without sound"),
    }

    // Handles for the shutdown path, cloned before `state` moves into Tauri.
    let meters = std::sync::Arc::clone(state.meters());
    let audio = std::sync::Arc::clone(state.audio());

    let app = tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            commands::get_ui_snapshot,
            commands::get_clap_plugins,
            commands::transport_play,
            commands::transport_stop,
            commands::subscribe_meters,
        ])
        .build(tauri::generate_context!());

    let app = match app {
        Ok(app) => app,
        Err(err) => {
            eprintln!("MYDAW: could not create the application window: {err}");
            meters.shutdown();
            audio.stop();
            shutdown_clap(clap_fx.as_mut());
            kernel.shutdown();
            return ExitCode::FAILURE;
        }
    };

    // The run loop owns the kernel. `RunEvent::Exit` fires after the last window
    // is gone, which is where the process tears itself down in order: stop
    // pushing frames at a dead webview, close the device, then stop plugins in
    // reverse dependency order.
    app.run(move |_handle, event| {
        if matches!(event, tauri::RunEvent::Exit) {
            log::info!("shutting down");
            meters.shutdown();
            // Closing the stream retires the graph, which is what eventually
            // drops the CLAP node and sends its processor back here. Only then
            // can the plugin be deactivated on this (main) thread.
            audio.stop();
            shutdown_clap(clap_fx.as_mut());
            kernel.shutdown();
        }
    });

    ExitCode::SUCCESS
}

/// Deactivate and destroy a hosted CLAP plugin, on the main thread.
///
/// Waits for the node the audio graph owns to be retired by the `basedrop`
/// collector; see `mydaw_plugin_host_clap::hosting` for why that wait exists.
/// A timeout is not fatal -- clack leaks the instance rather than destroying it
/// from the wrong thread -- but it is worth a log line.
fn shutdown_clap(plugin: Option<&mut mydaw_plugin_host_clap::LoadedClapPlugin>) {
    let Some(plugin) = plugin else { return };
    if plugin.shutdown() {
        log::info!("CLAP effect '{}' deactivated", plugin.info().id);
    } else {
        log::warn!(
            "CLAP effect '{}' was still active at shutdown; its instance will be leaked",
            plugin.info().id
        );
    }
}

/// `RUST_LOG` if set, otherwise `info`. `try_init` so a second call (tests,
/// embedding) is a no-op rather than a panic.
fn init_logging() {
    let env = env_logger::Env::default().default_filter_or("info");
    let _ = env_logger::Builder::from_env(env)
        .format_timestamp_millis()
        .try_init();
}
