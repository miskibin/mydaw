//! The sacrificial process that opens one CLAP bundle.
//!
//! MYDAW itself can do this job -- `mydaw-app`'s `main` calls the same function,
//! so a shipped single binary needs no sidecar -- but this one starts in
//! milliseconds instead of loading a webview. When it sits next to the
//! executable that is scanning, it is preferred automatically; see
//! `mydaw_plugin_scan::worker`.
//!
//! It is not a tool. Run without the environment a scan sets, it says so and
//! exits non-zero rather than doing anything.

fn main() -> std::process::ExitCode {
    // Returns only if this process was *not* spawned to scan something.
    mydaw_plugin_scan::run_worker_if_requested(&[mydaw_plugin_host_clap::scan_worker_handler()]);

    eprintln!(
        "{}: MYDAW's CLAP scan worker. It is started by MYDAW with {}=clap and {}=<bundle> in \
         its environment, opens that one bundle, prints what is inside it and exits. There is \
         nothing to run here by hand.",
        env!("CARGO_BIN_NAME"),
        mydaw_plugin_scan::worker::FORMAT_ENV,
        mydaw_plugin_scan::worker::MODULE_ENV,
    );
    std::process::ExitCode::from(2)
}
