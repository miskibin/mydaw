//! Crash recovery for projects that were never saved.
//!
//! A saved project is autosaved to its own file (and the previous save is kept as
//! `Song (backup).wave`). A project without a file has nowhere to go, so the autosave
//! timer writes it to `<data>/WAVE/Recovery/<session>.wave` instead. The file is
//! removed when the project gets a real file, is replaced (New/Open), or WAVE exits
//! normally; a copy that is still there at the next start therefore belongs to a
//! session that crashed or was killed, and the UI offers it back.
//!
//! Every running WAVE holds an exclusive lock on its own `<session>.lock` for its
//! whole life, so two windows never offer (or delete) each other's copy: a copy is
//! an orphan only when nobody holds its lock.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde::Serialize;
use wave_engine::Engine;

/// A copy left behind by a session that did not exit normally.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Orphan {
    /// Opaque id for `restore` / `discard` (the copy's file stem).
    pub id: String,
    /// When the copy was last written, ms since the Unix epoch.
    pub saved_ms: u64,
}

pub struct Recovery {
    dir: PathBuf,
    /// This session's copy.
    file: PathBuf,
    /// Held for the session's lifetime; `None` when the folder is not writable
    /// (recovery is then off and every call is a no-op).
    lock: Mutex<Option<File>>,
}

pub fn default_dir() -> PathBuf {
    wave_engine::engine::scratch_dir()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir)
        .join("Recovery")
}

fn now_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Ids are file stems this module made: digits and dashes. Anything else is refused,
/// so a bad id can never point outside the folder.
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_digit() || c == '-')
}

impl Recovery {
    pub fn start(dir: PathBuf) -> Self {
        // unique even for two sessions of one process started in the same millisecond (tests)
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let id = format!("{}-{}-{seq}", std::process::id(), now_ms(SystemTime::now()));
        let file = dir.join(format!("{id}.wave"));
        let lock = std::fs::create_dir_all(&dir)
            .and_then(|_| File::create(dir.join(format!("{id}.lock"))))
            .and_then(|f| f.try_lock().map(|_| f).map_err(std::io::Error::other));
        let lock = match lock {
            Ok(f) => Some(f),
            Err(e) => {
                log::warn!("crash recovery is off: {}: {e}", dir.display());
                None
            }
        };
        Self {
            dir,
            file,
            lock: Mutex::new(lock),
        }
    }

    fn enabled(&self) -> bool {
        self.lock.lock().is_some()
    }

    /// The autosave tick: keeps this session's copy in step with an unsaved project.
    /// Returns whether a copy was written. A project that has a file (or nothing to
    /// lose) needs no copy, so an old one is removed.
    pub fn write(&self, engine: &Engine) -> Result<bool, String> {
        if !self.enabled() {
            return Ok(false);
        }
        let st = engine.state();
        if st.path.is_some() || !st.dirty {
            self.clear();
            return Ok(false);
        }
        engine.save_copy(&self.file).map_err(|e| e.to_string())?;
        Ok(true)
    }

    /// Drops this session's copy (the project was saved, replaced, or WAVE is exiting
    /// after the user answered the unsaved-changes question).
    pub fn clear(&self) {
        let _ = std::fs::remove_file(&self.file);
    }

    /// Ends the session: the copy and the lock go.
    pub fn close(&self) {
        self.clear();
        if let Some(f) = self.lock.lock().take() {
            drop(f);
            let _ = std::fs::remove_file(self.file.with_extension("lock"));
        }
    }

    /// Copies of sessions that are not running any more, newest first.
    pub fn orphans(&self) -> Vec<Orphan> {
        if !self.enabled() {
            return vec![];
        }
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return vec![];
        };
        let mut out = vec![];
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("wave") || p == self.file {
                continue;
            }
            let Some(id) = p
                .file_stem()
                .and_then(|s| s.to_str())
                .filter(|s| valid_id(s))
                .map(str::to_owned)
            else {
                continue;
            };
            if !self.owner_gone(&id) {
                continue;
            }
            let saved_ms = e
                .metadata()
                .and_then(|m| m.modified())
                .map(now_ms)
                .unwrap_or(0);
            out.push(Orphan { id, saved_ms });
        }
        out.sort_by_key(|o| std::cmp::Reverse(o.saved_ms));
        out
    }

    /// True when no running WAVE holds `id`'s lock (or the lock file is gone).
    fn owner_gone(&self, id: &str) -> bool {
        match File::options()
            .write(true)
            .open(self.dir.join(format!("{id}.lock")))
        {
            Ok(f) => f.try_lock().is_ok(),
            Err(_) => true,
        }
    }

    fn orphan_path(&self, id: &str) -> Result<PathBuf, String> {
        if !valid_id(id) || !self.owner_gone(id) {
            return Err(format!("no recovered project `{id}`"));
        }
        let p = self.dir.join(format!("{id}.wave"));
        if p.is_file() {
            Ok(p)
        } else {
            Err(format!("no recovered project `{id}`"))
        }
    }

    /// Opens an orphan as this session's unsaved project. The copy becomes this
    /// session's own, so it stays protected until the user saves it.
    pub fn restore(&self, engine: &Engine, id: &str) -> Result<Vec<String>, String> {
        let p = self.orphan_path(id)?;
        let warnings = engine.open_recovered(&p).map_err(|e| e.to_string())?;
        if std::fs::rename(&p, &self.file).is_err() {
            // another file system or a locked file: write a fresh copy, then drop the old one
            engine.save_copy(&self.file).map_err(|e| e.to_string())?;
            let _ = std::fs::remove_file(&p);
        }
        let _ = std::fs::remove_file(self.dir.join(format!("{id}.lock")));
        Ok(warnings)
    }

    /// Deletes an orphan the user does not want back.
    pub fn discard(&self, id: &str) -> Result<(), String> {
        let p = self.orphan_path(id)?;
        std::fs::remove_file(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        let _ = std::fs::remove_file(self.dir.join(format!("{id}.lock")));
        Ok(())
    }
}

impl Drop for Recovery {
    fn drop(&mut self) {
        // only the lock: a session that ends without `close` (a panic unwinding through
        // here) keeps its copy so the next start can offer it
        if let Some(f) = self.lock.lock().take() {
            drop(f);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use wave_engine::{NoPlugins, Op, Project};

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "wave-recovery-{tag}-{}-{}",
            std::process::id(),
            now_ms(SystemTime::now())
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn edited() -> Arc<Engine> {
        let e = Engine::new(Project::empty(), Arc::new(NoPlugins));
        e.apply(&[Op::SetBpm { bpm: 137.0 }], false, None).unwrap();
        assert!(e.state().dirty);
        e
    }

    #[test]
    fn an_unsaved_project_survives_a_crash_and_comes_back_unsaved() {
        let dir = tmp("crash");
        let e = edited();
        let a = Recovery::start(dir.clone());
        assert!(a.write(&e).unwrap());
        // the session "crashes": no close(), the process lock just goes away
        drop(a);

        let b = Recovery::start(dir.clone());
        let o = b.orphans();
        assert_eq!(o.len(), 1, "{o:?}");
        let e2 = Engine::new(Project::empty(), Arc::new(NoPlugins));
        b.restore(&e2, &o[0].id).unwrap();
        let st = e2.state();
        assert_eq!(st.project.bpm, 137.0);
        assert!(
            st.path.is_none(),
            "a recovered project must not pretend to have a file"
        );
        assert!(st.dirty, "a recovered project has unsaved changes");
        // the copy is now this session's own: offered to nobody, still on disk
        assert!(b.orphans().is_empty());
        assert!(b.file.is_file());
        b.close();
        assert!(!b.file.is_file());
        assert!(Recovery::start(dir.clone()).orphans().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_running_session_is_never_offered_to_another() {
        let dir = tmp("live");
        let e = edited();
        let a = Recovery::start(dir.clone());
        a.write(&e).unwrap();
        let b = Recovery::start(dir.clone());
        assert!(b.orphans().is_empty());
        let id = a.file.file_stem().unwrap().to_str().unwrap().to_owned();
        assert!(b.discard(&id).is_err());
        assert!(a.file.is_file());
        a.close();
        b.close();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_normal_exit_leaves_nothing_to_recover() {
        let dir = tmp("clean");
        let e = edited();
        let a = Recovery::start(dir.clone());
        a.write(&e).unwrap();
        a.close();
        assert!(Recovery::start(dir.clone()).orphans().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn saved_or_clean_projects_need_no_copy() {
        let dir = tmp("saved");
        let e = edited();
        let a = Recovery::start(dir.clone());
        assert!(a.write(&e).unwrap());
        e.save(&dir.join("Song.wave")).unwrap();
        assert!(!a.write(&e).unwrap());
        assert!(
            !a.file.is_file(),
            "the copy goes once the project has a file"
        );
        let clean = Engine::new(Project::empty(), Arc::new(NoPlugins));
        assert!(!a.write(&clean).unwrap());
        a.close();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn discard_deletes_the_orphan_and_bad_ids_are_refused() {
        let dir = tmp("discard");
        let e = edited();
        let a = Recovery::start(dir.clone());
        a.write(&e).unwrap();
        drop(a);
        let b = Recovery::start(dir.clone());
        let id = b.orphans()[0].id.clone();
        assert!(b.discard("../../etc/passwd").is_err());
        b.discard(&id).unwrap();
        assert!(b.orphans().is_empty());
        b.close();
        std::fs::remove_dir_all(&dir).ok();
    }
}
