//! `wave --project FILE …`: the CLI's actions run in this process on a project file, with
//! no window, no audio device, no MIDI and no session socket. The same [`actions`] the
//! running app uses, against an engine of its own.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use serde_json::Value;
use wave_engine::keys::KeysBank;
use wave_engine::{Engine, Project};

use crate::actions::{self, Host, R};
use crate::plugins::Plugins;

/// Sample rate and block of the inert audio half that keeps VST3 instances live.
const LIVE_SR: f32 = 48_000.0;
const LIVE_BLOCK: usize = 512;

pub struct Files;

impl wave_cli::Files for Files {
    fn open(&self, path: &Path) -> Result<(Box<dyn wave_cli::Doc>, Vec<String>), String> {
        let h = Headless::new(Project::empty());
        let warnings = h.engine.open(path).map_err(|e| e.to_string())?;
        Ok((Box::new(h), warnings))
    }

    fn create(&self, template: &str) -> Result<Box<dyn wave_cli::Doc>, String> {
        let p = match template {
            "empty" => Project::empty(),
            "sketch" => Project::sketch(),
            t => return Err(format!("unknown template `{t}` (empty, sketch)")),
        };
        Ok(Box::new(Headless::new(p)))
    }
}

pub struct Headless {
    engine: Arc<Engine>,
    plugins: Arc<Plugins>,
    /// The pitch model, loaded on first need: why it could not be, if it could not.
    model: OnceLock<Option<String>>,
    keys: OnceLock<R<()>>,
    /// The audio-thread half nobody plays: it makes the engine instantiate hosted plugins
    /// live (their parameters and state are reachable), pumped by hand after each command.
    rt: Mutex<Option<wave_engine::rt::Rt>>,
}

impl Headless {
    fn new(project: Project) -> Self {
        let plugins = Plugins::new();
        let engine = Engine::new(Project::empty(), plugins.factory());
        engine.new_project(project);
        Self { engine, plugins, model: OnceLock::new(), keys: OnceLock::new(), rt: Mutex::new(None) }
    }

    /// Lets the inert audio half take the commands and graphs queued for it.
    fn pump(&self) {
        if let Some(rt) = self.rt.lock().as_mut() {
            let (mut l, mut r) = ([0f32; LIVE_BLOCK], [0f32; LIVE_BLOCK]);
            rt.process(&mut l, &mut r, None);
        }
    }
}

/// Where an installed app keeps the piano recordings (Tauri resources) relative to the binary.
fn bundled_keys() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let mut c = vec![dir.join("keys"), dir.join("../Resources/keys")];
    if cfg!(debug_assertions) {
        c.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("resources").join("keys"));
    }
    c.into_iter().find(|d| KeysBank::present(d))
}

impl Host for Headless {
    fn engine(&self) -> &Arc<Engine> {
        &self.engine
    }
    fn plugins(&self) -> &Arc<Plugins> {
        &self.plugins
    }
    fn live(&self) -> bool {
        false
    }
    fn audio_json(&self) -> Value {
        Value::Null
    }
    fn cli_json(&self) -> Value {
        Value::Null
    }
    fn model_error(&self) -> Option<String> {
        self.model.get().cloned().flatten()
    }
    fn wait_keys(&self) -> R<()> {
        if !self.engine.uses_keys() {
            return Ok(());
        }
        self.keys
            .get_or_init(|| {
                let d = crate::keys::find_dir(bundled_keys()).ok_or(
                    "KEYS_MISSING: the project plays WAVE Keys, but the piano recordings are not on this computer \
                     (download the kit in the app: Library → WAVE Keys); the bounce would be silent",
                )?;
                let bank = KeysBank::load(&d).map_err(|e| format!("KEYS_ERROR: WAVE Keys: {e}"))?;
                self.engine.set_keys_bank(Some(Arc::new(bank)));
                Ok(())
            })
            .clone()
    }
    fn ensure_pitch_model(&self) {
        self.model.get_or_init(|| crate::models::activate(&self.engine).err());
    }
    fn ensure_live_plugins(&self) {
        let mut rt = self.rt.lock();
        if rt.is_none() {
            *rt = Some(self.engine.create_rt(LIVE_SR, LIVE_BLOCK));
        }
    }
}

impl wave_cli::Doc for Headless {
    fn call(&self, command: &str, args: &Value, if_revision: Option<u64>) -> Result<Value, String> {
        let r = actions::control(self, command, args, if_revision);
        self.pump();
        r
    }
    fn revision(&self) -> u64 {
        self.engine.revision()
    }
    fn dirty(&self) -> bool {
        self.engine.state().dirty
    }
    fn save(&self, path: &Path) -> Result<(), String> {
        self.engine.save(path).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::Files;
    use serde_json::json;
    use wave_cli::Files as _;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("wave-headless-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_file_is_edited_saved_and_reopened_without_the_app() {
        let d = tmp("roundtrip");
        let path = d.join("song.wave");
        let doc = Files.create("sketch").unwrap();
        assert!(!doc.dirty());
        doc.call("apply", &json!({"ops": [{"op": "setBpm", "bpm": 97}]}), None).unwrap();
        doc.call("pattern", &json!({"id": 1, "ch": "kick", "steps": "x.x. x.x. x.x. x.x."}), None).unwrap();
        assert!(doc.dirty());
        doc.save(&path).unwrap();
        assert!(!doc.dirty());

        let (doc, warnings) = Files.open(&path).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(!doc.dirty());
        let ctx = doc.call("context", &json!({}), None).unwrap();
        assert_eq!(ctx["bpm"], 97.0);
        let pat = doc.call("pattern", &json!({"id": 1}), None).unwrap();
        let kick = pat["rows"].as_array().unwrap().iter().find(|r| r["name"] == "Kick").unwrap();
        assert_eq!(kick["steps"], "x.x. x.x. x.x. x.x.");
        let st = doc.call("status", &json!({}), None).unwrap();
        assert_eq!(st["mode"], "file");
        assert!(st["path"].as_str().unwrap().ends_with("song.wave"));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn transport_undo_and_auditions_need_the_app() {
        let doc = Files.create("sketch").unwrap();
        for cmd in crate::actions::LIVE_ONLY {
            let e = doc.call(cmd, &json!({"id": 1, "path": "x.wave", "clipId": 1, "ch": 1}), None).unwrap_err();
            assert!(e.starts_with("NEEDS_APP"), "{cmd}: {e}");
        }
        assert!(!doc.dirty());
    }

    #[test]
    fn renders_and_stems_come_from_the_file() {
        let d = tmp("render");
        let doc = Files.create("sketch").unwrap();
        let out = d.join("mix.wav");
        let r = doc.call("render", &json!({"out": out.display().to_string(), "to": 8.0, "tail": 0.0, "rate": 44100.0}), None).unwrap();
        assert_eq!(r["stats"]["silent"], false, "{r}");
        assert_eq!(r["stats"]["sampleRate"], 44100);
        assert!(out.is_file());
        let stems = d.join("stems");
        let r = doc.call("render", &json!({"out": stems.display().to_string(), "to": 16.0, "stems": true}), None).unwrap();
        assert_eq!(r["files"].as_array().unwrap().len(), 4, "{r}");
        assert!(!doc.dirty(), "a bounce is not an edit");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn plugin_access_starts_live_instances_without_breaking_edits_or_bounces() {
        let d = tmp("live");
        let doc = Files.create("sketch").unwrap();
        // the master limiter is a built-in effect, not a hosted plugin
        let e = doc.call("vst3_params", &json!({"fxId": 900}), None).unwrap_err();
        assert!(e.contains("not a loaded VST3 plugin"), "{e}");
        doc.call("set_param", &json!({"fxId": 900, "param": "ceil", "value": -1.0}), None).unwrap();
        let out = d.join("after.wav");
        let r = doc.call("render", &json!({"out": out.display().to_string(), "to": 4.0}), None).unwrap();
        assert_eq!(r["stats"]["silent"], false, "{r}");
        doc.save(&d.join("s.wave")).unwrap();
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_broken_file_is_an_error_not_an_empty_project() {
        let d = tmp("broken");
        let p = d.join("x.wave");
        std::fs::write(&p, "not json").unwrap();
        let e = Files.open(&p).err().unwrap();
        assert!(e.contains("not a WAVE project"), "{e}");
        assert!(Files.open(&d.join("missing.wave")).is_err());
        assert!(Files.create("nope").is_err());
        std::fs::remove_dir_all(&d).ok();
    }
}
