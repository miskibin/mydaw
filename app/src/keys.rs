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

/// Loads the piano in the background if `needed` and it is not loaded (or loading) yet.
/// Cheap when there is nothing to do, so it runs on every project change.
pub fn ensure(app: &Arc<App>, needed: bool) {
    {
        let mut st = app.keys.st.lock();
        if !needed || matches!(st.0, Phase::Loading | Phase::Ready) {
            return;
        }
        st.0 = Phase::Loading;
    }
    let a = app.clone();
    std::thread::spawn(move || {
        let Some(d) = dir(&a) else { return set(&a, Phase::Missing, None) };
        set(&a, Phase::Loading, None);
        match KeysBank::load(&d) {
            Ok(b) => {
                a.engine.set_keys_bank(Some(Arc::new(b)));
                set(&a, Phase::Ready, None);
            }
            Err(e) => {
                log::warn!("WAVE Keys: {e}");
                set(&a, Phase::Error, Some(e));
            }
        }
    });
}

/// For an offline bounce: waits until the piano a project needs has finished loading.
pub fn wait(app: &Arc<App>) {
    if !app.engine.uses_keys() {
        return;
    }
    ensure(app, true);
    while app.keys.st.lock().0 == Phase::Loading {
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
}
