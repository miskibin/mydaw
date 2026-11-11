//! A drag sends one transient batch per frame. Listeners (the app's "state" event: the whole
//! project, ~1 MB on a big song) hear about the gesture once, when it ends, not per batch.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use wave_engine::*;

fn engine() -> (Arc<Engine>, Arc<AtomicUsize>, Arc<std::sync::Mutex<u64>>) {
    let e = Engine::new(Project::empty(), Arc::new(NoPlugins));
    let (n, last) = (
        Arc::new(AtomicUsize::new(0)),
        Arc::new(std::sync::Mutex::new(0)),
    );
    let (n2, l2) = (n.clone(), last.clone());
    e.on_change(move |dto| {
        n2.fetch_add(1, Ordering::Relaxed);
        *l2.lock().unwrap() = dto.revision;
    });
    (e, n, last)
}

fn vol(e: &Engine, v: f32) -> Op {
    let mut t = e.project().tracks[0].clone();
    t.vol = v;
    Op::PutTrack { track: t }
}

#[test]
fn a_drag_is_reported_once_when_it_ends() {
    let (e, n, last) = engine();
    e.begin_gesture();
    for k in 0..20 {
        let op = vol(&e, 0.3 + k as f32 * 0.01);
        e.apply(&[op], true, None).unwrap();
    }
    assert_eq!(n.load(Ordering::Relaxed), 0, "no state per transient batch");
    e.end_gesture();
    assert_eq!(n.load(Ordering::Relaxed), 1);
    assert_eq!(
        *last.lock().unwrap(),
        e.revision(),
        "the report carries the final revision"
    );
    assert!(e.state().can_undo);
}

#[test]
fn a_drag_back_to_the_start_still_reports_its_revisions() {
    let (e, n, last) = engine();
    let v0 = e.project().tracks[0].vol;
    e.begin_gesture();
    let op = vol(&e, v0 * 0.5);
    e.apply(&[op], true, None).unwrap();
    let op = vol(&e, v0);
    e.apply(&[op], true, None).unwrap();
    e.end_gesture();
    assert_eq!(n.load(Ordering::Relaxed), 1);
    assert_eq!(*last.lock().unwrap(), e.revision());
}

#[test]
fn edits_outside_a_gesture_are_reported_each_time() {
    let (e, n, _) = engine();
    for k in 0..3 {
        let op = vol(&e, 0.3 + k as f32 * 0.1);
        e.apply(&[op], true, None).unwrap();
        let op = vol(&e, 0.2 + k as f32 * 0.1);
        e.apply(&[op], false, None).unwrap();
    }
    assert_eq!(n.load(Ordering::Relaxed), 6);
}
