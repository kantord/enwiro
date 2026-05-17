use super::types::*;
use std::collections::HashMap;

/// Declarative target slot per managed env. Slot-bijection (no two envs
/// share a slot) is a contract `optimize` produces and `derive` re-checks
/// via `debug_assert!`; downstream layers trust.
#[derive(Clone, Debug, Default)]
pub struct LayoutSpec {
    pub targets: HashMap<EnvName, Slot>,
}
