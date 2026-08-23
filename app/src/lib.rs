//! `mydaw-app` -- the shell.
//!
//! Everything above the microkernel is a plugin; this crate is what turns that
//! pile of plugins into an application:
//!
//! 1. build a [`Kernel`](mydaw_kernel::Kernel) and register the Phase 0 plugin
//!    set (metering, debug-tap, test-tone, instruments, sequencer, audio-io,
//!    host-clap, plugin-windows),
//! 2. boot it -- dependency order and lifecycle are the kernel's problem,
//! 3. snapshot plugins and contributed panels for the webview,
//! 4. open the audio device and publish the mixer graph -- one swappable
//!    [`Source`] per track into a transport gate into a strip, summed into a
//!    master bus with one swappable [`InsertSlot`],
//! 5. run a Tauri v2 window over the SolidJS UI in `ui/`, exposing the transport,
//!    note, mixer, pattern, tempo, plugin and capture commands plus one meter
//!    stream,
//! 6. own the hosted CLAP plugins ([`ClapHost`]) on this thread, and serve the
//!    webview's requests for them from the event loop's idle point,
//! 7. stop audio and shut the kernel down when the window closes.
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

mod audio_settings;
pub mod clap_bridge;
pub mod clap_host;
pub mod clips;
mod commands;
pub mod debug_capture;
mod drum_samples;
mod dto;
pub mod effects;
mod file_preview;
mod graph;
pub mod insert;
pub mod ipc_bench;
mod library;
mod meters;
mod preview;
mod preview_settings;
pub mod project;
pub mod project_bridge;
pub mod project_file;
pub mod project_host;
pub mod source;
pub mod spectrum;
mod state;
pub mod tracks;
pub mod vst3_bridge;
pub mod vst3_host;

pub use clap_bridge::{
    from_base64, to_base64, ClapAnswer, ClapBoard, ClapCommand, ClapGuiStatusDto, ClapHostBridge,
    ClapSlotDto, ClapTarget, NO_MAIN_THREAD, REQUEST_TIMEOUT,
};
pub use clap_host::ClapHost;
pub use clips::{audio_clip_node, AudioClipBus};
pub use debug_capture::{
    capture_root, capture_to_disk, captures_dir, debug_capture_seconds_from_env,
    parse_capture_seconds, report_text, run_headless_capture, CaptureArtifacts, CaptureOutcome,
    CaptureReportDto, DebugCaptureError, CAPTURE_ROOT_ENV, DEBUG_CAPTURE_ENV, REPORT_BEGIN,
    REPORT_END,
};
pub use dto::{
    ticks_for_seconds, AudioOutputDto, AudioSourceDto, ChannelClipDto, ClapPluginInfoDto, ClipDto,
    MelodicNoteDto, MeterFrameDto, PatternDto, PatternSetDto, PatternSummaryDto, PlaylistDto,
    PlaylistTrackDto, SequencerStateDto, StepDto, StepRowDto, StreamInfoDto, TrackMeterDto,
    Vst3PluginInfoDto, WaveformDto,
};
pub use effects::{
    catalogue, chain_dto, chain_readouts_dto, insert_dto, EffectInfoDto, InsertDef, InsertDto,
    InsertEdit, InsertError, InsertReadoutsDto, InsertTarget, ParamDefDto, ParamValueDto,
    ReadoutDefDto, SpectrumDto, MAX_INSERTS,
};
pub use graph::{engine_graph, source_chain, track_chain, TransportGate};
pub use insert::{insert_slot, InsertHandle, InsertSlot};
pub use ipc_bench::{
    auto_mode as ipc_bench_auto_mode, BenchReport, BenchTransport, StreamConfig, DEV_PANELS_ENV,
    IPC_BENCH_ENV, IPC_BENCH_PANEL,
};
pub use meters::{FrameSink, MeterPump, POLL_INTERVAL};
pub use preview::{PreviewError, SourcePreviewDto};
pub use project::{
    check_gain_db, check_pan, check_send_level, mixer_state_dto, BusDef, BusDto, MasterDto,
    MissingPluginDto, MissingPlugins, MixerStateDto, NoPitchReasonDto, PitchAnalysisDto,
    PitchSourceDto, Project, RootMode, RootPitchDto, RouteError, SendDef, SendDto, TrackDef,
    TrackDto, TrackError,
};
pub use project_bridge::{
    ProjectAnswer, ProjectBridge, ProjectCommand, ProjectInfoDto, PROJECT_TIMEOUT,
};
pub use project_file::{
    migrate_single_pattern, parse_project, project_name, read_project_file, temp_path_for,
    write_project_file, AudioSourceEntry, ClapEntry, ClipEntry, EffectEntry, EffectRef,
    LegacyNoteDto, LegacyPatternDto, LegacyRowDto, PlaylistEntry, PlaylistTrackEntry, ProjectFile,
    ProjectFileError, Song, SourceEntry, TrackEntry, Vst3Entry, FORMAT_VERSION, PROJECT_EXTENSION,
    UNTITLED,
};
pub use project_host::{
    attach, capture as capture_project, default_project_file, Hosts, LoadReport, ProjectHost,
    Session,
};
pub use source::{
    check_bundle_path, ClapSourceRef, SamplerSourceRef, SilentNode, Source, SourceDto, SourceError,
    SourceHandle, SourceKindDto, SourceSlot, Vst3SourceRef, SAMPLER_SOURCE_PREFIX,
};
pub use state::{
    boot_kernel, ui_snapshot, AppState, NoteError, SequencerError, StartupError,
    KERNEL_STATUS_PANEL, MIXER_PANEL,
};
pub use tracks::{MixerError, TrackHost};
pub use vst3_bridge::{
    Vst3Answer, Vst3Board, Vst3Command, Vst3GuiStatusDto, Vst3HostBridge, Vst3SlotDto, Vst3Target,
};
pub use vst3_host::Vst3Host;

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

    let audio_prefs = crate::audio_settings::load_settings(&crate::audio_settings::store_path());
    state.audio().set_output_request(audio_prefs.to_request());

    // The session's own tracks -- drums and a synth.
    //
    // Except in headless capture mode, which presses Play and records whatever
    // comes out. An instrument is silent until somebody plays it, and there is
    // nobody there, so the capture would correctly report "no audio" and exit
    // non-zero. That mode wants a single free-running tone track, which is
    // exactly what it has always recorded.
    let started = if debug_capture_seconds_from_env().is_some() {
        state.start_audio_with_source(Source::TestTone)
    } else {
        state.start_audio_with_project()
    };

    match started {
        Ok(info) => log::info!(
            "audio output: {} @ {} Hz, {} ch (max block {}); {} track(s), selected source '{}'",
            info.device_name,
            info.sample_rate,
            info.channels,
            info.max_block,
            state.tracks().track_count(),
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
            shutdown(&state, &mut kernel);
            return if captured {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
        }
        Some(Err(message)) => {
            eprintln!("MYDAW: {message}");
            shutdown(&state, &mut kernel);
            return ExitCode::FAILURE;
        }
        None => {}
    }

    // Handles for the shutdown path and the run loop, cloned before `state` moves
    // into Tauri.
    let meters = std::sync::Arc::clone(state.meters());
    let audio = std::sync::Arc::clone(state.audio());
    let midi = std::sync::Arc::clone(state.midi());
    // The CLAP path: the one mailbox a command posts to, the mixer it edits, and
    // the device it needs. All `Arc`s, cloned before `state` moves into Tauri's
    // managed state.
    let clap_bridge = std::sync::Arc::clone(state.clap_bridge());
    let vst3_bridge = std::sync::Arc::clone(state.vst3_bridge());
    let track_host = std::sync::Arc::clone(state.tracks());
    // The project path: saving asks every plugin for its state and loading
    // destroys and recreates all of them, so it is served from the run loop too
    // -- and from *here* rather than from either plugin host, because it is the
    // only place that holds both. See `crate::project_bridge`.
    let project_bridge = std::sync::Arc::clone(state.project_bridge());
    let sequencer = std::sync::Arc::clone(state.sequencer());
    let audio_sources = std::sync::Arc::clone(state.audio_sources());
    let mut project_host = ProjectHost::new();
    // The window service belongs to this thread -- the one tao's loop will run on
    // -- which is exactly what makes it legal to open plugin windows from the run
    // callback below.
    let window_service = kernel
        .ctx()
        .services
        .get::<mydaw_plugin_windows::PluginWindowService>()
        .expect("plugin-windows provides PluginWindowService; AppState checked at boot");
    let mut clap_host = ClapHost::new(std::sync::Arc::clone(&window_service));
    let mut vst3_host = Vst3Host::new(window_service);
    // Published before the first command can arrive, so a `get_tracks` during
    // startup sees an accurate (empty) board rather than nothing at all, and so
    // a hosted-plugin command fails with a sentence instead of a five-second
    // timeout.
    clap_host.publish(&track_host, clap_bridge.board());
    vst3_host.publish(&track_host, vst3_bridge.board());
    clap_bridge.set_served(true);
    vst3_bridge.set_served(true);
    project_bridge.set_served(true);

    let app = tauri::Builder::default()
        // Native Open/Save dialogs. Driven from Rust rather than from the
        // webview so the UI's mock backend keeps working in a plain browser --
        // see the project commands in `crate::commands`.
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            commands::get_ui_snapshot,
            commands::get_clap_plugins,
            commands::transport_play,
            commands::transport_stop,
            commands::transport_seek,
            commands::note_on,
            commands::note_off,
            commands::get_sources,
            commands::set_source,
            // The bundled sample content: is it installed, and install it.
            commands::get_asset_status,
            commands::start_asset_download,
            // The mixer.
            commands::get_tracks,
            commands::set_track_gain,
            commands::set_track_pan,
            commands::set_track_pitch,
            commands::analyze_track_pitch,
            commands::set_track_sample_root,
            commands::set_track_sample_region,
            commands::set_track_fades,
            commands::set_source_param,
            commands::set_track_mute,
            commands::set_track_solo,
            commands::set_master_gain,
            commands::set_selected_track,
            commands::set_track_source,
            commands::add_track,
            commands::add_sample_channel,
            commands::remove_track,
            // --- buses, sends and insert chains ---
            commands::get_effects,
            commands::add_bus,
            commands::remove_bus,
            commands::rename_bus,
            commands::set_bus_color,
            commands::set_bus_gain,
            commands::set_bus_pan,
            commands::set_bus_mute,
            commands::set_bus_solo,
            commands::add_send,
            commands::remove_send,
            commands::set_send_level,
            commands::set_send_pre,
            commands::add_insert,
            commands::remove_insert,
            commands::move_insert,
            commands::set_insert_param,
            commands::set_insert_bypass,
            commands::get_insert_readouts,
            commands::get_chain_readouts,
            commands::watch_spectrum,
            commands::get_insert_spectrum,
            commands::set_insert_listen,
            // Hosted CLAP plugins: the picker, the master insert, the editors
            // and the state groundwork the project file will use.
            commands::set_track_source_clap,
            commands::get_master_effects,
            commands::set_master_effect_clap,
            commands::clear_master_effect,
            commands::open_clap_gui,
            commands::close_clap_gui,
            commands::save_clap_state,
            commands::load_clap_state,
            // Hosted VST3 plugins: the same surface, one format over.
            commands::get_vst3_plugins,
            commands::get_vst3_master_effects,
            commands::set_track_source_vst3,
            commands::set_master_effect_vst3,
            commands::open_vst3_gui,
            commands::close_vst3_gui,
            commands::save_vst3_state,
            commands::load_vst3_state,
            // Patterns, the arrangement and the play mode.
            commands::get_pattern,
            commands::get_pattern_content,
            commands::set_pattern,
            commands::add_pattern,
            commands::clone_pattern,
            commands::split_pattern_by_channel,
            commands::remove_pattern,
            commands::rename_pattern,
            commands::set_pattern_color,
            commands::set_pattern_length,
            commands::set_active_pattern,
            commands::set_play_mode,
            commands::set_loop,
            commands::add_playlist_track,
            commands::remove_playlist_track,
            commands::rename_playlist_track,
            commands::set_playlist_track_mute,
            commands::set_playlist_track_solo,
            commands::add_pattern_clip,
            commands::remove_clip,
            commands::move_clip,
            commands::resize_clip,
            commands::load_audio_source,
            commands::open_audio_file,
            commands::get_audio_sources,
            commands::unload_audio_source,
            commands::get_audio_waveform,
            commands::preview_source,
            commands::preview_instrument,
            commands::get_preview_settings,
            commands::set_preview_volume,
            commands::get_library_roots,
            commands::add_library_folder,
            commands::remove_library_folder,
            commands::list_library_dir,
            commands::import_dropped_audio,
            commands::preview_audio_file,
            commands::preview_track_sample,
            commands::add_audio_clip,
            commands::set_clip_gain,
            commands::set_clip_offset,
            commands::set_bpm,
            commands::get_stream_info,
            commands::get_audio_settings,
            commands::set_audio_settings,
            commands::list_midi_ports,
            commands::set_midi_port_enabled,
            commands::get_midi_status,
            // The project file: New / Open / Save / Save As, plus the name and
            // the missing-plugin warnings the title and status bars draw.
            commands::get_project_info,
            commands::new_project,
            commands::open_project,
            commands::save_project,
            commands::debug_capture,
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
            clap_bridge.set_served(false);
            vst3_bridge.set_served(false);
            project_bridge.set_served(false);
            midi.stop();
            meters.shutdown();
            audio.stop();
            vst3_host.shutdown();
            clap_host.shutdown();
            kernel.shutdown();
            return ExitCode::FAILURE;
        }
    };

    // Waking the loop: a CLAP request arrives on a command thread while the loop
    // may be blocked in `GetMessage` with nothing to do. `run_on_main_thread`
    // posts an event, which makes the loop turn once -- and one turn is all the
    // bridge needs, because the drain happens in `MainEventsCleared`.
    {
        let handle = app.handle().clone();
        clap_bridge.set_waker(Box::new(move || {
            let _ = handle.run_on_main_thread(|| {});
        }));
        let handle = app.handle().clone();
        vst3_bridge.set_waker(Box::new(move || {
            let _ = handle.run_on_main_thread(|| {});
        }));
        let handle = app.handle().clone();
        project_bridge.set_waker(Box::new(move || {
            let _ = handle.run_on_main_thread(|| {});
        }));
    }

    {
        use tauri::Manager;
        let midi_path = app
            .path()
            .app_config_dir()
            .unwrap_or_else(|_| std::env::temp_dir().join("mydaw"))
            .join("midi.json");
        app.state::<AppState>().midi().attach_store(midi_path);
        let preview_path = app
            .path()
            .app_config_dir()
            .unwrap_or_else(|_| std::env::temp_dir().join("mydaw"))
            .join("preview.json");
        let preview = crate::preview_settings::load_settings(&preview_path);
        app.state::<AppState>()
            .tracks()
            .file_preview()
            .set_gain(preview.linear_gain());
    }

    // The run loop owns the kernel. `RunEvent::Exit` fires after the last window
    // is gone, which is where the process tears itself down in order: stop
    // pushing frames at a dead webview, close the device, then stop plugins in
    // reverse dependency order.
    app.run(move |_handle, event| match event {
        // Every pass of the event loop, and therefore the whole CLAP heartbeat:
        // the requests the webview queued, every open editor's resize
        // negotiation, every plugin's `clap.timer-support` timers and
        // `on_main_thread` callbacks. While an editor is open its window's own
        // `WM_TIMER` keeps this firing at ~60 Hz; the rest of the time it fires
        // whenever anything else happens, which is exactly when there is
        // something to do.
        tauri::RunEvent::MainEventsCleared => {
            // The master insert holds one plugin of either format, so a CLAP
            // placement has to evict a VST3 one. `ClapHost` cannot do that for
            // itself -- it does not know the other host exists -- so the run
            // loop, which knows both, watches for the placement and says so.
            // The other direction is inside `Vst3Host::place_master_effect`,
            // which is handed the CLAP host for exactly this reason.
            let clap_master_before = clap_host.instance_at(ClapTarget::Master);
            clap_host.tick(&track_host, &audio, &clap_bridge);
            let clap_master_now = clap_host.instance_at(ClapTarget::Master);
            if clap_master_now.is_some() && clap_master_now != clap_master_before {
                vst3_host.evict_master();
                vst3_host.publish(&track_host, vst3_bridge.board());
            }

            vst3_host.tick(&track_host, &audio, &vst3_bridge, &mut clap_host);

            // Last, and after both plugin hosts have had their turn: a project
            // load destroys and recreates everything either of them is holding,
            // so it must not run in the middle of a half-served mailbox. It
            // republishes both boards itself, because it moved everything on
            // them and neither host's own tick would have noticed.
            project_host.tick(
                &project_host::Session {
                    tracks: &track_host,
                    audio: &audio,
                    sequencer: &sequencer,
                    sources: &audio_sources,
                },
                &project_bridge,
                &mut project_host::Hosts {
                    clap: &mut clap_host,
                    clap_board: clap_bridge.board(),
                    vst3: &mut vst3_host,
                    vst3_board: vst3_bridge.board(),
                },
            );
        }
        tauri::RunEvent::Exit => {
            log::info!("shutting down");
            // A command that arrives during teardown must be told, not left to
            // time out against a loop that will never turn again.
            clap_bridge.set_served(false);
            vst3_bridge.set_served(false);
            project_bridge.set_served(false);
            midi.stop();
            meters.shutdown();
            // Closing the stream retires the graph, which is what eventually
            // drops the CLAP nodes and sends their processors back here. Only
            // then can the plugins be deactivated on this (main) thread --
            // which is what `ClapHost::shutdown` does, editors first, because a
            // window whose procedure lives in an unloaded DLL crashes the
            // process on its next message.
            audio.stop();
            vst3_host.shutdown();
            clap_host.shutdown();
            kernel.shutdown();
        }
        _ => {}
    });

    ExitCode::SUCCESS
}

/// Tear everything down in the same order the window-close path uses.
///
/// The headless paths (capture mode, a window that would not open) never create
/// a [`ClapHost`], so there is nothing hosted to destroy here.
fn shutdown(state: &AppState, kernel: &mut mydaw_kernel::Kernel) {
    state.midi().stop();
    state.meters().shutdown();
    state.audio().stop();
    kernel.shutdown();
}

/// `RUST_LOG` if set, otherwise `info`. `try_init` so a second call (tests,
/// embedding) is a no-op rather than a panic.
fn init_logging() {
    let env = env_logger::Env::default().default_filter_or("info");
    let _ = env_logger::Builder::from_env(env)
        .format_timestamp_millis()
        .try_init();
}
