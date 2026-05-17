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

// ── proptest: any plan + any matching state → compile output is i3-safe ──

use proptest::prelude::*;

/// Generate a `Vec<Env>` of 0..=6 entries with unique names and slots, plus a
/// disjoint `Vec<Slot>` of 0..=3 unmanaged slots. All slots are within
/// 1..=20 — the algorithm uses `max_slot=9` so the range covers both the
/// shortcut zone and overflow.
fn arb_state() -> impl Strategy<Value = (Vec<Env>, Vec<Slot>)> {
    proptest::collection::vec(1i32..=20, 0..=10)
        .prop_filter("slots must be unique", |slots| {
            let mut sorted = slots.clone();
            sorted.sort();
            sorted.dedup();
            sorted.len() == slots.len()
        })
        .prop_flat_map(|slots| {
            let managed_count = std::cmp::min(slots.len(), 6);
            (Just(slots), 0usize..=managed_count)
        })
        .prop_flat_map(|(slots, managed_count)| {
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

fn arb_incoming(existing: &[Env], unmanaged: &[Slot]) -> impl Strategy<Value = Env> + 'static {
    let taken: std::collections::HashSet<i32> = existing
        .iter()
        .map(|e| e.slot.0)
        .chain(unmanaged.iter().map(|s| s.0))
        .collect();
    let free_slot = (1..=21).find(|n| !taken.contains(n)).unwrap_or(21);
    (0.0f64..=1.0).prop_map(move |score| Env {
        name: EnvName("incoming".into()),
        slot: Slot(free_slot),
        score,
    })
}

proptest! {
    /// For any well-formed (existing, unmanaged) snapshot and any incoming
    /// env, the full pipeline must produce an op sequence the simulator
    /// accepts at every step. Failure points to a real i3-safety violation
    /// (rename of non-existent OLD, reserved-prefix target, duplicate num).
    #[test]
    fn pipeline_is_i3_safe_for_any_state(
        (existing, unmanaged) in arb_state(),
    ) {
        let incoming_strategy = arb_incoming(&existing, &unmanaged);
        proptest!(|(incoming in incoming_strategy)| {
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
        });
    }
}
