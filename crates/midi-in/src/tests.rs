use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};

use super::*;

fn dev(id: &str, name: &str) -> MidiDevice {
    MidiDevice { id: id.into(), name: name.into() }
}

#[derive(Default)]
struct Fake {
    ports: Mutex<Vec<MidiDevice>>,
    cb: Mutex<Option<RawSink>>,
    fail: AtomicBool,
    opens: AtomicUsize,
    closes: Arc<AtomicUsize>,
}

struct Guard(Arc<AtomicUsize>);
impl Drop for Guard {
    fn drop(&mut self) {
        self.0.fetch_add(1, SeqCst);
    }
}

impl Backend for Fake {
    fn list(&self) -> Vec<MidiDevice> {
        self.ports.lock().clone()
    }
    fn connect(&self, id: &str, on_msg: RawSink) -> Result<Connection, String> {
        if self.fail.load(SeqCst) {
            return Err("busy".into());
        }
        if !self.ports.lock().iter().any(|p| p.id == id) {
            return Err("the port is gone".into());
        }
        self.opens.fetch_add(1, SeqCst);
        *self.cb.lock() = Some(on_msg);
        Ok(Box::new(Guard(self.closes.clone())))
    }
}

impl Fake {
    fn send(&self, bytes: &[u8]) {
        let mut g = self.cb.lock();
        if let Some(cb) = g.as_mut() {
            cb(bytes);
        }
    }
}

struct Rig {
    fake: Arc<Fake>,
    input: Arc<MidiInput>,
    msgs: Arc<Mutex<Vec<MidiMsg>>>,
    statuses: Arc<AtomicUsize>,
    live: Arc<AtomicBool>,
}

fn rig() -> Rig {
    let fake = Arc::new(Fake::default());
    let msgs = Arc::new(Mutex::new(vec![]));
    let statuses = Arc::new(AtomicUsize::new(0));
    let live = Arc::new(AtomicBool::new(false));
    let (m, s, l) = (msgs.clone(), statuses.clone(), live.clone());
    let hooks = Hooks {
        message: Arc::new(move |x| m.lock().push(x)),
        status: Arc::new(move |st| {
            s.fetch_add(1, SeqCst);
            l.store(st.connected.is_some(), SeqCst);
        }),
        note: Arc::new(|_| {}),
    };
    // the watcher is exercised by calling `reconcile` directly: no timing in tests
    let input = MidiInput::start_with(fake.clone(), hooks, Duration::from_secs(3600));
    Rig { fake, input, msgs, statuses, live }
}

#[test]
fn plan_covers_every_case() {
    let k = dev("a", "Keys");
    let ports = vec![k.clone()];
    assert_eq!(plan(None, None, &ports).0, Plan::Nothing);
    assert!(matches!(plan(None, Some(&k), &ports).0, Plan::Close(_)));
    assert_eq!(plan(Some(&k), None, &ports).0, Plan::Open("a".into()));
    assert_eq!(plan(Some(&k), Some(&k), &ports).0, Plan::Nothing);
    assert!(matches!(plan(Some(&k), Some(&k), &[]).0, Plan::Close(m) if m.contains("disconnected")));
    assert_eq!(plan(Some(&k), None, &[]).0, Plan::Absent);
    // the same device back on another slot: found by name
    let moved = vec![dev("b", "Keys")];
    assert_eq!(plan(Some(&k), None, &moved).0, Plan::Open("b".into()));
}

#[test]
fn lists_devices() {
    let r = rig();
    assert!(r.input.list().is_empty());
    *r.fake.ports.lock() = vec![dev("a", "Keys"), dev("b", "Pads")];
    assert_eq!(r.input.list().len(), 2);
}

#[test]
fn select_opens_and_forwards_decoded_messages() {
    let r = rig();
    *r.fake.ports.lock() = vec![dev("a", "Keys")];
    let st = r.input.select(Some("a"), None);
    assert_eq!(st.connected, Some(dev("a", "Keys")));
    assert_eq!(st.selected, Some(dev("a", "Keys")));
    assert_eq!(st.error, None);
    assert!(r.live.load(SeqCst));
    r.fake.send(&[0x90, 60, 100]);
    r.fake.send(&[0x90, 60, 0]);
    r.fake.send(&[0xB0, 64, 127]);
    r.fake.send(&[0xF8]); // clock: ignored
    assert_eq!(*r.msgs.lock(), vec![MidiMsg::NoteOn { ch: 0, key: 60, vel: 100 }, MidiMsg::NoteOff { ch: 0, key: 60 }, MidiMsg::Sustain { ch: 0, down: true }]);
}

#[test]
fn status_reports_the_last_note_and_a_running_count() {
    let r = rig();
    *r.fake.ports.lock() = vec![dev("a", "Keys")];
    assert_eq!(r.input.status().last_note, None);
    r.input.select(Some("a"), None);
    r.fake.send(&[0x90, 64, 90]);
    let st = r.input.status();
    assert_eq!(st.note_count, 1);
    assert_eq!(st.last_note, Some(LastNote { key: 64, velocity: 90, on: true }));
    r.fake.send(&[0x80, 64, 0]);
    r.fake.send(&[0xB0, 64, 127]); // not a key event: no activity
    let st = r.input.status();
    assert_eq!(st.note_count, 2);
    assert_eq!(st.last_note, Some(LastNote { key: 64, velocity: 0, on: false }));
}

#[test]
fn unplugging_closes_without_panic_and_replugging_reopens() {
    let r = rig();
    *r.fake.ports.lock() = vec![dev("a", "Keys")];
    r.input.select(Some("a"), None);
    assert!(r.live.load(SeqCst));
    // pulled out
    r.fake.ports.lock().clear();
    let st = r.input.reconcile();
    assert_eq!(st.connected, None);
    assert_eq!(st.selected, Some(dev("a", "Keys")), "the choice is remembered");
    assert!(st.error.as_deref().unwrap().contains("disconnected"));
    assert!(!r.live.load(SeqCst));
    assert_eq!(r.fake.closes.load(SeqCst), 1);
    // still gone on the next tick: nothing churns
    let n = r.statuses.load(SeqCst);
    r.input.reconcile();
    assert_eq!(r.statuses.load(SeqCst), n);
    // plugged in again
    *r.fake.ports.lock() = vec![dev("a", "Keys")];
    let st = r.input.reconcile();
    assert_eq!(st.connected, Some(dev("a", "Keys")));
    assert_eq!(st.error, None);
    assert!(r.live.load(SeqCst));
    assert_eq!(r.fake.opens.load(SeqCst), 2);
}

#[test]
fn a_saved_device_that_is_not_plugged_in_reports_it_and_opens_later() {
    let r = rig();
    let st = r.input.select(Some("a"), Some("Keys"));
    assert_eq!(st.connected, None);
    assert_eq!(st.selected, Some(dev("a", "Keys")));
    assert!(st.error.as_deref().unwrap().contains("Keys"));
    *r.fake.ports.lock() = vec![dev("a", "Keys")];
    assert_eq!(r.input.reconcile().connected, Some(dev("a", "Keys")));
}

#[test]
fn the_same_device_on_another_port_id_is_found_by_name() {
    let r = rig();
    *r.fake.ports.lock() = vec![dev("a", "Keys")];
    r.input.select(Some("a"), None);
    r.fake.ports.lock().clear();
    r.input.reconcile();
    *r.fake.ports.lock() = vec![dev("z", "Keys")];
    let st = r.input.reconcile();
    assert_eq!(st.connected, Some(dev("z", "Keys")));
    assert_eq!(st.selected, Some(dev("z", "Keys")));
}

#[test]
fn selecting_none_closes_the_device() {
    let r = rig();
    *r.fake.ports.lock() = vec![dev("a", "Keys")];
    r.input.select(Some("a"), None);
    let st = r.input.select(None, None);
    assert_eq!(st.connected, None);
    assert_eq!(st.selected, None);
    assert_eq!(st.error, None);
    assert!(!r.live.load(SeqCst));
    assert_eq!(r.fake.closes.load(SeqCst), 1);
}

#[test]
fn switching_devices_closes_the_old_one() {
    let r = rig();
    *r.fake.ports.lock() = vec![dev("a", "Keys"), dev("b", "Pads")];
    r.input.select(Some("a"), None);
    let st = r.input.select(Some("b"), None);
    assert_eq!(st.connected, Some(dev("b", "Pads")));
    assert_eq!(r.fake.closes.load(SeqCst), 1);
    assert_eq!(r.fake.opens.load(SeqCst), 2);
}

#[test]
fn an_open_failure_is_reported_not_fatal() {
    let r = rig();
    *r.fake.ports.lock() = vec![dev("a", "Keys")];
    r.fake.fail.store(true, SeqCst);
    let st = r.input.select(Some("a"), None);
    assert_eq!(st.connected, None);
    assert!(st.error.as_deref().unwrap().contains("busy"));
    r.fake.fail.store(false, SeqCst);
    assert_eq!(r.input.reconcile().connected, Some(dev("a", "Keys")));
}

#[test]
fn target_is_part_of_the_status() {
    let r = rig();
    assert_eq!(r.input.status().target, None);
    assert_eq!(r.input.set_target(Some(1003)).target, Some(1003));
    assert_eq!(r.input.target(), Some(1003));
}

#[test]
fn last_note_packing_round_trips() {
    for (count, n) in [(1u64, LastNote { key: 0, velocity: 0, on: false }), (999_999, LastNote { key: 127, velocity: 127, on: true })] {
        assert_eq!(unpack(pack(count, n)), (count, Some(n)));
    }
    assert_eq!(unpack(0), (0, None));
}
