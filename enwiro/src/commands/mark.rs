use std::io::Write;

use anyhow::Context;
use enwiro_sdk::rpc::{EnvMarkParams, EnwiroRpcClient};

use crate::CommandContext;

#[derive(clap::Args)]
#[command(about = "Set the status of the current environment")]
pub struct MarkArgs {
    #[arg(value_enum)]
    pub status: MarkStatus,
}

#[derive(clap::ValueEnum, Clone)]
pub enum MarkStatus {
    Ready,
    Active,
    Waiting,
    Done,
    Evergreen,
}

impl MarkStatus {
    fn as_str(&self) -> &'static str {
        match self {
            MarkStatus::Ready => "ready",
            MarkStatus::Active => "active",
            MarkStatus::Waiting => "waiting",
            MarkStatus::Done => "done",
            MarkStatus::Evergreen => "evergreen",
        }
    }
}

pub fn mark<W: Write>(context: &mut CommandContext<W>, args: MarkArgs) -> anyhow::Result<()> {
    let env_name = context.resolve_environment_name(&None)?;
    let status_label = args.status.as_str();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("could not start async runtime")?;

    rt.block_on(async {
        let client = enwiro_sdk::rpc::connect()
            .await
            .context("could not connect to enwiro-daemon")?;
        EnwiroRpcClient::env_mark(
            &client,
            EnvMarkParams {
                env_name: env_name.clone(),
                status: status_label.to_string(),
                source: enwiro_sdk::rpc::MarkSource::User,
            },
        )
        .await
        .map_err(|e| anyhow::anyhow!("daemon error: {e}"))?;
        Ok::<(), anyhow::Error>(())
    })?;

    writeln!(context.writer, "Marked '{}' as {}", env_name, status_label)
        .context("Could not write to output")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::path::Path;

    use enwiro_daemon::meta::{CookedPhase, EventType, Status, load_env_meta};

    use crate::test_utils::test_utilities::{
        AdapterLog, FakeContext, NotificationLog, context_object,
    };
    use rstest::rstest;

    fn run_mark_direct(
        workspaces_dir: &Path,
        env_name: &str,
        status: MarkStatus,
    ) -> (anyhow::Result<()>, String) {
        use enwiro_daemon::meta::{
            EventLogEntry, EventType as ET, StatusSource, load_env_meta, now_utc, save_env_meta,
        };

        let env_dir = workspaces_dir.join(env_name);
        let new_status = match &status {
            MarkStatus::Ready => Status::Cooked {
                phase: None,
                detail: None,
            },
            MarkStatus::Active => Status::Cooked {
                phase: Some(CookedPhase::Active),
                detail: None,
            },
            MarkStatus::Waiting => Status::Cooked {
                phase: Some(CookedPhase::Waiting),
                detail: None,
            },
            MarkStatus::Done => Status::Done { outcome: None },
            MarkStatus::Evergreen => Status::Evergreen,
        };

        let status_label = status.as_str();
        let now = now_utc();
        let mut meta = load_env_meta(&env_dir);
        meta.status = Some(new_status);
        meta.event_log.push(EventLogEntry {
            event_type: ET::StatusChange,
            detail: status_label.to_string(),
            set_by: Some(StatusSource::User),
            started: now,
            ended: Some(now),
        });

        let result = save_env_meta(&env_dir, &meta).map_err(anyhow::Error::from);

        let mut out: Cursor<Vec<u8>> = Cursor::new(vec![]);
        if result.is_ok() {
            let _ = writeln!(out, "Marked '{}' as {}", env_name, status_label);
        }
        let output = String::from_utf8(out.into_inner()).unwrap();
        (result, output)
    }

    #[rstest]
    fn mark_ready_sets_status(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context, _, _) = context_object;
        context.create_mock_environment("my-env");
        let workspaces = Path::new(&context.config.workspaces_directory);

        let (result, output) = run_mark_direct(workspaces, "my-env", MarkStatus::Ready);
        assert!(result.is_ok(), "mark ready must succeed: {:?}", result);
        assert!(output.contains("ready"));

        let meta = load_env_meta(&workspaces.join("my-env"));
        assert!(matches!(
            meta.status,
            Some(Status::Cooked { phase: None, .. })
        ));
    }

    #[rstest]
    fn mark_active_sets_status(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context, _, _) = context_object;
        context.create_mock_environment("my-env");
        let workspaces = Path::new(&context.config.workspaces_directory);

        let (result, _) = run_mark_direct(workspaces, "my-env", MarkStatus::Active);
        assert!(result.is_ok());

        let meta = load_env_meta(&workspaces.join("my-env"));
        assert!(matches!(
            meta.status,
            Some(Status::Cooked {
                phase: Some(CookedPhase::Active),
                ..
            })
        ));
    }

    #[rstest]
    fn mark_waiting_sets_status(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context, _, _) = context_object;
        context.create_mock_environment("my-env");
        let workspaces = Path::new(&context.config.workspaces_directory);

        let (result, _) = run_mark_direct(workspaces, "my-env", MarkStatus::Waiting);
        assert!(result.is_ok());

        let meta = load_env_meta(&workspaces.join("my-env"));
        assert!(matches!(
            meta.status,
            Some(Status::Cooked {
                phase: Some(CookedPhase::Waiting),
                ..
            })
        ));
    }

    #[rstest]
    fn mark_done_sets_status(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context, _, _) = context_object;
        context.create_mock_environment("my-env");
        let workspaces = Path::new(&context.config.workspaces_directory);

        let (result, _) = run_mark_direct(workspaces, "my-env", MarkStatus::Done);
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

        let (result, _) = run_mark_direct(workspaces, "my-env", MarkStatus::Evergreen);
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

        let (result, _) = run_mark_direct(workspaces, "my-env", MarkStatus::Active);
        assert!(result.is_ok());

        let meta = load_env_meta(&workspaces.join("my-env"));
        assert_eq!(meta.event_log.len(), 1);
        assert_eq!(meta.event_log[0].event_type, EventType::StatusChange);
        assert_eq!(meta.event_log[0].detail, "active");
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

        let (result, _) = run_mark_direct(workspaces, "my-env", MarkStatus::Done);
        assert!(result.is_ok());

        let meta = load_env_meta(&env_dir);
        assert_eq!(meta.cookbook.as_deref(), Some("github"));
        assert_eq!(meta.description.as_deref(), Some("Test"));
        assert!(matches!(meta.status, Some(Status::Done { .. })));
    }
}
