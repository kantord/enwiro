use anyhow::Context;
use clap::Parser;
use i3ipc_types::reply::Workspace;
use tokio_i3ipc::I3;

/// Minimum NetBenefit required to justify evicting a workspace from a single-digit slot.
/// A swap that yields less than this gain is treated as not worth the disruption.
/// Tune upward to make the layout more stable; downward to make it more aggressive.
const STABILITY_THRESHOLD: f64 = 0.05;

/// DCG position discount: the value of occupying slot `i`.
/// Mirrors the standard formula `1 / log₂(i + 1)` from information retrieval.
fn disc(slot: i32) -> f64 {
    1.0_f64 / (slot as f64 + 1.0).log2()
}

#[derive(serde::Deserialize, Debug, Clone)]
struct ManagedEnvInfo {
    name: String,
    slot_score: f64,
}

#[derive(Parser)]
enum EnwiroAdapterI3WmCLI {
    GetActiveWorkspaceId(GetActiveWorkspaceIdArgs),
    Activate(ActivateArgs),
}

#[derive(clap::Args)]
pub struct GetActiveWorkspaceIdArgs {}

#[derive(clap::Args)]
pub struct ActivateArgs {
    pub name: String,
}

/// The result of the eviction-chain decision.
///
/// Returned by `find_eviction_chain`; the caller translates each variant into
/// the appropriate sequence of i3 `rename workspace` and `workspace` commands.
#[derive(Debug, PartialEq)]
enum EvictionChain {
    /// No eviction is warranted — place the incoming env at the given free slot ≥ 10.
    NoEviction,
    /// 2-element swap: move the victim to `free_num`, put incoming at `victim_slot`.
    TwoElement {
        victim_name: String,
        victim_slot: i32,
    },
    /// 3-element rotation: move `chain_mover_name` from `chain_slot` to `victim_slot`,
    /// evict the victim to `free_num`, and place incoming at `chain_slot`.
    ThreeElement {
        victim_name: String,
        chain_mover_name: String,
        chain_slot: i32,
        victim_slot: i32,
    },
}

/// Decide the optimal eviction chain when all single-digit slots are full.
///
/// # Parameters
/// * `workspaces` – current i3 workspace list.
/// * `managed_envs` – scored environments known to enwiro.
/// * `incoming_score` – `slot_score` of the environment being activated.
///
/// # Algorithm
///
/// 1. Run `find_eviction_candidate` to locate the 2-element victim at slot X.
///    If none exists → `NoEviction`.
///
/// 2. Among all single-digit slots Y < X that contain a managed occupant with
///    score S_Y < `incoming_score`, compute the additional DCG gain of the
///    3-element rotation over the plain 2-element swap:
///
///    ```text
///    chain_gain(Y) = (incoming_score − S_Y) × (disc(Y) − disc(X))
///    ```
///
///    Since Y < X, disc(Y) > disc(X) and the factor is always positive, so any
///    occupant with S_Y < incoming_score is beneficial to chain through.
///    Pick the Y that maximises `chain_gain(Y)`.
///
/// 3. If a beneficial Y exists → `ThreeElement { victim_name, chain_mover_name, chain_slot: Y, victim_slot: X }`.
///    Otherwise → `TwoElement { victim_name, victim_slot: X }`.
fn find_eviction_chain(
    workspaces: &[Workspace],
    managed_envs: &[ManagedEnvInfo],
    incoming_score: f64,
) -> EvictionChain {
    let score_map: std::collections::HashMap<&str, f64> = managed_envs
        .iter()
        .map(|e| (e.name.as_str(), e.slot_score))
        .collect();

    // Step 1: find the 2-element victim.
    let victim = match find_eviction_candidate_with_map(workspaces, &score_map, incoming_score) {
        Some(v) => v,
        None => return EvictionChain::NoEviction,
    };
    let victim_slot = victim.num;
    let victim_name = extract_environment_name(victim);

    // Step 2: look for a beneficial chain-mover at slot Y < victim_slot.
    let disc_x = disc(victim_slot);

    let best_chain = workspaces
        .iter()
        .filter(|ws| ws.num >= 1 && ws.num < victim_slot)
        .filter_map(|ws| {
            let chain_mover_name = extract_environment_name(ws);
            let s_y = *score_map.get(chain_mover_name.as_str())?;
            // Only chain through occupants the incoming env outscores.
            if incoming_score <= s_y {
                return None;
            }
            let chain_gain = (incoming_score - s_y) * (disc(ws.num) - disc_x);
            Some((ws.num, chain_mover_name, chain_gain))
        })
        .max_by(|(_, _, ga), (_, _, gb)| ga.partial_cmp(gb).unwrap_or(std::cmp::Ordering::Equal));

    match best_chain {
        Some((chain_slot, chain_mover_name, _)) => EvictionChain::ThreeElement {
            victim_name,
            chain_mover_name,
            chain_slot,
            victim_slot,
        },
        None => EvictionChain::TwoElement {
            victim_name,
            victim_slot,
        },
    }
}

/// Translate an `EvictionChain` decision into an ordered list of i3 rename-workspace
/// pairs `(old_name, new_name)` that must be executed in sequence.
///
/// # Parameters
/// * `chain` – the eviction decision returned by `find_eviction_chain`.
/// * `workspaces` – current i3 workspace list used to look up the full i3 name for
///   each slot; if no workspace is found at a given slot the fallback
///   `"{slot}: {env_name}"` is used.
/// * `free_num` – the free workspace number where an evicted workspace will land.
///
/// # Returns
/// * `NoEviction` → empty vec.
/// * `TwoElement` → one pair: victim's current name → `"{free_num}: {victim_name}"`.
/// * `ThreeElement` → two pairs: chain-mover first (from `chain_slot` to `victim_slot`),
///   then victim (from `victim_slot` to `free_num`).
fn build_eviction_commands(
    chain: &EvictionChain,
    workspaces: &[Workspace],
    free_num: i32,
) -> Vec<(String, String)> {
    let ws_name_at = |slot: i32, env_name: &str| -> String {
        workspaces
            .iter()
            .find(|ws| ws.num == slot)
            .map(|ws| ws.name.clone())
            .unwrap_or_else(|| format!("{}: {}", slot, env_name))
    };

    match chain {
        EvictionChain::NoEviction => vec![],
        EvictionChain::TwoElement {
            victim_name,
            victim_slot,
        } => {
            let old_name = ws_name_at(*victim_slot, victim_name);
            let new_name = format!("{}: {}", free_num, victim_name);
            vec![(old_name, new_name)]
        }
        EvictionChain::ThreeElement {
            victim_name,
            chain_mover_name,
            chain_slot,
            victim_slot,
        } => {
            let chain_mover_old = ws_name_at(*chain_slot, chain_mover_name);
            let chain_mover_new = format!("{}: {}", victim_slot, chain_mover_name);
            let victim_old = ws_name_at(*victim_slot, victim_name);
            let victim_new = format!("{}: {}", free_num, victim_name);
            vec![(chain_mover_old, chain_mover_new), (victim_old, victim_new)]
        }
    }
}

fn build_workspace_command(workspace_name: &str) -> String {
    let escaped = workspace_name.replace('\\', r"\\").replace('"', r#"\""#);
    format!(r#"workspace "{}""#, escaped)
}

fn build_rename_workspace_command(old_name: &str, new_name: &str) -> String {
    let esc_old = old_name.replace('\\', r"\\").replace('"', r#"\""#);
    let esc_new = new_name.replace('\\', r"\\").replace('"', r#"\""#);
    format!(r#"rename workspace "{}" to "{}""#, esc_old, esc_new)
}

/// Read managed env list from stdin. Returns empty vec on any parse failure.
fn read_managed_envs() -> Vec<ManagedEnvInfo> {
    use std::io::Read;
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() {
        return vec![];
    }
    serde_json::from_str(&buf).unwrap_or_default()
}

/// Find the best single-digit enwiro-managed workspace to evict when all slots 1–9 are full.
///
/// # Why DCG-based scoring?
///
/// Single-digit slots (1–9) are reachable with one keypress and therefore more
/// valuable than slots ≥ 10. This mirrors the position-discount in **Discounted
/// Cumulative Gain (DCG)**, a standard metric from information retrieval: an item
/// at rank *i* contributes `score / log₂(i + 1)`, so early positions are worth
/// exponentially more than later ones.
///
/// Applying that idea here: slot 1 has discount 1.0 (full value), slot 3 has 0.5,
/// slot 9 ≈ 0.32. A high-scored environment belongs in a low-numbered slot — and
/// conversely, evicting a low-scored occupant from a low-numbered slot yields a
/// large DCG improvement.
///
/// # The NetBenefit formula
///
/// For each managed workspace occupying single-digit slot *i*:
///
/// ```text
/// disc(i)      = 1.0 / log₂(i + 1)
/// NetBenefit   = (score_incoming − score_occupant) × disc(i) − STABILITY_THRESHOLD
/// ```
///
/// The stability threshold prevents thrashing: a swap is only made if the gain
/// clearly exceeds the cost of disruption. The candidate with the highest positive
/// NetBenefit is evicted; if no candidate clears the threshold, `None` is returned
/// and the new environment is placed in whatever free slot is available (≥ 10).
fn find_eviction_candidate_with_map<'a>(
    workspaces: &'a [Workspace],
    score_map: &std::collections::HashMap<&str, f64>,
    incoming_score: f64,
) -> Option<&'a Workspace> {
    workspaces
        .iter()
        .filter(|ws| ws.num >= 1 && ws.num <= 9)
        .filter_map(|ws| {
            let occupant_score = score_map.get(extract_environment_name(ws).as_str())?;
            let net_benefit =
                (incoming_score - occupant_score) * disc(ws.num) - STABILITY_THRESHOLD;
            if net_benefit > 0.0 {
                Some((ws, net_benefit))
            } else {
                None
            }
        })
        .max_by(|(_, na), (_, nb)| na.partial_cmp(nb).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(ws, _)| ws)
}

async fn run_i3_command(i3: &mut I3, command: String) -> anyhow::Result<()> {
    let outcomes = i3.run_command(command).await?;
    if let Some(outcome) = outcomes.first()
        && !outcome.success
    {
        let msg = outcome.error.as_deref().unwrap_or("unknown error");
        tracing::error!(error = %msg, "i3 command failed");
        anyhow::bail!("i3 command failed: {}", msg);
    }
    Ok(())
}

fn extract_environment_name(workspace: &Workspace) -> String {
    workspace
        .name
        .split_once(':')
        .map(|(_, name)| name.trim().to_string())
        .unwrap_or_default()
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let _guard = enwiro_logging::init_logging("enwiro-adapter-i3wm.log");

    let args = EnwiroAdapterI3WmCLI::parse();

    match args {
        EnwiroAdapterI3WmCLI::GetActiveWorkspaceId(_) => {
            let mut i3 = I3::connect().await?;
            let workspaces = i3.get_workspaces().await?;
            tracing::debug!(count = workspaces.len(), "Retrieved workspaces");
            let focused_workspace = workspaces
                .into_iter()
                .find(|workspace| workspace.focused)
                .context("No active workspace. This should never happen.")?;
            let environment_name = extract_environment_name(&focused_workspace);
            tracing::debug!(name = %environment_name, "Extracted environment name");
            print!("{}", environment_name);
        }
        EnwiroAdapterI3WmCLI::Activate(args) => {
            let managed_envs = read_managed_envs();
            let mut i3 = I3::connect().await?;
            let workspaces = i3.get_workspaces().await?;
            tracing::debug!(count = workspaces.len(), name = %args.name, "Activating environment");

            // Check if a workspace with this environment name already exists
            if let Some(existing) = workspaces
                .iter()
                .find(|ws| extract_environment_name(ws) == args.name)
            {
                tracing::info!(workspace = %existing.name, "Found existing workspace");
                run_i3_command(&mut i3, build_workspace_command(&existing.name)).await?;
            } else {
                // Find the lowest unused workspace number
                let used_numbers: std::collections::HashSet<i32> =
                    workspaces.iter().map(|ws| ws.num).collect();
                let mut free_num = 1;
                while used_numbers.contains(&free_num) {
                    free_num += 1;
                }

                // Determine optimal eviction chain (2-element swap, 3-element rotation,
                // or no eviction) and execute the required i3 rename commands.
                let incoming_score = managed_envs
                    .iter()
                    .find(|e| e.name == args.name)
                    .map(|e| e.slot_score)
                    .unwrap_or(0.0);
                let chain = find_eviction_chain(&workspaces, &managed_envs, incoming_score);
                let target_num = match &chain {
                    EvictionChain::NoEviction => free_num,
                    EvictionChain::TwoElement { victim_slot, .. } => *victim_slot,
                    EvictionChain::ThreeElement { chain_slot, .. } => *chain_slot,
                };
                for (old_name, new_name) in build_eviction_commands(&chain, &workspaces, free_num) {
                    tracing::info!(from = %old_name, to = %new_name, "Renaming workspace");
                    run_i3_command(
                        &mut i3,
                        build_rename_workspace_command(&old_name, &new_name),
                    )
                    .await?;
                }

                let workspace_name = format!("{}: {}", target_num, args.name);
                tracing::info!(workspace = %workspace_name, num = target_num, "Creating new workspace");
                run_i3_command(&mut i3, build_workspace_command(&workspace_name)).await?;
            }
        }
    };

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use i3ipc_types::reply::Rect;

    fn make_workspace(id: usize, num: i32, name: &str) -> Workspace {
        Workspace {
            id,
            num,
            name: name.to_string(),
            visible: true,
            focused: false,
            urgent: false,
            rect: Rect {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            },
            output: "eDP-1".to_string(),
        }
    }

    #[test]
    fn test_extract_plain_environment_name() {
        let ws = make_workspace(123, 1, "1: my-project");
        assert_eq!(extract_environment_name(&ws), "my-project");
    }

    #[test]
    fn test_extract_empty_for_numbered_workspace() {
        let ws = make_workspace(1, 1, "1");
        assert_eq!(extract_environment_name(&ws), "");
    }

    #[test]
    fn test_extract_name_containing_workspace_number() {
        // Workspace "1: project1" — the name contains "1" as a substring
        let ws = make_workspace(123, 1, "1: project1");
        assert_eq!(extract_environment_name(&ws), "project1");
    }

    #[test]
    fn test_extract_name_containing_workspace_number_in_middle() {
        // Workspace "3: a3b" — the name contains "3" as a substring
        let ws = make_workspace(456, 3, "3: a3b");
        assert_eq!(extract_environment_name(&ws), "a3b");
    }

    /// Test wrapper: build score_map and call the internal implementation.
    fn find_eviction_candidate<'a>(
        workspaces: &'a [Workspace],
        managed_envs: &[ManagedEnvInfo],
        incoming_score: f64,
    ) -> Option<&'a Workspace> {
        let score_map: std::collections::HashMap<&str, f64> = managed_envs
            .iter()
            .map(|e| (e.name.as_str(), e.slot_score))
            .collect();
        find_eviction_candidate_with_map(workspaces, &score_map, incoming_score)
    }

    fn make_managed(name: &str, slot_score: f64) -> ManagedEnvInfo {
        ManagedEnvInfo {
            name: name.to_string(),
            slot_score,
        }
    }

    /// `ManagedEnvInfo` in the i3wm adapter must deserialize from JSON using the key
    /// `"slot_score"` (not `"frecency"`).  This mirrors the rename on the core side.
    #[test]
    fn test_i3wm_deserializes_slot_score_from_json() {
        let json = r#"[{"name":"my-project","slot_score":0.5}]"#;
        let envs: Vec<ManagedEnvInfo> =
            serde_json::from_str(json).expect("must parse slot_score from JSON");
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].name, "my-project");
        assert!(
            (envs[0].slot_score - 0.5).abs() < 1e-10,
            "slot_score must round-trip from JSON, got {}",
            envs[0].slot_score
        );
    }

    /// Confirm that a JSON payload using the old `"frecency"` key is NOT silently
    /// accepted under the new field name — this guards against accidentally keeping
    /// a serde rename alias.
    #[test]
    fn test_i3wm_does_not_accept_frecency_key() {
        let json = r#"[{"name":"my-project","frecency":0.5}]"#;
        // With `deny_unknown_fields` this would error; without it the struct default (0.0)
        // is used.  Either way, the slot_score must NOT be 0.5 (i.e. the old key is gone).
        let envs: Vec<ManagedEnvInfo> = serde_json::from_str(json).unwrap_or_default();
        if !envs.is_empty() {
            assert!(
                (envs[0].slot_score - 0.5).abs() > 1e-10,
                "slot_score must NOT be populated from the old `frecency` JSON key; \
                 if it is, the rename has a hidden alias"
            );
        }
    }

    #[test]
    fn test_find_eviction_candidate_ignores_unmanaged_workspaces() {
        // Unmanaged workspace at slot 1 must never be evicted even though the
        // managed one at slot 2 has a lower score — only managed envs are eligible.
        let workspaces = vec![
            make_workspace(1, 1, "1: unmanaged"),
            make_workspace(2, 2, "2: managed"),
        ];
        let managed = vec![make_managed("managed", 0.0)];
        // incoming_score = 1.0 → NB for slot 2 is clearly positive
        let candidate = find_eviction_candidate(&workspaces, &managed, 1.0).unwrap();
        assert_eq!(extract_environment_name(candidate), "managed");
    }

    #[test]
    fn test_find_eviction_candidate_ignores_multi_digit_workspaces() {
        // The env at slot 10 must never be a candidate — only slots 1–9 are eligible.
        let workspaces = vec![
            make_workspace(1, 10, "10: managed-but-multi-digit"),
            make_workspace(2, 5, "5: managed-single-digit"),
        ];
        let managed = vec![
            make_managed("managed-but-multi-digit", 0.0),
            make_managed("managed-single-digit", 0.0),
        ];
        // incoming_score = 1.0 → NB for slot 5 is positive; slot 10 excluded
        let candidate = find_eviction_candidate(&workspaces, &managed, 1.0).unwrap();
        assert_eq!(candidate.num, 5);
    }

    #[test]
    fn test_find_eviction_candidate_returns_none_when_no_managed_single_digit() {
        // All single-digit workspaces are unmanaged → no eligible candidate.
        let workspaces = vec![
            make_workspace(1, 1, "1: unmanaged-a"),
            make_workspace(2, 2, "2: unmanaged-b"),
        ];
        let managed = vec![make_managed("something-else", 0.0)];
        assert!(find_eviction_candidate(&workspaces, &managed, 1.0).is_none());
    }

    #[test]
    fn test_find_eviction_candidate_returns_none_for_empty_workspaces() {
        let managed = vec![make_managed("some-env", 0.5)];
        assert!(find_eviction_candidate(&[], &managed, 1.0).is_none());
    }

    // ── NetBenefit-based eviction tests ──────────────────────────────────────
    //
    // NetBenefit(swap A out of slot i, incoming into slot i)
    //   = (score_new − score_A) × disc(i) − stability_threshold
    // where:
    //   disc(i)             = 1.0 / log2(i + 1.0)
    //   stability_threshold = STABILITY_THRESHOLD (= 0.05)
    //
    // Only evict when NetBenefit > 0; otherwise return None.

    /// Test 1: High incoming score → positive NetBenefit → eviction happens.
    ///
    /// One managed workspace at slot 3 with score 0.1.
    /// Incoming score = 1.0.
    /// disc(3) = 1/log2(4) = 0.5
    /// NetBenefit = (1.0 − 0.1) × 0.5 − 0.05 = 0.45 − 0.05 = 0.40 > 0  ✓
    #[test]
    fn test_net_benefit_positive_triggers_eviction() {
        let workspaces = vec![make_workspace(1, 3, "3: low-score-env")];
        let managed = vec![make_managed("low-score-env", 0.1)];
        let candidate = find_eviction_candidate(&workspaces, &managed, 1.0);
        assert!(
            candidate.is_some(),
            "expected eviction when NetBenefit is clearly positive"
        );
        assert_eq!(
            extract_environment_name(candidate.unwrap()),
            "low-score-env"
        );
    }

    /// Test 2: Low incoming score → NetBenefit ≤ 0 → no eviction (returns None).
    ///
    /// Occupant at slot 3 with score 0.1. Incoming score = 0.0.
    /// NetBenefit = (0.0 − 0.1) × 0.5 − 0.05 = −0.05 − 0.05 = −0.10 < 0  ✗
    #[test]
    fn test_net_benefit_negative_returns_none() {
        let workspaces = vec![make_workspace(1, 3, "3: higher-score-env")];
        let managed = vec![make_managed("higher-score-env", 0.1)];
        let candidate = find_eviction_candidate(&workspaces, &managed, 0.0);
        assert!(
            candidate.is_none(),
            "expected None when incoming score is lower than all occupants"
        );
    }

    /// Test 3: Multiple candidates with positive NetBenefit → picks the one with
    /// the highest NetBenefit.
    ///
    /// Candidate A: slot 1, occupant score 0.5, incoming 1.0
    ///   disc(1) = 1/log2(2) = 1.0
    ///   NetBenefit_A = (1.0 − 0.5) × 1.0 − 0.05 = 0.45
    ///
    /// Candidate B: slot 2, occupant score 0.1, incoming 1.0
    ///   disc(2) = 1/log2(3) ≈ 0.6309
    ///   NetBenefit_B = (1.0 − 0.1) × 0.6309 − 0.05 ≈ 0.518
    ///
    /// NetBenefit_B > NetBenefit_A → pick B (slot 2, "very-low-score-env").
    #[test]
    fn test_net_benefit_picks_highest_among_multiple_candidates() {
        let workspaces = vec![
            make_workspace(1, 1, "1: medium-score-env"),
            make_workspace(2, 2, "2: very-low-score-env"),
        ];
        let managed = vec![
            make_managed("medium-score-env", 0.5),
            make_managed("very-low-score-env", 0.1),
        ];
        let candidate = find_eviction_candidate(&workspaces, &managed, 1.0).unwrap();
        assert_eq!(
            extract_environment_name(candidate),
            "very-low-score-env",
            "should pick the candidate with the highest NetBenefit, not just lowest occupant score"
        );
    }

    /// Test 4: Stability threshold — low-numbered slots require a smaller absolute
    /// score gap because disc(i) is larger there.
    ///
    /// Incoming score = 0.7; occupant score = 0.64 (diff = 0.06) at both slots.
    ///
    /// Slot 1: disc=1.0, NB = 0.06×1.0 − 0.05 = 0.01 > 0  → evict
    /// Slot 5: disc=1/log2(6)≈0.387, NB = 0.06×0.387 − 0.05 ≈ −0.027 < 0  → keep
    ///
    /// With both workspaces present only slot 1 yields positive NetBenefit,
    /// so slot 1's occupant is evicted.
    #[test]
    fn test_stability_threshold_low_slot_evicts_slot1_not_slot5() {
        let workspaces = vec![
            make_workspace(1, 1, "1: env-at-slot1"),
            make_workspace(2, 5, "5: env-at-slot5"),
        ];
        let managed = vec![
            make_managed("env-at-slot1", 0.64),
            make_managed("env-at-slot5", 0.64),
        ];
        // incoming=0.7 → diff 0.06 is above threshold for slot 1 but below for slot 5
        let candidate = find_eviction_candidate(&workspaces, &managed, 0.7).unwrap();
        assert_eq!(
            candidate.num, 1,
            "only slot 1 has positive NetBenefit with a small score gap"
        );
    }

    /// Test 5: All candidates have non-positive NetBenefit → returns None even
    /// when managed single-digit workspaces exist.
    #[test]
    fn test_net_benefit_all_negative_returns_none() {
        let workspaces = vec![
            make_workspace(1, 1, "1: env-a"),
            make_workspace(2, 2, "2: env-b"),
        ];
        let managed = vec![make_managed("env-a", 0.8), make_managed("env-b", 0.9)];
        // incoming=0.2 is much lower than all occupants → all NetBenefits are negative
        assert!(
            find_eviction_candidate(&workspaces, &managed, 0.2).is_none(),
            "must return None when no swap yields positive NetBenefit"
        );
    }

    /// Test 6: Unmanaged single-digit workspaces are still excluded (they have no
    /// slot_score entry and cannot be evicted).
    #[test]
    fn test_net_benefit_ignores_unmanaged_workspaces() {
        let workspaces = vec![
            make_workspace(1, 1, "1: unmanaged"),
            make_workspace(2, 2, "2: managed-low"),
        ];
        let managed = vec![make_managed("managed-low", 0.1)];
        // incoming=1.0 → should evict managed-low (NB > 0), not unmanaged
        let candidate = find_eviction_candidate(&workspaces, &managed, 1.0).unwrap();
        assert_eq!(extract_environment_name(candidate), "managed-low");
    }

    /// Test 7: Multi-digit workspaces are not candidates for eviction even if
    /// their NetBenefit would be positive.
    #[test]
    fn test_net_benefit_ignores_multi_digit_workspaces() {
        let workspaces = vec![
            make_workspace(1, 10, "10: managed-multi-digit"),
            make_workspace(2, 5, "5: managed-single-digit"),
        ];
        let managed = vec![
            make_managed("managed-multi-digit", 0.0),
            make_managed("managed-single-digit", 0.1),
        ];
        // incoming=1.0 → managed-single-digit (slot 5) should be the only candidate
        let candidate = find_eviction_candidate(&workspaces, &managed, 1.0).unwrap();
        assert_eq!(candidate.num, 5);
    }

    /// Test 8: Empty workspace list → always returns None.
    #[test]
    fn test_net_benefit_empty_workspaces_returns_none() {
        let managed = vec![make_managed("some-env", 0.0)];
        assert!(find_eviction_candidate(&[], &managed, 1.0).is_none());
    }

    /// Test 9: Incoming env not present in managed_envs → its score is treated as
    /// 0.0; eviction still possible if occupant score is also 0.0 or below.
    ///
    /// For an occupant with score 0.0 and incoming score 0.0:
    ///   NetBenefit = (0.0 − 0.0) × disc(i) − 0.05 = −0.05 < 0  → None
    #[test]
    fn test_net_benefit_unknown_incoming_env_uses_zero_score() {
        let workspaces = vec![make_workspace(1, 1, "1: occupant")];
        let managed = vec![make_managed("occupant", 0.0)];
        // incoming env "new-env" is not in managed_envs → treated as score 0.0
        // NetBenefit = (0.0 - 0.0) * 1.0 - 0.05 = -0.05 < 0 → None
        assert!(
            find_eviction_candidate(&workspaces, &managed, 0.0).is_none(),
            "when incoming score is 0 and occupant score is 0, NetBenefit is negative"
        );
    }

    #[test]
    fn test_build_rename_workspace_command() {
        let cmd = build_rename_workspace_command("5: old-project", "10: old-project");
        assert_eq!(
            cmd,
            r#"rename workspace "5: old-project" to "10: old-project""#
        );
    }

    #[test]
    fn test_build_rename_workspace_command_escapes_quotes() {
        let cmd = build_rename_workspace_command(r#"5: has"quote"#, "10: safe");
        assert!(cmd.contains(r#"\""#), "Quote in old name should be escaped");
    }

    #[test]
    fn test_workspace_command_with_semicolon_is_quoted() {
        // A semicolon outside quotes would cause i3 to parse a second command.
        // The workspace name must be wrapped in quotes to prevent injection.
        let cmd = build_workspace_command("1: evil;exec rm -rf /");
        assert!(
            cmd.starts_with(r#"workspace ""#) && cmd.ends_with('"'),
            "Workspace name with semicolon must be quoted: {cmd}"
        );
    }

    #[test]
    fn test_workspace_command_with_quote_is_safe() {
        let cmd = build_workspace_command(r#"1: has"quote"#);
        // The command should be quoted so the " doesn't break out
        assert!(
            cmd.starts_with(r#"workspace ""#) && cmd.ends_with('"'),
            "Workspace name should be quoted in the i3 command: {cmd}"
        );
    }

    // ── find_eviction_chain tests ─────────────────────────────────────────────
    //
    // These tests exercise the pure decision logic for 2-element vs 3-element
    // eviction. The i3 IPC calls that execute the moves are an integration
    // concern and are not tested here.

    /// Build a fully-occupied set of single-digit workspaces (slots 1–9).
    /// Returns `(workspaces, managed_envs)`.
    ///
    /// All occupants are given the supplied uniform score unless overridden by the
    /// `overrides` slice of `(slot, score)` pairs.
    fn make_full_single_digit(
        base_score: f64,
        overrides: &[(i32, f64)],
    ) -> (Vec<Workspace>, Vec<ManagedEnvInfo>) {
        let mut workspaces = Vec::new();
        let mut managed = Vec::new();

        for slot in 1..=9_i32 {
            let score = overrides
                .iter()
                .find(|(s, _)| *s == slot)
                .map(|(_, sc)| *sc)
                .unwrap_or(base_score);
            let name = format!("env-slot-{}", slot);
            workspaces.push(make_workspace(
                slot as usize,
                slot,
                &format!("{}: {}", slot, name),
            ));
            managed.push(make_managed(&name, score));
        }

        (workspaces, managed)
    }

    /// Test C1: 3-element chain fires when beneficial.
    ///
    /// Setup: slots 1–9 all occupied, free_num = 11, incoming_score = 0.8.
    ///
    /// Scores (chosen so slot 9 has the highest NetBenefit):
    ///   slot 9  → 0.0   (victim — highest NB = 0.8*disc(9) - 0.05 ≈ 0.191)
    ///   slot 1  → 0.59  (chain-mover — score < incoming, NB = 0.21-0.05 = 0.16 < NB(9))
    ///   slots 2–8 → 0.9  (score > incoming → NB negative → not candidates)
    ///
    /// disc(i) = 1/log2(i+1)
    ///   disc(9) = 1/log2(10) ≈ 0.30103
    ///   disc(1) = 1/log2(2)  = 1.0
    ///
    /// NB(9) = 0.8 * 0.30103 - 0.05 ≈ 0.191
    /// NB(1) = (0.8 - 0.59) * 1.0 - 0.05 = 0.21 - 0.05 = 0.16
    /// NB(9) > NB(1)  ✓ → slot 9 is the 2-element victim.
    ///
    /// Chain-mover check: slot 1 has S1=0.59 < incoming=0.8
    ///   chain_gain(1) = (0.8 - 0.59) * (disc(1) - disc(9)) ≈ 0.21 * 0.699 ≈ 0.147 > 0
    ///   → ThreeElement fires: chain_slot=1, victim_slot=9.
    #[test]
    fn test_three_element_chain_fires_when_beneficial() {
        // Scores: slot 9 = 0.0 (victim), slot 1 = 0.59 (chain-mover), slots 2-8 = 0.9
        let overrides = vec![(9, 0.0_f64), (1, 0.59_f64)];
        let (workspaces, managed) = make_full_single_digit(0.9, &overrides);
        let incoming_score = 0.8_f64;

        // Verify precondition numerically: NB(9) must exceed NB(1).
        let nb9 = incoming_score * disc(9) - STABILITY_THRESHOLD;
        let nb1 = (incoming_score - 0.59) * disc(1) - STABILITY_THRESHOLD;
        assert!(
            nb9 > nb1,
            "precondition: NB(9)={nb9} must exceed NB(1)={nb1}"
        );

        // Verify via find_eviction_candidate.
        let victim = find_eviction_candidate(&workspaces, &managed, incoming_score)
            .expect("there should be a victim");
        assert_eq!(
            victim.num, 9,
            "precondition: slot 9 must be the 2-element victim (highest NB)"
        );

        let chain = find_eviction_chain(&workspaces, &managed, incoming_score);
        match chain {
            EvictionChain::ThreeElement {
                victim_name,
                chain_mover_name,
                chain_slot,
                victim_slot,
            } => {
                assert_eq!(victim_slot, 9, "victim should be moved from slot 9");
                assert_eq!(chain_slot, 1, "incoming env should land at slot 1");
                assert_eq!(
                    victim_name, "env-slot-9",
                    "victim is the occupant of slot 9"
                );
                assert_eq!(
                    chain_mover_name, "env-slot-1",
                    "chain-mover is the occupant of slot 1"
                );
            }
            other => panic!("expected ThreeElement, got {:?}", other),
        }
    }

    /// Test C2: Falls back to 2-element when no chain improves layout.
    ///
    /// All slot-Y < X occupants have score ≥ incoming — no chain gain.
    ///
    /// Setup: slots 1–9 occupied.
    ///   slot 9: score = 0.0 → victim (highest NB given incoming = 0.8)
    ///   slots 1–8: score = 0.85 (all above incoming 0.8 → chain_gain negative → no chain)
    ///
    /// NB(9) = (0.8 - 0.0) * disc(9) - 0.05 ≈ 0.2075 > 0  → victim
    /// No slot Y < 9 has S_Y < 0.8 → no chain → TwoElement.
    #[test]
    fn test_three_element_falls_back_to_two_element_when_no_chain_improves() {
        let overrides = vec![(9, 0.0_f64)];
        let (workspaces, managed) = make_full_single_digit(0.85, &overrides);
        let incoming_score = 0.8_f64;

        // Precondition: slot 9 is the victim.
        let victim = find_eviction_candidate(&workspaces, &managed, incoming_score)
            .expect("there should be a victim");
        assert_eq!(
            victim.num, 9,
            "precondition: slot 9 is the 2-element victim"
        );

        let chain = find_eviction_chain(&workspaces, &managed, incoming_score);
        match chain {
            EvictionChain::TwoElement {
                victim_name,
                victim_slot,
            } => {
                assert_eq!(victim_slot, 9, "2-element victim is at slot 9");
                assert_eq!(victim_name, "env-slot-9");
            }
            other => panic!("expected TwoElement fallback, got {:?}", other),
        }
    }

    /// Test C3: NoEviction when all occupants outscore the incoming env.
    ///
    /// No slot has positive NetBenefit → NoEviction (place at free slot ≥ 10).
    ///
    /// All occupants have score 0.9, incoming = 0.2.
    /// NB(i) = (0.2 - 0.9) * disc(i) - 0.05 < 0 for all i.
    #[test]
    fn test_no_eviction_when_all_occupants_outscore_incoming() {
        let (workspaces, managed) = make_full_single_digit(0.9, &[]);
        let chain = find_eviction_chain(&workspaces, &managed, 0.2);
        assert_eq!(
            chain,
            EvictionChain::NoEviction,
            "should not evict when all occupants outscore the incoming env"
        );
    }

    /// Test C4: Among multiple chain-mover candidates, pick the one with the
    /// highest chain_gain.
    ///
    /// Setup: slots 1–9 occupied, free = 11, incoming = 0.8.
    ///   slot 9: score = 0.0 → victim (NB ≈ 0.191)
    ///   slot 1: score = 0.73 → chain-mover candidate (NB = 0.02 < 0.191, S < incoming)
    ///   slot 2: score = 0.5  → chain-mover candidate (NB = 0.139 < 0.191, S < incoming)
    ///   slots 3–8: score = 0.9 → above incoming (no positive NB, not chain-movers)
    ///
    /// disc(1) = 1/log2(2) = 1.0
    /// disc(2) = 1/log2(3) ≈ 0.6309
    /// disc(9) = 1/log2(10) ≈ 0.30103
    ///
    /// NB(9) = 0.8 * 0.30103 - 0.05 ≈ 0.191   (highest → victim)
    /// NB(1) = (0.8-0.73)*1.0 - 0.05 = 0.02    (positive but < NB(9) ✓)
    /// NB(2) = (0.8-0.5)*0.6309 - 0.05 ≈ 0.139 (positive but < NB(9) ✓)
    ///
    /// chain_gain(1) = (0.8-0.73)*(disc(1)-disc(9)) = 0.07*0.699 ≈ 0.049
    /// chain_gain(2) = (0.8-0.5)*(disc(2)-disc(9)) = 0.3*0.330  ≈ 0.099
    ///
    /// chain_gain(2) > chain_gain(1) → chain_slot = 2.
    #[test]
    fn test_three_element_picks_best_chain_slot() {
        let overrides = vec![(9, 0.0_f64), (1, 0.73_f64), (2, 0.5_f64)];
        let (workspaces, managed) = make_full_single_digit(0.9, &overrides);
        let incoming_score = 0.8_f64;

        // Precondition: slot 9 is the victim (highest NB).
        let victim = find_eviction_candidate(&workspaces, &managed, incoming_score)
            .expect("victim must exist");
        assert_eq!(
            victim.num, 9,
            "precondition: slot 9 must be the victim (highest NB)"
        );

        // Verify chain_gain ordering numerically.
        let cg1 = (incoming_score - 0.73) * (disc(1) - disc(9));
        let cg2 = (incoming_score - 0.5) * (disc(2) - disc(9));
        assert!(
            cg2 > cg1,
            "precondition: chain_gain(2)={cg2} must exceed chain_gain(1)={cg1}"
        );

        let chain = find_eviction_chain(&workspaces, &managed, incoming_score);
        match chain {
            EvictionChain::ThreeElement { chain_slot, .. } => {
                assert_eq!(
                    chain_slot, 2,
                    "slot 2 has higher chain_gain ({cg2:.4}) than slot 1 ({cg1:.4})"
                );
            }
            other => panic!("expected ThreeElement, got {:?}", other),
        }
    }

    /// Test C5: Chain-mover must be a managed env (has a score_map entry).
    ///
    /// Slot 1 is unmanaged — even though it is lower-numbered than the victim,
    /// it cannot be a chain-mover. Falls back to TwoElement.
    ///
    /// Setup:
    ///   slot 1: unmanaged (not in managed_envs)
    ///   slots 2–8: score = 0.9
    ///   slot 9: score = 0.0 → victim
    ///   incoming = 0.8
    #[test]
    fn test_three_element_ignores_unmanaged_chain_mover_candidates() {
        let mut workspaces: Vec<Workspace> = (1..=9_i32)
            .map(|slot| {
                make_workspace(slot as usize, slot, &format!("{}: env-slot-{}", slot, slot))
            })
            .collect();
        // Make slot 1 unmanaged by giving it a non-matching name
        workspaces[0] = make_workspace(1, 1, "1: unmanaged-ws");

        let mut managed: Vec<ManagedEnvInfo> = (2..=9_i32)
            .map(|slot| {
                make_managed(
                    &format!("env-slot-{}", slot),
                    if slot == 9 { 0.0 } else { 0.9 },
                )
            })
            .collect();
        // Also add the incoming (not at a slot but part of managed list)
        // Note: unmanaged-ws is NOT in managed_envs.

        let _ = &mut managed; // suppress warning

        let chain = find_eviction_chain(&workspaces, &managed, 0.8);
        match chain {
            EvictionChain::TwoElement { victim_slot, .. } => {
                assert_eq!(
                    victim_slot, 9,
                    "falls back to TwoElement with victim at slot 9"
                );
            }
            other => panic!(
                "expected TwoElement (unmanaged slot 1 is not a chain-mover), got {:?}",
                other
            ),
        }
    }

    /// Test C6: NoEviction is returned when workspaces is empty.
    #[test]
    fn test_eviction_chain_empty_workspaces_returns_no_eviction() {
        let chain = find_eviction_chain(&[], &[], 1.0);
        assert_eq!(chain, EvictionChain::NoEviction);
    }

    // ── build_eviction_commands tests ─────────────────────────────────────────
    //
    // `build_eviction_commands` is a pure function that translates an
    // `EvictionChain` decision into an ordered `Vec<(old_name, new_name)>` of
    // i3 rename-workspace pairs.  No IPC is involved.

    /// EC-1: `NoEviction` always produces an empty command list.
    #[test]
    fn test_build_eviction_commands_no_eviction_is_empty() {
        let workspaces: Vec<Workspace> = vec![];
        let result = build_eviction_commands(&EvictionChain::NoEviction, &workspaces, 11);
        assert!(
            result.is_empty(),
            "NoEviction must produce zero rename commands, got {:?}",
            result
        );
    }

    /// EC-2: `TwoElement` → one rename pair: victim's current i3 name → "{free_num}: {victim_name}".
    ///
    /// There is no workspace matching `victim_slot` in the list, so the
    /// fallback name `"{victim_slot}: {victim_name}"` is used for `old_name`.
    #[test]
    fn test_build_eviction_commands_two_element_single_pair() {
        let workspaces: Vec<Workspace> = vec![];
        let chain = EvictionChain::TwoElement {
            victim_name: "my-env".to_string(),
            victim_slot: 7,
        };
        let result = build_eviction_commands(&chain, &workspaces, 11);
        assert_eq!(
            result.len(),
            1,
            "TwoElement must produce exactly one rename pair"
        );
        let (old, new) = &result[0];
        assert_eq!(
            old, "7: my-env",
            "old_name should be the fallback slot:name form"
        );
        assert_eq!(
            new, "11: my-env",
            "new_name should use free_num and victim_name"
        );
    }

    /// EC-3: `TwoElement` — when the victim's workspace is found in the list by
    /// `ws.num == victim_slot`, use `ws.name` (the full i3 name) as `old_name`.
    #[test]
    fn test_build_eviction_commands_two_element_uses_workspace_name_from_list() {
        let workspaces = vec![make_workspace(3, 3, "3: my-env")];
        let chain = EvictionChain::TwoElement {
            victim_name: "my-env".to_string(),
            victim_slot: 3,
        };
        let result = build_eviction_commands(&chain, &workspaces, 10);
        assert_eq!(result.len(), 1);
        let (old, new) = &result[0];
        assert_eq!(
            old, "3: my-env",
            "old_name should be the full i3 workspace name found by slot"
        );
        assert_eq!(new, "10: my-env");
    }

    /// EC-4: `ThreeElement` → two pairs, chain-mover rename first, victim eviction second.
    ///
    /// Uses fallback names (no matching workspace in list) to isolate the
    /// ordering logic from slot-lookup logic.
    #[test]
    fn test_build_eviction_commands_three_element_two_pairs_chain_mover_first() {
        let workspaces: Vec<Workspace> = vec![];
        let chain = EvictionChain::ThreeElement {
            victim_name: "victim-env".to_string(),
            chain_mover_name: "mover-env".to_string(),
            chain_slot: 2,
            victim_slot: 8,
        };
        let result = build_eviction_commands(&chain, &workspaces, 11);
        assert_eq!(
            result.len(),
            2,
            "ThreeElement must produce exactly two rename pairs"
        );
        // First pair: chain-mover moves from chain_slot to victim_slot
        let (old0, new0) = &result[0];
        assert_eq!(old0, "2: mover-env", "chain-mover old_name uses fallback");
        assert_eq!(
            new0, "8: mover-env",
            "chain-mover new_name = victim_slot:chain_mover_name"
        );
        // Second pair: victim moves to free_num
        let (old1, new1) = &result[1];
        assert_eq!(old1, "8: victim-env", "victim old_name uses fallback");
        assert_eq!(
            new1, "11: victim-env",
            "victim new_name = free_num:victim_name"
        );
    }

    /// EC-5: Ordering invariant — `result[0]` is the chain-mover rename and
    /// `result[1]` is the victim eviction.  This is the correctness requirement
    /// flagged in review: executing these out of order would leave i3 in an
    /// inconsistent state (victim renamed before chain-mover vacates its slot).
    #[test]
    fn test_build_eviction_commands_three_element_ordering_chain_mover_before_victim() {
        let workspaces = vec![
            make_workspace(2, 2, "2: mover-env"),
            make_workspace(8, 8, "8: victim-env"),
        ];
        let chain = EvictionChain::ThreeElement {
            victim_name: "victim-env".to_string(),
            chain_mover_name: "mover-env".to_string(),
            chain_slot: 2,
            victim_slot: 8,
        };
        let result = build_eviction_commands(&chain, &workspaces, 11);
        assert_eq!(result.len(), 2);
        // result[0] must be the chain-mover (its new_name ends with chain_mover_name)
        assert!(
            result[0].1.ends_with("mover-env"),
            "result[0] must be the chain-mover rename, not the victim eviction; got {:?}",
            result[0]
        );
        // result[1] must be the victim (its new_name starts with free_num)
        assert!(
            result[1].1.starts_with("11:"),
            "result[1] must be the victim eviction to free_num, got {:?}",
            result[1]
        );
    }

    /// EC-6: Fallback name — when a workspace for a given slot is NOT present in
    /// the list, `build_eviction_commands` constructs the name as
    /// `"{slot}: {env_name}"` rather than panicking or omitting the command.
    ///
    /// Tested for both the chain-mover and the victim in a ThreeElement chain.
    #[test]
    fn test_build_eviction_commands_three_element_fallback_when_workspace_not_found() {
        // Workspace list is empty — neither slot 3 nor slot 7 is present.
        let workspaces: Vec<Workspace> = vec![];
        let chain = EvictionChain::ThreeElement {
            victim_name: "missing-victim".to_string(),
            chain_mover_name: "missing-mover".to_string(),
            chain_slot: 3,
            victim_slot: 7,
        };
        let result = build_eviction_commands(&chain, &workspaces, 12);
        assert_eq!(result.len(), 2);
        // Chain-mover fallback: "3: missing-mover"
        assert_eq!(
            result[0].0, "3: missing-mover",
            "chain-mover old_name must fall back to slot:name when not found"
        );
        assert_eq!(result[0].1, "7: missing-mover");
        // Victim fallback: "7: missing-victim"
        assert_eq!(
            result[1].0, "7: missing-victim",
            "victim old_name must fall back to slot:name when not found"
        );
        assert_eq!(result[1].1, "12: missing-victim");
    }

    #[test]
    fn test_workspace_command_with_backslash_quote_does_not_inject() {
        // A name containing \" (literal backslash+quote) must not allow
        // the quote to end the quoted string. Without escaping backslashes,
        // \" becomes \\" which i3 parses as \\ (literal backslash) + "
        // (end of string), enabling injection.
        let cmd = build_workspace_command(r#"1: evil\";exec bad"#);
        // After proper escaping: backslash → \\, then quote → \"
        // Result should be: workspace "1: evil\\\";exec bad"
        // The key check: the command must not contain an unescaped quote
        // in the middle that would terminate the string early.
        let inner = cmd
            .strip_prefix(r#"workspace ""#)
            .and_then(|s| s.strip_suffix('"'))
            .expect("Command should be wrapped in workspace \"...\"");

        // Walk the inner string: no unescaped quotes should appear
        let mut chars = inner.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '"' {
                panic!("Found unescaped quote in workspace command interior: {cmd}");
            }
            if ch == '\\' {
                // Skip the next char (it's escaped)
                chars.next();
            }
        }
    }
}
