//! Mirror of `tauler/optative/src/reconcile.rs` @ rev 70d71de.
//! TODO: replace with `optative` from crates.io once published.

#![allow(dead_code)]

use super::Lifecycle;

pub type ReconcileErrors<K, E> = Vec<(K, E)>;

pub trait Reconcile<T: Lifecycle> {
    fn reconcile(
        &mut self,
        desired: impl IntoIterator<Item = T>,
        ctx: &mut T::Context,
        output: &mut T::Output,
    ) -> ReconcileErrors<T::Key, T::Error>;
}
