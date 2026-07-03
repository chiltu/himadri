use std::collections::HashMap;
use std::sync::Arc;

/// Process-level registry of named, shared plugin stores.
///
/// Stateful plugins (budget spend, rate-limit windows, …) keep their
/// in-memory state in a store registered under a `store_id`, so that
/// multiple plugin instances configured with the same id share state and
/// admin APIs can reach a store by name.
///
/// Declare one static registry per store type:
///
/// ```ignore
/// static STORES: Lazy<StoreRegistry<SpendStore>> = Lazy::new(StoreRegistry::default);
/// ```
pub struct StoreRegistry<T> {
    stores: parking_lot::RwLock<HashMap<String, Arc<T>>>,
}

impl<T> Default for StoreRegistry<T> {
    fn default() -> Self {
        Self {
            stores: parking_lot::RwLock::new(HashMap::new()),
        }
    }
}

impl<T> StoreRegistry<T> {
    /// Return the store registered under `store_id`, creating it with
    /// `create` if absent. The first plugin instance to reference a store id
    /// fixes that store's construction parameters.
    pub fn get_or_create(&self, store_id: &str, create: impl FnOnce() -> T) -> Arc<T> {
        if let Some(store) = self.stores.read().get(store_id) {
            return store.clone();
        }

        self.stores
            .write()
            .entry(store_id.to_string())
            .or_insert_with(|| Arc::new(create()))
            .clone()
    }

    /// Run `f` against the store registered under `store_id`, if any.
    /// Used by the free-function admin helpers (reset / inspect by name).
    pub fn with<R>(&self, store_id: &str, f: impl FnOnce(&T) -> R) -> Option<R> {
        self.stores.read().get(store_id).map(|store| f(store))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_or_create_shares_instances_per_id() {
        let registry: StoreRegistry<i32> = StoreRegistry::default();
        let a = registry.get_or_create("a", || 1);
        let a2 = registry.get_or_create("a", || 2);
        assert!(Arc::ptr_eq(&a, &a2));
        assert_eq!(*a2, 1);

        let b = registry.get_or_create("b", || 3);
        assert_eq!(*b, 3);
    }

    #[test]
    fn with_only_touches_existing_stores() {
        let registry: StoreRegistry<i32> = StoreRegistry::default();
        assert_eq!(registry.with("missing", |v| *v), None);
        registry.get_or_create("present", || 7);
        assert_eq!(registry.with("present", |v| *v), Some(7));
    }
}
