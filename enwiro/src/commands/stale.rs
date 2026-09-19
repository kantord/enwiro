use anyhow::Context;
use std::io::Write;
use std::path::Path;

use crate::commands::ls::{colorize_status, status_label};
use crate::commands::rm::remove_env;
use crate::context::CommandContext;
use crate::environments::Environment;
use crate::usage_stats::EnvStats;
use enwiro_daemon::meta::Status;
use enwiro_sdk::process::ENWIRO_ENV_VAR;

const DEFAULT_STALE_DAYS: i64 = 30;
const SECONDS_PER_DAY: i64 = 86_400;

#[derive(clap::Args)]
#[command(
    author,
    version,
    about = "List environments that have not been used in a while"
)]
pub struct StaleArgs {
    /// Consider an environment stale after this many days without activity
    #[arg(long, default_value_t = DEFAULT_STALE_DAYS)]
    pub days: i64,
    /// Output in JSON lines format
    #[arg(long, conflicts_with = "rm")]
    pub json: bool,
    /// Remove the listed environments whose status is `done` (merged/closed).
    /// Other stale environments (active, waiting, ready, or unknown status)
    /// are left untouched even though they are listed.
    #[arg(long)]
    pub rm: bool,
    /// Skip the confirmation prompt when removing with --rm
    #[arg(short = 'y', long = "yes")]
    pub yes: bool,
}

#[derive(serde::Serialize)]
struct StaleEntry {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<Status>,
    last_used: i64,
    days_idle: i64,
}

/// Most recent timestamp across every recorded usage signal, or `None` if
/// the environment has never recorded one.
fn last_signal_timestamp(meta: &EnvStats) -> Option<i64> {
    meta.signals
        .activation_buffer
        .iter()
        .chain(meta.signals.switch_buffer.iter())
        .chain(meta.signals.prep_buffer.iter())
        .map(|(timestamp, _)| *timestamp)
        .max()
}

/// When an environment has no recorded usage signal, its directory's mtime
/// stands in for "last touched" - close enough to creation time for a
/// never-activated env, and it also picks up direct filesystem edits.
fn directory_mtime(env_dir: &Path) -> Option<i64> {
    let modified = std::fs::metadata(env_dir).ok()?.modified().ok()?;
    let seconds = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    i64::try_from(seconds).ok()
}

pub fn stale<W: Write>(context: &mut CommandContext<W>, args: StaleArgs) -> anyhow::Result<()> {
    let envs: Vec<Environment> = context.get_all_environments()?.into_values().collect();
    let meta_map =
        crate::usage_stats::collect_env_meta_map(&context.config.workspaces_directory, &envs);

    let now = crate::usage_stats::now_timestamp();
    let threshold_seconds = args.days * SECONDS_PER_DAY;

    let mut entries: Vec<StaleEntry> = envs
        .iter()
        .filter_map(|env| {
            let meta = meta_map.get(&env.name)?;
            if matches!(meta.status, Some(Status::Evergreen)) {
                return None;
            }
            let env_dir = Path::new(&context.config.workspaces_directory).join(&env.name);
            let last_used = last_signal_timestamp(meta).or_else(|| directory_mtime(&env_dir))?;
            if now - last_used < threshold_seconds {
                return None;
            }
            Some(StaleEntry {
                name: env.name.clone(),
                status: meta.status.clone(),
                last_used,
                days_idle: (now - last_used) / SECONDS_PER_DAY,
            })
        })
        .collect();

    entries.sort_by_key(|entry| entry.last_used);

    if args.json {
        for entry in &entries {
            let line = serde_json::to_string(entry).unwrap();
            writeln!(context.writer, "{}", line).context("Could not write to output")?;
        }
    } else {
        let name_width = entries.iter().map(|e| e.name.len()).max().unwrap_or(0);
        let status_width = entries
            .iter()
            .map(|e| status_label(e.status.as_ref()).len())
            .max()
            .unwrap_or(0);
        for entry in &entries {
            let label = status_label(entry.status.as_ref());
            let colored = colorize_status(label);
            let status_pad = " ".repeat(status_width.saturating_sub(label.len()));
            writeln!(
                context.writer,
                "{colored}{status_pad}  {:<name_width$}  {} days ago",
                entry.name, entry.days_idle,
            )
            .context("Could not write to output")?;
        }
    }

    if args.rm {
        let workspaces_directory = Path::new(&context.config.workspaces_directory).to_path_buf();
        let active_env = std::env::var(ENWIRO_ENV_VAR).ok();
        for entry in &entries {
            if !matches!(entry.status, Some(Status::Done { .. })) {
                continue;
            }
            if let Err(err) = remove_env(
                &workspaces_directory,
                &entry.name,
                args.yes,
                active_env.as_deref(),
                &mut context.writer,
            ) {
                writeln!(context.writer, "Could not remove '{}': {err}", entry.name)
                    .context("Could not write to output")?;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::test_utilities::{
        AdapterLog, FakeContext, NotificationLog, context_object,
    };
    use enwiro_daemon::meta::UserIntentSignals;
    use rstest::rstest;

    fn write_meta(env_dir: &Path, meta: &EnvStats) {
        std::fs::write(
            env_dir.join("meta.json"),
            serde_json::to_string(meta).unwrap(),
        )
        .unwrap();
    }

    #[rstest]
    fn test_stale_excludes_recently_used_environments(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("fresh-env");

        let now = crate::usage_stats::now_timestamp();
        write_meta(
            &temp_dir.path().join("fresh-env"),
            &EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![(now, 1.0)],
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        stale(
            &mut context_object,
            StaleArgs {
                days: 30,
                json: false,
                rm: false,
                yes: false,
            },
        )
        .unwrap();

        let output = context_object.get_output();
        assert!(output.is_empty(), "expected no stale envs, got: {output}");
    }

    #[rstest]
    fn test_stale_includes_long_unused_environments(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("old-env");

        let now = crate::usage_stats::now_timestamp();
        let sixty_days_ago = now - 60 * SECONDS_PER_DAY;
        write_meta(
            &temp_dir.path().join("old-env"),
            &EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![(sixty_days_ago, 1.0)],
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        stale(
            &mut context_object,
            StaleArgs {
                days: 30,
                json: false,
                rm: false,
                yes: false,
            },
        )
        .unwrap();

        let output = context_object.get_output();
        assert!(output.contains("old-env"));
        assert!(output.contains("60 days ago"));
    }

    #[rstest]
    fn test_stale_respects_custom_threshold(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("ten-days-idle");

        let now = crate::usage_stats::now_timestamp();
        let ten_days_ago = now - 10 * SECONDS_PER_DAY;
        write_meta(
            &temp_dir.path().join("ten-days-idle"),
            &EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![(ten_days_ago, 1.0)],
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        stale(
            &mut context_object,
            StaleArgs {
                days: 30,
                json: false,
                rm: false,
                yes: false,
            },
        )
        .unwrap();
        assert!(context_object.get_output().is_empty());

        stale(
            &mut context_object,
            StaleArgs {
                days: 5,
                json: false,
                rm: false,
                yes: false,
            },
        )
        .unwrap();
        assert!(context_object.get_output().contains("ten-days-idle"));
    }

    #[rstest]
    fn test_stale_excludes_evergreen_environments(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("evergreen-env");

        let now = crate::usage_stats::now_timestamp();
        let year_ago = now - 365 * SECONDS_PER_DAY;
        write_meta(
            &temp_dir.path().join("evergreen-env"),
            &EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![(year_ago, 1.0)],
                    ..Default::default()
                },
                status: Some(Status::Evergreen),
                ..Default::default()
            },
        );

        stale(
            &mut context_object,
            StaleArgs {
                days: 30,
                json: false,
                rm: false,
                yes: false,
            },
        )
        .unwrap();

        let output = context_object.get_output();
        assert!(
            !output.contains("evergreen-env"),
            "evergreen envs must never be listed as stale, got: {output}"
        );
    }

    #[rstest]
    fn test_stale_falls_back_to_directory_mtime_without_signals(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("never-activated");

        let old_time = std::time::UNIX_EPOCH
            + std::time::Duration::from_secs(
                (crate::usage_stats::now_timestamp() - 90 * SECONDS_PER_DAY) as u64,
            );
        let file = std::fs::File::open(temp_dir.path().join("never-activated")).unwrap();
        file.set_modified(old_time).unwrap();

        stale(
            &mut context_object,
            StaleArgs {
                days: 30,
                json: false,
                rm: false,
                yes: false,
            },
        )
        .unwrap();

        let output = context_object.get_output();
        assert!(output.contains("never-activated"), "got: {output}");
    }

    #[rstest]
    fn test_stale_text_shows_status_label(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("waiting-env");

        let now = crate::usage_stats::now_timestamp();
        let sixty_days_ago = now - 60 * SECONDS_PER_DAY;
        write_meta(
            &temp_dir.path().join("waiting-env"),
            &EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![(sixty_days_ago, 1.0)],
                    ..Default::default()
                },
                status: Some(Status::Cooked {
                    phase: Some(enwiro_daemon::meta::CookedPhase::Waiting),
                    detail: None,
                }),
                ..Default::default()
            },
        );

        stale(
            &mut context_object,
            StaleArgs {
                days: 30,
                json: false,
                rm: false,
                yes: false,
            },
        )
        .unwrap();

        let output = context_object.get_output();
        assert!(
            output.contains("waiting") && output.contains("waiting-env"),
            "expected the status label alongside the env name, got: {output}"
        );
    }

    #[rstest]
    fn test_stale_json_output(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("old-env");

        let now = crate::usage_stats::now_timestamp();
        let sixty_days_ago = now - 60 * SECONDS_PER_DAY;
        write_meta(
            &temp_dir.path().join("old-env"),
            &EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![(sixty_days_ago, 1.0)],
                    ..Default::default()
                },
                status: Some(Status::Cooked {
                    phase: Some(enwiro_daemon::meta::CookedPhase::Active),
                    detail: None,
                }),
                ..Default::default()
            },
        );

        stale(
            &mut context_object,
            StaleArgs {
                days: 30,
                json: true,
                rm: false,
                yes: false,
            },
        )
        .unwrap();

        let output = context_object.get_output();
        let entry: serde_json::Value =
            serde_json::from_str(output.lines().next().unwrap()).unwrap();
        assert_eq!(entry["name"], "old-env");
        assert_eq!(entry["days_idle"], 60);
        assert_eq!(entry["status"]["type"], "cooked");
        assert_eq!(entry["status"]["phase"], "active");
    }

    #[rstest]
    fn test_stale_sorts_most_idle_first(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("idle-40");
        context_object.create_mock_environment("idle-100");

        let now = crate::usage_stats::now_timestamp();
        write_meta(
            &temp_dir.path().join("idle-40"),
            &EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![(now - 40 * SECONDS_PER_DAY, 1.0)],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        write_meta(
            &temp_dir.path().join("idle-100"),
            &EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![(now - 100 * SECONDS_PER_DAY, 1.0)],
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        stale(
            &mut context_object,
            StaleArgs {
                days: 30,
                json: false,
                rm: false,
                yes: false,
            },
        )
        .unwrap();

        let output = context_object.get_output();
        let idle_40_pos = output.find("idle-40").unwrap();
        let idle_100_pos = output.find("idle-100").unwrap();
        assert!(
            idle_100_pos < idle_40_pos,
            "most-idle env should be listed first, got: {output}"
        );
    }

    #[rstest]
    fn test_stale_rm_removes_done_environments(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("old-done");

        let now = crate::usage_stats::now_timestamp();
        write_meta(
            &temp_dir.path().join("old-done"),
            &EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![(now - 60 * SECONDS_PER_DAY, 1.0)],
                    ..Default::default()
                },
                status: Some(Status::Done {
                    outcome: Some(enwiro_daemon::meta::DoneOutcome::Completed),
                }),
                ..Default::default()
            },
        );

        stale(
            &mut context_object,
            StaleArgs {
                days: 30,
                json: false,
                rm: true,
                yes: true,
            },
        )
        .unwrap();

        assert!(
            !temp_dir.path().join("old-done").exists(),
            "done + stale env should be removed by --rm"
        );
    }

    #[rstest]
    fn test_stale_rm_leaves_non_done_environments(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("old-active");

        let now = crate::usage_stats::now_timestamp();
        write_meta(
            &temp_dir.path().join("old-active"),
            &EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![(now - 60 * SECONDS_PER_DAY, 1.0)],
                    ..Default::default()
                },
                status: Some(Status::Cooked {
                    phase: Some(enwiro_daemon::meta::CookedPhase::Active),
                    detail: None,
                }),
                ..Default::default()
            },
        );

        stale(
            &mut context_object,
            StaleArgs {
                days: 30,
                json: false,
                rm: true,
                yes: true,
            },
        )
        .unwrap();

        assert!(
            temp_dir.path().join("old-active").exists(),
            "--rm must only remove environments whose status is done, \
             not merely stale ones"
        );
    }

    #[rstest]
    fn test_stale_rm_without_yes_does_not_remove(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("old-done");

        let now = crate::usage_stats::now_timestamp();
        write_meta(
            &temp_dir.path().join("old-done"),
            &EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![(now - 60 * SECONDS_PER_DAY, 1.0)],
                    ..Default::default()
                },
                status: Some(Status::Done { outcome: None }),
                ..Default::default()
            },
        );

        stale(
            &mut context_object,
            StaleArgs {
                days: 30,
                json: false,
                rm: true,
                yes: false,
            },
        )
        .unwrap();

        assert!(
            temp_dir.path().join("old-done").exists(),
            "must not remove without confirmation"
        );
        let output = context_object.get_output();
        assert!(
            output.contains("-y"),
            "should hint at -y when it can't prompt, got: {output}"
        );
    }
}
