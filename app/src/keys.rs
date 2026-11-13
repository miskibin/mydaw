//! WAVE Keys: finds the piano's recordings (shipped inside the app, or downloaded into the
//! sample library) and loads them the first time a project has a Keys channel.

use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::Serialize;
use tauri::{Emitter, Manager};
use wave_engine::keys::{KeysBank, KIT_ID};

use crate::App;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// No Keys channel has needed the piano yet.
    Idle,
    /// The recordings are not on this computer (a build without them: download the kit).
    Missing,
    Loading,
    Ready,
    Error,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeysStatus {
    pub state: Phase,
    pub error: Option<String>,
    /// Library kit that holds the piano (for the download button).
    pub kit: &'static str,
    /// The recordings are on disk (bundled or downloaded).
    pub installed: bool,
}

pub struct Keys {
    st: Mutex<(Phase, Option<String>)>,
}

impl Default for Keys {
    fn default() -> Self {
        Self { st: Mutex::new((Phase::Idle, None)) }
    }
}

/// Where the recordings are: inside the app first, then the sample library.
fn dir(app: &App) -> Option<PathBuf> {
    let bundled = app.handle.get().and_then(|h| h.path().resource_dir().ok()).map(|d| d.join("keys"));
    find_dir(bundled)
}

/// `bundled` (the app's resource folder, if known) first, then the sample library.
pub fn find_dir(bundled: Option<PathBuf>) -> Option<PathBuf> {
    bundled.into_iter().chain([crate::library::library_dir().join(KIT_ID)]).find(|d| KeysBank::present(d))
}

pub fn status(app: &App) -> KeysStatus {
    let (state, error) = app.keys.st.lock().clone();
    KeysStatus { state, error, kit: KIT_ID, installed: state == Phase::Ready || dir(app).is_some() }
}

fn set(app: &App, phase: Phase, error: Option<String>) {
    *app.keys.st.lock() = (phase, error);
    if let Some(h) = app.handle.get() {
        let _ = h.emit("keys-status", status(app));
    }
}

/// Claims the load if nobody has: true when the caller must load the piano now.
fn claim(app: &App, needed: bool) -> bool {
    let mut st = app.keys.st.lock();
    if !needed || matches!(st.0, Phase::Loading | Phase::Ready) {
        return false;
    }
    st.0 = Phase::Loading;
    true
}

fn load_now(app: &App) {
    let Some(d) = dir(app) else { return set(app, Phase::Missing, None) };
    set(app, Phase::Loading, None);
    match KeysBank::load(&d) {
        Ok(b) => {
            app.engine.set_keys_bank(Some(Arc::new(b)));
            set(app, Phase::Ready, None);
        }
        Err(e) => {
            log::warn!("WAVE Keys: {e}");
            set(app, Phase::Error, Some(e));
        }
    }
}

/// Loads the piano in the background if `needed` and it is not loaded (or loading) yet.
/// Cheap when there is nothing to do, so it runs on every project change.
pub fn ensure(app: &Arc<App>, needed: bool) {
    if claim(app, needed) {
        let a = app.clone();
        std::thread::spawn(move || load_now(&a));
    }
}

/// For an offline bounce: waits until the piano a project needs has finished loading.
pub fn wait(app: &App) {
    if !app.engine.uses_keys() {
        return;
    }
    if claim(app, true) {
        return load_now(app);
    }
    while app.keys.st.lock().0 == Phase::Loading {
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
}
