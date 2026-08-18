//! Typed publish/subscribe event bus.
//!
//! # !!! NON-REAL-TIME ONLY !!!
//!
//! **Never publish to or subscribe on this bus from an audio callback.**
//! `publish` takes a lock, clones a `Vec` (allocates), and invokes arbitrary
//! subscriber code synchronously on the calling thread -- every one of those is a
//! violation of the RT rules in `mydaw-rt-core`.
//!
//! The audio thread communicates through `rtrb` queues and atomics only
//! (`RtCommand`, `TransportClock`, meter frames). A non-RT thread may translate
//! what it pops off those queues into bus events; the audio thread itself must
//! not know this type exists.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

/// Marker for anything publishable on the [`EventBus`].
///
/// `Send + Sync` because handlers may be registered from any (non-RT) thread;
/// `'static` because the bus stores type-erased handlers keyed by `TypeId`.
pub trait Event: Send + Sync + 'static {}

/// Blanket impl: any type that satisfies the bounds is an event. Keeps plugins
/// free to publish plain structs without ceremony.
impl<T: Send + Sync + 'static> Event for T {}

/// Type-erased subscriber. `Arc` (not `Box`) so `publish` can snapshot the handler
/// list and release the lock before dispatching -- see [`EventBus::publish`].
type ErasedHandler = Arc<dyn Fn(&dyn Any) + Send + Sync>;

/// Synchronous, typed pub/sub. **Non-RT only** (see module docs).
#[derive(Default)]
pub struct EventBus {
    handlers: RwLock<HashMap<TypeId, Vec<ErasedHandler>>>,
}

impl EventBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `handler` for every future `E` published on this bus.
    ///
    /// Handlers cannot be removed in Phase 0: plugins live for the whole process,
    /// so unsubscribe has no caller yet. Add it when hot-unload appears.
    pub fn subscribe<E, F>(&self, handler: F)
    where
        E: Event,
        F: Fn(&E) + Send + Sync + 'static,
    {
        let erased: ErasedHandler = Arc::new(move |any: &dyn Any| {
            // Always `Some`: the handler is only ever invoked for its own TypeId key.
            if let Some(event) = any.downcast_ref::<E>() {
                handler(event);
            }
        });
        self.handlers
            .write()
            .entry(TypeId::of::<E>())
            .or_default()
            .push(erased);
    }

    /// Dispatch `event` to every subscriber of `E`, synchronously, on this thread.
    ///
    /// The handler list is cloned out from under the lock before dispatch, so a
    /// handler is free to publish further events or subscribe without deadlocking.
    /// That clone allocates -- one more reason this bus is non-RT only.
    pub fn publish<E: Event>(&self, event: &E) {
        let handlers = self.handlers.read().get(&TypeId::of::<E>()).cloned();
        if let Some(handlers) = handlers {
            for handler in handlers {
                handler(event);
            }
        }
    }

    /// Number of subscribers registered for `E`. Diagnostics/tests.
    pub fn subscriber_count<E: Event>(&self) -> usize {
        self.handlers
            .read()
            .get(&TypeId::of::<E>())
            .map_or(0, Vec::len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    struct Tick(u64);
    struct Other;

    #[test]
    fn publish_reaches_multiple_subscribers_in_order() {
        let bus = EventBus::new();
        let seen = Arc::new(Mutex::new(Vec::new()));

        for tag in ["a", "b", "c"] {
            let seen = Arc::clone(&seen);
            bus.subscribe::<Tick, _>(move |t: &Tick| {
                seen.lock().unwrap().push(format!("{tag}{}", t.0));
            });
        }
        assert_eq!(bus.subscriber_count::<Tick>(), 3);

        bus.publish(&Tick(1));
        bus.publish(&Tick(2));

        assert_eq!(
            *seen.lock().unwrap(),
            vec!["a1", "b1", "c1", "a2", "b2", "c2"]
        );
    }

    #[test]
    fn events_are_routed_by_type() {
        let bus = EventBus::new();
        let ticks = Arc::new(AtomicUsize::new(0));
        let others = Arc::new(AtomicUsize::new(0));

        let t = Arc::clone(&ticks);
        bus.subscribe::<Tick, _>(move |_| {
            t.fetch_add(1, Ordering::Relaxed);
        });
        let o = Arc::clone(&others);
        bus.subscribe::<Other, _>(move |_| {
            o.fetch_add(1, Ordering::Relaxed);
        });

        bus.publish(&Tick(0));
        assert_eq!(ticks.load(Ordering::Relaxed), 1);
        assert_eq!(others.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn publishing_without_subscribers_is_a_no_op() {
        let bus = EventBus::new();
        bus.publish(&Tick(7));
        assert_eq!(bus.subscriber_count::<Tick>(), 0);
    }

    /// A handler that publishes must not deadlock (lock released before dispatch).
    #[test]
    fn reentrant_publish_does_not_deadlock() {
        let bus = Arc::new(EventBus::new());
        let hits = Arc::new(AtomicUsize::new(0));

        let h = Arc::clone(&hits);
        bus.subscribe::<Other, _>(move |_| {
            h.fetch_add(1, Ordering::Relaxed);
        });

        let inner = Arc::clone(&bus);
        bus.subscribe::<Tick, _>(move |_| inner.publish(&Other));

        bus.publish(&Tick(1));
        assert_eq!(hits.load(Ordering::Relaxed), 1);
    }
}
