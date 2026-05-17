//! Diff current i3 state against a target `LayoutSpec`, producing a `Plan`.
//! Uses optative's reconcile machinery: envs only in the spec become
//! `Spawn`; envs at a different slot become `Relocation`. The "more than
//! one new env per activation" invariant comes from upstream (the activate
//! handler creates at most one); we `debug_assert!` it inside `enter`.

use super::optative::{Lifecycle, ManagedSet, Reconcile};
use super::plan::*;
use super::spec::LayoutSpec;
use super::types::*;
use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt;

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
        assert!(
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
        // Rebalance never deactivates envs; managed-set drop is a no-op.
        Ok(())
    }
}

pub fn derive(current: &HashMap<EnvName, Slot>, spec: &LayoutSpec) -> Plan {
    debug_assert!(
        {
            let mut seen = std::collections::HashSet::new();
            spec.targets.values().all(|s| seen.insert(*s))
        },
        "LayoutSpec.targets must be a bijection — two envs share a slot",
    );

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
