use super::types::*;

/// Typed delta between the current i3 layout and a target `LayoutSpec`.
///
/// Invariants enforced by the type:
/// - Each `Relocation` references an env that exists in i3 (no phantom OLDs).
/// - At most one `Spawn` per plan (`Option`, not `Vec`).
/// - `Spawn` is structurally separated from `relocations`, so the compiler
///   can emit it AFTER all relocations — no way to interleave.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub relocations: Vec<Relocation>,
    pub spawn: Option<Spawn>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relocation {
    pub env: EnvName,
    pub from: Slot,
    pub to: Slot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spawn {
    pub env: EnvName,
    pub at: Slot,
}
