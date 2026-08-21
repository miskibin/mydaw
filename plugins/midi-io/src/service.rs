//! MIDI I/O service: pairing, connections, hot-plug poll, notes into the engine.
//!
//! Notes never leave Rust. The UI sees port rows and a throttled activity
//! timestamp; it does not see raw MIDI bytes.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

use crate::held::{HeldNotes, NoteAction};
use crate::pairing::{
    load_settings, merge_port_list, save_settings, MidiPortDto, MidiSettings, MidiStoreError,
};
use crate::parser::MidiParser;
use crate::ports::{MidiPortConn, MidiPorts};

/// Where a decoded note goes. The shell binds this to `AudioIoService` and the
/// selected track; tests bind a recorder.
pub trait MidiNoteSink: Send + Sync {
    fn note_on(&self, key: u8, velocity: f32);
    fn note_off(&self, key: u8);
}

/// Snapshot the status bar polls. `activity_at_ms` is Unix milliseconds of the
/// last decoded event (0 = never).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MidiStatusDto {
    pub activity_at_ms: u64,
    pub enabled_count: u32,
    /// First currently open paired port, for the status-bar label.
    pub active_name: Option<String>,
}

const POLL_EVERY: Duration = Duration::from_millis(500);

struct OpenPort {
    conn: Box<dyn MidiPortConn>,
    held: HeldNotes,
}

struct Inner {
    settings: MidiSettings,
    store_path: Option<PathBuf>,
    sink: Option<Arc<dyn MidiNoteSink>>,
    open: HashMap<String, OpenPort>,
    last_os: Vec<String>,
    errors: HashMap<String, String>,
    /// Names currently inside `ports.open`, so two reconcile calls cannot
    /// double-connect the same port.
    opening: HashSet<String>,
}

struct Shared {
    ports: Arc<dyn MidiPorts>,
    inner: Mutex<Inner>,
    activity_at_ms: AtomicU64,
    stop: AtomicBool,
}

/// Shared MIDI state. `Send + Sync` so Tauri commands and the poll thread
/// can both call it.
pub struct MidiIoService {
    shared: Arc<Shared>,
    poller: Mutex<Option<JoinHandle<()>>>,
}

impl MidiIoService {
    pub fn new(ports: Arc<dyn MidiPorts>) -> Self {
        Self {
            shared: Arc::new(Shared {
                ports,
                inner: Mutex::new(Inner {
                    settings: MidiSettings::default(),
                    store_path: None,
                    sink: None,
                    open: HashMap::new(),
                    last_os: Vec::new(),
                    errors: HashMap::new(),
                    opening: HashSet::new(),
                }),
                activity_at_ms: AtomicU64::new(0),
                stop: AtomicBool::new(false),
            }),
            poller: Mutex::new(None),
        }
    }

    /// Production constructor: `midir` on the OS.
    pub fn with_midir() -> Self {
        Self::new(Arc::new(crate::ports::MidirPorts))
    }

    /// Notes go here. Called once from the shell after boot.
    pub fn set_sink(&self, sink: Arc<dyn MidiNoteSink>) {
        self.shared.inner.lock().sink = Some(sink);
    }

    /// Load `midi.json` and start the hot-plug poller. Idempotent.
    pub fn attach_store(&self, path: PathBuf) {
        {
            let mut inner = self.shared.inner.lock();
            inner.store_path = Some(path.clone());
            inner.settings = load_settings(&path);
        }
        self.shared.reconcile();
        self.ensure_poller();
    }

    pub fn list_ports(&self) -> Vec<MidiPortDto> {
        self.shared.list_ports()
    }

    pub fn status(&self) -> MidiStatusDto {
        self.shared.status()
    }

    /// Pair or unpair `name`. Persists and opens/closes the port.
    pub fn set_enabled(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<Vec<MidiPortDto>, MidiStoreError> {
        {
            let mut inner = self.shared.inner.lock();
            inner.settings.set_enabled(name, enabled);
            if let Some(path) = inner.store_path.clone() {
                let settings = inner.settings.clone();
                drop(inner);
                save_settings(&path, &settings)?;
            }
        }
        if !enabled {
            self.shared.close_port(name);
        }
        self.shared.reconcile();
        Ok(self.list_ports())
    }

    /// Refresh the OS list and open/close to match pairing.
    pub fn reconcile(&self) {
        self.shared.reconcile();
    }

    fn ensure_poller(&self) {
        let mut slot = self.poller.lock();
        if slot.is_some() {
            return;
        }
        self.shared.stop.store(false, Ordering::Relaxed);
        let shared = Arc::clone(&self.shared);
        let handle = thread::Builder::new()
            .name("midi-hotplug".into())
            .spawn(move || {
                while !shared.stop.load(Ordering::Relaxed) {
                    shared.reconcile();
                    thread::sleep(POLL_EVERY);
                }
            })
            .expect("spawn midi-hotplug");
        *slot = Some(handle);
    }

    /// Stop the poller and close every port (panic notes).
    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.poller.lock().take() {
            let _ = handle.join();
        }
        let names: Vec<String> = self.shared.inner.lock().open.keys().cloned().collect();
        for name in names {
            self.shared.close_port(&name);
        }
    }
}

impl Shared {
    fn list_ports(&self) -> Vec<MidiPortDto> {
        let inner = self.inner.lock();
        let errors: Vec<(String, String)> = inner
            .errors
            .iter()
            .map(|(name, err)| (name.clone(), err.clone()))
            .collect();
        let open: Vec<String> = inner.open.keys().cloned().collect();
        merge_port_list(&inner.last_os, &inner.settings, &errors, &open)
    }

    fn status(&self) -> MidiStatusDto {
        let inner = self.inner.lock();
        let enabled_count = inner
            .settings
            .devices
            .iter()
            .filter(|device| device.enabled)
            .count() as u32;
        let active_name = inner
            .settings
            .devices
            .iter()
            .filter(|device| device.enabled)
            .find(|device| inner.open.contains_key(&device.name))
            .map(|device| device.name.clone())
            .or_else(|| inner.open.keys().next().cloned());
        MidiStatusDto {
            activity_at_ms: self.activity_at_ms.load(Ordering::Relaxed),
            enabled_count,
            active_name,
        }
    }

    fn reconcile(self: &Arc<Self>) {
        let os = self.ports.list();
        let wanted: Vec<String> = {
            let mut inner = self.inner.lock();
            inner.last_os = os.clone();
            inner
                .settings
                .devices
                .iter()
                .filter(|device| device.enabled)
                .map(|device| device.name.clone())
                .collect()
        };

        let open_names: Vec<String> = self.inner.lock().open.keys().cloned().collect();
        for name in &open_names {
            let still_wanted = wanted.iter().any(|wanted_name| {
                crate::pairing::match_port_name(wanted_name, std::iter::once(name.as_str()))
                    .is_some()
            });
            let still_present =
                crate::pairing::match_port_name(name, os.iter().map(String::as_str)).is_some();
            if !still_wanted || !still_present {
                self.close_port(name);
            }
        }

        for name in wanted {
            if self.inner.lock().open.contains_key(&name) {
                continue;
            }
            if crate::pairing::match_port_name(&name, os.iter().map(String::as_str)).is_none() {
                continue;
            }
            open_named(self, &name);
        }
    }

    fn close_port(&self, name: &str) {
        let mut inner = self.inner.lock();
        let Some(mut port) = inner.open.remove(name) else {
            return;
        };
        inner.errors.remove(name);
        let actions = port.held.panic();
        let sink = inner.sink.clone();
        port.conn.close();
        drop(inner);
        if let Some(sink) = sink {
            for action in actions {
                apply_action(&*sink, action);
            }
        }
    }

    fn dispatch(&self, port: &str, events: &[crate::parser::MidiEvent]) {
        if events.is_empty() {
            return;
        }
        self.activity_at_ms.store(now_ms(), Ordering::Relaxed);
        let mut inner = self.inner.lock();
        let Some(open) = inner.open.get_mut(port) else {
            return;
        };
        let actions = open.held.apply(events);
        let sink = inner.sink.clone();
        drop(inner);
        let Some(sink) = sink else {
            return;
        };
        for action in actions {
            apply_action(&*sink, action);
        }
    }
}

fn open_named(shared: &Arc<Shared>, name: &str) {
    {
        let mut inner = shared.inner.lock();
        if inner.open.contains_key(name) || inner.opening.contains(name) {
            return;
        }
        inner.opening.insert(name.to_owned());
    }
    let dispatch = Arc::clone(shared);
    let port_name = name.to_owned();
    let parser = Mutex::new(MidiParser::new());
    match shared.ports.open(
        name,
        Arc::new(move |bytes| {
            let events = parser.lock().feed(bytes);
            dispatch.dispatch(&port_name, &events);
        }),
    ) {
        Ok(conn) => {
            let mut inner = shared.inner.lock();
            inner.opening.remove(name);
            inner.errors.remove(name);
            let still_wanted = inner.settings.devices.iter().any(|device| {
                device.enabled
                    && crate::pairing::match_port_name(&device.name, std::iter::once(name))
                        .is_some()
            });
            if !still_wanted || shared.stop.load(Ordering::Relaxed) {
                drop(conn);
                return;
            }
            inner.open.insert(
                name.to_owned(),
                OpenPort {
                    conn,
                    held: HeldNotes::new(),
                },
            );
        }
        Err(err) => {
            log::warn!("MIDI: could not open '{name}': {err}");
            let mut inner = shared.inner.lock();
            inner.opening.remove(name);
            inner.errors.insert(name.to_owned(), err);
        }
    }
}

fn apply_action(sink: &dyn MidiNoteSink, action: NoteAction) {
    match action {
        NoteAction::On { key, velocity } => sink.note_on(key, velocity),
        NoteAction::Off { key } => sink.note_off(key),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Drop for MidiIoService {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::FakeMidiPorts;

    #[derive(Default)]
    struct Rec {
        ons: Mutex<Vec<(u8, u32)>>,
        offs: Mutex<Vec<u8>>,
    }

    impl MidiNoteSink for Rec {
        fn note_on(&self, key: u8, velocity: f32) {
            self.ons
                .lock()
                .push((key, (velocity * 127.0).round() as u32));
        }
        fn note_off(&self, key: u8) {
            self.offs.lock().push(key);
        }
    }

    fn setup(names: &[&str]) -> (Arc<FakeMidiPorts>, MidiIoService, Arc<Rec>) {
        let fake = Arc::new(FakeMidiPorts::new(names.iter().copied()));
        let service = MidiIoService::new(Arc::clone(&fake) as Arc<dyn MidiPorts>);
        let rec = Arc::new(Rec::default());
        service.set_sink(Arc::clone(&rec) as Arc<dyn MidiNoteSink>);
        service.reconcile();
        (fake, service, rec)
    }

    #[test]
    fn enabling_a_present_port_plays_injected_notes() {
        let (fake, service, rec) = setup(&["MPK mini 3"]);
        service.set_enabled("MPK mini 3", true).unwrap();
        fake.inject("MPK mini 3", &[0x90, 60, 127]);
        assert_eq!(*rec.ons.lock(), vec![(60, 127)]);
        let row = service
            .list_ports()
            .into_iter()
            .find(|p| p.name == "MPK mini 3")
            .unwrap();
        assert_eq!(row.status, "connected");
        assert!(row.enabled);
        assert!(service.status().activity_at_ms > 0);
    }

    #[test]
    fn unplugging_an_open_port_releases_held_notes() {
        let (fake, service, rec) = setup(&["MPK mini 3"]);
        service.set_enabled("MPK mini 3", true).unwrap();
        fake.inject("MPK mini 3", &[0x90, 64, 100]);
        fake.set_names(Vec::<String>::new());
        service.reconcile();
        assert_eq!(*rec.offs.lock(), vec![64]);
        let row = service
            .list_ports()
            .into_iter()
            .find(|p| p.name == "MPK mini 3")
            .unwrap();
        assert_eq!(row.status, "offline");
    }

    #[test]
    fn disabling_a_port_releases_held_notes() {
        let (fake, service, rec) = setup(&["MPK mini 3"]);
        service.set_enabled("MPK mini 3", true).unwrap();
        fake.inject("MPK mini 3", &[0x90, 67, 80]);
        service.set_enabled("MPK mini 3", false).unwrap();
        assert_eq!(*rec.offs.lock(), vec![67]);
    }

    #[test]
    fn note_on_velocity_zero_is_a_note_off() {
        let (fake, service, rec) = setup(&["pads"]);
        service.set_enabled("pads", true).unwrap();
        fake.inject("pads", &[0x90, 36, 100, 36, 0]);
        assert_eq!(rec.ons.lock().len(), 1);
        assert_eq!(*rec.offs.lock(), vec![36]);
    }
}
