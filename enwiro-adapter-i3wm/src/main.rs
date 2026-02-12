use anyhow::Context;
use clap::Parser;
use i3ipc_types::reply::Workspace;
use tokio_i3ipc::I3;

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

async fn run_i3_command(i3: &mut I3, command: String) -> anyhow::Result<()> {
    let outcomes = i3.run_command(command).await?;
    if let Some(outcome) = outcomes.first()
        && !outcome.success
    {
        let msg = outcome.error.as_deref().unwrap_or("unknown error");
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
    let args = EnwiroAdapterI3WmCLI::parse();

    match args {
        EnwiroAdapterI3WmCLI::GetActiveWorkspaceId(_) => {
            let mut i3 = I3::connect().await?;
            let workspaces = i3.get_workspaces().await?;
            let focused_workspace = workspaces
                .into_iter()
                .find(|workspace| workspace.focused)
                .context("No active workspace. This should never happen.")?;
            let environment_name = extract_environment_name(&focused_workspace);
            print!("{}", environment_name);
        }
        EnwiroAdapterI3WmCLI::Activate(args) => {
            let mut i3 = I3::connect().await?;
            let workspaces = i3.get_workspaces().await?;

            // Check if a workspace with this environment name already exists
            if let Some(existing) = workspaces
                .iter()
                .find(|ws| extract_environment_name(ws) == args.name)
            {
                run_i3_command(&mut i3, build_workspace_command(&existing.name)).await?;
            } else {
                // Find the lowest unused workspace number
                let used_numbers: std::collections::HashSet<i32> =
                    workspaces.iter().map(|ws| ws.num).collect();
                let mut free_num = 1;
                while used_numbers.contains(&free_num) {
                    free_num += 1;
                }

                let workspace_name = format!("{}: {}", free_num, args.name);
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
