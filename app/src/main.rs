//! Binary entry point. All of the shell lives in the library so it can be tested;
//! this file exists to call it, to set the Windows subsystem, and to answer the
//! one request that must be answered before anything else starts.

// Release builds are GUI apps: no console window behind the webview. Debug builds
// keep the console, because that is where the kernel boot log lands.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    // A plugin scan opens strangers' code in a process it is willing to lose, and
    // this binary is what it spawns when no dedicated worker was shipped
    // alongside it. If MYDAW was started that way, the call below reads the one
    // module it was asked about, prints the answer and exits -- **before** an
    // audio device, a window or a kernel exists, which is the whole point of
    // being expendable. Otherwise it returns immediately, having recorded that
    // this executable understands the protocol.
    //
    // See `mydaw_plugin_scan::worker`. Adding a format here is what makes it
    // scannable in a shipped build.
    mydaw_plugin_scan::run_worker_if_requested(&[
        mydaw_plugin_host_vst3::scan_worker_handler(),
        mydaw_plugin_host_clap::scan_worker_handler(),
    ]);

    mydaw_app::run()
}
