fn main() {
    // the frontend may not be built yet on a fresh clone; tauri-build needs the folder to exist
    let dist = std::path::Path::new("../ui/dist");
    if !dist.exists() {
        let _ = std::fs::create_dir_all(dist);
        let _ = std::fs::write(
            dist.join("index.html"),
            "<!doctype html><title>WAVE</title><p>Run <code>pnpm --dir ui build</code>.</p>",
        );
    }
    tauri_build::build();
}
