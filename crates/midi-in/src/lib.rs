//! MIDI keyboard input. One device at a time, chosen by id, reopened when it comes back.
//!
//! The device layer is behind [`Backend`] so the manager (selection, hot-plug, status,
//! activity) is tested without hardware; [`MidirBackend`] is the real one.

use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Weak};
use std::thread;
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use wave_engine::midi::MidiMsg;

/// An input port as the OS lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MidiDevice {
    /// Stable per port (survives re-plugging on the same USB port); what `select` takes.
    pub id: String,
    pub name: String,
}

/// The last key event, for an activity light.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LastNote {
    pub key: u8,
    /// 1..127 (0 for a release).
    pub velocity: u8,
    pub on: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MidiStatus {
    /// The device that is open right now.
    pub connected: Option<MidiDevice>,
    /// The device the user chose, open or not (it may be unplugged).
    pub selected: Option<MidiDevice>,
    /// Why the chosen device is not open, or the last open/IO error.
    pub error: Option<String>,
    pub last_note: Option<LastNote>,
    /// Key events received since the app started; a change means new activity.
    pub note_count: u64,
    /// The engine channel the keyboard plays (`None` = first pitched channel).
    pub target: Option<u64>,
}

/// What to tell the UI about a key event as it happens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MidiNoteEvent {
    pub note: LastNote,
    pub count: u64,
}

/// An open connection; dropping it closes the port.
pub type Connection = Box<dyn Any + Send>;
/// Receives every raw message on the driver thread.
pub type RawSink = Box<dyn FnMut(&[u8]) + Send>;

pub trait Backend: Send + Sync {
    fn list(&self) -> Vec<MidiDevice>;
    /// Opens port `id`; `on_msg` gets every raw message on the driver's thread.
    fn connect(&self, id: &str, on_msg: RawSink) -> Result<Connection, String>;
}

/// Sinks the owner of the manager wires up.
pub struct Hooks {
    /// Every decoded message, in order (the engine's `midi_event`).
    pub message: Arc<dyn Fn(MidiMsg) + Send + Sync>,
    /// A device connected, disappeared or failed, or the selection changed (status.connected
    /// tells whether notes are flowing).
    pub status: Arc<dyn Fn(&MidiStatus) + Send + Sync>,
    /// A key went down or up.
    pub note: Arc<dyn Fn(&MidiNoteEvent) + Send + Sync>,
}

/// What reconciling the wish against the ports requires.
#[derive(Debug, PartialEq, Eq)]
enum Plan {
    Nothing,
    Open(String),
    Close(String),
    /// Wanted but not plugged in.
    Absent,
}

/// Decides from the chosen device, the open one and the listed ports.
fn plan(wanted: Option<&MidiDevice>, open: Option<&MidiDevice>, ports: &[MidiDevice]) -> (Plan, Option<MidiDevice>) {
    let Some(w) = wanted else {
        return (open.map(|o| Plan::Close(format!("{} closed", o.name))).unwrap_or(Plan::Nothing), None);
    };
    // the same port by id, else a port of the same name (ids may change with the USB slot)
    let found = ports.iter().find(|p| p.id == w.id).or_else(|| ports.iter().find(|p| p.name == w.name)).cloned();
    match (open, found) {
        (Some(o), Some(f)) if o.id == f.id => (Plan::Nothing, Some(f)),
        (Some(o), Some(f)) => (Plan::Close(format!("{} replaced by {}", o.name, f.name)), Some(f)),
        (Some(o), None) => (Plan::Close(format!("{} was disconnected", o.name)), None),
        (None, Some(f)) => (Plan::Open(f.id.clone()), Some(f)),
        (None, None) => (Plan::Absent, None),
    }
}

struct Inner {
    wanted: Option<MidiDevice>,
    open: Option<(MidiDevice, Connection)>,
    error: Option<String>,
    target: Option<u64>,
}

pub struct MidiInput {
    backend: Arc<dyn Backend>,
    hooks: Hooks,
    inner: Mutex<Inner>,
    /// Packed last key event: count in the top 40 bits, `on << 16 | key << 8 | velocity`.
    last: Arc<AtomicU64>,
}

fn pack(count: u64, n: LastNote) -> u64 {
    count << 24 | (n.on as u64) << 16 | (n.key as u64) << 8 | n.velocity as u64
}

fn unpack(v: u64) -> (u64, Option<LastNote>) {
    let count = v >> 24;
    if count == 0 {
        return (0, None);
    }
    (count, Some(LastNote { key: (v >> 8) as u8 & 0x7F, velocity: v as u8, on: v >> 16 & 1 == 1 }))
}

impl MidiInput {
    /// Creates the manager and its hot-plug watcher thread (it ends when the manager is dropped).
    pub fn start(backend: Arc<dyn Backend>, hooks: Hooks) -> Arc<Self> {
        Self::start_with(backend, hooks, Duration::from_millis(1000))
    }

    pub fn start_with(backend: Arc<dyn Backend>, hooks: Hooks, poll: Duration) -> Arc<Self> {
        let m = Arc::new(Self {
            backend,
            hooks,
            inner: Mutex::new(Inner { wanted: None, open: None, error: None, target: None }),
            last: Arc::new(AtomicU64::new(0)),
        });
        let weak: Weak<Self> = Arc::downgrade(&m);
        thread::Builder::new()
            .name("wave-midi-watch".into())
            .spawn(move || loop {
                thread::sleep(poll);
                let Some(m) = weak.upgrade() else { break };
                m.reconcile();
            })
            .expect("spawn midi watcher");
        m
    }

    pub fn list(&self) -> Vec<MidiDevice> {
        self.backend.list()
    }

    pub fn status(&self) -> MidiStatus {
        let g = self.inner.lock();
        self.status_of(&g)
    }

    fn status_of(&self, g: &Inner) -> MidiStatus {
        let (note_count, last_note) = unpack(self.last.load(Relaxed));
        MidiStatus {
            connected: g.open.as_ref().map(|(d, _)| d.clone()),
            selected: g.wanted.clone(),
            error: g.error.clone(),
            last_note,
            note_count,
            target: g.target,
        }
    }

    /// Chooses the device (`None` = no device) and opens it now. Remembered across unplugging:
    /// when it returns it is reopened. Blocking (driver calls): not for the UI thread.
    pub fn select(&self, id: Option<&str>, name_hint: Option<&str>) -> MidiStatus {
        let ports = self.backend.list();
        let wanted = id.map(|id| {
            let name = ports
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.name.clone())
                .or_else(|| name_hint.map(String::from))
                .unwrap_or_else(|| id.to_string());
            MidiDevice { id: id.to_string(), name }
        });
        {
            let mut g = self.inner.lock();
            g.wanted = wanted;
            g.error = None;
        }
        self.reconcile()
    }

    /// The engine channel the keyboard plays.
    pub fn set_target(&self, target: Option<u64>) -> MidiStatus {
        let mut g = self.inner.lock();
        g.target = target;
        self.status_of(&g)
    }

    pub fn target(&self) -> Option<u64> {
        self.inner.lock().target
    }

    /// Opens, closes or reports the chosen device according to what is plugged in.
    pub fn reconcile(&self) -> MidiStatus {
        let ports = self.backend.list();
        let mut g = self.inner.lock();
        let before = (g.open.as_ref().map(|(d, _)| d.id.clone()), g.error.clone(), g.wanted.clone());
        let open_dev = g.open.as_ref().map(|(d, _)| d.clone());
        let (action, found) = plan(g.wanted.as_ref(), open_dev.as_ref(), &ports);
        if let (Some(f), Some(w)) = (&found, g.wanted.as_mut()) {
            *w = f.clone(); // keep the freshest id/name of the chosen device
        }
        match action {
            Plan::Nothing => {
                if g.open.is_some() {
                    g.error = None;
                }
            }
            Plan::Absent => {
                let n = g.wanted.as_ref().map(|d| d.name.clone()).unwrap_or_default();
                g.error.get_or_insert(format!("{n} is not connected"));
            }
            Plan::Close(why) => {
                g.open = None; // drops the connection
                g.error = if g.wanted.is_none() { None } else { Some(why) };
                if let (Some(f), true) = (found.as_ref(), g.wanted.is_some()) {
                    // replaced by another port of the same device: reopen below
                    let id = f.id.clone();
                    self.open_port(&mut g, &id, f.clone());
                }
            }
            Plan::Open(id) => {
                let dev = found.clone().expect("open implies a port");
                self.open_port(&mut g, &id, dev);
            }
        }
        let after = (g.open.as_ref().map(|(d, _)| d.id.clone()), g.error.clone(), g.wanted.clone());
        let st = self.status_of(&g);
        drop(g);
        if before != after {
            (self.hooks.status)(&st);
        }
        st
    }

    fn open_port(&self, g: &mut Inner, id: &str, dev: MidiDevice) {
        let sink = CallbackState { message: self.hooks.message.clone(), note: self.hooks.note.clone(), last: self.last.clone() };
        match self.backend.connect(id, Box::new(move |raw: &[u8]| sink.handle(raw))) {
            Ok(conn) => {
                g.open = Some((dev, conn));
                g.error = None;
            }
            Err(e) => {
                g.open = None;
                g.error = Some(format!("could not open {}: {e}", dev.name));
            }
        }
    }
}

/// Everything a driver callback needs.
struct CallbackState {
    message: Arc<dyn Fn(MidiMsg) + Send + Sync>,
    note: Arc<dyn Fn(&MidiNoteEvent) + Send + Sync>,
    last: Arc<AtomicU64>,
}

impl CallbackState {
    fn handle(&self, raw: &[u8]) {
        let Some(m) = MidiMsg::parse(raw) else { return };
        (self.message)(m);
        let n = match m {
            MidiMsg::NoteOn { key, vel, .. } => LastNote { key, velocity: vel, on: true },
            MidiMsg::NoteOff { key, .. } => LastNote { key, velocity: 0, on: false },
            _ => return,
        };
        let count = (self.last.load(Relaxed) >> 24) + 1;
        self.last.store(pack(count, n), Relaxed);
        (self.note)(&MidiNoteEvent { note: n, count });
    }
}

// ---------------------------------------------------------------- the real backend

/// MIDI inputs through `midir` (WinMM / CoreMIDI / ALSA).
pub struct MidirBackend;

impl Backend for MidirBackend {
    fn list(&self) -> Vec<MidiDevice> {
        let Ok(mut input) = midir::MidiInput::new("wave-list") else { return vec![] };
        input.ignore(midir::Ignore::All);
        input.ports().iter().filter_map(|p| Some(MidiDevice { id: p.id(), name: input.port_name(p).ok()? })).collect()
    }

    fn connect(&self, id: &str, mut on_msg: RawSink) -> Result<Connection, String> {
        let mut input = midir::MidiInput::new("wave").map_err(|e| e.to_string())?;
        input.ignore(midir::Ignore::All);
        let port = input.ports().into_iter().find(|p| p.id() == id).ok_or("the port is gone")?;
        let conn = input.connect(&port, "wave-in", move |_us, bytes, _| on_msg(bytes), ()).map_err(|e| e.to_string())?;
        Ok(Box::new(conn))
    }
}

#[cfg(test)]
mod tests;
