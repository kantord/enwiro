use anyhow::Context;
use clap::Parser;
use i3ipc_types::event::{Event, Subscribe};
use i3ipc_types::reply::Workspace;
use tokio_i3ipc::I3;
use tokio_stream::StreamExt;

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

#[derive(clap::Args)]
pub struct ListenArgs {
    #[arg(long, default_value_t = 300)]
    pub debounce_secs: u64,
}

#[derive(Parser)]
enum EnwiroAdapterI3WmCLI {
    GetActiveWorkspaceId(GetActiveWorkspaceIdArgs),
    Activate(ActivateArgs),
    Listen(ListenArgs),
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

/// Parse newline-delimited JSON from `enwiro list-all --json`.
///
/// Each line is parsed independently as a `serde_json::Value`. Lines that are
/// blank or fail to parse are silently skipped. Only entries where
/// `cookbook == "_"` AND `scores.slot` is a number are included in the output.
fn parse_managed_envs(json_lines: &str) -> Vec<ManagedEnvInfo> {
    json_lines
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).ok()?;
            // Only include entries managed by enwiro itself (cookbook == "_").
            if value.get("cookbook")?.as_str()? != "_" {
                return None;
            }
            let name = value.get("name")?.as_str()?.to_string();
            let slot_score = value.get("scores")?.get("slot")?.as_f64()?;
            Some(ManagedEnvInfo { name, slot_score })
        })
        .collect()
}

/// Run `enwiro list-all --json`, capture stdout, and parse via `parse_managed_envs`.
///
/// Returns an empty vec on any subprocess or parse error.
fn fetch_managed_envs() -> Vec<ManagedEnvInfo> {
    let enwiro_bin = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("enwiro")))
        .unwrap_or_else(|| std::path::PathBuf::from("enwiro"));
    let output = std::process::Command::new(&enwiro_bin)
        .args(["list-all", "--json"])
        .output();
    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            if !out.status.success() {
                tracing::warn!(status = %out.status, stderr = %stderr, "enwiro list-all failed");
            } else if stdout.is_empty() {
                tracing::warn!("enwiro list-all returned empty output");
            } else {
                tracing::debug!(lines = stdout.lines().count(), "enwiro list-all output");
            }
            parse_managed_envs(&stdout)
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to run enwiro list-all --json");
            vec![]
        }
    }
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

/// Returns `true` when a rebalance should run, `false` when it should be skipped
/// because one already ran within the debounce window.
///
/// # Parameters
/// * `last` – when the previous rebalance completed, or `None` if none has run yet.
/// * `debounce` – minimum time that must have elapsed before another rebalance is allowed.
/// * `now` – the current instant (passed in for testability).
fn should_rebalance(
    last: Option<std::time::Instant>,
    debounce: std::time::Duration,
    now: std::time::Instant,
) -> bool {
    match last {
        None => true,
        Some(last_instant) => now.duration_since(last_instant) >= debounce,
    }
}

/// A workspace slot entry used by `find_best_move`.
///
/// Represents a currently-placed managed environment occupying `slot` with the
/// given `name` and frecency-based `score`.
struct WorkspaceSlot {
    slot: i32,
    name: String,
    score: f64,
    managed: bool,
}

/// Find the single highest-NetBenefit swap across ALL currently placed managed envs.
///
/// # Algorithm
///
/// **Score-swap** (both slots occupied, slot_i < slot_j, score_j > score_i):
///   NetBenefit = (score_j − score_i) × (disc(slot_i) − disc(slot_j)) − STABILITY_THRESHOLD
///   The threshold guards against thrashing when two occupied slots are nearly equal.
///
/// **Compaction** (slot_i ≤ max_shortcut_slot is empty, env at slot_j, j > i):
///   NetBenefit = score_j × (disc(slot_i) − disc(slot_j))
///   No STABILITY_THRESHOLD — filling an empty slot has no thrashing risk, so the
///   move fires whenever score_j > 0 and disc(slot_i) > disc(slot_j) (i.e. NB > 0).
///
/// Boost the effective slot score of a newly-activated env that landed outside
/// the shortcut zone.
///
/// If `env_name` sits at a slot > `max_shortcut_slot` and its current score is
/// below the minimum score among managed shortcut envs, raise it to just above
/// that minimum (`min + f64::EPSILON`). This guarantees the env will win exactly
/// one swap into the shortcut zone during the convergence loop that follows.
///
/// No-op when:
/// - the env is already inside the shortcut zone, or
/// - there are no managed envs in the shortcut zone (all slots unmanaged).
fn boost_incoming_score(slots: &mut [WorkspaceSlot], env_name: &str, max_shortcut_slot: i32) {
    let free_num = match slots.iter().find(|ws| ws.name == env_name) {
        Some(ws) => ws.slot,
        None => return,
    };
    if free_num <= max_shortcut_slot {
        return;
    }
    let min_shortcut_score = slots
        .iter()
        .filter(|ws| ws.managed && ws.slot <= max_shortcut_slot)
        .map(|ws| ws.score)
        .fold(f64::INFINITY, f64::min);
    if min_shortcut_score.is_finite()
        && let Some(new_ws) = slots.iter_mut().find(|ws| ws.name == env_name)
    {
        new_ws.score = new_ws.score.max(min_shortcut_score + f64::EPSILON);
    }
}

/// Picks the pair with highest NetBenefit > 0; returns empty vec if none.
///
/// For a swap between two occupied slots: returns two rename pairs (lower-slot env
/// moves to higher slot first, then higher-slot env moves to lower slot).
/// For compaction (env moves into empty slot): returns one rename pair.
fn find_best_move(
    workspaces: &[WorkspaceSlot],
    max_shortcut_slot: i32,
    stability_threshold: f64,
) -> Vec<(String, String)> {
    // All physically occupied slots (managed + unmanaged) — used to detect truly empty slots.
    let all_occupied: std::collections::HashSet<i32> =
        workspaces.iter().map(|ws| ws.slot).collect();

    // Only managed workspaces — used for score-swap candidates and move generation.
    let managed_occupied: std::collections::HashMap<i32, (&str, f64)> = workspaces
        .iter()
        .filter(|ws| ws.managed)
        .map(|ws| (ws.slot, (ws.name.as_str(), ws.score)))
        .collect();

    let managed_slots: Vec<&WorkspaceSlot> = workspaces.iter().filter(|ws| ws.managed).collect();

    let mut best_nb: f64 = 0.0;
    let mut best_move: Option<(i32, i32, bool)> = None; // (slot_i, slot_j, is_swap)

    // --- Score-swap: both slots occupied (managed only), slot_i < slot_j, score_j > score_i ---
    for i in 0..managed_slots.len() {
        for j in (i + 1)..managed_slots.len() {
            let (slot_i, score_i) = (managed_slots[i].slot, managed_slots[i].score);
            let (slot_j, score_j) = (managed_slots[j].slot, managed_slots[j].score);
            let (slot_lo, score_lo, slot_hi, score_hi) = if slot_i < slot_j {
                (slot_i, score_i, slot_j, score_j)
            } else {
                (slot_j, score_j, slot_i, score_i)
            };
            if score_hi > score_lo {
                let nb =
                    (score_hi - score_lo) * (disc(slot_lo) - disc(slot_hi)) - stability_threshold;
                if nb > best_nb {
                    best_nb = nb;
                    best_move = Some((slot_lo, slot_hi, true));
                }
            }
        }
    }

    // --- Compaction: empty slot i (1 ≤ i ≤ max_shortcut_slot, not in all_occupied), managed env at slot j > i ---
    for slot_i in 1..=max_shortcut_slot {
        if all_occupied.contains(&slot_i) {
            continue; // slot physically taken (managed OR unmanaged)
        }
        for ws_j in managed_slots.iter() {
            let slot_j = ws_j.slot;
            let score_j = ws_j.score;
            if slot_j <= slot_i || score_j <= 0.0 {
                continue;
            }
            let nb = score_j * (disc(slot_i) - disc(slot_j));
            if nb > best_nb {
                best_nb = nb;
                best_move = Some((slot_i, slot_j, false));
            }
        }
    }

    match best_move {
        None => vec![],
        Some((slot_i, slot_j, is_swap)) => {
            if is_swap {
                // Both occupied: move lower-slot env out first (avoids name collision in i3),
                // then higher-slot env into the lower slot.
                let (name_i, _) = managed_occupied[&slot_i];
                let (name_j, _) = managed_occupied[&slot_j];
                vec![
                    (
                        format!("{}: {}", slot_i, name_i),
                        format!("{}: {}", slot_j, name_i),
                    ),
                    (
                        format!("{}: {}", slot_j, name_j),
                        format!("{}: {}", slot_i, name_j),
                    ),
                ]
            } else {
                // Compaction: env at slot_j moves to empty slot_i.
                let (name_j, _) = managed_occupied[&slot_j];
                vec![(
                    format!("{}: {}", slot_j, name_j),
                    format!("{}: {}", slot_i, name_j),
                )]
            }
        }
    }
}

fn extract_environment_name(workspace: &Workspace) -> String {
    workspace
        .name
        .split_once(':')
        .map(|(_, name)| name.trim().to_string())
        .unwrap_or_default()
}

fn extract_environment_name_from_str(name: &str) -> String {
    name.split_once(':')
        .map(|(_, rest)| rest.trim().to_string())
        .unwrap_or_else(|| name.to_string())
}

/// Build a `Vec<WorkspaceSlot>` from a list of i3 workspaces, including only managed
/// environments (those with a score entry in `managed_envs`).
fn build_workspace_slots(
    workspaces: &[Workspace],
    managed_envs: &[ManagedEnvInfo],
) -> Vec<WorkspaceSlot> {
    let score_map: std::collections::HashMap<&str, f64> = managed_envs
        .iter()
        .map(|e| (e.name.as_str(), e.slot_score))
        .collect();
    workspaces
        .iter()
        .map(|ws| {
            let env_name = extract_environment_name(ws);
            match score_map.get(env_name.as_str()) {
                Some(&score) => WorkspaceSlot {
                    slot: ws.num,
                    name: env_name,
                    score,
                    managed: true,
                },
                None => WorkspaceSlot {
                    slot: ws.num,
                    name: env_name,
                    score: 0.0,
                    managed: false,
                },
            }
        })
        .collect()
}

fn format_workspace_switch_event(env_name: &str, timestamp: i64) -> String {
    serde_json::json!({
        "type": "workspace_switch",
        "timestamp": timestamp,
        "env_name": env_name,
    })
    .to_string()
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

                // Build the slot list: existing managed workspaces + the incoming env at free_num.
                let incoming_score = managed_envs
                    .iter()
                    .find(|e| e.name == args.name)
                    .map(|e| e.slot_score)
                    .unwrap_or(0.0);
                let mut slots = build_workspace_slots(&workspaces, &managed_envs);
                slots.push(WorkspaceSlot {
                    slot: free_num,
                    name: args.name.clone(),
                    score: incoming_score,
                    managed: true,
                });

                // If the new env lands outside the shortcut zone with a low score,
                // boost its effective score so it can always displace the worst
                // managed shortcut env. This represents the "just activated" recency
                // signal that hasn't been written to disk yet.
                boost_incoming_score(&mut slots, &args.name, 9);

                // Create the workspace at free_num first so it exists in i3
                // before any rename commands reference it.
                let initial_workspace_name = format!("{}: {}", free_num, args.name);
                tracing::info!(workspace = %initial_workspace_name, "Creating workspace at free slot");
                run_i3_command(&mut i3, build_workspace_command(&initial_workspace_name)).await?;

                // Run find_best_move to convergence with no stability threshold
                // (explicit user activation = no thrash risk, loop until stable).
                let mut all_rename_cmds: Vec<(String, String)> = vec![];
                loop {
                    let moves = find_best_move(&slots, 9, 0.0);
                    if moves.is_empty() {
                        break;
                    }
                    for (old_name, new_name) in &moves {
                        let new_slot: i32 = new_name
                            .split_once(": ")
                            .and_then(|(s, _)| s.parse().ok())
                            .unwrap_or(0);
                        if let Some(ws) = slots
                            .iter_mut()
                            .find(|ws| format!("{}: {}", ws.slot, ws.name) == *old_name)
                        {
                            ws.slot = new_slot;
                        }
                    }
                    all_rename_cmds.extend(moves);
                }

                // Determine the final slot for the incoming env from the converged slot state.
                let target_num = slots
                    .iter()
                    .find(|ws| ws.name == args.name)
                    .map(|ws| ws.slot)
                    .unwrap_or(free_num);

                for (old_name, new_name) in all_rename_cmds {
                    tracing::info!(from = %old_name, to = %new_name, "Renaming workspace");
                    run_i3_command(
                        &mut i3,
                        build_rename_workspace_command(&old_name, &new_name),
                    )
                    .await?;
                }

                let workspace_name = format!("{}: {}", target_num, args.name);
                tracing::info!(workspace = %workspace_name, num = target_num, "Focusing final workspace slot");
                run_i3_command(&mut i3, build_workspace_command(&workspace_name)).await?;
            }
        }
        EnwiroAdapterI3WmCLI::Listen(listen_args) => {
            let debounce = std::time::Duration::from_secs(listen_args.debounce_secs);
            let mut last_rebalance: Option<std::time::Instant> = None;
            let mut i3 = I3::connect().await?;
            i3.subscribe([Subscribe::Workspace]).await?;
            let mut listener = i3.listen();
            loop {
                if let Some(Ok(Event::Workspace(ws_event))) = listener.next().await
                    && let Some(current) = ws_event.current
                {
                    let raw_name = current.name.unwrap_or_default();
                    let env_name = extract_environment_name_from_str(&raw_name);
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    println!("{}", format_workspace_switch_event(&env_name, ts));

                    let now = std::time::Instant::now();
                    if should_rebalance(last_rebalance, debounce, now) {
                        tracing::debug!("Rebalance check triggered");
                        let managed_envs = fetch_managed_envs();
                        tracing::debug!(count = managed_envs.len(), "Fetched managed envs");
                        let mut i3_rebalance = I3::connect().await?;
                        let workspaces = i3_rebalance.get_workspaces().await?;
                        let slots = build_workspace_slots(&workspaces, &managed_envs);
                        tracing::debug!(count = slots.len(), "Built workspace slots");
                        let commands = find_best_move(&slots, 9, STABILITY_THRESHOLD);
                        if commands.is_empty() {
                            tracing::debug!("No rebalance needed");
                        }
                        for (old_name, new_name) in commands {
                            tracing::info!(from = %old_name, to = %new_name, "Rebalancing workspace");
                            run_i3_command(
                                &mut i3_rebalance,
                                build_rename_workspace_command(&old_name, &new_name),
                            )
                            .await?;
                        }
                        last_rebalance = Some(std::time::Instant::now());
                    } else {
                        tracing::debug!("Rebalance skipped (rate limited)");
                    }
                }
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

    // ── listen subcommand tests ───────────────────────────────────────────────

    /// Test 1: Output of format_workspace_switch_event is valid JSON and has the
    /// expected keys: `type`, `timestamp`, and `env_name`.
    #[test]
    fn test_format_workspace_switch_event_is_valid_json_with_expected_keys() {
        let output = format_workspace_switch_event("my-project", 1700000000);
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output must be valid JSON");
        assert!(parsed.get("type").is_some(), "missing key: type");
        assert!(parsed.get("timestamp").is_some(), "missing key: timestamp");
        assert!(parsed.get("env_name").is_some(), "missing key: env_name");
    }

    /// Test 2: The `type` field is always the literal string `"workspace_switch"`.
    #[test]
    fn test_format_workspace_switch_event_type_is_workspace_switch() {
        let output = format_workspace_switch_event("any-env", 0);
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output must be valid JSON");
        assert_eq!(
            parsed["type"].as_str(),
            Some("workspace_switch"),
            "type field must be \"workspace_switch\""
        );
    }

    /// Test 3: The `env_name` field is passed through as-is (no transformation).
    #[test]
    fn test_format_workspace_switch_event_env_name_passthrough() {
        let env_name = "some-complex-env_name.with.dots";
        let output = format_workspace_switch_event(env_name, 42);
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output must be valid JSON");
        assert_eq!(
            parsed["env_name"].as_str(),
            Some(env_name),
            "env_name must be passed through unchanged"
        );
    }

    /// Test 4: The `timestamp` field round-trips correctly as an i64.
    #[test]
    fn test_format_workspace_switch_event_timestamp_roundtrips() {
        let timestamp: i64 = 1_700_000_000;
        let output = format_workspace_switch_event("proj", timestamp);
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output must be valid JSON");
        assert_eq!(
            parsed["timestamp"].as_i64(),
            Some(timestamp),
            "timestamp must round-trip as i64"
        );
    }

    /// Test 5: The output does NOT end with a trailing newline — the caller
    /// adds one via `println!`.
    #[test]
    fn test_format_workspace_switch_event_no_trailing_newline() {
        let output = format_workspace_switch_event("proj", 0);
        assert!(
            !output.ends_with('\n'),
            "format_workspace_switch_event must not append a trailing newline; \
             the caller uses println! to add one"
        );
    }

    /// Test 6: The CLI enum has a `Listen` variant that can be constructed.
    /// This test will fail to compile until the variant and `ListenArgs` exist.
    #[test]
    fn test_cli_has_listen_variant() {
        let _ = EnwiroAdapterI3WmCLI::Listen(ListenArgs { debounce_secs: 300 });
    }

    // ── should_rebalance tests ────────────────────────────────────────────────
    //
    // `should_rebalance` is a pure function:
    //
    //   fn should_rebalance(
    //       last: Option<std::time::Instant>,
    //       debounce: std::time::Duration,
    //       now: std::time::Instant,
    //   ) -> bool
    //
    // It answers: "given when the last rebalance ran (if ever), the debounce window,
    // and the current instant, should we run a rebalance now?"
    //
    // The rule:
    //   - If `last` is `None`  → always rebalance (no prior run to suppress it).
    //   - If `now - last < debounce` → skip (too soon).
    //   - If `now - last >= debounce` → rebalance (window has expired, boundary inclusive).

    /// SR-1: No prior rebalance has run — `last` is `None`.
    ///
    /// Expected: `should_rebalance(None, any_debounce, now)` returns `true`.
    #[test]
    fn test_should_rebalance_returns_true_when_no_prior_run() {
        let now = std::time::Instant::now();
        let debounce = std::time::Duration::from_secs(300);

        assert!(
            should_rebalance(None, debounce, now),
            "should_rebalance must return true when last is None (no prior rebalance has run)"
        );
    }

    /// SR-2: A prior rebalance ran, but enough time has elapsed (elapsed > debounce).
    ///
    /// Setup: `last` = 400 seconds ago, `debounce` = 300 s.
    /// elapsed (400 s) > debounce (300 s) → should rebalance.
    ///
    /// Expected: `true`.
    #[test]
    fn test_should_rebalance_returns_true_when_elapsed_exceeds_debounce() {
        let debounce = std::time::Duration::from_secs(300);
        let elapsed_since_last = std::time::Duration::from_secs(400);
        let now = std::time::Instant::now();
        // Simulate `last` by computing a past instant: now - elapsed_since_last
        let last = now
            .checked_sub(elapsed_since_last)
            .expect("test machine clock must support subtracting 400 s from Instant::now()");

        assert!(
            should_rebalance(Some(last), debounce, now),
            "should_rebalance must return true when elapsed ({elapsed_since_last:?}) \
             exceeds debounce ({debounce:?})"
        );
    }

    /// SR-3: A prior rebalance ran recently — elapsed < debounce → skip.
    ///
    /// Setup: `last` = 100 seconds ago, `debounce` = 300 s.
    /// elapsed (100 s) < debounce (300 s) → must skip.
    ///
    /// Expected: `false`.
    #[test]
    fn test_should_rebalance_returns_false_when_elapsed_is_less_than_debounce() {
        let debounce = std::time::Duration::from_secs(300);
        let elapsed_since_last = std::time::Duration::from_secs(100);
        let now = std::time::Instant::now();
        let last = now
            .checked_sub(elapsed_since_last)
            .expect("test machine clock must support subtracting 100 s from Instant::now()");

        assert!(
            !should_rebalance(Some(last), debounce, now),
            "should_rebalance must return false when elapsed ({elapsed_since_last:?}) \
             is less than debounce ({debounce:?})"
        );
    }

    /// SR-4: Boundary — elapsed equals the debounce exactly.
    ///
    /// Setup: `last` = exactly 300 seconds ago, `debounce` = 300 s.
    /// elapsed (300 s) == debounce (300 s) → boundary is inclusive, should rebalance.
    ///
    /// Expected: `true`.
    #[test]
    fn test_should_rebalance_returns_true_when_elapsed_equals_debounce_exactly() {
        let debounce = std::time::Duration::from_secs(300);
        let elapsed_since_last = std::time::Duration::from_secs(300);
        let now = std::time::Instant::now();
        let last = now
            .checked_sub(elapsed_since_last)
            .expect("test machine clock must support subtracting 300 s from Instant::now()");

        assert!(
            should_rebalance(Some(last), debounce, now),
            "should_rebalance must return true when elapsed ({elapsed_since_last:?}) \
             equals debounce exactly ({debounce:?}) — the boundary is inclusive"
        );
    }

    // ── parse_managed_envs tests ──────────────────────────────────────────────
    //
    // `parse_managed_envs` accepts newline-delimited JSON (one entry per line)
    // produced by `enwiro list-all --json` and returns only managed environments
    // (cookbook == "_") that carry a `scores.slot` value.
    //
    // Each returned `ManagedEnvInfo` has:
    //   - `name`       – the `name` field of the JSON entry
    //   - `slot_score` – taken from `scores.slot`

    /// An environment entry with `cookbook == "_"` and a `scores.slot` value is
    /// included in the output.
    #[test]
    fn test_parse_managed_envs_includes_env_with_slot_score() {
        let input = r#"{"cookbook":"_","name":"my-env","sort_order":0,"scores":{"launcher":0.9,"slot":0.7}}"#;
        let result = parse_managed_envs(input);
        assert_eq!(result.len(), 1, "expected one entry, got {}", result.len());
        assert_eq!(result[0].name, "my-env");
        assert!(
            (result[0].slot_score - 0.7).abs() < 1e-10,
            "slot_score must come from scores.slot; got {}",
            result[0].slot_score
        );
    }

    /// A recipe entry (cookbook != "_") is excluded even if it has a scores.slot.
    #[test]
    fn test_parse_managed_envs_excludes_recipe_entry() {
        let input =
            r#"{"cookbook":"github","name":"some-repo","sort_order":0,"scores":{"slot":0.5}}"#;
        let result = parse_managed_envs(input);
        assert!(
            result.is_empty(),
            "recipe entries (cookbook != \"_\") must be excluded; got {:?}",
            result
        );
    }

    /// An environment entry with `cookbook == "_"` but without any `scores` field
    /// is excluded.
    #[test]
    fn test_parse_managed_envs_excludes_env_without_scores() {
        let input = r#"{"cookbook":"_","name":"no-scores-env","sort_order":0}"#;
        let result = parse_managed_envs(input);
        assert!(
            result.is_empty(),
            "entries without a scores field must be excluded; got {:?}",
            result
        );
    }

    /// An environment entry with `cookbook == "_"` and a `scores` object that
    /// lacks the `slot` key is excluded.
    #[test]
    fn test_parse_managed_envs_excludes_env_without_slot_key() {
        let input =
            r#"{"cookbook":"_","name":"no-slot-env","sort_order":0,"scores":{"launcher":0.9}}"#;
        let result = parse_managed_envs(input);
        assert!(
            result.is_empty(),
            "entries whose scores object has no 'slot' key must be excluded; got {:?}",
            result
        );
    }

    /// Empty input string returns an empty vec.
    #[test]
    fn test_parse_managed_envs_empty_input_returns_empty_vec() {
        let result = parse_managed_envs("");
        assert!(result.is_empty(), "empty input must produce an empty vec");
    }

    /// Blank / whitespace-only lines are silently skipped.
    #[test]
    fn test_parse_managed_envs_skips_blank_lines() {
        let input = "\n   \n\t\n";
        let result = parse_managed_envs(input);
        assert!(
            result.is_empty(),
            "blank lines must be skipped silently; got {:?}",
            result
        );
    }

    /// Malformed JSON lines are silently skipped; valid lines on other rows are
    /// still processed.
    #[test]
    fn test_parse_managed_envs_skips_malformed_json_lines() {
        let input = concat!(
            "not-valid-json\n",
            r#"{"cookbook":"_","name":"good-env","sort_order":0,"scores":{"slot":0.3}}"#,
            "\n",
            "{broken",
        );
        let result = parse_managed_envs(input);
        assert_eq!(
            result.len(),
            1,
            "only the valid managed entry must be returned; got {:?}",
            result
        );
        assert_eq!(result[0].name, "good-env");
        assert!((result[0].slot_score - 0.3).abs() < 1e-10);
    }

    /// Multiple valid environment entries all appear in the output, and their
    /// slot_score values are taken from `scores.slot` independently.
    #[test]
    fn test_parse_managed_envs_returns_multiple_entries() {
        let input = concat!(
            r#"{"cookbook":"_","name":"env-a","sort_order":0,"scores":{"slot":0.8}}"#,
            "\n",
            r#"{"cookbook":"_","name":"env-b","sort_order":1,"scores":{"launcher":0.4,"slot":0.2}}"#,
        );
        let result = parse_managed_envs(input);
        assert_eq!(
            result.len(),
            2,
            "both valid entries must be returned; got {:?}",
            result
        );
        let names: Vec<&str> = result.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"env-a"), "env-a must be present");
        assert!(names.contains(&"env-b"), "env-b must be present");
        let a = result.iter().find(|e| e.name == "env-a").unwrap();
        let b = result.iter().find(|e| e.name == "env-b").unwrap();
        assert!(
            (a.slot_score - 0.8).abs() < 1e-10,
            "env-a slot_score wrong: {}",
            a.slot_score
        );
        assert!(
            (b.slot_score - 0.2).abs() < 1e-10,
            "env-b slot_score wrong: {}",
            b.slot_score
        );
    }

    /// Mixed input: recipe, env-without-scores, valid env, malformed — only the
    /// valid env survives.
    #[test]
    fn test_parse_managed_envs_mixed_input_only_valid_env_survives() {
        let input = concat!(
            r#"{"cookbook":"github","name":"a-recipe","sort_order":0}"#,
            "\n",
            r#"{"cookbook":"_","name":"no-scores","sort_order":0}"#,
            "\n",
            r#"{"cookbook":"_","name":"the-keeper","sort_order":0,"scores":{"slot":0.6}}"#,
            "\n",
            "totally-broken",
        );
        let result = parse_managed_envs(input);
        assert_eq!(
            result.len(),
            1,
            "only 'the-keeper' should survive filtering; got {:?}",
            result
        );
        assert_eq!(result[0].name, "the-keeper");
        assert!((result[0].slot_score - 0.6).abs() < 1e-10);
    }

    // ── find_best_move tests ──────────────────────────────────────────────────
    //
    // `find_best_move` is a pure function:
    //
    //   fn find_best_move(
    //       workspaces: &[WorkspaceSlot],
    //       max_shortcut_slot: i32,
    //   ) -> Vec<(String, String)>
    //
    // It finds the single highest-NetBenefit swap across ALL currently-placed
    // managed envs, considering:
    //
    //   Score-swap (both slots occupied, slot_i < slot_j, score_j > score_i):
    //     NB = (score_j − score_i) × (disc(slot_i) − disc(slot_j)) − STABILITY_THRESHOLD
    //     (threshold guards against thrashing between two occupied slots)
    //
    //   Compaction (slot_i ≤ max_shortcut_slot is empty, env at slot_j, j > i):
    //     NB = score_j × (disc(slot_i) − disc(slot_j))
    //     (no STABILITY_THRESHOLD — filling an empty slot has no thrashing risk;
    //      fires whenever score_j > 0 and NB > 0)
    //
    // Returns empty vec when no NB > 0.
    // Score-swap: two rename pairs (lower-slot env out first, higher-slot env in).
    // Compaction: one rename pair.

    fn make_slot(slot: i32, name: &str, score: f64) -> WorkspaceSlot {
        WorkspaceSlot {
            slot,
            name: name.to_string(),
            score,
            managed: true,
        }
    }

    /// FBM-1: Empty input — no workspaces at all → empty vec.
    #[test]
    fn test_find_best_move_empty_input_returns_empty() {
        let result = find_best_move(&[], 9, STABILITY_THRESHOLD);
        assert!(
            result.is_empty(),
            "find_best_move with empty input must return [], got {:?}",
            result
        );
    }

    /// FBM-2: Score-zero env — no move fires.
    ///
    /// A single workspace with score 0.0 has no profitable compaction
    /// (NB = 0 × disc_diff = 0, which does not beat `best_nb` initialised to 0.0)
    /// and no swap partner. Expected: empty vec.
    #[test]
    fn test_find_best_move_single_workspace_no_swap_possible() {
        // score = 0.0: compaction NB = 0×(disc(i)-disc(j)) - 0.05 < 0, no score-swap partner
        let ws = [make_slot(5, "zero-score-env", 0.0)];
        let result = find_best_move(&ws, 9, STABILITY_THRESHOLD);
        assert!(
            result.is_empty(),
            "single workspace with score 0.0 cannot benefit from compaction or swap, \
             got {:?}",
            result
        );
    }

    /// FBM-3: Score-swap between two occupied slots fires.
    ///
    /// slot 1: low-score-env,  score = 0.1
    /// slot 5: high-score-env, score = 0.9
    ///
    /// NB = (0.9 − 0.1) × (disc(1) − disc(5)) − 0.05
    ///    disc(1) = 1.0, disc(5) = 1/log2(6) ≈ 0.387
    ///    NB ≈ 0.8 × 0.613 − 0.05 ≈ 0.440 > 0  ✓
    ///
    /// Expected: two rename pairs — low-score-env from slot 1 → slot 5 first,
    /// then high-score-env from slot 5 → slot 1.
    #[test]
    fn test_find_best_move_score_swap_between_two_occupied_slots() {
        let ws = [
            make_slot(1, "low-score-env", 0.1),
            make_slot(5, "high-score-env", 0.9),
        ];
        // max_shortcut_slot = 9; both slots are well within range
        let result = find_best_move(&ws, 9, STABILITY_THRESHOLD);

        let nb = (0.9_f64 - 0.1) * (disc(1) - disc(5)) - STABILITY_THRESHOLD;
        assert!(nb > 0.0, "precondition: NB={nb} must be positive");

        assert!(
            !result.is_empty(),
            "score-swap with positive NB must fire, got []"
        );
        let new_names: Vec<&str> = result.iter().map(|(_, n)| n.as_str()).collect();
        assert!(
            new_names
                .iter()
                .any(|n| n.contains("high-score-env") && n.starts_with("1:")),
            "high-score-env must end up in slot 1; got {:?}",
            result
        );
        assert!(
            new_names
                .iter()
                .any(|n| n.contains("low-score-env") && n.starts_with("5:")),
            "low-score-env must end up in slot 5; got {:?}",
            result
        );
    }

    /// FBM-4: Score-swap still fires when one slot is > max_shortcut_slot.
    ///
    /// The slot restriction only applies to empty-slot compaction targets; occupied
    /// score-swaps can involve ANY slot number.
    ///
    /// To isolate the score-swap path, all shortcut slots 1–9 must be occupied so
    /// the unified compaction loop finds no empty target.
    ///
    /// Setup (max_shortcut_slot = 9):
    ///   slot 1:  filler-1,      score = 0.9  ─┐
    ///   slot 2:  filler-2,      score = 0.9   │  occupy all shortcut slots
    ///   slot 4:  filler-4,      score = 0.9   │  except slot 3 is low-score
    ///   slots 5–9: filler envs, score = 0.9  ─┘
    ///   slot 3:  low-score-env, score = 0.1   ← inside shortcut range, low score
    ///   slot 11: high-score-env, score = 0.9  ← outside shortcut range
    ///
    /// No empty slot ≤ 9 → no compaction.
    /// Score-swap 3 ↔ 11:
    ///   NB = (0.9 − 0.1) × (disc(3) − disc(11)) − 0.05
    ///      disc(3) = 0.5,  disc(11) ≈ 0.278
    ///      NB ≈ 0.8 × 0.222 − 0.05 ≈ 0.128 > 0  ✓
    ///
    /// Expected: score-swap fires, high-score-env ends up in slot 3.
    #[test]
    fn test_find_best_move_score_swap_fires_when_one_slot_exceeds_max_shortcut() {
        let mut ws: Vec<WorkspaceSlot> = vec![
            make_slot(3, "low-score-env", 0.1),
            make_slot(11, "high-score-env", 0.9),
        ];
        // Fill all shortcut slots except slot 3 with neutral-score envs.
        for s in [1i32, 2, 4, 5, 6, 7, 8, 9] {
            ws.push(make_slot(s, &format!("filler-{s}"), 0.9));
        }
        let result = find_best_move(&ws, 9, STABILITY_THRESHOLD);

        let nb = (0.9_f64 - 0.1) * (disc(3) - disc(11)) - STABILITY_THRESHOLD;
        assert!(nb > 0.0, "precondition: NB={nb} must be positive");

        assert!(
            !result.is_empty(),
            "score-swap must fire even when one slot ({}) > max_shortcut_slot (9); got []",
            11
        );
        let new_names: Vec<&str> = result.iter().map(|(_, n)| n.as_str()).collect();
        assert!(
            new_names
                .iter()
                .any(|n| n.contains("high-score-env") && n.starts_with("3:")),
            "high-score-env must end up in slot 3; got {:?}",
            result
        );
    }

    /// FBM-5: Compaction — empty slot ≤ max_shortcut_slot, env at slot > max_shortcut_slot.
    ///
    /// slot 11: my-env, score = 0.9
    /// Slots 1–9: all empty (no WorkspaceSlots for those slots).
    /// max_shortcut_slot = 9
    ///
    /// Best compaction: slot 11 → slot 1.
    /// NB = 0.9 × (disc(1) − disc(11)) − 0.05
    ///    disc(1) = 1.0, disc(11) ≈ 0.278
    ///    NB ≈ 0.9 × 0.722 − 0.05 ≈ 0.600 > 0  ✓
    ///
    /// Expected: one rename pair moving "11: my-env" → "1: my-env".
    #[test]
    fn test_find_best_move_compaction_from_above_shortcut_into_empty_shortcut_slot() {
        let ws = [make_slot(11, "my-env", 0.9)];
        let result = find_best_move(&ws, 9, STABILITY_THRESHOLD);

        let nb = 0.9_f64 * (disc(1) - disc(11)) - STABILITY_THRESHOLD;
        assert!(nb > 0.0, "precondition: NB={nb} must be positive");

        assert_eq!(
            result.len(),
            1,
            "compaction must produce exactly one rename pair, got {:?}",
            result
        );
        let (old, new) = &result[0];
        assert_eq!(old, "11: my-env", "old name should reflect original slot");
        assert_eq!(
            new, "1: my-env",
            "compaction must target the best empty slot (slot 1)"
        );
    }

    /// FBM-6: No move when all shortcut slots are occupied and scores are equal.
    ///
    /// Slots 1–9 all occupied (score 0.9), env-slot-11 at slot 11 (score 0.9).
    /// No empty slot ≤ 9 → compaction cannot fire.
    /// All score diffs = 0 → NB = 0 − 0.05 < 0 for every swap pair → no swap fires.
    /// Expected: empty vec.
    #[test]
    fn test_find_best_move_no_compaction_when_all_shortcut_slots_occupied() {
        // Slots 1–9 occupied with score 0.9, slot 11 also score 0.9.
        // No empty slot ≤ 9.
        // Score-swap between any pair: all scores equal → NB = 0 × (…) − 0.05 < 0.
        let mut ws: Vec<WorkspaceSlot> = (1..=9_i32)
            .map(|s| make_slot(s, &format!("env-slot-{}", s), 0.9))
            .collect();
        ws.push(make_slot(11, "env-slot-11", 0.9));
        let result = find_best_move(&ws, 9, STABILITY_THRESHOLD);
        assert!(
            result.is_empty(),
            "no compaction when all shortcut slots are occupied and no score difference, \
             got {:?}",
            result
        );
    }

    /// FBM-8: Stability threshold blocks a small-gain swap.
    ///
    /// To isolate the score-swap path (and prevent the unified compaction loop from
    /// firing on empty slots), use max_shortcut_slot = 1 with slot 1 already occupied.
    /// There are then no empty slots ≤ max_shortcut_slot, so no compaction is possible.
    ///
    /// slot 1: env-a, score = 0.64   (occupies the only shortcut slot)
    /// slot 5: env-b, score = 0.65
    /// max_shortcut_slot = 1
    ///
    /// Score-swap NB = (0.65 − 0.64) × (disc(1) − disc(5)) − 0.05
    ///    = 0.01 × (1.0 − 0.387) − 0.05
    ///    ≈ 0.006 − 0.05 = −0.044 < 0
    ///
    /// No compaction targets (slot 1 occupied, max_shortcut_slot = 1).
    /// Expected: empty vec (gain too small to clear STABILITY_THRESHOLD).
    #[test]
    fn test_find_best_move_stability_threshold_blocks_small_gain_swap() {
        let ws = [make_slot(1, "env-a", 0.64), make_slot(5, "env-b", 0.65)];
        let nb = (0.65_f64 - 0.64) * (disc(1) - disc(5)) - STABILITY_THRESHOLD;
        assert!(
            nb < 0.0,
            "precondition: NB={nb} must be negative (below stability threshold)"
        );

        // max_shortcut_slot=1: slot 1 is occupied → no empty compaction targets.
        let result = find_best_move(&ws, 1, STABILITY_THRESHOLD);
        assert!(
            result.is_empty(),
            "stability threshold must block a swap with NB={nb:.5} < 0, got {:?}",
            result
        );
    }

    /// FBM-9: Multiple candidates — picks the highest NetBenefit.
    ///
    /// Candidates:
    ///   A — compaction: env at slot 7, score = 0.8, empty slot 1.
    ///       NB_A = 0.8 × (disc(1) − disc(7)) − 0.05
    ///            = 0.8 × (1.0 − 0.356) − 0.05 ≈ 0.8 × 0.644 − 0.05 ≈ 0.465
    ///
    ///   B — score-swap: slot 2: low-score-env (0.1) ↔ slot 3: mid-score-env (0.5).
    ///       NB_B = (0.5 − 0.1) × (disc(2) − disc(3)) − 0.05
    ///            = 0.4 × (0.631 − 0.5) − 0.05 ≈ 0.4 × 0.131 − 0.05 ≈ 0.002 > 0
    ///
    /// NB_A ≈ 0.465 >> NB_B ≈ 0.002 → compaction (A) wins.
    ///
    /// Verify that result is the compaction pair (env-at-7 → slot 1).
    #[test]
    fn test_find_best_move_picks_highest_net_benefit_among_multiple_candidates() {
        // Slot 1: empty (no WorkspaceSlot).
        // Slot 2: low-score-env, score = 0.1
        // Slot 3: mid-score-env, score = 0.5
        // Slot 7: compact-env,   score = 0.8
        let ws = [
            make_slot(2, "low-score-env", 0.1),
            make_slot(3, "mid-score-env", 0.5),
            make_slot(7, "compact-env", 0.8),
        ];
        let max_shortcut_slot = 9;

        // Verify NB values numerically.
        let nb_a = 0.8_f64 * (disc(1) - disc(7)) - STABILITY_THRESHOLD;
        let nb_b = (0.5_f64 - 0.1) * (disc(2) - disc(3)) - STABILITY_THRESHOLD;
        assert!(nb_a > 0.0, "precondition: NB_A={nb_a} must be positive");
        assert!(nb_b > 0.0, "precondition: NB_B={nb_b} must be positive");
        assert!(
            nb_a > nb_b,
            "precondition: NB_A={nb_a} must exceed NB_B={nb_b}"
        );

        let result = find_best_move(&ws, max_shortcut_slot, STABILITY_THRESHOLD);

        assert_eq!(
            result.len(),
            1,
            "compaction produces one rename pair; got {:?}",
            result
        );
        let (old, new) = &result[0];
        assert_eq!(
            old, "7: compact-env",
            "argmax must select compaction of compact-env out of slot 7; got old={old:?}"
        );
        assert_eq!(
            new, "1: compact-env",
            "compact-env must move to empty slot 1; got new={new:?}"
        );
    }

    /// FBM-10: Cross-boundary compaction still finds an empty slot when the slot
    /// immediately after max_shortcut_occupied is itself occupied.
    ///
    /// The original "next-after-max" heuristic only checked ONE candidate slot.
    /// If that slot was occupied it silently gave up.  The unified loop must
    /// scan ALL empty slots ≤ max_shortcut_slot.
    ///
    /// Setup (max_shortcut_slot = 9):
    ///   slot  5: anchor-env,  score = 0.9   (inside shortcut range, occupies slot 5 + 6 = 6)
    ///   slot  6: filler-env,  score = 0.9   (occupies the "next-after-5" candidate slot)
    ///   slot 11: cross-env,   score = 0.9   (outside shortcut range)
    ///
    /// "Next after max occupied shortcut slot (5)" is slot 6, but slot 6 is occupied.
    /// The unified loop must find slot 7 (or any other empty slot ≤ 9) and compact
    /// cross-env into it.
    ///
    /// NB for cross-env (slot 11) → slot 7:
    ///   NB = 0.9 × (disc(7) - disc(11)) - 0.05
    ///      disc(7)  = 1/log2(8) = 1/3 ≈ 0.333
    ///      disc(11) = 1/log2(12) ≈ 0.278
    ///      NB ≈ 0.9 × 0.055 - 0.05 ≈ 0.0 -- possibly below threshold
    ///
    /// Use slot 3 (empty) instead of slot 7 to get a clearly positive NB:
    ///   slot  5: anchor-env, score = 0.9
    ///   slot  6: filler-env, score = 0.9
    ///   slot 11: cross-env,  score = 0.9
    ///   (slots 1–4 are all empty, as are 7–9)
    ///
    /// NB for cross-env → slot 1:
    ///   NB = 0.9 × (disc(1) - disc(11)) - 0.05
    ///      = 0.9 × (1.0 - 0.278) - 0.05 ≈ 0.600 > 0  ✓
    ///
    /// Expected: exactly one rename pair moving "11: cross-env" → "1: cross-env".
    #[test]
    fn test_find_best_move_cross_boundary_skips_occupied_next_slot_finds_other_empty() {
        let ws = [
            make_slot(5, "anchor-env", 0.9),
            make_slot(6, "filler-env", 0.9),
            make_slot(11, "cross-env", 0.9),
        ];
        let max_shortcut_slot = 9;

        let nb = 0.9_f64 * (disc(1) - disc(11)) - STABILITY_THRESHOLD;
        assert!(nb > 0.0, "precondition: NB={nb} must be positive");

        let result = find_best_move(&ws, max_shortcut_slot, STABILITY_THRESHOLD);

        assert_eq!(
            result.len(),
            1,
            "cross-boundary compaction must produce exactly one rename pair; got {:?}",
            result
        );
        let (old, new_name) = &result[0];
        assert_eq!(
            old, "11: cross-env",
            "source must be '11: cross-env'; got {old:?}"
        );
        assert_eq!(
            new_name, "1: cross-env",
            "cross-env must compact into slot 1; got {new_name:?}"
        );
    }

    /// FBM-11: Internal shortcut gap — compaction fills a gap inside the shortcut range,
    /// not just before the minimum occupied shortcut slot.
    ///
    /// The original "within-shortcut" branch only targeted slots BEFORE the minimum
    /// occupied shortcut slot, so an internal gap (e.g. slot 5 empty while slots 1–3
    /// and 7 are occupied) was never targeted.
    ///
    /// Setup (max_shortcut_slot = 9):
    ///   slot 1: env-a, score = 0.9
    ///   slot 2: env-b, score = 0.9
    ///   slot 3: env-c, score = 0.9
    ///   slot 7: env-d, score = 0.9
    ///   (slot 5 is empty — internal gap)
    ///
    /// NB for env-d (slot 7) → slot 5 (empty):
    ///   NB = 0.9 × (disc(5) - disc(7)) - 0.05
    ///      disc(5) = 1/log2(6) ≈ 0.387
    ///      disc(7) = 1/log2(8) ≈ 0.333
    ///      NB ≈ 0.9 × 0.054 - 0.05 ≈ -0.001 — likely below threshold
    ///
    /// Use slot 4 (empty) instead, and env at slot 8:
    ///   slot 1: env-a, score = 0.9
    ///   slot 2: env-b, score = 0.9
    ///   slot 3: env-c, score = 0.9
    ///   slot 8: env-d, score = 0.9
    ///   (slot 4 is empty — internal gap between 3 and 8)
    ///
    /// NB for env-d (slot 8) → slot 4 (empty):
    ///   NB = 0.9 × (disc(4) - disc(8)) - 0.05
    ///      disc(4) = 1/log2(5) ≈ 0.431
    ///      disc(8) = 1/log2(9) ≈ 0.315
    ///      NB ≈ 0.9 × 0.116 - 0.05 ≈ 0.054 > 0  ✓
    ///
    /// Expected: exactly one rename pair moving "8: env-d" → "4: env-d".
    #[test]
    fn test_find_best_move_internal_shortcut_gap_compaction() {
        let ws = [
            make_slot(1, "env-a", 0.9),
            make_slot(2, "env-b", 0.9),
            make_slot(3, "env-c", 0.9),
            make_slot(8, "env-d", 0.9),
        ];
        let max_shortcut_slot = 9;

        let nb = 0.9_f64 * (disc(4) - disc(8)) - STABILITY_THRESHOLD;
        assert!(nb > 0.0, "precondition: NB={nb:.4} must be positive");

        let result = find_best_move(&ws, max_shortcut_slot, STABILITY_THRESHOLD);

        assert_eq!(
            result.len(),
            1,
            "internal gap compaction must produce exactly one rename pair; got {:?}",
            result
        );
        let (old, new_name) = &result[0];
        assert_eq!(old, "8: env-d", "source must be '8: env-d'; got {old:?}");
        assert_eq!(
            new_name, "4: env-d",
            "env-d must move to internal empty slot 4; got {new_name:?}"
        );
    }

    /// FBM-12-pre: Compaction between adjacent high-numbered slots fires without stability
    /// threshold.
    ///
    /// Filling an empty slot has no thrashing risk, so STABILITY_THRESHOLD must NOT be
    /// applied to compaction moves.  Only score-swaps between two occupied slots need it.
    ///
    /// Setup:
    ///   slot 7: empty
    ///   slot 8: chezmoi, score = 0.8
    ///   max_shortcut_slot = 9
    ///
    /// disc(7) ≈ 0.333, disc(8) ≈ 0.315 → disc diff ≈ 0.018, well below STABILITY_THRESHOLD=0.05.
    ///
    /// With threshold:    NB = 0.8 × 0.018 − 0.05 ≈ −0.036 < 0  → currently blocked (bug)
    /// Without threshold: NB = 0.8 × 0.018 ≈ 0.014 > 0           → should fire (fix)
    ///
    /// Expected: one rename pair moving "8: chezmoi" → "7: chezmoi".
    #[test]
    fn test_find_best_move_compaction_fires_without_stability_threshold() {
        // Occupy slots 1–6 so slot 7 is the only available empty shortcut slot.
        // chezmoi sits at slot 8 with score 0.8.
        let mut ws: Vec<WorkspaceSlot> = (1..=6_i32)
            .map(|s| make_slot(s, &format!("filler-{s}"), 0.9))
            .collect();
        ws.push(make_slot(8, "chezmoi", 0.8));
        let max_shortcut_slot = 9;

        // Confirm the disc diff is below STABILITY_THRESHOLD (this is what the bug is about).
        let disc_diff = disc(7) - disc(8);
        assert!(
            disc_diff < STABILITY_THRESHOLD,
            "precondition: disc diff {disc_diff:.6} must be < STABILITY_THRESHOLD \
             {STABILITY_THRESHOLD} to reproduce the bug"
        );
        // Confirm move has positive benefit without the threshold.
        let nb_no_threshold = 0.8_f64 * disc_diff;
        assert!(
            nb_no_threshold > 0.0,
            "precondition: NB without threshold {nb_no_threshold:.6} must be > 0"
        );

        let result = find_best_move(&ws, max_shortcut_slot, STABILITY_THRESHOLD);

        assert_eq!(
            result.len(),
            1,
            "compaction of chezmoi from slot 8 → 7 must fire (no threshold for compaction); \
             got {:?}",
            result
        );
        let (old, new_name) = &result[0];
        assert_eq!(
            old, "8: chezmoi",
            "source must be '8: chezmoi'; got {old:?}"
        );
        assert_eq!(
            new_name, "7: chezmoi",
            "chezmoi must move to empty slot 7; got {new_name:?}"
        );
    }

    /// FBM-12: Score-swap pair ordering — the lower-slot env moves OUT first.
    ///
    /// i3 identifies workspaces by name, so both cannot share a name even momentarily.
    /// The correct order is:
    ///   result[0]: move lower-slot env from slot_lo to slot_hi  (vacates slot_lo)
    ///   result[1]: move higher-slot env from slot_hi to slot_lo (fills vacated slot_lo)
    ///
    /// Setup:
    ///   slot 1: low-score-env,  score = 0.1
    ///   slot 5: high-score-env, score = 0.9
    ///
    /// NB = (0.9 - 0.1) × (disc(1) - disc(5)) - 0.05 > 0  ✓  (same as FBM-3)
    ///
    /// Expected ordering:
    ///   result[0]: ("1: low-score-env",  "5: low-score-env")   ← lower slot out first
    ///   result[1]: ("5: high-score-env", "1: high-score-env")  ← higher slot fills gap
    #[test]
    fn test_find_best_move_swap_pair_lower_slot_moves_out_first() {
        let ws = [
            make_slot(1, "low-score-env", 0.1),
            make_slot(5, "high-score-env", 0.9),
        ];
        let result = find_best_move(&ws, 9, STABILITY_THRESHOLD);

        assert_eq!(
            result.len(),
            2,
            "score-swap must produce exactly two pairs; got {:?}",
            result
        );

        // result[0]: lower-slot env moves OUT (from slot 1 → slot 5)
        let (old0, new0) = &result[0];
        assert_eq!(
            old0, "1: low-score-env",
            "result[0] old must be '1: low-score-env' (lower-slot env moves out first); got {old0:?}"
        );
        assert_eq!(
            new0, "5: low-score-env",
            "result[0] new must be '5: low-score-env'; got {new0:?}"
        );

        // result[1]: higher-slot env moves IN (from slot 5 → slot 1)
        let (old1, new1) = &result[1];
        assert_eq!(
            old1, "5: high-score-env",
            "result[1] old must be '5: high-score-env'; got {old1:?}"
        );
        assert_eq!(
            new1, "1: high-score-env",
            "result[1] new must be '1: high-score-env'; got {new1:?}"
        );
    }

    // ── Unmanaged workspace slot tests ───────────────────────────────────────
    //
    // These tests require `WorkspaceSlot` to have a `managed: bool` field.
    // The `ws!` macro and helpers below will fail to COMPILE until that field
    // is added — the compile error IS the expected "red" state for a data model
    // change under TDD.

    /// Build a `Vec<WorkspaceSlot>` using a compact fixture syntax.
    ///
    /// Usage:
    ///   `ws![ N => unmanaged, N => "name" @ score, … ]`
    ///
    /// - `N => unmanaged`      → slot N, name = N.to_string(), score = 0.0, managed = false
    /// - `N => "name" @ score` → slot N, name = "name",        score,       managed = true
    ///
    /// NOTE: This macro requires `WorkspaceSlot` to have a `managed: bool` field.
    /// It will fail to compile until that field is added.
    macro_rules! ws {
        [ $( $slot:literal => $kind:tt $( @ $score:expr )? ),* $(,)? ] => {
            {
                let mut v: Vec<WorkspaceSlot> = Vec::new();
                $(
                    ws!(@push v, $slot, $kind $(, $score)?);
                )*
                v
            }
        };

        // unmanaged arm
        (@push $v:expr, $slot:expr, unmanaged) => {
            $v.push(WorkspaceSlot {
                slot: $slot,
                name: $slot.to_string(),
                score: 0.0,
                managed: false,
            });
        };

        // managed arm: "name" @ score
        (@push $v:expr, $slot:expr, $name:literal, $score:expr) => {
            $v.push(WorkspaceSlot {
                slot: $slot,
                name: $name.to_string(),
                score: $score,
                managed: true,
            });
        };
    }

    /// Apply a list of rename moves to a set of workspace slots (for convergence testing).
    ///
    /// Each move is `(old_name, new_name)` in `"N: env_name"` format.
    /// Finds the slot whose formatted name matches `old_name` and updates its slot number.
    fn apply_moves(
        mut slots: Vec<WorkspaceSlot>,
        moves: &[(String, String)],
    ) -> Vec<WorkspaceSlot> {
        for (old_name, new_name) in moves {
            let new_slot: i32 = new_name.split(": ").next().unwrap().parse().unwrap();
            if let Some(ws) = slots
                .iter_mut()
                .find(|ws| format!("{}: {}", ws.slot, ws.name) == *old_name)
            {
                ws.slot = new_slot;
            }
        }
        slots
    }

    /// Run `find_best_move` repeatedly until it returns an empty vec (fixed point).
    fn run_to_convergence(
        mut slots: Vec<WorkspaceSlot>,
        max_shortcut_slot: i32,
        stability_threshold: f64,
    ) -> Vec<WorkspaceSlot> {
        loop {
            let moves = find_best_move(&slots, max_shortcut_slot, stability_threshold);
            if moves.is_empty() {
                break;
            }
            slots = apply_moves(slots, &moves);
        }
        slots
    }

    /// Assert that `find_best_move` produces exactly the expected moves.
    #[allow(dead_code)]
    fn assert_rebalances_to(
        slots: &[WorkspaceSlot],
        max_shortcut_slot: i32,
        stability_threshold: f64,
        expected: &[(String, String)],
    ) {
        let result = find_best_move(slots, max_shortcut_slot, stability_threshold);
        assert_eq!(result, expected, "find_best_move returned unexpected moves");
    }

    /// Run to convergence, verify it is a fixed point, then check that each named
    /// env ends up at the expected slot.
    #[allow(dead_code)]
    fn assert_converges_to(
        slots: Vec<WorkspaceSlot>,
        max_shortcut_slot: i32,
        stability_threshold: f64,
        expected_slots: &[(i32, &str)],
    ) {
        let final_slots = run_to_convergence(slots, max_shortcut_slot, stability_threshold);

        // Verify it is truly a fixed point: three more rounds must all return empty.
        for _ in 0..3 {
            let moves = find_best_move(&final_slots, max_shortcut_slot, stability_threshold);
            assert!(
                moves.is_empty(),
                "Not at fixed point after convergence: {:?}",
                moves
            );
        }

        // Check each expected (slot, name) pair.
        for (expected_slot, expected_name) in expected_slots {
            let found = final_slots.iter().find(|ws| ws.name == *expected_name);
            assert!(
                found.is_some(),
                "Expected env '{}' not found after convergence",
                expected_name
            );
            assert_eq!(
                found.unwrap().slot,
                *expected_slot,
                "Env '{}' should be at slot {}, but is at slot {}",
                expected_name,
                expected_slot,
                found.unwrap().slot
            );
        }
    }

    /// T1 — Unmanaged slot blocks compaction (THIS IS THE BUG TEST).
    ///
    /// Real-session fixture:
    ///   slot 1: unmanaged  (bare "1" workspace, not managed by enwiro)
    ///   slot 2: "enwiro"   @ 0.60  (managed)
    ///   slot 3: "chezmoi"  @ 0.80  (managed)
    ///   slot 4: "apple"    @ 0.94  (managed)
    ///   slot 5: "banana"   @ 0.71  (managed)
    ///   slot 6: "cherry"   @ 0.55  (managed)
    ///   slot 8: "grape"    @ 0.45  (managed)
    ///
    /// Bug: because `build_workspace_slots` never adds an entry for the unmanaged
    /// workspace, `find_best_move` sees slot 1 as empty and moves a managed env
    /// there, creating two slot-1 workspaces in i3.
    ///
    /// Fix: `WorkspaceSlot` gains `managed: bool`; `find_best_move` treats ANY
    /// occupied slot (managed or not) as non-empty for compaction targets.
    ///
    /// Expected after fix: no move in the returned vec has "1: ..." as its
    /// destination, because slot 1 is occupied by an unmanaged workspace.
    #[test]
    fn test_unmanaged_slot_blocks_compaction() {
        // NOTE: This test requires WorkspaceSlot to have a `managed: bool` field.
        // The ws! macro will fail to compile until that field is added.
        let slots = ws![
            1 => unmanaged,
            2 => "enwiro"  @ 0.60,
            3 => "chezmoi" @ 0.80,
            4 => "apple"   @ 0.94,
            5 => "banana"  @ 0.71,
            6 => "cherry"  @ 0.55,
            8 => "grape"   @ 0.45,
        ];

        let moves = find_best_move(&slots, 9, STABILITY_THRESHOLD);

        // The bug: before the fix, a move targeting "1: ..." appears here.
        // After the fix, slot 1 is treated as occupied, so no move targets it.
        for (_, new_name) in &moves {
            assert!(
                !new_name.starts_with("1: "),
                "find_best_move must NOT target slot 1 (occupied by unmanaged workspace); \
                 got move to {:?}",
                new_name
            );
        }
    }

    /// T2 — Compaction into a truly empty slot fires correctly (ws! macro smoke test).
    ///
    /// All slots 1–3 are genuinely absent from the slice (no WorkspaceSlot at all),
    /// so they are empty. The algorithm must compact the lowest-numbered env that
    /// benefits from moving toward slot 1.
    ///
    ///   slot 4: "apple"  @ 0.94
    ///   slot 5: "banana" @ 0.71
    ///   slot 8: "grape"  @ 0.45
    ///
    /// Slots 1–3 are absent → NB for apple → slot 1 is strongly positive.
    /// Expected: at least one move targets a slot with number < the source slot,
    /// and specifically a move into slot 1 (highest gain).
    #[test]
    fn test_compaction_into_truly_empty_slot() {
        let slots = ws![
            4 => "apple"  @ 0.94,
            5 => "banana" @ 0.71,
            8 => "grape"  @ 0.45,
        ];

        let moves = find_best_move(&slots, 9, STABILITY_THRESHOLD);

        assert!(
            !moves.is_empty(),
            "compaction must fire when slots 1–3 are genuinely empty; got []"
        );

        // At least one move should target slot 1 (highest-gain empty slot).
        let targets_slot_1 = moves
            .iter()
            .any(|(_, new_name)| new_name.starts_with("1: "));
        assert!(
            targets_slot_1,
            "highest-gain compaction should target slot 1; got {:?}",
            moves
        );
    }

    /// T3 — All-managed base fixture converges to a fixed point.
    ///
    /// Same score distribution as T1 but WITHOUT the unmanaged slot — all entries
    /// are managed. Starting layout has a gap (slot 7 empty) and apple buried at slot 4.
    ///
    /// The algorithm guarantees:
    ///   1. A true fixed point is reached.
    ///   2. apple (0.94, highest score, starts at slot 4) compacts to slot 1 — the
    ///      NB for that move (≈ 0.535) is far above STABILITY_THRESHOLD, so it always fires.
    ///   3. All 6 envs compact into contiguous slots 1–6 (slot 7 gap gets filled).
    ///
    /// Note: the algorithm uses STABILITY_THRESHOLD = 0.05 to prevent thrashing between
    /// similarly-scored envs. Pairs like banana (0.71) vs enwiro (0.60) have a score diff
    /// too small to clear the threshold for any adjacent slot pair, so their relative order
    /// is not guaranteed by the algorithm.
    #[test]
    fn test_real_session_fixture_converges() {
        let slots = ws![
            2 => "enwiro"  @ 0.60,
            3 => "chezmoi" @ 0.80,
            4 => "apple"   @ 0.94,
            5 => "banana"  @ 0.71,
            6 => "cherry"  @ 0.55,
            8 => "grape"   @ 0.45,
        ];

        let final_slots = run_to_convergence(slots, 9, STABILITY_THRESHOLD);

        // Must be a true fixed point (3 extra rounds all return empty).
        for _ in 0..3 {
            let moves = find_best_move(&final_slots, 9, STABILITY_THRESHOLD);
            assert!(
                moves.is_empty(),
                "Not at fixed point after convergence: {:?}",
                moves
            );
        }

        // apple (0.94) has the largest compaction NB and must end up at slot 1.
        let apple_slot = final_slots
            .iter()
            .find(|ws| ws.name == "apple")
            .unwrap()
            .slot;
        assert_eq!(
            apple_slot, 1,
            "apple (0.94) must compact to slot 1 (highest NB), but ended at slot {}",
            apple_slot
        );

        // All 6 managed envs must have compacted into contiguous slots 1–6 (no gap at slot 7).
        let mut occupied_slots: Vec<i32> = final_slots.iter().map(|ws| ws.slot).collect();
        occupied_slots.sort();
        assert_eq!(
            occupied_slots,
            vec![1, 2, 3, 4, 5, 6],
            "All 6 envs must compact into contiguous slots 1–6; got {:?}",
            occupied_slots
        );
    }

    /// T5 — Activate: new env placed at free_num must reach a shortcut slot via
    /// convergence with no stability threshold.
    ///
    /// Two bugs compound when a new env lands at free_num > max_shortcut_slot:
    ///
    ///   Bug A (single call): the Activate path calls `find_best_move` once. When a
    ///   higher-NB swap between two existing envs fires first, the new env is not
    ///   involved and stays at free_num.
    ///
    ///   Bug B (threshold): `STABILITY_THRESHOLD` makes score-swaps from slot > 9 into
    ///   slot ≤ 9 impossible when the slots are close together in disc space.
    ///
    /// Fix: Activate must call `find_best_move(..., 0.0)` (no threshold — explicit user
    /// intent, no thrash risk) and loop to convergence.
    ///
    /// Concrete scenario (all slots 1–9 occupied):
    ///   slot 9: chezmoi @ 0.95 — high score, badly placed (drives a big first-round swap)
    ///   slot 1: apple   @ 0.10 — low score  → chezmoi↔apple NB ≈ 0.544 (wins round 1)
    ///   slot 10: kiwi   @ 0.80 — new env    → kiwi↔enwiro  NB ≈ 0.068 (wins round 2)
    ///
    /// After full convergence with threshold=0.0, kiwi reaches slot ≤ 9. ✓
    ///
    /// NOTE: Calls `find_best_move` with a third `stability_threshold: f64` argument
    /// that does not exist yet — will fail to COMPILE until the parameter is added.
    #[test]
    fn test_new_env_gets_shortcut_slot_after_activate() {
        // All shortcut slots 1–9 occupied. "kiwi" is the new env placed at free_num = 10.
        let mut slots = ws![
             1 => "apple"   @ 0.10,
             2 => "enwiro"  @ 0.60,
             3 => "banana"  @ 0.75,
             4 => "cherry"  @ 0.55,
             5 => "grape"   @ 0.50,
             6 => "fig"     @ 0.48,
             7 => "date"    @ 0.45,
             8 => "lime"    @ 0.42,
             9 => "chezmoi" @ 0.95,
            10 => "kiwi"    @ 0.80,
        ];

        // Precondition: first find_best_move (with threshold) does NOT move kiwi.
        // chezmoi↔apple NB ≈ 0.544 > any kiwi-involving move.
        let first_moves = find_best_move(&slots, 9, STABILITY_THRESHOLD);
        assert!(
            !first_moves
                .iter()
                .any(|(_, n)| n.split_once(": ").map(|(_, e)| e) == Some("kiwi")),
            "precondition: chezmoi↔apple should fire first, not kiwi; got {:?}",
            first_moves
        );

        // Simulate the fixed Activate path: loop find_best_move with threshold=0.0.
        loop {
            let moves = find_best_move(&slots, 9, 0.0);
            if moves.is_empty() {
                break;
            }
            slots = apply_moves(slots, &moves);
        }
        let kiwi_slot = slots.iter().find(|ws| ws.name == "kiwi").unwrap().slot;

        assert!(
            kiwi_slot <= 9,
            "kiwi should be at a shortcut slot (≤ 9) after activate convergence, \
             but stayed at slot {}",
            kiwi_slot
        );
    }

    /// T6 — Cross-boundary swap (slot > max_shortcut_slot → slot ≤ max_shortcut_slot)
    /// fires with `stability_threshold=0.0` but is blocked by `STABILITY_THRESHOLD`.
    ///
    /// Models the real production scenario (observed in logs 2026-04-16):
    ///   - slots 1, 2, 4, 9, 10 occupied by unmanaged bare workspaces
    ///   - slots 3, 5, 6, 7, 8 occupied by managed envs (all 9 shortcut slots full)
    ///   - "chezmoi" (score 0.910) just activated → lands at free_num = 11
    ///   - listen loop says "no rebalance needed" forever: disc(6)−disc(11)=0.078,
    ///     so NB = 0.129×0.078 − 0.05 = −0.040 < 0 (threshold blocks the swap)
    ///   - but with threshold=0.0: NB = 0.129×0.078 = 0.010 > 0 → swap fires ✓
    ///
    /// NOTE: Calls `find_best_move` with a third `stability_threshold: f64` argument
    /// that does not exist yet — will fail to COMPILE until the parameter is added.
    #[test]
    fn test_cross_boundary_swap_fires_without_stability_threshold() {
        let slots = ws![
             1 => unmanaged,
             2 => unmanaged,
             3 => "enwiro"     @ 0.955,
             4 => unmanaged,
             5 => "costae"     @ 0.942,
             6 => "blogtato"   @ 0.781,
             7 => "board-game" @ 0.800,
             8 => "newsboat"   @ 0.665,
             9 => unmanaged,
            10 => unmanaged,
            11 => "chezmoi"    @ 0.910,
        ];

        // With STABILITY_THRESHOLD (listen-loop behaviour): no move fires.
        // Best cross-boundary NB = 0.129×0.078 − 0.05 = −0.040 < 0.
        let listen_moves = find_best_move(&slots, 9, STABILITY_THRESHOLD);
        assert!(
            listen_moves.is_empty(),
            "listen loop must NOT rebalance when threshold blocks cross-boundary swap; \
             got {:?}",
            listen_moves
        );

        // With threshold=0.0 (Activate behaviour): chezmoi↔blogtato fires.
        // NB = (0.910−0.781) × (disc(6)−disc(11)) = 0.129 × 0.078 = 0.010 > 0.
        let activate_moves = find_best_move(&slots, 9, 0.0);
        assert!(
            !activate_moves.is_empty(),
            "activate path must fire cross-boundary swap with threshold=0.0; got []"
        );
        let chezmoi_slot = activate_moves.iter().find_map(|(_, n)| {
            let (s, e) = n.split_once(": ")?;
            if e == "chezmoi" {
                s.parse::<i32>().ok()
            } else {
                None
            }
        });
        assert!(
            chezmoi_slot.is_some_and(|s| s <= 9),
            "chezmoi must move to a shortcut slot (≤ 9); got activate_moves={:?}",
            activate_moves
        );
    }

    /// T7 — Score-zero new env is boosted into the shortcut zone.
    ///
    /// A brand-new env (score 0.0) lands at free_num = 10 while all nine shortcut
    /// slots are occupied by managed envs. Without a score boost, `find_best_move`
    /// would never swap it in (it has the lowest score). The activate path calls
    /// `boost_incoming_score` to raise its effective score to just above the worst
    /// managed shortcut env before the convergence loop runs.
    ///
    /// Expected: after `boost_incoming_score` + loop with threshold=0.0, the new
    /// env lands at a shortcut slot (≤ 9).
    #[test]
    fn test_score_zero_new_env_boosted_into_shortcut_zone() {
        let mut slots = ws![
            1 => "alpha"   @ 0.90,
            2 => "beta"    @ 0.80,
            3 => "gamma"   @ 0.70,
            4 => "delta"   @ 0.65,
            5 => "epsilon" @ 0.60,
            6 => "zeta"    @ 0.55,
            7 => "eta"     @ 0.50,
            8 => "theta"   @ 0.45,
            9 => "iota"    @ 0.40,
           10 => "new-env" @ 0.00,
        ];

        boost_incoming_score(&mut slots, "new-env", 9);

        let new_env_score = slots.iter().find(|ws| ws.name == "new-env").unwrap().score;
        assert!(
            new_env_score > 0.0,
            "score must be boosted above 0.0 before loop; got {new_env_score}"
        );

        loop {
            let moves = find_best_move(&slots, 9, 0.0);
            if moves.is_empty() {
                break;
            }
            slots = apply_moves(slots, &moves);
        }

        let new_env_slot = slots.iter().find(|ws| ws.name == "new-env").unwrap().slot;
        assert!(
            new_env_slot <= 9,
            "new env must reach a shortcut slot after boost + convergence; got slot {new_env_slot}"
        );
    }

    /// T8 — Score boost causes exactly one swap: the worst shortcut env is displaced,
    /// all others stay put (shortcut stability).
    ///
    /// Same fixture as T7. After boost + convergence:
    ///   - "iota" (the lowest-scored shortcut env, 0.40 @ slot 9) is displaced to slot 10.
    ///   - Every other shortcut env remains at its original slot number.
    ///
    /// This is the "one swap" / muscle-memory invariant: activating a new workspace
    /// only ever moves one thing out of the shortcut zone.
    #[test]
    fn test_score_boost_displaces_only_the_worst_shortcut_env() {
        let mut slots = ws![
            1 => "alpha"   @ 0.90,
            2 => "beta"    @ 0.80,
            3 => "gamma"   @ 0.70,
            4 => "delta"   @ 0.65,
            5 => "epsilon" @ 0.60,
            6 => "zeta"    @ 0.55,
            7 => "eta"     @ 0.50,
            8 => "theta"   @ 0.45,
            9 => "iota"    @ 0.40,
           10 => "new-env" @ 0.00,
        ];

        boost_incoming_score(&mut slots, "new-env", 9);

        loop {
            let moves = find_best_move(&slots, 9, 0.0);
            if moves.is_empty() {
                break;
            }
            slots = apply_moves(slots, &moves);
        }

        // iota (lowest score) must have been displaced.
        let iota_slot = slots.iter().find(|ws| ws.name == "iota").unwrap().slot;
        assert!(
            iota_slot > 9,
            "iota (lowest score 0.40) must be displaced to slot > 9; got slot {iota_slot}"
        );

        // All other shortcut envs must be unchanged.
        for (expected_slot, name) in [
            (1, "alpha"),
            (2, "beta"),
            (3, "gamma"),
            (4, "delta"),
            (5, "epsilon"),
            (6, "zeta"),
            (7, "eta"),
            (8, "theta"),
        ] {
            let actual = slots.iter().find(|ws| ws.name == name).unwrap().slot;
            assert_eq!(
                actual, expected_slot,
                "{name} must stay at slot {expected_slot} (shortcut stability); \
                 got slot {actual}"
            );
        }
    }

    /// T9 — When all shortcut slots are unmanaged, boost is skipped and the new
    /// env stays at its free slot.
    ///
    /// `boost_incoming_score` finds no managed env in slots 1–9, so
    /// `min_shortcut_score` is `f64::INFINITY` and the boost is a no-op.
    /// `find_best_move` with threshold=0.0 also finds nothing (no managed swap
    /// partner, all shortcut slots blocked by unmanaged workspaces).
    ///
    /// Expected: score stays 0.0, `find_best_move` returns empty.
    #[test]
    fn test_no_boost_when_all_shortcut_slots_unmanaged() {
        let mut slots = ws![
            1 => unmanaged,  2 => unmanaged,  3 => unmanaged,
            4 => unmanaged,  5 => unmanaged,  6 => unmanaged,
            7 => unmanaged,  8 => unmanaged,  9 => unmanaged,
           10 => "new-env" @ 0.00,
        ];

        boost_incoming_score(&mut slots, "new-env", 9);

        let new_env_score = slots.iter().find(|ws| ws.name == "new-env").unwrap().score;
        assert_eq!(
            new_env_score, 0.0,
            "score must not be boosted when all shortcut slots are unmanaged; \
             got {new_env_score}"
        );

        let moves = find_best_move(&slots, 9, 0.0);
        assert!(
            moves.is_empty(),
            "no move must fire when all shortcut slots are unmanaged; got {:?}",
            moves
        );
    }

    /// T4 — No-op when already at a fixed point (all envs in score-descending slot order,
    /// no gaps between them).
    ///
    ///   slot 1: "apple"   @ 0.94
    ///   slot 2: "banana"  @ 0.71
    ///   slot 3: "chezmoi" @ 0.60
    ///   slot 4: "cherry"  @ 0.55
    ///   slot 5: "grape"   @ 0.45
    ///
    /// Every possible score-swap NB < 0 (would move a high-score env to a worse slot).
    /// No gaps exist for compaction.
    /// Expected: `find_best_move` returns an empty vec.
    #[test]
    fn test_no_op_at_fixed_point() {
        let slots = ws![
            1 => "apple"   @ 0.94,
            2 => "banana"  @ 0.71,
            3 => "chezmoi" @ 0.60,
            4 => "cherry"  @ 0.55,
            5 => "grape"   @ 0.45,
        ];

        // Verify numerically that NB < 0 for any swap of adjacent envs.
        // Swapping apple (slot 1, 0.94) ↔ banana (slot 2, 0.71) would put
        // lower-score banana at slot 1 and higher-score apple at slot 2 — never chosen.
        // The algorithm only swaps when score_hi > score_lo AND env at lower slot is
        // lower-scored, which cannot happen in a perfectly sorted layout.
        //
        // Additionally, no slot in [1..5] is empty (all occupied), so no compaction target.

        let moves = find_best_move(&slots, 9, STABILITY_THRESHOLD);
        assert!(
            moves.is_empty(),
            "already-at-fixed-point layout must return [], got {:?}",
            moves
        );
    }
}
