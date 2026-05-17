//! Plan → i3-op sequence. The only place that knows about i3's quirks
//! (reserved `__` prefix, num collisions, empty-ws reaping).
//!
//! Correctness sketch:
//! 1. Park each relocated workspace under a unique non-numbered name.
//!    After this phase no place-target num is occupied by a live workspace.
//! 2. Place each parked workspace at its target — succeeds because the
//!    parked workspace still exists under its unique name and the target
//!    num was vacated in phase 1.
//! 3. Spawn (if any) goes last: its slot is either initially empty (no
//!    relocation targeted it) or vacated in phase 1+2. Emitted as a single
//!    `workspace "N: env"` which creates+focuses atomically — there's no
//!    window where an empty unfocused workspace exists for i3 to reap.

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
        assert_eq!(ops.len(), 3);
    }

    #[test]
    fn compile_empty_plan_produces_no_ops() {
        let plan = Plan::default();
        assert_eq!(compile(&plan), vec![]);
    }

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
