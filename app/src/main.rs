//! Binary entry point. All of the shell lives in the library so it can be tested;
//! this file exists to call it and to set the Windows subsystem.

// Release builds are GUI apps: no console window behind the webview. Debug builds
// keep the console, because that is where the kernel boot log lands.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    mydaw_app::run()
}
