//! End-to-end regression tests for the four historical bug classes that
//! drove this refactor. Each scenario is preserved with its original
//! parameters; assertions go through `optimize → derive → compile → I3Model`
//! so a future reader can map a bug report to the test that locks it.

use super::compile::compile;
use super::derive::derive;
use super::i3_model::I3Model;
use super::optimize::optimize;
use super::types::*;
use std::collections::HashMap;

fn env_name(s: &str) -> EnvName {
    EnvName(s.into())
}

fn env(name: &str, slot: i32, score: f64) -> Env {
    Env {
        name: env_name(name),
        slot: Slot(slot),
        score,
    }
}

/// Seed an `I3Model` with the supplied managed envs (all marked as having
/// content so the empty-ws-reaping rule never fires on the initial state)
/// plus any unmanaged-occupied slots under generic placeholder names.
fn seed_model(existing: &[Env], unmanaged_slots: &[Slot]) -> I3Model {
    let mut m = I3Model::default();
    for e in existing {
        m.insert(Handle::slotted(e.slot, &e.name), true);
    }
    for s in unmanaged_slots {
        m.insert(Handle(format!("{}", s.0)), true);
    }
    m
}

fn run_full_pipeline(existing: &[Env], incoming: Env, unmanaged: &[Slot]) -> I3Model {
    let mut model = seed_model(existing, unmanaged);
    let spec = optimize(existing, incoming, unmanaged, Slot(9));
    let current: HashMap<EnvName, Slot> =
        existing.iter().map(|e| (e.name.clone(), e.slot)).collect();
    let plan = derive(&current, &spec);
    for op in compile(&plan) {
        model
            .apply(&op)
            .expect("pipeline must produce i3-safe op sequence");
    }
    model
}

/// Issue #346 — swap-cycle bug. Original scenario: a low-score env at slot 3
/// and a high-score env at slot 11, with fillers locking all shortcut slots
/// so the only profitable move is the swap. The OLD path's lack of parking
/// meant the intermediate state had both at num=11. Compile's park-then-place
/// makes the bug structurally impossible.
#[test]
fn regression_346_swap_cycle_has_no_duplicate_num() {
    let mut existing = vec![env("low", 3, 0.1), env("high", 11, 0.9)];
    for s in [1i32, 2, 4, 5, 6, 7, 8, 9] {
        existing.push(env(&format!("filler-{s}"), s, 0.9));
    }
    let incoming = env("new", 12, 0.5);
    let model = run_full_pipeline(&existing, incoming, &[]);
    assert!(
        model
            .ws
            .contains_key(&Handle::slotted(Slot(3), &env_name("high")))
    );
}

/// Multi-hop phantom OLDs. Pre-fix: an env that walked through multiple slots
/// across `find_best_move` iterations (e.g. high score from 5 → 4 → 3) produced
/// two rename pairs whose intermediate OLD (`4: high`) never existed in i3.
/// Each `Relocation` now carries a single `from`/`to`, so a chain is
/// unrepresentable.
#[test]
fn regression_multi_hop_collapses_to_single_relocation() {
    let existing = vec![
        env("filler-1", 1, 0.9),
        env("filler-2", 2, 0.9),
        env("low", 3, 0.1),
        env("mid", 4, 0.5),
        env("high", 5, 0.95),
    ];
    let incoming = env("new", 6, 0.5);
    let model = run_full_pipeline(&existing, incoming, &[]);
    assert!(
        model
            .ws
            .contains_key(&Handle::slotted(Slot(1), &env_name("high")))
    );
}

/// Unmanaged-name collision. Pre-fix: a `HashMap<String, ...>` keyed by the
/// extracted env name collapsed all unmanaged workspaces (whose extracted
/// name is `""`) into a single entry, then emitted phantom renames for the
/// "lost" ones. The new pipeline keeps unmanaged slots out of `Plan`
/// entirely — they appear only as occupied-slot blockers for `optimize`.
#[test]
fn regression_unmanaged_collision_does_not_emit_phantom_renames() {
    let existing = vec![env("low", 3, 0.1), env("high", 5, 0.95)];
    let unmanaged = vec![Slot(1), Slot(2), Slot(10)];
    let incoming = env("new", 6, 0.5);
    let model = run_full_pipeline(&existing, incoming, &unmanaged);
    for s in [Slot(1), Slot(2), Slot(10)] {
        assert!(model.ws.contains_key(&Handle(format!("{}", s.0))));
    }
}

/// Empty-workspace race. Pre-fix: `activate_new_workspace` created the new
/// env's workspace eagerly, then ran sibling renames; i3 reaped the empty
/// freshly-focused workspace mid-flight. `compile` now emits the spawn
/// (single `workspace "N: env"` create+focus) as the LAST op, so there is
/// no window where an empty unfocused workspace can be reaped.
#[test]
fn regression_empty_ws_race_spawn_lands_safely_last() {
    let existing = vec![env("enwiro#376", 5, 0.8), env("enwiro#327", 6, 0.7)];
    let incoming = env("enwiro", 7, 0.95);
    let model = run_full_pipeline(&existing, incoming, &[]);
    assert!(
        model
            .ws
            .contains_key(&Handle::slotted(Slot(1), &env_name("enwiro")))
    );
}

use proptest::prelude::*;
use std::collections::HashSet;

/// Single strategy yielding a fully-consistent `(existing, unmanaged, incoming)`
/// triple: all slots disjoint, incoming's slot the lowest unused in `1..=21`.
fn arb_scenario() -> impl Strategy<Value = (Vec<Env>, Vec<Slot>, Env)> {
    proptest::collection::vec(1i32..=20, 0..=10)
        .prop_filter("slots must be unique", |slots| {
            let mut sorted = slots.clone();
            sorted.sort();
            sorted.dedup();
            sorted.len() == slots.len()
        })
        .prop_flat_map(|slots| {
            let managed_count = std::cmp::min(slots.len(), 6);
            let scores = proptest::collection::vec(0.0f64..=1.0, managed_count);
            let incoming_score = 0.0f64..=1.0;
            (Just(slots), Just(managed_count), scores, incoming_score)
        })
        .prop_map(|(slots, managed_count, scores, incoming_score)| {
            let mut managed = Vec::new();
            let mut unmanaged = Vec::new();
            for (i, slot) in slots.into_iter().enumerate() {
                if i < managed_count {
                    managed.push(Env {
                        name: EnvName(format!("env-{i}")),
                        slot: Slot(slot),
                        score: scores[i],
                    });
                } else {
                    unmanaged.push(Slot(slot));
                }
            }
            let taken: std::collections::HashSet<i32> = managed
                .iter()
                .map(|e| e.slot.0)
                .chain(unmanaged.iter().map(|s| s.0))
                .collect();
            let free_slot = (1..=21).find(|n| !taken.contains(n)).unwrap_or(21);
            let incoming = Env {
                name: EnvName("incoming".into()),
                slot: Slot(free_slot),
                score: incoming_score,
            };
            (managed, unmanaged, incoming)
        })
}

proptest! {
    /// For any well-formed (existing, unmanaged, incoming) snapshot, the full
    /// pipeline must produce an op sequence the simulator accepts at every
    /// step. Failure points to a real i3-safety violation (rename of
    /// non-existent OLD, reserved-prefix target, duplicate num).
    #[test]
    fn pipeline_is_i3_safe_for_any_state(
        (existing, unmanaged, incoming) in arb_scenario(),
    ) {
        let mut model = seed_model(&existing, &unmanaged);
        let spec = optimize(&existing, incoming, &unmanaged, Slot(9));
        let current: HashMap<EnvName, Slot> = existing
            .iter()
            .map(|e| (e.name.clone(), e.slot))
            .collect();
        let plan = derive(&current, &spec);
        for op in compile(&plan) {
            model.apply(&op).expect("compile must produce i3-safe ops");
        }
        for (env_name, target_slot) in &spec.targets {
            prop_assert!(
                model.ws.contains_key(&Handle::slotted(*target_slot, env_name)),
                "env {:?} did not end at slot {:?}", env_name, target_slot,
            );
        }
    }

    #[test]
    fn optimize_preserves_all_envs(
        (existing, unmanaged, incoming) in arb_scenario(),
    ) {
        let spec = optimize(&existing, incoming.clone(), &unmanaged, Slot(9));
        for e in &existing {
            prop_assert!(
                spec.targets.contains_key(&e.name),
                "existing env {:?} lost after optimize", e.name,
            );
        }
        prop_assert!(
            spec.targets.contains_key(&incoming.name),
            "incoming env {:?} lost after optimize", incoming.name,
        );
    }

    #[test]
    fn optimize_produces_slot_bijection(
        (existing, unmanaged, incoming) in arb_scenario(),
    ) {
        let spec = optimize(&existing, incoming, &unmanaged, Slot(9));
        let slots: Vec<Slot> = spec.targets.values().copied().collect();
        let unique: HashSet<Slot> = slots.iter().copied().collect();
        prop_assert_eq!(
            slots.len(), unique.len(),
            "duplicate slots in LayoutSpec: {:?}", slots,
        );
    }

    #[test]
    fn optimize_does_not_place_into_unmanaged_slots(
        (existing, unmanaged, incoming) in arb_scenario(),
    ) {
        let spec = optimize(&existing, incoming, &unmanaged, Slot(9));
        let unmanaged_set: HashSet<Slot> = unmanaged.iter().copied().collect();
        for (env_name, slot) in &spec.targets {
            prop_assert!(
                !unmanaged_set.contains(slot),
                "env {:?} placed at unmanaged slot {:?}", env_name, slot,
            );
        }
    }
}

#[derive(Clone, Debug)]
enum WorkflowStep {
    Activate { env_idx: usize, score: f64 },
    ActivateWithGear { env_idx: usize, score: f64 },
    ListenerRebalance,
    CloseAllWindows { env_idx: usize },
    SwitchWorkspace { env_idx: usize },
    GearWindowsArrive,
}

fn arb_initial_state() -> impl Strategy<Value = (Vec<Env>, Vec<Slot>)> {
    proptest::collection::vec(1i32..=15, 0..=6)
        .prop_filter("slots must be unique", |slots| {
            let mut sorted = slots.clone();
            sorted.sort();
            sorted.dedup();
            sorted.len() == slots.len()
        })
        .prop_flat_map(|slots| {
            let managed_count = std::cmp::min(slots.len(), 4);
            let scores = proptest::collection::vec(0.0f64..=1.0, managed_count);
            (Just(slots), Just(managed_count), scores)
        })
        .prop_map(|(slots, managed_count, scores)| {
            let mut managed = Vec::new();
            let mut unmanaged = Vec::new();
            for (i, slot) in slots.into_iter().enumerate() {
                if i < managed_count {
                    managed.push(Env {
                        name: EnvName(format!("env-{i}")),
                        slot: Slot(slot),
                        score: scores[i],
                    });
                } else {
                    unmanaged.push(Slot(slot));
                }
            }
            (managed, unmanaged)
        })
}

fn arb_workflow() -> impl Strategy<Value = ((Vec<Env>, Vec<Slot>), Vec<WorkflowStep>)> {
    arb_initial_state().prop_flat_map(|init| {
        let steps = proptest::collection::vec(
            prop_oneof![
                3 => (0usize..8, 0.0f64..=1.0)
                    .prop_map(|(idx, score)| WorkflowStep::Activate { env_idx: idx, score }),
                2 => (0usize..8, 0.0f64..=1.0)
                    .prop_map(|(idx, score)| WorkflowStep::ActivateWithGear { env_idx: idx, score }),
                1 => Just(WorkflowStep::ListenerRebalance),
                1 => (0usize..8).prop_map(|idx| WorkflowStep::CloseAllWindows { env_idx: idx }),
                1 => (0usize..8).prop_map(|idx| WorkflowStep::SwitchWorkspace { env_idx: idx }),
                1 => Just(WorkflowStep::GearWindowsArrive),
            ],
            1..=6,
        );
        (Just(init), steps)
    })
}

fn lowest_free_slot(managed: &[Env], unmanaged: &[Slot]) -> Slot {
    let taken: HashSet<i32> = managed
        .iter()
        .map(|e| e.slot.0)
        .chain(unmanaged.iter().map(|s| s.0))
        .collect();
    let n = (1..=30).find(|n| !taken.contains(n)).unwrap_or(30);
    Slot(n)
}

fn current_slot_map(managed: &[Env]) -> HashMap<EnvName, Slot> {
    managed.iter().map(|e| (e.name.clone(), e.slot)).collect()
}

proptest! {
    #[test]
    fn multi_step_workflow_preserves_invariants(
        ((initial_managed, unmanaged), steps) in arb_workflow(),
    ) {
        use super::optimize::{optimize, optimize_single_step, STABILITY_THRESHOLD};

        let mut model = seed_model(&initial_managed, &unmanaged);
        if let Some(first) = initial_managed.first() {
            model.focus(Handle::slotted(first.slot, &first.name));
        }
        let mut managed = initial_managed.clone();
        let env_pool: Vec<String> = (0..8).map(|i| format!("pool-{i}")).collect();
        let mut pending_gear: Vec<(EnvName, Slot)> = vec![];

        for (step_idx, step) in steps.iter().enumerate() {
            // Re-snapshot: filter out managed envs whose workspace was
            // reaped by i3 (mirrors real adapter's i3.get_workspaces()).
            managed.retain(|e| {
                model.ws.contains_key(&Handle::slotted(e.slot, &e.name))
            });

            // Invariant: envs with pending gear must not have been
            // reaped. This catches the empty-workspace reaping race
            // (issue #557): spawn Focus creates an empty workspace,
            // an intervening focus shift reaps it before gear windows
            // arrive. Look up by name since relocations can change
            // the slot.
            for (name, slot) in &mut pending_gear {
                if let Some(env) = managed.iter().find(|e| &e.name == name) {
                    *slot = env.slot;
                }
                let handle = Handle::slotted(*slot, name);
                prop_assert!(
                    model.ws.contains_key(&handle),
                    "step {}: env {:?} at slot {:?} with pending gear was reaped \
                     before gear windows arrived (empty-workspace reaping race)",
                    step_idx, name, slot,
                );
            }
            pending_gear.retain(|(name, slot)| {
                model.ws.contains_key(&Handle::slotted(*slot, name))
            });

            let mut pipeline_env_names: Option<HashSet<EnvName>> = None;

            match step {
                WorkflowStep::Activate { env_idx, score }
                | WorkflowStep::ActivateWithGear { env_idx, score } => {
                    let has_gear = matches!(step, WorkflowStep::ActivateWithGear { .. });
                    let env_name = &env_pool[*env_idx % env_pool.len()];
                    let already_exists = managed.iter().any(|e| e.name.0 == *env_name);
                    if already_exists {
                        continue;
                    }

                    // Exclude the focused empty workspace: i3 will reap it
                    // when the spawn Focus fires. Treat its slot as
                    // unmanaged-occupied so optimize won't target it.
                    let mut activate_unmanaged = unmanaged.clone();
                    let activate_managed: Vec<Env> = managed.iter()
                        .filter(|e| {
                            let h = Handle::slotted(e.slot, &e.name);
                            let is_focused = model.focused.as_ref() == Some(&h);
                            let is_empty = model.ws.get(&h) == Some(&false);
                            if is_focused && is_empty {
                                activate_unmanaged.push(e.slot);
                                false
                            } else {
                                true
                            }
                        })
                        .cloned()
                        .collect();

                    pipeline_env_names = Some(
                        activate_managed.iter().map(|e| e.name.clone()).collect(),
                    );

                    let free = lowest_free_slot(&activate_managed, &activate_unmanaged);
                    let incoming = Env {
                        name: EnvName(env_name.clone()),
                        slot: free,
                        score: *score,
                    };
                    let spec = optimize(&activate_managed, incoming.clone(), &activate_unmanaged, Slot(9));
                    let plan = derive(&current_slot_map(&activate_managed), &spec);
                    for op in compile(&plan) {
                        model
                            .apply(&op)
                            .unwrap_or_else(|e| panic!("step {step_idx}: i3 op failed: {e:?}"));
                    }

                    // Relocated workspaces already had content.
                    // The spawn workspace starts empty in i3 (Focus
                    // creates it without windows).
                    let spawn_env = plan.spawn.as_ref().map(|s| &s.env);
                    for (name, &slot) in &spec.targets {
                        let handle = Handle::slotted(slot, name);
                        let is_spawn = spawn_env == Some(name);
                        if is_spawn && has_gear {
                            // Fix for issue #557: the adapter combines
                            // the spawn Focus with the first gear exec
                            // in one i3 IPC message, so the workspace
                            // has a pending window immediately.
                            if let Some(has_content) = model.ws.get_mut(&handle) {
                                *has_content = true;
                            }
                            pending_gear.push((name.clone(), slot));
                        } else if is_spawn {
                            // No gear: workspace stays empty (user will
                            // open things manually). Not tracked as
                            // pending gear.
                        } else if let Some(has_content) = model.ws.get_mut(&handle) {
                            *has_content = true;
                        }
                    }

                    // Rebuild managed from spec, carrying scores
                    let score_map: HashMap<EnvName, f64> =
                        managed.iter().map(|e| (e.name.clone(), e.score)).collect();
                    managed = spec
                        .targets
                        .iter()
                        .map(|(name, &slot)| Env {
                            name: name.clone(),
                            slot,
                            score: if name == &incoming.name {
                                incoming.score
                            } else {
                                score_map.get(name).copied().unwrap_or(0.0)
                            },
                        })
                        .collect();
                    // Invariant: incoming env exists in model
                    prop_assert!(
                        spec.targets.contains_key(&EnvName(env_name.clone())),
                        "step {}: activated env '{}' not in spec", step_idx, env_name,
                    );
                }
                WorkflowStep::ListenerRebalance => {
                    pipeline_env_names = Some(
                        managed.iter().map(|e| e.name.clone()).collect(),
                    );
                    let spec = optimize_single_step(
                        &managed,
                        &unmanaged,
                        Slot(9),
                        STABILITY_THRESHOLD,
                    );
                    let plan = derive(&current_slot_map(&managed), &spec);
                    for op in compile(&plan) {
                        model
                            .apply(&op)
                            .unwrap_or_else(|e| panic!("step {step_idx}: listener op failed: {e:?}"));
                    }

                    let score_map: HashMap<EnvName, f64> =
                        managed.iter().map(|e| (e.name.clone(), e.score)).collect();
                    managed = spec
                        .targets
                        .iter()
                        .map(|(name, &slot)| Env {
                            name: name.clone(),
                            slot,
                            score: score_map.get(name).copied().unwrap_or(0.0),
                        })
                        .collect();
                }
                WorkflowStep::CloseAllWindows { env_idx } => {
                    if managed.is_empty() {
                        continue;
                    }
                    let idx = *env_idx % managed.len();
                    let has_pending = pending_gear.iter().any(|(n, _)| *n == managed[idx].name);
                    if has_pending {
                        continue;
                    }
                    let handle = Handle::slotted(managed[idx].slot, &managed[idx].name);
                    if let Some(has_content) = model.ws.get_mut(&handle) {
                        *has_content = false;
                    }
                }
                WorkflowStep::SwitchWorkspace { env_idx } => {
                    if managed.is_empty() {
                        continue;
                    }
                    let idx = *env_idx % managed.len();
                    let handle = Handle::slotted(managed[idx].slot, &managed[idx].name);
                    let _ = model.apply(&super::i3_op::I3Op::Focus { ws: handle });
                }
                WorkflowStep::GearWindowsArrive => {
                    for (name, slot) in pending_gear.drain(..) {
                        let handle = Handle::slotted(slot, &name);
                        if let Some(has_content) = model.ws.get_mut(&handle) {
                            *has_content = true;
                        }
                    }
                }
            }

            // Reconcile: remove managed envs that i3 reaped
            managed.retain(|e| {
                model.ws.contains_key(&Handle::slotted(e.slot, &e.name))
            });

            // Invariant: pipeline steps must not lose envs that were
            // included in the pipeline (after excluding empty-focused).
            if let Some(ref pipeline_names) = pipeline_env_names {
                for prev in pipeline_names {
                    prop_assert!(
                        managed.iter().any(|e| &e.name == prev),
                        "step {}: env {:?} lost during pipeline op", step_idx, prev,
                    );
                }
            }

            // Invariant: slot bijection
            let slots: Vec<Slot> = managed.iter().map(|e| e.slot).collect();
            let unique: HashSet<Slot> = slots.iter().copied().collect();
            prop_assert_eq!(
                slots.len(), unique.len(),
                "step {}: duplicate slots: {:?}", step_idx, slots,
            );

            // Invariant: no duplicate env names
            let names: Vec<&EnvName> = managed.iter().map(|e| &e.name).collect();
            let unique_names: HashSet<&EnvName> = names.iter().copied().collect();
            prop_assert_eq!(
                names.len(), unique_names.len(),
                "step {}: duplicate env names: {:?}", step_idx, names,
            );
        }
    }
}
