use anyhow::Context;
use clap::Parser;
use i3ipc_types::reply::Workspace;
use tokio_i3ipc::I3;

/// Minimum NetBenefit required to justify evicting a workspace from a single-digit slot.
/// A swap that yields less than this gain is treated as not worth the disruption.
/// Tune upward to make the layout more stable; downward to make it more aggressive.
const STABILITY_THRESHOLD: f64 = 0.05;

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
fn find_eviction_candidate<'a>(
    workspaces: &'a [Workspace],
    managed_envs: &[ManagedEnvInfo],
    incoming_score: f64,
) -> Option<&'a Workspace> {
    let score_map: std::collections::HashMap<&str, f64> = managed_envs
        .iter()
        .map(|e| (e.name.as_str(), e.slot_score))
        .collect();

    workspaces
        .iter()
        .filter(|ws| ws.num >= 1 && ws.num <= 9)
        .filter_map(|ws| {
            let occupant_score = score_map.get(extract_environment_name(ws).as_str())?;
            let disc = 1.0 / (ws.num as f64 + 1.0).log2();
            let net_benefit = (incoming_score - occupant_score) * disc - STABILITY_THRESHOLD;
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

                // If the free slot is multi-digit, try to evict the lowest-NetBenefit
                // enwiro-managed single-digit workspace to make room.
                let incoming_score = managed_envs
                    .iter()
                    .find(|e| e.name == args.name)
                    .map(|e| e.slot_score)
                    .unwrap_or(0.0);
                let target_num = if free_num > 9 {
                    if let Some(victim) =
                        find_eviction_candidate(&workspaces, &managed_envs, incoming_score)
                    {
                        let victim_num = victim.num;
                        let victim_new_name =
                            format!("{}: {}", free_num, extract_environment_name(victim));
                        tracing::info!(
                            victim = %victim.name,
                            new_name = %victim_new_name,
                            "Evicting least-frecent workspace to free single-digit slot"
                        );
                        run_i3_command(
                            &mut i3,
                            build_rename_workspace_command(&victim.name, &victim_new_name),
                        )
                        .await?;
                        victim_num
                    } else {
                        free_num
                    }
                } else {
                    free_num
                };

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
