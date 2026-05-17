//! Score-based optimization: given current managed envs + scores, incoming
//! env, and unmanaged-occupied slots, produce a `LayoutSpec` of target slots.
//!
//! Two move kinds drive convergence:
//! - **Score-swap** between two managed slots when a higher-score env sits at
//!   a worse slot. NetBenefit guarded by `STABILITY_THRESHOLD` to prevent
//!   thrash between near-equal envs.
//! - **Compaction** into an empty slot below an env's current slot. No
//!   threshold — filling an empty slot has no thrashing risk.

use super::spec::LayoutSpec;
use super::types::*;
use std::collections::HashMap;

pub const STABILITY_THRESHOLD: f64 = 0.05;

/// DCG position discount — the value of occupying a given slot. Lower slot →
/// higher discount; convergence pushes high-score envs toward low slots.
fn disc(slot: Slot) -> f64 {
    1.0_f64 / (slot.0 as f64 + 1.0).log2()
}

enum BestMove {
    Swap { lo_idx: usize, hi_idx: usize },
    Compaction { env_idx: usize, target: Slot },
}

fn find_best_move(
    managed: &[Env],
    unmanaged_slots: &[Slot],
    max_slot: Slot,
    stability_threshold: f64,
) -> Option<BestMove> {
    let mut all_occupied: std::collections::HashSet<Slot> =
        managed.iter().map(|e| e.slot).collect();
    all_occupied.extend(unmanaged_slots.iter().copied());

    let mut best_nb: f64 = 0.0;
    let mut best: Option<BestMove> = None;

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
                    best = Some(BestMove::Swap { lo_idx, hi_idx });
                }
            }
        }
    }

    for raw in 1..=max_slot.0 {
        let target = Slot(raw);
        if all_occupied.contains(&target) {
            continue;
        }
        for (env_idx, env_j) in managed.iter().enumerate() {
            if env_j.slot <= target || env_j.score <= 0.0 {
                continue;
            }
            let nb = env_j.score * (disc(target) - disc(env_j.slot));
            if nb > best_nb {
                best_nb = nb;
                best = Some(BestMove::Compaction { env_idx, target });
            }
        }
    }

    best
}

fn apply_move(managed: &mut [Env], mv: BestMove) {
    match mv {
        BestMove::Swap { lo_idx, hi_idx } => {
            let lo_slot = managed[lo_idx].slot;
            managed[lo_idx].slot = managed[hi_idx].slot;
            managed[hi_idx].slot = lo_slot;
        }
        BestMove::Compaction { env_idx, target } => {
            managed[env_idx].slot = target;
        }
    }
}

/// Recency boost: ensures a newly-activated env that landed outside the
/// shortcut zone wins exactly one swap into it, even if its on-disk score
/// is stale. Implements the "just activated" signal that hasn't been
/// persisted yet.
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
    // Skip when no managed envs occupy the shortcut zone: nothing to displace,
    // and writing INFINITY into the score would poison later comparisons.
    if min_shortcut_score.is_finite()
        && let Some(target) = managed.iter_mut().find(|e| &e.name == env_name)
    {
        target.score = target.score.max(min_shortcut_score + f64::EPSILON);
    }
}

/// Converges the layout until no profitable move remains. `incoming.slot`
/// is the free slot the new env temporarily occupies in the optimization
/// model; the returned `LayoutSpec` may place it elsewhere.
pub fn optimize(
    existing: &[Env],
    incoming: Env,
    unmanaged_slots: &[Slot],
    max_slot: Slot,
) -> LayoutSpec {
    let mut managed: Vec<Env> = existing.to_vec();
    managed.push(incoming.clone());
    boost_incoming_score(&mut managed, &incoming.name, max_slot);

    while let Some(mv) = find_best_move(&managed, unmanaged_slots, max_slot, 0.0) {
        apply_move(&mut managed, mv);
    }

    let targets: HashMap<EnvName, Slot> = managed.into_iter().map(|e| (e.name, e.slot)).collect();
    LayoutSpec { targets }
}

/// Listener-path variant: at most one swap or compaction per call,
/// caller-supplied threshold. Pairs with rate-limited debounce upstream.
pub fn optimize_single_step(
    existing: &[Env],
    unmanaged_slots: &[Slot],
    max_slot: Slot,
    stability_threshold: f64,
) -> LayoutSpec {
    let mut managed: Vec<Env> = existing.to_vec();
    if let Some(mv) = find_best_move(&managed, unmanaged_slots, max_slot, stability_threshold) {
        apply_move(&mut managed, mv);
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
