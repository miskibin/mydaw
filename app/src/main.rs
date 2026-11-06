// Release builds are GUI apps on Windows (no console window behind the UI).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `WAVE --cli …` is the `wave` command (see cli_shim): same binary, no window.
    if args.first().map(String::as_str) == Some("--cli") {
        attach_console();
        return wave_cli::run_cli(args[1..].to_vec());
    }
    wave_app::run();
    std::process::ExitCode::SUCCESS
}

/// A GUI-subsystem exe has no console; borrow the terminal it was started from
/// so the CLI prints there (pipes and redirects work without this).
#[cfg(windows)]
fn attach_console() {
    extern "system" {
        fn AttachConsole(pid: u32) -> i32;
    }
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
fn attach_console() {}
