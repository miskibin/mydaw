//! The sacrificial process that opens one VST3 module.
//!
//! MYDAW itself can do this job -- `mydaw-app`'s `main` calls the same function,
//! so a shipped single binary needs no sidecar -- but this one starts in
//! milliseconds instead of loading a webview, which on a cold scan of a
//! well-stocked machine is dozens of seconds. When it sits next to the
//! executable that is scanning, it is preferred automatically; see
//! `mydaw_plugin_scan::worker`.
//!
//! It is not a tool. Run without the environment a scan sets, it says so and
//! exits non-zero rather than doing anything.

fn main() -> std::process::ExitCode {
    // Returns only if this process was *not* spawned to scan something.
    mydaw_plugin_scan::run_worker_if_requested(&[
        mydaw_plugin_host_vst3::scan::scan_worker_handler(),
    ]);

    eprintln!(
        "{}: MYDAW's VST3 scan worker. It is started by MYDAW with {}=vst3 and {}=<module> in \
         its environment, opens that one module, prints what is inside it and exits. There is \
         nothing to run here by hand.",
        env!("CARGO_BIN_NAME"),
        mydaw_plugin_scan::worker::FORMAT_ENV,
        mydaw_plugin_scan::worker::MODULE_ENV,
    );
    std::process::ExitCode::from(2)
}
