//! Self-update from GitHub Releases (the repository is public: no token).
//!
//! The release workflow publishes `latest.json` next to the installers, signed with the
//! project's minisign key (the public half is `plugins.updater.pubkey` in `tauri.conf.json`).
//! The flow is check → download (progress, signature verified) → install, in three steps so
//! the user can keep working while the installer downloads and decides when to restart.
//! Nothing here restarts the app by itself.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::{Update, UpdaterExt};

pub const REPO_URL: &str = "https://github.com/miskibin/wave-DAW";

/// Release notes are the CHANGELOG section; the release body continues after this line with
/// install tips that belong on the release page, not in the app.
const NOTES_END: &str = "\n---";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub version: String,
    pub notes: Option<String>,
    /// Publication date, `YYYY-MM-DD`.
    pub date: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub version: String,
    /// The version whose release notes were last shown (`None` on a fresh install).
    pub seen_version: Option<String>,
    pub auto_update: bool,
    /// Whether VST3 plugins can open their own editor windows on this platform
    /// (Windows only for now; on Linux they are hosted with WAVE's parameter list).
    pub plugin_editors: bool,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
}

struct Pending {
    update: Update,
    /// The verified installer, once downloaded.
    bytes: Option<Arc<Vec<u8>>>,
}

/// The update found by the last check and, after `download`, its installer.
#[derive(Default)]
pub struct State {
    pending: Mutex<Option<Pending>>,
    busy: AtomicBool,
}

/// Holds `State::busy` so two downloads or installs never run at once.
struct Busy<'a>(&'a AtomicBool);
impl<'a> Busy<'a> {
    fn take(flag: &'a AtomicBool) -> Result<Self, String> {
        if flag.swap(true, Ordering::AcqRel) {
            Err("an update is already being downloaded or installed".into())
        } else {
            Ok(Busy(flag))
        }
    }
}
impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// The notes of a release body: everything before the `---` rule, trimmed; `None` when empty.
pub fn notes_of(body: Option<&str>) -> Option<String> {
    let body = body?.replace("\r\n", "\n");
    let body = body.trim_start_matches('\n');
    let cut = if body.starts_with("---") { 0 } else { body.find(NOTES_END).unwrap_or(body.len()) };
    let notes = body[..cut].trim();
    (!notes.is_empty()).then(|| notes.to_string())
}

/// Release page of `version` (`None` = the releases list). The version is checked so the
/// result can only ever point into this repository.
pub fn release_url(version: Option<&str>) -> Result<String, String> {
    let Some(v) = version else {
        return Ok(format!("{REPO_URL}/releases"));
    };
    let ok = !v.is_empty()
        && v.len() <= 32
        && v.starts_with(|c: char| c.is_ascii_digit())
        && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '+');
    if ok {
        Ok(format!("{REPO_URL}/releases/tag/v{v}"))
    } else {
        Err(format!("not a version: {v}"))
    }
}

/// Opens an https link into this repository in the system browser.
pub fn open_release(version: Option<&str>) -> Result<(), String> {
    let url = release_url(version)?;
    #[cfg(windows)]
    let mut cmd = {
        let mut c = std::process::Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", &url]);
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(&url);
        c
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(&url);
        c
    };
    cmd.spawn().map(|_| ()).map_err(|e| format!("could not open {url}: {e}"))
}

/// Limits `update-progress` events: one per percent (or per 512 KiB when the size is
/// unknown), at most about every 80 ms, and always the last one.
pub struct Throttle {
    last_pct: i64,
    last_at: Option<Instant>,
}

impl Throttle {
    pub fn new() -> Self {
        Throttle { last_pct: -1, last_at: None }
    }
    pub fn due(&mut self, done: u64, total: u64, now: Instant) -> bool {
        let pct = done.saturating_mul(100).checked_div(total).unwrap_or(done / (512 * 1024)) as i64;
        let last = total > 0 && done >= total;
        let waited = self.last_at.is_none_or(|t| now.duration_since(t) >= Duration::from_millis(80));
        if pct == self.last_pct && !last {
            return false;
        }
        if !waited && !last {
            return false;
        }
        self.last_pct = pct;
        self.last_at = Some(now);
        true
    }
}

/// Asks GitHub whether a newer release exists. The found update is kept for `download`.
pub async fn check(handle: &AppHandle, st: &State) -> Result<Option<UpdateInfo>, String> {
    // The passive NSIS installer should reopen the last project from settings. The
    // updater's default /ARGS would replay the project supplied to the original process.
    #[cfg(windows)]
    let updater = handle.updater_builder().restart_after_install(false).installer_arg("/R").build().map_err(|e| e.to_string())?;
    #[cfg(not(windows))]
    let updater = handle.updater().map_err(|e| e.to_string())?;
    let found = updater.check().await.map_err(|e| e.to_string())?;
    let mut pending = st.pending.lock();
    Ok(match found {
        Some(update) => {
            let info = UpdateInfo {
                version: update.version.clone(),
                notes: notes_of(update.body.as_deref()),
                date: update.date.map(|d| d.date().to_string()),
            };
            // the same release found again keeps its downloaded installer
            let same = pending.as_ref().is_some_and(|p| p.update.version == update.version);
            if !same {
                *pending = Some(Pending { update, bytes: None });
            }
            Some(info)
        }
        None => {
            *pending = None;
            None
        }
    })
}

/// Downloads the installer announced by the last check (progress on `update-progress`) and
/// verifies its signature. A second call with the installer already here does nothing.
pub async fn download(handle: &AppHandle, st: &State) -> Result<(), String> {
    let _busy = Busy::take(&st.busy)?;
    let update = {
        let p = st.pending.lock();
        let p = p.as_ref().ok_or("no update was found: check for updates first")?;
        if p.bytes.is_some() {
            return Ok(());
        }
        p.update.clone()
    };
    let mut done = 0u64;
    let mut throttle = Throttle::new();
    let h = handle.clone();
    let bytes = update
        .download(
            |chunk, total| {
                done += chunk as u64;
                let total = total.unwrap_or(0);
                if throttle.due(done, total, Instant::now()) {
                    let _ = h.emit("update-progress", Progress { done, total });
                }
            },
            || {},
        )
        .await
        .map_err(|e| e.to_string())?;
    let mut p = st.pending.lock();
    match p.as_mut() {
        Some(p) if p.update.version == update.version => p.bytes = Some(Arc::new(bytes)),
        _ => return Err("the update changed while downloading: check again".into()),
    }
    Ok(())
}

/// The downloaded installer, ready to run. Marks the state busy until `Ready` drops.
pub struct Ready<'a> {
    pub update: Update,
    pub bytes: Arc<Vec<u8>>,
    _busy: Busy<'a>,
}

pub fn ready(st: &State) -> Result<Ready<'_>, String> {
    let busy = Busy::take(&st.busy)?;
    let p = st.pending.lock();
    let p = p.as_ref().ok_or("no update was found: check for updates first")?;
    let bytes = p.bytes.clone().ok_or("the update is not downloaded yet")?;
    Ok(Ready { update: p.update.clone(), bytes, _busy: busy })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_stop_at_the_rule() {
        let body = "### Added\n- Auto update\n\n---\nWAVE: a fast DAW.\n* installer tips";
        assert_eq!(notes_of(Some(body)).as_deref(), Some("### Added\n- Auto update"));
    }

    #[test]
    fn notes_handle_missing_empty_and_windows_line_ends() {
        assert_eq!(notes_of(None), None);
        assert_eq!(notes_of(Some("  \n ")), None);
        assert_eq!(notes_of(Some("---\nonly tips")), None);
        assert_eq!(notes_of(Some("- one\r\n- two\r\n\r\n---\r\ntips")).as_deref(), Some("- one\n- two"));
        assert_eq!(notes_of(Some("- no rule at all")).as_deref(), Some("- no rule at all"));
    }

    #[test]
    fn release_urls_stay_inside_the_repository() {
        assert_eq!(release_url(None).unwrap(), format!("{REPO_URL}/releases"));
        assert_eq!(release_url(Some("0.9.3")).unwrap(), format!("{REPO_URL}/releases/tag/v0.9.3"));
        assert!(release_url(Some("1.0.0-beta.1")).is_ok());
        for bad in ["", "v1", "../x", "1.0 && calc", "1.0/../../x", "https://evil", "1\n2"] {
            assert!(release_url(Some(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn progress_is_throttled_but_the_end_always_arrives() {
        let t0 = Instant::now();
        let mut th = Throttle::new();
        assert!(th.due(1, 1000, t0)); // first
        assert!(!th.due(5, 1000, t0 + Duration::from_millis(10))); // same percent
        assert!(!th.due(50, 1000, t0 + Duration::from_millis(20))); // new percent, too soon
        assert!(th.due(60, 1000, t0 + Duration::from_millis(100))); // new percent, waited
        assert!(th.due(1000, 1000, t0 + Duration::from_millis(101))); // the end is never skipped
    }

    #[test]
    fn progress_without_a_known_size_moves_per_chunk_of_512k() {
        let t0 = Instant::now();
        let mut th = Throttle::new();
        assert!(th.due(1000, 0, t0));
        assert!(!th.due(2000, 0, t0 + Duration::from_secs(1)));
        assert!(th.due(600 * 1024, 0, t0 + Duration::from_secs(2)));
    }

    #[test]
    fn only_one_download_or_install_at_a_time() {
        let st = State::default();
        let a = Busy::take(&st.busy).unwrap();
        assert!(Busy::take(&st.busy).is_err());
        drop(a);
        assert!(Busy::take(&st.busy).is_ok());
    }

    #[test]
    fn install_needs_a_checked_and_downloaded_update() {
        let st = State::default();
        assert!(ready(&st).is_err());
        // the failed call released the busy flag
        assert!(Busy::take(&st.busy).is_ok());
    }
}
