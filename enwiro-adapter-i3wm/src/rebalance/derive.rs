#![allow(dead_code)]

//! Layer 3 → Layer 2 — diff `current` vs `LayoutSpec`, produce a `Plan`.
//!
//! Uses optative's `Lifecycle` + `ManagedSet::reconcile`:
//! - Seed the managed set with current i3 state via the local
//!   `ManagedSet::insert` extension (no `enter` calls).
//! - Reconcile against the desired layout.
//! - `enter` is called for envs in desired but not in current → these are
//!   the spawn (at most one per activation, asserted).
//! - `reconcile_self` is called for envs in both → emit `Relocation` if the
//!   slot changed.
//! - `exit` is called for envs in current but not in desired → never used
//!   for rebalance (we don't deactivate envs through reconciliation).

use super::optative::{Lifecycle, ManagedSet, Reconcile};
use super::plan::*;
use super::spec::LayoutSpec;
use super::types::*;
use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt;

/// Lifecycle item: "this env should be at this slot."
struct ManagedEnv {
    env: EnvName,
    target: Slot,
}

impl fmt::Display for ManagedEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.env.0)
    }
}

impl Lifecycle for ManagedEnv {
    type Key = String;
    type State = Slot;
    type Context = ();
    type Output = Plan;
    type Error = Infallible;

    fn key(&self) -> String {
        self.env.0.clone()
    }

    fn enter(self, _ctx: &mut (), out: &mut Plan) -> Result<Slot, Infallible> {
        debug_assert!(
            out.spawn.is_none(),
            "more than one new env in spec — upstream invariant violated"
        );
        out.spawn = Some(Spawn {
            env: self.env,
            at: self.target,
        });
        Ok(self.target)
    }

    fn reconcile_self(
        self,
        state: &mut Slot,
        _ctx: &mut (),
        out: &mut Plan,
    ) -> Result<(), Infallible> {
        if *state != self.target {
            out.relocations.push(Relocation {
                env: self.env,
                from: *state,
                to: self.target,
            });
            *state = self.target;
        }
        Ok(())
    }

    fn exit(_state: Slot, _ctx: &mut (), _out: &mut Plan) -> Result<(), Infallible> {
        // Rebalance never deactivates envs.
        Ok(())
    }
}

pub fn derive(current: &HashMap<EnvName, Slot>, spec: &LayoutSpec) -> Plan {
    let mut managed = ManagedSet::<ManagedEnv>::default();
    for (env, &slot) in current {
        managed.insert(env.0.clone(), slot);
    }

    let desired: Vec<ManagedEnv> = spec
        .targets
        .iter()
        .map(|(env, &target)| ManagedEnv {
            env: env.clone(),
            target,
        })
        .collect();

    let mut plan = Plan::default();
    let errors = managed.reconcile(desired, &mut (), &mut plan);
    debug_assert!(errors.is_empty(), "Infallible: errors must be empty");
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(name: &str) -> EnvName {
        EnvName(name.into())
    }

    #[test]
    fn derive_empty_when_current_matches_spec() {
        let current = HashMap::from([(env("a"), Slot(3))]);
        let spec = LayoutSpec {
            targets: HashMap::from([(env("a"), Slot(3))]),
        };
        let plan = derive(&current, &spec);
        assert!(plan.relocations.is_empty());
        assert!(plan.spawn.is_none());
    }

    #[test]
    fn derive_emits_relocation_for_slot_change() {
        let current = HashMap::from([(env("a"), Slot(3))]);
        let spec = LayoutSpec {
            targets: HashMap::from([(env("a"), Slot(5))]),
        };
        let plan = derive(&current, &spec);
        assert_eq!(
            plan.relocations,
            vec![Relocation {
                env: env("a"),
                from: Slot(3),
                to: Slot(5),
            }]
        );
        assert!(plan.spawn.is_none());
    }

    #[test]
    fn derive_emits_spawn_for_env_only_in_spec() {
        let current: HashMap<EnvName, Slot> = HashMap::new();
        let spec = LayoutSpec {
            targets: HashMap::from([(env("new"), Slot(4))]),
        };
        let plan = derive(&current, &spec);
        assert_eq!(
            plan.spawn,
            Some(Spawn {
                env: env("new"),
                at: Slot(4),
            })
        );
        assert!(plan.relocations.is_empty());
    }

    #[test]
    fn derive_emits_relocations_and_spawn_for_mixed_case() {
        let current = HashMap::from([(env("a"), Slot(3)), (env("b"), Slot(5))]);
        let spec = LayoutSpec {
            targets: HashMap::from([
                (env("a"), Slot(3)),   // unchanged
                (env("b"), Slot(6)),   // relocate
                (env("new"), Slot(7)), // spawn
            ]),
        };
        let plan = derive(&current, &spec);
        assert_eq!(
            plan.relocations,
            vec![Relocation {
                env: env("b"),
                from: Slot(5),
                to: Slot(6),
            }]
        );
        assert_eq!(
            plan.spawn,
            Some(Spawn {
                env: env("new"),
                at: Slot(7),
            })
        );
    }
}
