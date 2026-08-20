//! Tauri's build step: reads `tauri.conf.json`, embeds the Windows resources
//! (icon, manifest) and generates the ACL/capability code that
//! `tauri::generate_context!` consumes.
//!
//! `tauri.conf.json` lists `../assets/packs` as a bundle resource so a packaged
//! build ships the factory kit. That directory is gitignored (`assets/` holds
//! downloaded samples, not source) and is absent on a fresh clone. tauri-build
//! refuses to proceed if a listed resource path is missing, so this script
//! creates an empty packs folder first. Runtime listing treats a missing or
//! empty folder as "no kits" — see `library::list_dir`.

use std::path::PathBuf;

fn main() {
    ensure_packs_dir();
    tauri_build::build();
}

fn ensure_packs_dir() {
    let packs = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("assets")
        .join("packs");
    if packs.is_dir() {
        return;
    }
    std::fs::create_dir_all(&packs).unwrap_or_else(|err| {
        panic!(
            "could not create the factory packs directory {}: {err}",
            packs.display()
        );
    });
}
