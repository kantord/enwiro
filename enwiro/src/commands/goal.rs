use std::io::Write;
use std::path::Path;

use anyhow::Context;
use enwiro_daemon::meta::{GoalDetail, save_env_meta};

use crate::CommandContext;
use crate::usage_stats::load_env_meta;

#[derive(clap::Args)]
#[command(about = "Show, set, or clear the current environment's goal")]
pub struct GoalArgs {
    #[command(subcommand)]
    pub command: Option<GoalCommand>,
}

#[derive(clap::Subcommand)]
pub enum GoalCommand {
    /// Print the current goal (default when no subcommand is given)
    Show {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Set the current environment's goal to free text
    Set { text: String },
    /// Clear the current environment's goal
    Clear,
}

pub fn goal<W: Write>(context: &mut CommandContext<W>, args: GoalArgs) -> anyhow::Result<()> {
    let env_name = context.resolve_environment_name(&None)?;
    let env_dir = Path::new(&context.config.workspaces_directory).join(&env_name);

    match args.command.unwrap_or(GoalCommand::Show { json: false }) {
        GoalCommand::Show { json } => show_goal(context, &env_dir, json),
        GoalCommand::Set { text } => set_goal(context, &env_dir, &text),
        GoalCommand::Clear => clear_goal(context, &env_dir),
    }
}

fn show_goal<W: Write>(
    context: &mut CommandContext<W>,
    env_dir: &Path,
    json: bool,
) -> anyhow::Result<()> {
    let meta = load_env_meta(env_dir);
    if json {
        let value = meta
            .goal
            .map(|g| serde_json::to_value(g).expect("GoalDetail is always serializable"))
            .unwrap_or(serde_json::Value::Null);
        write!(context.writer, "{}", value).context("Could not write to output")?;
    } else {
        match meta.goal {
            Some(g) => writeln!(context.writer, "{}", g.label),
            None => writeln!(context.writer, "no goal set"),
        }
        .context("Could not write to output")?;
    }
    Ok(())
}

fn set_goal<W: Write>(
    context: &mut CommandContext<W>,
    env_dir: &Path,
    text: &str,
) -> anyhow::Result<()> {
    let mut meta = load_env_meta(env_dir);
    meta.goal = Some(GoalDetail {
        kind: "manual".to_string(),
        label: text.to_string(),
        detail: None,
    });
    save_env_meta(env_dir, &meta).context("Could not save goal")?;
    writeln!(context.writer, "Goal set: {}", text).context("Could not write to output")?;
    Ok(())
}

fn clear_goal<W: Write>(context: &mut CommandContext<W>, env_dir: &Path) -> anyhow::Result<()> {
    let mut meta = load_env_meta(env_dir);
    meta.goal = None;
    save_env_meta(env_dir, &meta).context("Could not save goal")?;
    writeln!(context.writer, "Goal cleared").context("Could not write to output")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    use crate::test_utils::test_utilities::{
        AdapterLog, FakeContext, NotificationLog, context_object,
    };

    fn env_dir(context: &FakeContext, env_name: &str) -> std::path::PathBuf {
        Path::new(&context.config.workspaces_directory).join(env_name)
    }

    #[rstest]
    fn show_with_no_goal_prints_placeholder(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context, _, _) = context_object;
        context.create_mock_environment("my-env");
        context.global_env = Some("my-env".to_string());

        goal(
            &mut context,
            GoalArgs {
                command: Some(GoalCommand::Show { json: false }),
            },
        )
        .unwrap();

        assert_eq!(context.get_output().trim(), "no goal set");
    }

    #[rstest]
    fn show_json_with_no_goal_prints_null(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context, _, _) = context_object;
        context.create_mock_environment("my-env");
        context.global_env = Some("my-env".to_string());

        goal(
            &mut context,
            GoalArgs {
                command: Some(GoalCommand::Show { json: true }),
            },
        )
        .unwrap();

        assert_eq!(context.get_output().trim(), "null");
    }

    #[rstest]
    fn set_writes_manual_goal_to_meta(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context, _, _) = context_object;
        context.create_mock_environment("my-env");
        context.global_env = Some("my-env".to_string());
        let dir = env_dir(&context, "my-env");

        goal(
            &mut context,
            GoalArgs {
                command: Some(GoalCommand::Set {
                    text: "Ship the release".to_string(),
                }),
            },
        )
        .unwrap();

        let meta = load_env_meta(&dir);
        assert_eq!(
            meta.goal,
            Some(GoalDetail {
                kind: "manual".to_string(),
                label: "Ship the release".to_string(),
                detail: None,
            })
        );
    }

    #[rstest]
    fn set_overwrites_an_existing_goal(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context, _, _) = context_object;
        context.create_mock_environment("my-env");
        context.global_env = Some("my-env".to_string());
        let dir = env_dir(&context, "my-env");
        let mut meta = load_env_meta(&dir);
        meta.goal = Some(GoalDetail {
            kind: "github_issue".to_string(),
            label: "Fix auth bug".to_string(),
            detail: None,
        });
        save_env_meta(&dir, &meta).unwrap();

        goal(
            &mut context,
            GoalArgs {
                command: Some(GoalCommand::Set {
                    text: "Actually do this instead".to_string(),
                }),
            },
        )
        .unwrap();

        let meta = load_env_meta(&dir);
        assert_eq!(meta.goal.as_ref().map(|g| g.kind.as_str()), Some("manual"));
        assert_eq!(
            meta.goal.as_ref().map(|g| g.label.as_str()),
            Some("Actually do this instead")
        );
    }

    #[rstest]
    fn clear_removes_an_existing_goal(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context, _, _) = context_object;
        context.create_mock_environment("my-env");
        context.global_env = Some("my-env".to_string());
        let dir = env_dir(&context, "my-env");
        let mut meta = load_env_meta(&dir);
        meta.goal = Some(GoalDetail {
            kind: "manual".to_string(),
            label: "Ship it".to_string(),
            detail: None,
        });
        save_env_meta(&dir, &meta).unwrap();

        goal(
            &mut context,
            GoalArgs {
                command: Some(GoalCommand::Clear),
            },
        )
        .unwrap();

        let meta = load_env_meta(&dir);
        assert_eq!(meta.goal, None);
    }

    #[rstest]
    fn clear_with_no_goal_is_a_no_op(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context, _, _) = context_object;
        context.create_mock_environment("my-env");
        context.global_env = Some("my-env".to_string());

        let result = goal(
            &mut context,
            GoalArgs {
                command: Some(GoalCommand::Clear),
            },
        );

        assert!(result.is_ok());
    }

    #[rstest]
    fn no_subcommand_defaults_to_show(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context, _, _) = context_object;
        context.create_mock_environment("my-env");
        context.global_env = Some("my-env".to_string());

        goal(&mut context, GoalArgs { command: None }).unwrap();

        assert_eq!(context.get_output().trim(), "no goal set");
    }
}
