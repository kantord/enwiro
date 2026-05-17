#![allow(dead_code)]

use super::types::*;
use std::collections::HashMap;

/// Declarative target: every managed env should end up at this slot after
/// rebalance. The bijection (one env per slot) is an invariant `optimize`
/// produces; `optimize` is tested for it. Other layers trust.
#[derive(Clone, Debug, Default)]
pub struct LayoutSpec {
    pub targets: HashMap<EnvName, Slot>,
}
