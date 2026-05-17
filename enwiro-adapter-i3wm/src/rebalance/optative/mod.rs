//! Mirror of `tauler/optative/src/lib.rs` @ rev 70d71de.
//!
//! TODO: replace with `optative` from crates.io once published. While
//! inline-copied, keep the surface area minimal and avoid divergence — any
//! local change should be paired with an upstream issue/PR. The only
//! intentional addition here is `ManagedSet::insert`, used by `rebalance::derive`
//! to seed initial state from i3 without firing `enter` for existing
//! workspaces; propose upstream as a peer to `reconcile`.

#![allow(dead_code, unused_imports)]

use std::collections::HashMap;
use std::fmt::{Debug, Display};
use std::hash::Hash;

pub mod reconcile;
pub use reconcile::{Reconcile, ReconcileErrors};

pub struct LifecycleContext {
    pub display_name: String,
    pub metadata: serde_json::Map<String, serde_json::Value>,
}

pub trait Lifecycle: Display {
    type Key: Hash + Eq + Clone + serde::Serialize + serde::de::DeserializeOwned;
    type State;
    type Context;
    type Output;
    type Error;

    fn key(&self) -> Self::Key;

    fn enter(
        self,
        ctx: &mut Self::Context,
        output: &mut Self::Output,
    ) -> Result<Self::State, Self::Error>;

    fn reconcile_self(
        self,
        state: &mut Self::State,
        ctx: &mut Self::Context,
        output: &mut Self::Output,
    ) -> Result<(), Self::Error>;

    fn exit(
        state: Self::State,
        ctx: &mut Self::Context,
        output: &mut Self::Output,
    ) -> Result<(), Self::Error>;

    fn enhance_lifecycle_context(&self, _ctx: &mut LifecycleContext) {}

    fn enhance_lifecycle_state_context(_state: &Self::State, _ctx: &mut LifecycleContext) {}

    fn lifecycle_context(&self) -> LifecycleContext {
        let mut ctx = LifecycleContext {
            display_name: self.to_string(),
            metadata: serde_json::Map::new(),
        };
        self.enhance_lifecycle_context(&mut ctx);
        ctx
    }

    fn lifecycle_state_context(state: &Self::State) -> LifecycleContext {
        let mut ctx = LifecycleContext {
            display_name: String::new(),
            metadata: serde_json::Map::new(),
        };
        Self::enhance_lifecycle_state_context(state, &mut ctx);
        ctx
    }

    fn wrap_enter(
        self,
        ctx: &mut Self::Context,
        output: &mut Self::Output,
    ) -> Result<Self::State, Self::Error>
    where
        Self: Sized,
    {
        self.enter(ctx, output)
    }

    fn wrap_reconcile(
        self,
        state: &mut Self::State,
        ctx: &mut Self::Context,
        output: &mut Self::Output,
    ) -> Result<(), Self::Error>
    where
        Self: Sized,
    {
        self.reconcile_self(state, ctx, output)
    }

    fn wrap_exit(
        state: Self::State,
        ctx: &mut Self::Context,
        output: &mut Self::Output,
    ) -> Result<(), Self::Error> {
        Self::exit(state, ctx, output)
    }
}

pub struct ManagedSet<T: Lifecycle> {
    store: HashMap<T::Key, T::State>,
}

impl<T: Lifecycle> Default for ManagedSet<T> {
    fn default() -> Self {
        Self {
            store: HashMap::new(),
        }
    }
}

impl<T: Lifecycle + 'static> ManagedSet<T>
where
    T::Error: Debug,
{
    pub fn new() -> Self {
        Self::default()
    }

    /// Local addition (not in upstream optative): seed initial state without
    /// firing `enter` lifecycle hooks. Used by `rebalance::derive` to populate
    /// the set from an external snapshot (the current i3 workspace layout).
    pub fn insert(&mut self, key: T::Key, state: T::State) {
        self.store.insert(key, state);
    }

    fn dedup_by_key(items: impl IntoIterator<Item = T>) -> HashMap<T::Key, T> {
        let mut map = HashMap::new();
        for item in items {
            map.insert(item.key(), item);
        }
        map
    }

    fn exit_removed(
        &mut self,
        new_map: &HashMap<T::Key, T>,
        ctx: &mut T::Context,
        output: &mut T::Output,
        errors: &mut ReconcileErrors<T::Key, T::Error>,
    ) {
        let exit_keys: Vec<T::Key> = self
            .store
            .keys()
            .filter(|k| !new_map.contains_key(*k))
            .cloned()
            .collect();
        for key in exit_keys {
            let state = self.store.remove(&key).unwrap();
            if let Err(e) = T::wrap_exit(state, ctx, output) {
                errors.push((key, e));
            }
        }
    }

    fn update_existing(
        &mut self,
        new_map: &mut HashMap<T::Key, T>,
        ctx: &mut T::Context,
        output: &mut T::Output,
        errors: &mut ReconcileErrors<T::Key, T::Error>,
    ) {
        let update_keys: Vec<T::Key> = new_map
            .keys()
            .filter(|k| self.store.contains_key(*k))
            .cloned()
            .collect();
        for key in update_keys {
            let item = new_map.remove(&key).unwrap();
            let state = self.store.get_mut(&key).unwrap();
            if let Err(e) = item.wrap_reconcile(state, ctx, output) {
                let old_state = self.store.remove(&key).unwrap();
                if let Err(exit_e) = T::wrap_exit(old_state, ctx, output) {
                    errors.push((key.clone(), exit_e));
                }
                errors.push((key, e));
            }
        }
    }

    fn enter_new(
        &mut self,
        mut new_map: HashMap<T::Key, T>,
        ctx: &mut T::Context,
        output: &mut T::Output,
        errors: &mut ReconcileErrors<T::Key, T::Error>,
    ) {
        let enter_keys: Vec<T::Key> = new_map
            .keys()
            .filter(|k| !self.store.contains_key(*k))
            .cloned()
            .collect();
        for key in enter_keys {
            let item = new_map.remove(&key).unwrap();
            match item.wrap_enter(ctx, output) {
                Ok(state) => {
                    self.store.insert(key, state);
                }
                Err(e) => {
                    errors.push((key, e));
                }
            }
        }
    }

    pub fn get(&self, key: &T::Key) -> Option<&T::State> {
        self.store.get(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&T::Key, &T::State)> {
        self.store.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&T::Key, &mut T::State)> {
        self.store.iter_mut()
    }

    pub fn get_mut(&mut self, key: &T::Key) -> Option<&mut T::State> {
        self.store.get_mut(key)
    }
}

impl<T: Lifecycle + 'static> reconcile::Reconcile<T> for ManagedSet<T>
where
    T::Error: Debug,
{
    fn reconcile(
        &mut self,
        desired: impl IntoIterator<Item = T>,
        ctx: &mut T::Context,
        output: &mut T::Output,
    ) -> ReconcileErrors<T::Key, T::Error> {
        let mut errors = ReconcileErrors::new();
        let mut new_map = Self::dedup_by_key(desired);
        self.exit_removed(&new_map, ctx, output, &mut errors);
        self.update_existing(&mut new_map, ctx, output, &mut errors);
        self.enter_new(new_map, ctx, output, &mut errors);
        errors
    }
}
