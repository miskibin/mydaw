//! Type-keyed service registry.

use std::any::{type_name, Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::error::{KernelError, KernelResult};

/// Dependency-injection container keyed by concrete type.
///
/// A plugin publishes a service in `register()` and any later plugin resolves it
/// by type: `ctx.services.get::<AudioDeviceService>()`. Because the key is the
/// type, there is exactly one instance per type -- registering a second one is an
/// error rather than a silent overwrite, which would otherwise produce two
/// plugins talking to different "the" audio device.
///
/// # Threading
/// Interior `RwLock`, so `provide`/`get` both take `&self` and the registry can be
/// shared behind an `Arc`. **Non-RT only** -- it takes a lock and clones an `Arc`.
/// Never touch this from an audio callback; resolve services once at start-up and
/// hand plain data across the RT wall.
#[derive(Default)]
pub struct ServiceRegistry {
    entries: RwLock<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
}

impl ServiceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish a service instance.
    ///
    /// # Errors
    /// [`KernelError::DuplicateService`] if a service of type `T` already exists.
    pub fn provide<T: Send + Sync + 'static>(&self, service: Arc<T>) -> KernelResult<()> {
        let mut entries = self.entries.write();
        if entries.contains_key(&TypeId::of::<T>()) {
            return Err(KernelError::DuplicateService(type_name::<T>()));
        }
        entries.insert(TypeId::of::<T>(), service);
        Ok(())
    }

    /// Resolve a service by type, or `None` if nothing provided it.
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        let entries = self.entries.read();
        entries
            .get(&TypeId::of::<T>())
            // The map is keyed by `TypeId::of::<T>()`, so this downcast cannot fail.
            .and_then(|any| Arc::clone(any).downcast::<T>().ok())
    }

    /// Whether a service of type `T` is registered.
    pub fn contains<T: Send + Sync + 'static>(&self) -> bool {
        self.entries.read().contains_key(&TypeId::of::<T>())
    }

    /// Number of registered services.
    pub fn len(&self) -> usize {
        self.entries.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.read().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Transport {
        bpm: f64,
    }
    struct Mixer;

    #[test]
    fn provide_then_get_roundtrip() {
        let reg = ServiceRegistry::new();
        reg.provide(Arc::new(Transport { bpm: 128.0 })).unwrap();
        let t = reg.get::<Transport>().expect("service should resolve");
        assert_eq!(t.bpm, 128.0);
    }

    #[test]
    fn get_unregistered_is_none() {
        let reg = ServiceRegistry::new();
        assert!(reg.get::<Mixer>().is_none());
        assert!(!reg.contains::<Mixer>());
        assert!(reg.is_empty());
    }

    #[test]
    fn duplicate_provide_is_an_error_and_keeps_the_original() {
        let reg = ServiceRegistry::new();
        reg.provide(Arc::new(Transport { bpm: 128.0 })).unwrap();
        let err = reg.provide(Arc::new(Transport { bpm: 90.0 })).unwrap_err();
        assert!(matches!(err, KernelError::DuplicateService(name) if name.contains("Transport")));
        assert_eq!(reg.get::<Transport>().unwrap().bpm, 128.0);
    }

    #[test]
    fn distinct_types_coexist() {
        let reg = ServiceRegistry::new();
        reg.provide(Arc::new(Transport { bpm: 174.0 })).unwrap();
        reg.provide(Arc::new(Mixer)).unwrap();
        assert_eq!(reg.len(), 2);
        assert!(reg.contains::<Transport>());
        assert!(reg.contains::<Mixer>());
    }

    #[test]
    fn shared_instances_are_the_same_object() {
        let reg = ServiceRegistry::new();
        let original = Arc::new(Transport { bpm: 100.0 });
        reg.provide(Arc::clone(&original)).unwrap();
        let resolved = reg.get::<Transport>().unwrap();
        assert!(Arc::ptr_eq(&original, &resolved));
    }
}
