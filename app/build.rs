//! Tauri's build step: reads `tauri.conf.json`, embeds the Windows resources
//! (icon, manifest) and generates the ACL/capability code that
//! `tauri::generate_context!` consumes.

fn main() {
    tauri_build::build();
}
