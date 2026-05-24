use std::io::Write;
use std::path::Path;

use anyhow::{Context, bail};
use enwiro_daemon::meta::{
    EventLogEntry, EventType, Status, load_env_meta, now_utc, save_env_meta,
};
use enwiro_sdk::process::ENWIRO_ENV_VAR;

use crate::CommandContext;

#[derive(clap::Args)]
#[command(about = "Set the status of the current environment")]
pub struct MarkArgs {
    #[arg(value_enum)]
    pub status: MarkStatus,
}

#[derive(clap::ValueEnum, Clone)]
pub enum MarkStatus {
    Cooked,
    Done,
    Evergreen,
}

pub fn mark<W: Write>(context: &mut CommandContext<W>, args: MarkArgs) -> anyhow::Result<()> {
    let env_name = std::env::var(ENWIRO_ENV_VAR)
        .context("Not inside an enwiro environment (ENWIRO_ENV is not set)")?;
    mark_env(
        Path::new(&context.config.workspaces_directory),
        &env_name,
        args.status,
        &mut context.writer,
    )
}

pub(crate) fn mark_env<W: Write>(
    workspaces_directory: &Path,
    env_name: &str,
    status: MarkStatus,
    writer: &mut W,
) -> anyhow::Result<()> {
    let env_dir = workspaces_directory.join(env_name);
    if !env_dir.is_dir() {
        bail!(
            "Environment directory does not exist: {}",
            env_dir.display()
        );
    }

    let (new_status, status_label) = match status {
        MarkStatus::Cooked => (
            Status::Cooked {
                phase: None,
                detail: None,
            },
            "cooked",
        ),
        MarkStatus::Done => (Status::Done { outcome: None }, "done"),
        MarkStatus::Evergreen => (Status::Evergreen, "evergreen"),
    };

    let now = now_utc();
    let mut meta = load_env_meta(&env_dir);
    meta.status = Some(new_status);
    meta.event_log.push(EventLogEntry {
        event_type: EventType::StatusChange,
        detail: status_label.to_string(),
        started: now,
        ended: Some(now),
    });

    save_env_meta(&env_dir, &meta).context("Could not save environment metadata")?;

    writeln!(writer, "Marked '{}' as {}", env_name, status_label)
        .context("Could not write to output")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use enwiro_daemon::meta::{EventType, load_env_meta};
    use std::io::Cursor;

    use crate::test_utils::test_utilities::{
        AdapterLog, FakeContext, NotificationLog, context_object,
    };
    use rstest::rstest;

    fn run_mark(
        workspaces_dir: &Path,
        env_name: &str,
        status: MarkStatus,
    ) -> (anyhow::Result<()>, String) {
        let mut out: Cursor<Vec<u8>> = Cursor::new(vec![]);
        let result = mark_env(workspaces_dir, env_name, status, &mut out);
        let output = String::from_utf8(out.into_inner()).unwrap();
        (result, output)
    }

    #[rstest]
    fn mark_cooked_sets_status(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context, _, _) = context_object;
        context.create_mock_environment("my-env");
        let workspaces = Path::new(&context.config.workspaces_directory);

        let (result, output) = run_mark(workspaces, "my-env", MarkStatus::Cooked);
        assert!(result.is_ok(), "mark cooked must succeed: {:?}", result);
        assert!(output.contains("cooked"));

        let meta = load_env_meta(&workspaces.join("my-env"));
        assert!(matches!(meta.status, Some(Status::Cooked { .. })));
    }

    #[rstest]
    fn mark_done_sets_status(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context, _, _) = context_object;
        context.create_mock_environment("my-env");
        let workspaces = Path::new(&context.config.workspaces_directory);

        let (result, _) = run_mark(workspaces, "my-env", MarkStatus::Done);
        assert!(result.is_ok());

        let meta = load_env_meta(&workspaces.join("my-env"));
        assert!(matches!(meta.status, Some(Status::Done { .. })));
    }

    #[rstest]
    fn mark_evergreen_sets_status(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context, _, _) = context_object;
        context.create_mock_environment("my-env");
        let workspaces = Path::new(&context.config.workspaces_directory);

        let (result, _) = run_mark(workspaces, "my-env", MarkStatus::Evergreen);
        assert!(result.is_ok());

        let meta = load_env_meta(&workspaces.join("my-env"));
        assert!(matches!(meta.status, Some(Status::Evergreen)));
    }

    #[rstest]
    fn mark_appends_event_log_entry(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context, _, _) = context_object;
        context.create_mock_environment("my-env");
        let workspaces = Path::new(&context.config.workspaces_directory);

        let (result, _) = run_mark(workspaces, "my-env", MarkStatus::Cooked);
        assert!(result.is_ok());

        let meta = load_env_meta(&workspaces.join("my-env"));
        assert_eq!(meta.event_log.len(), 1);
        assert_eq!(meta.event_log[0].event_type, EventType::StatusChange);
        assert_eq!(meta.event_log[0].detail, "cooked");
        assert!(meta.event_log[0].ended.is_some());
    }

    #[rstest]
    fn mark_preserves_existing_metadata(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context, _, _) = context_object;
        context.create_mock_environment("my-env");
        let workspaces = Path::new(&context.config.workspaces_directory);
        let env_dir = workspaces.join("my-env");

        enwiro_daemon::meta::record_cook_metadata_per_env(
            &env_dir,
            "github",
            "owner/repo#1",
            Some("Test"),
        );

        let (result, _) = run_mark(workspaces, "my-env", MarkStatus::Done);
        assert!(result.is_ok());

        let meta = load_env_meta(&env_dir);
        assert_eq!(meta.cookbook.as_deref(), Some("github"));
        assert_eq!(meta.description.as_deref(), Some("Test"));
        assert!(matches!(meta.status, Some(Status::Done { .. })));
    }

    #[rstest]
    fn mark_errors_for_nonexistent_env(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, context, _, _) = context_object;
        let workspaces = Path::new(&context.config.workspaces_directory);

        let (result, _) = run_mark(workspaces, "nonexistent", MarkStatus::Cooked);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("does not exist"));
    }
}
