//! The only function in the rebalance pipeline that knows about i3's quirks:
//! reserved `__` prefix, num collisions (resolved by park-then-place), and
//! spawn-last ordering. Everything above this layer is data; this is where
//! the i3 protocol surfaces.
//!
//! Correctness sketch:
//! 1. Park phase: every relocated workspace is renamed to a unique
//!    non-numbered name. After phase 1, no target num of any place is
//!    occupied by a live workspace.
//! 2. Place phase: each parked workspace is renamed to `N: env`. Each
//!    succeeds — the parked workspace still exists under its unique parking
//!    name, and the target num was vacated in phase 1.
//! 3. Spawn (if any): `workspace "N: env"` creates + focuses. Its target
//!    slot is either initially empty (no relocation targeted it) or vacated
//!    in phase 1+2.

use super::i3_op::I3Op;
use super::plan::*;
use super::types::*;

pub fn compile(plan: &Plan) -> Vec<I3Op> {
    let token = std::process::id();
    let mut ops = Vec::with_capacity(plan.relocations.len() * 2 + plan.spawn.iter().len());

    for (i, r) in plan.relocations.iter().enumerate() {
        ops.push(I3Op::Rename {
            from: Handle::slotted(r.from, &r.env),
            to: Handle::parked(token, i),
        });
    }
    for (i, r) in plan.relocations.iter().enumerate() {
        ops.push(I3Op::Rename {
            from: Handle::parked(token, i),
            to: Handle::slotted(r.to, &r.env),
        });
    }
    if let Some(s) = &plan.spawn {
        ops.push(I3Op::Focus {
            ws: Handle::slotted(s.at, &s.env),
        });
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::super::i3_model::I3Model;
    use super::*;

    /// Helper: build a tiny initial model where every relocation's `from`
    /// has content (real workspace) and the env named by `spawn` does NOT
    /// exist yet.
    fn model_for(plan: &Plan) -> I3Model {
        let mut m = I3Model::default();
        for r in &plan.relocations {
            m.insert(Handle::slotted(r.from, &r.env), true);
        }
        m
    }

    #[test]
    fn compile_emits_2x_relocations_plus_optional_spawn() {
        let plan = Plan {
            relocations: vec![Relocation {
                env: EnvName("a".into()),
                from: Slot(5),
                to: Slot(6),
            }],
            spawn: Some(Spawn {
                env: EnvName("b".into()),
                at: Slot(7),
            }),
        };
        let ops = compile(&plan);
        assert_eq!(ops.len(), 3); // 1 park + 1 place + 1 spawn
    }

    #[test]
    fn compile_empty_plan_produces_no_ops() {
        let plan = Plan::default();
        assert_eq!(compile(&plan), vec![]);
    }

    /// Bug class 1: rename of non-existent OLD. Cannot happen because
    /// every `Relocation` carries an env that the model is seeded for.
    /// This test pushes the swap-cycle scenario through compile + sim.
    #[test]
    fn compile_handles_swap_without_duplicate_num() {
        let plan = Plan {
            relocations: vec![
                Relocation {
                    env: EnvName("x".into()),
                    from: Slot(5),
                    to: Slot(6),
                },
                Relocation {
                    env: EnvName("y".into()),
                    from: Slot(6),
                    to: Slot(5),
                },
            ],
            spawn: None,
        };
        let mut model = model_for(&plan);
        for op in compile(&plan) {
            model.apply(&op).expect("compile must produce valid ops");
        }
        // Final state: x at 6, y at 5.
        assert!(
            model
                .ws
                .contains_key(&Handle::slotted(Slot(6), &EnvName("x".into())))
        );
        assert!(
            model
                .ws
                .contains_key(&Handle::slotted(Slot(5), &EnvName("y".into())))
        );
    }

    /// Bug class 4: spawn before relocations would leave a focused-empty
    /// workspace during renames, which i3 then reaps. Compile emits spawn
    /// LAST by construction. This test exercises the swap + spawn case end
    /// to end and asserts the final state matches.
    #[test]
    fn compile_spawn_lands_at_target_after_relocations() {
        let plan = Plan {
            relocations: vec![Relocation {
                env: EnvName("existing".into()),
                from: Slot(5),
                to: Slot(6),
            }],
            spawn: Some(Spawn {
                env: EnvName("new".into()),
                at: Slot(5),
            }),
        };
        let mut model = model_for(&plan);
        for op in compile(&plan) {
            model.apply(&op).expect("compile must produce valid ops");
        }
        assert!(
            model
                .ws
                .contains_key(&Handle::slotted(Slot(5), &EnvName("new".into())))
        );
        assert!(
            model
                .ws
                .contains_key(&Handle::slotted(Slot(6), &EnvName("existing".into())))
        );
    }

    /// The four-workspace cycle from yesterday's bug ("avoid empty-workspace
    /// race"). Existing path produced a 4-park sequence where the new env
    /// (which didn't exist) was parked last and i3 had already reaped it.
    /// The new design separates Spawn from relocations entirely.
    #[test]
    fn compile_three_way_cycle_with_spawn_succeeds() {
        let plan = Plan {
            relocations: vec![
                Relocation {
                    env: EnvName("a".into()),
                    from: Slot(5),
                    to: Slot(6),
                },
                Relocation {
                    env: EnvName("b".into()),
                    from: Slot(6),
                    to: Slot(7),
                },
            ],
            spawn: Some(Spawn {
                env: EnvName("c".into()),
                at: Slot(5),
            }),
        };
        let mut model = model_for(&plan);
        for op in compile(&plan) {
            model.apply(&op).expect("compile must produce valid ops");
        }
        assert!(
            model
                .ws
                .contains_key(&Handle::slotted(Slot(6), &EnvName("a".into())))
        );
        assert!(
            model
                .ws
                .contains_key(&Handle::slotted(Slot(7), &EnvName("b".into())))
        );
        assert!(
            model
                .ws
                .contains_key(&Handle::slotted(Slot(5), &EnvName("c".into())))
        );
    }
}
