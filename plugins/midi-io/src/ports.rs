//! MIDI input backends. Production uses `midir`; tests inject bytes.
//!
//! Listing always creates a fresh OS client so a virtual port that appeared
//! after launch is visible on the next poll (the huff/midir pattern).

use std::sync::Arc;

use parking_lot::Mutex;

/// Bytes from one port, delivered on the MIDI thread (or a test thread).
pub type MidiBytesHandler = Arc<dyn Fn(&[u8]) + Send + Sync>;

/// An open input connection. Dropping it closes the port.
pub trait MidiPortConn: Send {
    fn close(&mut self);
}

/// Enumerate and open MIDI inputs without the rest of the plugin knowing
/// whether the OS or a test double is behind the names.
pub trait MidiPorts: Send + Sync {
    fn list(&self) -> Vec<String>;
    fn open(&self, name: &str, on_bytes: MidiBytesHandler)
        -> Result<Box<dyn MidiPortConn>, String>;
}

/// Production backend: WinMM via `midir`.
pub struct MidirPorts;

impl MidiPorts for MidirPorts {
    fn list(&self) -> Vec<String> {
        let Ok(midi) = midir::MidiInput::new("MYDAW") else {
            return Vec::new();
        };
        midi.ports()
            .iter()
            .filter_map(|port| midi.port_name(port).ok())
            .collect()
    }

    fn open(
        &self,
        name: &str,
        on_bytes: MidiBytesHandler,
    ) -> Result<Box<dyn MidiPortConn>, String> {
        let midi = midir::MidiInput::new("MYDAW").map_err(|err| err.to_string())?;
        let ports = midi.ports();
        let names: Vec<String> = ports
            .iter()
            .filter_map(|port| midi.port_name(port).ok())
            .collect();
        let index = crate::pairing::match_port_name(name, names.iter().map(String::as_str))
            .ok_or_else(|| format!("MIDI port '{name}' is not connected"))?;
        let port = &ports[index];
        let conn = midi
            .connect(port, "MYDAW", move |_stamp, bytes, _| on_bytes(bytes), ())
            .map_err(|err| err.to_string())?;
        Ok(Box::new(MidirConn { conn: Some(conn) }))
    }
}

struct MidirConn {
    conn: Option<midir::MidiInputConnection<()>>,
}

impl MidiPortConn for MidirConn {
    fn close(&mut self) {
        self.conn.take();
    }
}

impl Drop for MidirConn {
    fn drop(&mut self) {
        self.close();
    }
}

/// In-memory ports for tests and CI. No hardware, no threads of its own.
pub struct FakeMidiPorts {
    inner: Arc<Mutex<FakeInner>>,
}

struct FakeInner {
    names: Vec<String>,
    open: Vec<(String, MidiBytesHandler)>,
}

impl FakeMidiPorts {
    pub fn new(names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(FakeInner {
                names: names.into_iter().map(Into::into).collect(),
                open: Vec::new(),
            })),
        }
    }

    pub fn set_names(&self, names: impl IntoIterator<Item = impl Into<String>>) {
        self.inner.lock().names = names.into_iter().map(Into::into).collect();
    }

    /// Deliver `bytes` to an open connection named `name`.
    pub fn inject(&self, name: &str, bytes: &[u8]) {
        let handlers: Vec<MidiBytesHandler> = self
            .inner
            .lock()
            .open
            .iter()
            .filter(|(port, _)| port == name)
            .map(|(_, handler)| Arc::clone(handler))
            .collect();
        for handler in handlers {
            handler(bytes);
        }
    }
}

impl MidiPorts for FakeMidiPorts {
    fn list(&self) -> Vec<String> {
        self.inner.lock().names.clone()
    }

    fn open(
        &self,
        name: &str,
        on_bytes: MidiBytesHandler,
    ) -> Result<Box<dyn MidiPortConn>, String> {
        let mut inner = self.inner.lock();
        if crate::pairing::match_port_name(name, inner.names.iter().map(String::as_str)).is_none() {
            return Err(format!("MIDI port '{name}' is not connected"));
        }
        inner.open.push((name.to_owned(), on_bytes));
        Ok(Box::new(FakeConn {
            name: name.to_owned(),
            inner: Arc::clone(&self.inner),
        }))
    }
}

struct FakeConn {
    name: String,
    inner: Arc<Mutex<FakeInner>>,
}

impl MidiPortConn for FakeConn {
    fn close(&mut self) {
        self.inner
            .lock()
            .open
            .retain(|(port, _)| port != &self.name);
    }
}

impl Drop for FakeConn {
    fn drop(&mut self) {
        self.close();
    }
}
