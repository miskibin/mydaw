//! `mydaw-app` -- the shell.
//!
//! Everything above the microkernel is a plugin; this crate is what turns that
//! pile of plugins into an application:
//!
//! 1. build a [`Kernel`](mydaw_kernel::Kernel) and register the Phase 0 plugin
//!    set (metering, debug-tap, test-tone, instruments, audio-io, host-clap,
//!    plugin-windows),
//! 2. boot it -- dependency order and lifecycle are the kernel's problem,
//! 3. snapshot plugins and contributed panels for the webview,
//! 4. open the audio device and publish the engine graph -- a swappable source
//!    ([`Source`]) into an optional CLAP effect into the transport gate,
//!    starting on the built-in poly synth (or a CLAP instrument when
//!    [`CLAP_INSTRUMENT_ENV`] names one),
//! 5. run a Tauri v2 window over the SolidJS UI in `ui/`, exposing the transport,
//!    note, plugin and capture commands plus one meter stream,
//! 6. stop audio and shut the kernel down when the window closes.
//!
//! # Headless capture mode
//! With [`DEBUG_CAPTURE_ENV`] set, steps 5 and 6 are replaced by: play, capture
//! that many seconds of the engine's output, write a WAV/PNG/JSON set under
//! `.debug/captures/`, print a report between
//! [`REPORT_BEGIN`]/[`REPORT_END`] markers, and exit -- no window is created at
//! all. That is the interface an automated agent uses to *see* what the DAW is
//! playing:
//!
//! ```text
//! MYDAW_DEBUG_CAPTURE=2 cargo run -p mydaw-app --features custom-protocol
//! ```
//! Exit code 0 if audio was captured, 1 if it was not. This mode starts on the
//! **test tone** rather than the shell's usual default: it presses Play with
//! nobody at the keyboard, and an instrument that is silent until it is played
//! would (correctly) record silence.
//!
//! # IPC benchmark mode
//! With `MYDAW_IPC_BENCH=1` the window opens as usual, but the UI immediately
//! runs the spike-4 transport matrix, writes `.debug/ipc-bench/<timestamp>.json`,
//! prints a report between [`ipc_bench::REPORT_BEGIN`]/[`ipc_bench::REPORT_END`]
//! and exits. `MYDAW_DEV_PANELS=1` contributes the same panel without running
//! anything, for measuring by hand. See [`ipc_bench`].
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
pub mod debug_capture;
mod dto;
mod graph;
pub mod gui_bridge;
pub mod gui_host;
pub mod ipc_bench;
mod meters;
pub mod source;
mod state;

pub use debug_capture::{
    capture_root, capture_to_disk, captures_dir, debug_capture_seconds_from_env,
    parse_capture_seconds, report_text, run_headless_capture, CaptureArtifacts, CaptureOutcome,
    CaptureReportDto, DebugCaptureError, CAPTURE_ROOT_ENV, DEBUG_CAPTURE_ENV, REPORT_BEGIN,
    REPORT_END,
};
pub use dto::{ClapPluginInfoDto, MeterFrameDto};
pub use graph::{
    clap_effect_node, clap_fx_spec_from_env, clap_instrument_node, clap_instrument_spec_from_env,
    clap_node, clap_spec_from_env, engine_graph, install_engine_graph, load_env_clap_effect,
    load_env_clap_instrument, load_env_clap_plugin, parse_clap_plugin_spec, source_chain,
    ClapPluginEnvError, ClapPluginSpec, TransportGate, CLAP_FX_ENV, CLAP_INSTRUMENT_ENV,
};
pub use gui_bridge::{ClapGuiBridge, ClapGuiCommand, ClapGuiStatusDto, GUI_REQUEST_TIMEOUT};
pub use gui_host::{ClapGuiHost, NO_PLUGIN_MESSAGE};
pub use ipc_bench::{
    auto_mode as ipc_bench_auto_mode, BenchReport, BenchTransport, StreamConfig, DEV_PANELS_ENV,
    IPC_BENCH_ENV, IPC_BENCH_PANEL,
};
pub use meters::{FrameSink, MeterPump, POLL_INTERVAL};
pub use source::{
    serve_clap_source_requests, ClapSourceBridge, Source, SourceDto, SourceError, SourceHandle,
    SourceSlot, CLAP_SOURCE_TIMEOUT,
};
pub use state::{boot_kernel, ui_snapshot, AppState, NoteError, StartupError, KERNEL_STATUS_PANEL};

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

    // Loaded before the device opens, activated after: the plugins need the
    // negotiated sample rate. Owned here, on the main thread, because a
    // `LoadedClapPlugin` is `!Send` and must outlive the node it hands the graph.
    let mut clap_instrument = load_env_clap_instrument(state.clap());
    let mut clap_fx = load_env_clap_effect(state.clap());
    // Published so the UI can offer (or grey out) the CLAP source, and so a
    // `set_source("clap")` on a session without one fails immediately with a
    // sentence instead of after a five-second round trip.
    state.clap_source().set_available(clap_instrument.is_some());

    // A configured CLAP instrument still wins at startup -- it is what the user
    // asked for by setting the variable. Otherwise the built-in synth, so the
    // keyboard plays something the moment the window opens.
    //
    // Except in headless capture mode, which presses Play and records whatever
    // comes out. An instrument is silent until somebody plays it, and there is
    // nobody there, so the capture would correctly report "no audio" and exit
    // non-zero. That mode wants the free-running tone, which is exactly what it
    // has always recorded.
    let startup_source = if clap_instrument.is_some() {
        Source::Clap
    } else if debug_capture_seconds_from_env().is_some() {
        Source::TestTone
    } else {
        Source::DEFAULT
    };

    match state.start_audio_with_source(startup_source, clap_instrument.as_mut(), clap_fx.as_mut())
    {
        Ok(info) => log::info!(
            "audio output: {} @ {} Hz, {} ch (max block {}); source '{}' installed",
            info.device_name,
            info.sample_rate,
            info.channels,
            info.max_block,
            state.current_source().id()
        ),
        // Headless machines, RDP sessions and CI runners land here. The window,
        // the panels and the plugin list all still work; only sound is missing.
        Err(err) => log::warn!("audio output unavailable ({err}); running without sound"),
    }

    // Headless capture: no window, no event loop, no user. Everything the
    // capture needs (device, graph, transport) is already up, so this runs
    // here and the process exits without ever creating a webview.
    match debug_capture_seconds_from_env() {
        Some(Ok(seconds)) => {
            let captured = run_headless_capture(&state, seconds);
            shutdown(
                &state,
                clap_instrument.as_mut(),
                clap_fx.as_mut(),
                &mut kernel,
            );
            return if captured {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
        }
        Some(Err(message)) => {
            eprintln!("MYDAW: {message}");
            shutdown(
                &state,
                clap_instrument.as_mut(),
                clap_fx.as_mut(),
                &mut kernel,
            );
            return ExitCode::FAILURE;
        }
        None => {}
    }

    // Handles for the shutdown path and the run loop, cloned before `state` moves
    // into Tauri.
    let meters = std::sync::Arc::clone(state.meters());
    let audio = std::sync::Arc::clone(state.audio());
    let gui_bridge = std::sync::Arc::clone(state.gui());
    // The CLAP source path: the mailbox a command posts to, and the slot the
    // main thread installs into. Both are `Arc`s, cloned before `state` moves
    // into Tauri's managed state.
    let clap_source = std::sync::Arc::clone(state.clap_source());
    let source_handle = std::sync::Arc::clone(state.source_handle());
    // The window service belongs to this thread -- the one tao's loop will run on
    // -- which is exactly what makes it legal to open plugin windows from the run
    // callback below.
    let window_service = kernel
        .ctx()
        .services
        .get::<mydaw_plugin_windows::PluginWindowService>()
        .expect("plugin-windows provides PluginWindowService; AppState checked at boot");
    let mut gui_host = ClapGuiHost::new(window_service);

    let app = tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            commands::get_ui_snapshot,
            commands::get_clap_plugins,
            commands::transport_play,
            commands::transport_stop,
            commands::note_on,
            commands::note_off,
            commands::get_sources,
            commands::set_source,
            commands::debug_capture,
            commands::open_clap_gui,
            commands::close_clap_gui,
            commands::subscribe_meters,
            // Spike 4's benchmark surface. Registered unconditionally (an
            // unused match arm is free); reachable only when the bench panel is
            // in the snapshot, which takes an environment variable.
            commands::bench_ping,
            commands::bench_ping_async,
            commands::bench_plan,
            commands::bench_sync,
            commands::bench_stream,
            commands::bench_meter_stamp,
            commands::bench_finish,
        ])
        .build(tauri::generate_context!());

    let app = match app {
        Ok(app) => app,
        Err(err) => {
            eprintln!("MYDAW: could not create the application window: {err}");
            meters.shutdown();
            audio.stop();
            shutdown_clap(clap_instrument.as_mut(), "instrument");
            shutdown_clap(clap_fx.as_mut(), "effect");
            kernel.shutdown();
            return ExitCode::FAILURE;
        }
    };

    // Waking the loop: a plugin-GUI request arrives on a command thread while the
    // loop may be blocked in `GetMessage` with nothing to do. `run_on_main_thread`
    // posts an event, which makes the loop turn once -- and one turn is all the
    // bridge needs, because the drain happens in `MainEventsCleared`.
    {
        let handle = app.handle().clone();
        gui_bridge.set_waker(Box::new(move || {
            let _ = handle.run_on_main_thread(|| {});
        }));
        let handle = app.handle().clone();
        clap_source.set_waker(Box::new(move || {
            let _ = handle.run_on_main_thread(|| {});
        }));
    }

    // The run loop owns the kernel. `RunEvent::Exit` fires after the last window
    // is gone, which is where the process tears itself down in order: stop
    // pushing frames at a dead webview, close the device, then stop plugins in
    // reverse dependency order.
    app.run(move |_handle, event| match event {
        // Every pass of the event loop, and therefore the plugin GUI's heartbeat:
        // CLAP timers, `on_main_thread` callbacks, editor resize negotiation and
        // the open/close requests the webview queued. While an editor is open its
        // window's own `WM_TIMER` keeps this firing at ~60 Hz; the rest of the
        // time it fires whenever anything else happens, which is exactly when
        // there is something to do.
        // The editor surface still hosts one plugin: the effect when there is
        // one, otherwise the instrument. A second editor window means a second
        // GUI session and a second `LoadedClapPlugin` to pump, which is a Phase
        // 2 concern (one editor per track insert) rather than a step-2 one.
        tauri::RunEvent::MainEventsCleared => {
            // The one source only this thread can build. Served before the GUI
            // tick so that switching to the CLAP instrument and opening its
            // editor in quick succession happen in the order they were asked
            // for.
            serve_clap_source_requests(
                &clap_source,
                &source_handle,
                clap_instrument.as_mut(),
                audio
                    .stream_info()
                    .map(|info| (f64::from(info.sample_rate), info.max_block)),
            );
            gui_host.tick(clap_fx.as_mut().or(clap_instrument.as_mut()), &gui_bridge);
        }
        tauri::RunEvent::Exit => {
            log::info!("shutting down");
            meters.shutdown();
            // Before anything else: the plugin's editor must be destroyed while
            // its bundle is still loaded. A window whose procedure lives in an
            // unloaded DLL crashes the process on the next message.
            gui_host.shutdown(clap_fx.as_mut().or(clap_instrument.as_mut()));
            // Closing the stream retires the graph, which is what eventually
            // drops the CLAP nodes and sends their processors back here. Only
            // then can the plugins be deactivated on this (main) thread.
            audio.stop();
            shutdown_clap(clap_instrument.as_mut(), "instrument");
            shutdown_clap(clap_fx.as_mut(), "effect");
            kernel.shutdown();
        }
        _ => {}
    });

    ExitCode::SUCCESS
}

/// Tear everything down in the same order the window-close path uses.
fn shutdown(
    state: &AppState,
    clap_instrument: Option<&mut mydaw_plugin_host_clap::LoadedClapPlugin>,
    clap_fx: Option<&mut mydaw_plugin_host_clap::LoadedClapPlugin>,
    kernel: &mut mydaw_kernel::Kernel,
) {
    state.meters().shutdown();
    // Closing the stream retires the graph, which is what eventually drops the
    // CLAP nodes and sends their processors back to the main thread.
    state.audio().stop();
    shutdown_clap(clap_instrument, "instrument");
    shutdown_clap(clap_fx, "effect");
    kernel.shutdown();
}

/// Deactivate and destroy a hosted CLAP plugin, on the main thread.
///
/// Waits for the node the audio graph owns to be retired by the `basedrop`
/// collector; see `mydaw_plugin_host_clap::hosting` for why that wait exists.
/// A timeout is not fatal -- clack leaks the instance rather than destroying it
/// from the wrong thread -- but it is worth a log line.
fn shutdown_clap(plugin: Option<&mut mydaw_plugin_host_clap::LoadedClapPlugin>, role: &str) {
    let Some(plugin) = plugin else { return };
    if plugin.shutdown() {
        log::info!("CLAP {role} '{}' deactivated", plugin.info().id);
    } else {
        log::warn!(
            "CLAP {role} '{}' was still active at shutdown; its instance will be leaked",
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
