//! The sacrificial process that opens one VST3 module.
//!
//! The WAVE app binary can do this job itself (its `main` calls
//! `wave_vst3::run_scan_worker_if_requested()`), so shipping this is optional;
//! it exists because it starts in milliseconds rather than loading a webview,
//! and because it is what the integration tests spawn. When it sits next to the
//! scanning executable it is preferred automatically.
//!
//! It is not a tool: run without the environment a scan sets, it says so and
//! exits non-zero.

fn main() -> std::process::ExitCode {
    // Returns only if this process was *not* spawned to scan something.
    if wave_vst3::run_scan_worker_if_requested() {
        return std::process::ExitCode::SUCCESS;
    }
    eprintln!(
        "{}: WAVE's VST3 scan worker. WAVE starts it with {}=vst3 and {}=<module> in its \
         environment; it opens that one module, prints what is inside and exits. There is \
         nothing to run here by hand.",
        env!("CARGO_BIN_NAME"),
        wave_plugin_scan::worker::FORMAT_ENV,
        wave_plugin_scan::worker::MODULE_ENV,
    );
    std::process::ExitCode::from(2)
}
