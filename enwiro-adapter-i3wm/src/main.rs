use anyhow::Context;
use clap::Parser;
use i3ipc_types::reply::Workspace;
use tokio_i3ipc::I3;

#[derive(serde::Deserialize, Debug, Clone)]
struct ManagedEnvInfo {
    name: String,
    frecency: f64,
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

/// Find the least-frecently-used single-digit workspace that is enwiro-managed,
/// returning its workspace and the frecency score.
fn find_eviction_candidate<'a>(
    workspaces: &'a [Workspace],
    managed_envs: &[ManagedEnvInfo],
) -> Option<&'a Workspace> {
    let frecency_map: std::collections::HashMap<&str, f64> = managed_envs
        .iter()
        .map(|e| (e.name.as_str(), e.frecency))
        .collect();

    workspaces
        .iter()
        .filter(|ws| ws.num >= 1 && ws.num <= 9)
        .filter_map(|ws| {
            let frecency = frecency_map.get(extract_environment_name(ws).as_str())?;
            Some((ws, *frecency))
        })
        .min_by(|(_, fa), (_, fb)| fa.partial_cmp(fb).unwrap_or(std::cmp::Ordering::Equal))
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

                // If the free slot is multi-digit, try to evict the least-frecent
                // enwiro-managed single-digit workspace to make room.
                let target_num = if free_num > 9 {
                    if let Some(victim) = find_eviction_candidate(&workspaces, &managed_envs) {
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

    fn make_managed(name: &str, frecency: f64) -> ManagedEnvInfo {
        ManagedEnvInfo {
            name: name.to_string(),
            frecency,
        }
    }

    #[test]
    fn test_find_eviction_candidate_picks_least_frecent() {
        let workspaces = vec![
            make_workspace(1, 1, "1: high-frecency"),
            make_workspace(2, 2, "2: low-frecency"),
            make_workspace(3, 3, "3: mid-frecency"),
        ];
        let managed = vec![
            make_managed("high-frecency", 100.0),
            make_managed("low-frecency", 1.0),
            make_managed("mid-frecency", 50.0),
        ];
        let candidate = find_eviction_candidate(&workspaces, &managed).unwrap();
        assert_eq!(extract_environment_name(candidate), "low-frecency");
    }

    #[test]
    fn test_find_eviction_candidate_ignores_unmanaged_workspaces() {
        let workspaces = vec![
            make_workspace(1, 1, "1: unmanaged"),
            make_workspace(2, 2, "2: managed"),
        ];
        let managed = vec![make_managed("managed", 0.0)];
        let candidate = find_eviction_candidate(&workspaces, &managed).unwrap();
        assert_eq!(extract_environment_name(candidate), "managed");
    }

    #[test]
    fn test_find_eviction_candidate_ignores_multi_digit_workspaces() {
        let workspaces = vec![
            make_workspace(1, 10, "10: managed-but-multi-digit"),
            make_workspace(2, 5, "5: managed-single-digit"),
        ];
        let managed = vec![
            make_managed("managed-but-multi-digit", 0.0),
            make_managed("managed-single-digit", 999.0),
        ];
        // Should pick workspace 5 even though it has higher frecency —
        // workspace 10 is excluded because it's already multi-digit.
        let candidate = find_eviction_candidate(&workspaces, &managed).unwrap();
        assert_eq!(candidate.num, 5);
    }

    #[test]
    fn test_find_eviction_candidate_returns_none_when_no_managed_single_digit() {
        let workspaces = vec![
            make_workspace(1, 1, "1: unmanaged-a"),
            make_workspace(2, 2, "2: unmanaged-b"),
        ];
        let managed = vec![make_managed("something-else", 0.0)];
        assert!(find_eviction_candidate(&workspaces, &managed).is_none());
    }

    #[test]
    fn test_find_eviction_candidate_returns_none_for_empty_workspaces() {
        let managed = vec![make_managed("some-env", 1.0)];
        assert!(find_eviction_candidate(&[], &managed).is_none());
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
