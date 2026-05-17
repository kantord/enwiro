#![allow(dead_code)]

//! Layer 4 → 3 — score-based optimization. Given the current managed envs +
//! their scores, the incoming env, and which slots are occupied by unmanaged
//! workspaces, return a `LayoutSpec` of target slot assignments.
//!
//! Direct port of `find_best_move` + `boost_incoming_score` + the convergence
//! loop from `main.rs`, restated against typed `Env` / `Slot` inputs.
//!
//! The algorithm itself is unchanged — the simulator-validation gate proves
//! the rename-pair sequence the OLD code produces is i3-safe, and `compile`
//! produces the same rename pairs from any equivalent `LayoutSpec`. Porting
//! is therefore a pure type-level refactor; the scoring logic stays.

use super::spec::LayoutSpec;
use super::types::*;
use std::collections::HashMap;

pub const STABILITY_THRESHOLD: f64 = 0.05;

/// DCG position discount — value of occupying slot `i`.
fn disc(slot: Slot) -> f64 {
    1.0_f64 / (slot.0 as f64 + 1.0).log2()
}

/// One iteration: pick the single highest-net-benefit move (swap or
/// compaction) and return how to apply it. Empty if no profitable move.
///
/// Output: a `Vec<(EnvName, Slot)>` listing each env that moves and its new
/// slot. Swap → 2 entries; compaction → 1 entry; nothing to do → 0 entries.
fn find_best_move(
    managed: &[Env],
    unmanaged_slots: &[Slot],
    max_slot: Slot,
    stability_threshold: f64,
) -> Vec<(EnvName, Slot)> {
    // Slots physically taken (managed + unmanaged) — used to detect truly empty slots.
    let mut all_occupied: std::collections::HashSet<Slot> =
        managed.iter().map(|e| e.slot).collect();
    all_occupied.extend(unmanaged_slots.iter().copied());

    let mut best_nb: f64 = 0.0;
    let mut best_move: Option<(usize, usize, bool)> = None; // (lo_idx, hi_idx, is_swap)

    // Score-swap: both slots managed-occupied, slot_lo < slot_hi, score_hi > score_lo.
    for i in 0..managed.len() {
        for j in (i + 1)..managed.len() {
            let (lo_idx, hi_idx) = if managed[i].slot < managed[j].slot {
                (i, j)
            } else {
                (j, i)
            };
            let (slot_lo, score_lo) = (managed[lo_idx].slot, managed[lo_idx].score);
            let (slot_hi, score_hi) = (managed[hi_idx].slot, managed[hi_idx].score);
            if score_hi > score_lo {
                let nb =
                    (score_hi - score_lo) * (disc(slot_lo) - disc(slot_hi)) - stability_threshold;
                if nb > best_nb {
                    best_nb = nb;
                    best_move = Some((lo_idx, hi_idx, true));
                }
            }
        }
    }

    // Compaction: empty slot_i in [1..=max_slot], managed env at slot_j > slot_i.
    for raw in 1..=max_slot.0 {
        let slot_i = Slot(raw);
        if all_occupied.contains(&slot_i) {
            continue;
        }
        for (j_idx, env_j) in managed.iter().enumerate() {
            if env_j.slot <= slot_i || env_j.score <= 0.0 {
                continue;
            }
            let nb = env_j.score * (disc(slot_i) - disc(env_j.slot));
            if nb > best_nb {
                best_nb = nb;
                // Encode compaction as (target_idx_placeholder, env_j_idx, false).
                // We don't have a managed entry at the target slot; we use the
                // env_j index and the raw target slot via the bool=false branch
                // below, but we still need slot_i to emit the rename. Stash
                // slot_i in the loop variable via a wrapping technique:
                // simplest is to recompute on the match arm.
                best_move = Some((slot_i.0 as usize, j_idx, false));
            }
        }
    }

    match best_move {
        None => Vec::new(),
        Some((lo_idx, hi_idx, true)) => {
            // Swap: lo_idx env → hi.slot, hi_idx env → lo.slot.
            let lo = &managed[lo_idx];
            let hi = &managed[hi_idx];
            vec![(lo.name.clone(), hi.slot), (hi.name.clone(), lo.slot)]
        }
        Some((target_slot_raw, j_idx, false)) => {
            // Compaction: env_j moves to slot encoded in lo_idx (= raw slot number).
            vec![(managed[j_idx].name.clone(), Slot(target_slot_raw as i32))]
        }
    }
}

/// If `env_name` sits at a slot > `max_slot` and its current score is below
/// the minimum score among managed shortcut envs, raise it to just above that
/// minimum. This guarantees the env will win exactly one swap into the
/// shortcut zone during the convergence loop.
fn boost_incoming_score(managed: &mut [Env], env_name: &EnvName, max_slot: Slot) {
    let Some(incoming) = managed.iter().find(|e| &e.name == env_name) else {
        return;
    };
    if incoming.slot <= max_slot {
        return;
    }
    let min_shortcut_score = managed
        .iter()
        .filter(|e| e.slot <= max_slot)
        .map(|e| e.score)
        .fold(f64::INFINITY, f64::min);
    if min_shortcut_score.is_finite()
        && let Some(target) = managed.iter_mut().find(|e| &e.name == env_name)
    {
        target.score = target.score.max(min_shortcut_score + f64::EPSILON);
    }
}

/// Score-based optimization. Builds the desired layout for managed envs after
/// the incoming env joins. `unmanaged_slots` are physically occupied but
/// untouchable.
///
/// `incoming.slot` is the free slot we'll initially park the new env at; the
/// algorithm may move it to a better slot in the shortcut zone.
pub fn optimize(
    existing: &[Env],
    incoming: Env,
    unmanaged_slots: &[Slot],
    max_slot: Slot,
) -> LayoutSpec {
    let mut managed: Vec<Env> = existing.to_vec();
    managed.push(incoming.clone());
    boost_incoming_score(&mut managed, &incoming.name, max_slot);

    loop {
        let moves = find_best_move(&managed, unmanaged_slots, max_slot, 0.0);
        if moves.is_empty() {
            break;
        }
        for (env_name, new_slot) in moves {
            if let Some(env) = managed.iter_mut().find(|e| e.name == env_name) {
                env.slot = new_slot;
            }
        }
    }

    let targets: HashMap<EnvName, Slot> = managed.into_iter().map(|e| (e.name, e.slot)).collect();
    LayoutSpec { targets }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(name: &str, slot: i32, score: f64) -> Env {
        Env {
            name: EnvName(name.into()),
            slot: Slot(slot),
            score,
        }
    }

    #[test]
    fn optimize_no_existing_envs_places_new_at_free_slot() {
        let spec = optimize(&[], env("new", 1, 0.5), &[], Slot(9));
        assert_eq!(spec.targets.get(&EnvName("new".into())), Some(&Slot(1)));
    }

    #[test]
    fn optimize_high_score_new_env_converges_through_swap_then_compaction() {
        // Existing: 3 low-score envs at slots 1, 2, 4. Slot 3 empty.
        // New env arrives at slot 5 with high score 0.95.
        // Iter 1 (swap): highest-NB move is `new` ↔ `a`. After: new at 1, a at 5.
        // Iter 2 (compaction): slot 3 still empty; `a` at 5 compacts into 3.
        // Final: new=1, b=2, a=3, c=4.
        let existing = vec![env("a", 1, 0.5), env("b", 2, 0.5), env("c", 4, 0.5)];
        let spec = optimize(&existing, env("new", 5, 0.95), &[], Slot(9));
        assert_eq!(spec.targets.get(&EnvName("new".into())), Some(&Slot(1)));
        assert_eq!(spec.targets.get(&EnvName("b".into())), Some(&Slot(2)));
        assert_eq!(spec.targets.get(&EnvName("a".into())), Some(&Slot(3)));
        assert_eq!(spec.targets.get(&EnvName("c".into())), Some(&Slot(4)));
    }

    #[test]
    fn optimize_unmanaged_slot_blocks_compaction_into_it() {
        // Slot 3 looks empty managed-wise but is held by an unmanaged workspace.
        // Compaction must NOT target it.
        let existing = vec![env("a", 1, 0.5), env("b", 2, 0.5), env("c", 4, 0.5)];
        let spec = optimize(&existing, env("new", 5, 0.95), &[Slot(3)], Slot(9));
        // New must not land at 3; it should stay at 5 (no other empty slot
        // ≤ max_slot to compact into here).
        assert_ne!(spec.targets.get(&EnvName("new".into())), Some(&Slot(3)));
    }

    #[test]
    fn optimize_swaps_higher_score_into_lower_slot() {
        // Existing: low-score at slot 1, high-score at slot 5. Algorithm should
        // swap them so high-score is closer to slot 1.
        let existing = vec![env("low", 1, 0.1), env("high", 5, 0.9)];
        let spec = optimize(&existing, env("new", 6, 0.5), &[], Slot(9));
        // Expect high at 1 (or close), low at 5 (or shifted). Exact slot
        // depends on convergence; assert ordering invariant only.
        let low_slot = spec.targets.get(&EnvName("low".into())).unwrap();
        let high_slot = spec.targets.get(&EnvName("high".into())).unwrap();
        assert!(
            high_slot.0 < low_slot.0,
            "high-score env should end up at a lower slot than low-score env; \
             got high={high_slot:?}, low={low_slot:?}"
        );
    }

    #[test]
    fn optimize_keeps_layout_when_no_profitable_move() {
        // All scores equal — no incentive to swap.
        let existing = vec![env("a", 1, 0.5), env("b", 2, 0.5)];
        let spec = optimize(&existing, env("new", 3, 0.5), &[], Slot(9));
        assert_eq!(spec.targets.get(&EnvName("a".into())), Some(&Slot(1)));
        assert_eq!(spec.targets.get(&EnvName("b".into())), Some(&Slot(2)));
        assert_eq!(spec.targets.get(&EnvName("new".into())), Some(&Slot(3)));
    }

    #[test]
    fn boost_lifts_low_score_new_env_into_shortcut_range() {
        // New env at slot 10 (outside shortcut zone) with very low score.
        // Inside shortcut zone has envs with scores 0.5..0.9. Boost should
        // raise new's score just above min (0.5).
        let mut managed = vec![env("a", 1, 0.5), env("b", 2, 0.9), env("new", 10, 0.0)];
        boost_incoming_score(&mut managed, &EnvName("new".into()), Slot(9));
        let new = managed
            .iter()
            .find(|e| e.name == EnvName("new".into()))
            .unwrap();
        assert!(new.score > 0.5);
    }

    #[test]
    fn boost_noop_when_new_env_already_in_shortcut_zone() {
        let mut managed = vec![env("a", 1, 0.5), env("new", 3, 0.1)];
        let before = managed[1].score;
        boost_incoming_score(&mut managed, &EnvName("new".into()), Slot(9));
        assert_eq!(managed[1].score, before);
    }
}
