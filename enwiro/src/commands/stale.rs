use anyhow::{Context, bail};
use std::io::Write;
use std::path::Path;

use crate::commands::rm::{RemoveOutcome, remove_env, write_outcome};
use crate::context::CommandContext;
use crate::environments::Environment;
use crate::status_display::{colorize_status, status_label};
use enwiro_daemon::meta::Status;
use enwiro_sdk::process::ENWIRO_ENV_VAR;

const DEFAULT_STALE_DAYS: i64 = 30;
const SECONDS_PER_DAY: i64 = 86_400;

#[derive(clap::Args)]
#[command(about = "List or remove environments that have not been used in a while")]
pub struct StaleArgs {
    #[command(subcommand)]
    pub command: StaleCommand,
}

#[derive(clap::Subcommand)]
pub enum StaleCommand {
    /// List environments that have not been used in a while
    #[command(
        long_about = "List environments that have not been used in a while.\n\n\
                      Environments with an `evergreen` status are never listed, \
                      regardless of --days - that status exists specifically to \
                      mark environments meant to persist indefinitely."
    )]
    Ls(StaleLsArgs),
    /// Remove done, stale environments and prune their cookbook resources
    #[command(
        long_about = "Remove environments that are both stale and whose status is \
                      `done` (merged/closed). Other stale environments (active, \
                      waiting, ready, or unknown status) are left untouched. Each \
                      removed environment's owning cookbook is given a chance to \
                      clean up whatever it materialized for it (e.g. a git \
                      worktree). An environment whose cookbook cannot fully \
                      clean up (e.g. a worktree with uncommitted changes) is \
                      kept and reported, and the command then exits non-zero."
    )]
    Prune(StalePruneArgs),
}

#[derive(clap::Args)]
pub struct StaleLsArgs {
    /// Consider an environment stale after this many days without activity
    #[arg(long, default_value_t = DEFAULT_STALE_DAYS)]
    pub days: i64,
    /// Output in JSON lines format
    #[arg(long)]
    pub json: bool,
}

#[derive(clap::Args)]
pub struct StalePruneArgs {
    /// Consider an environment stale after this many days without activity
    #[arg(long, default_value_t = DEFAULT_STALE_DAYS)]
    pub days: i64,
    /// Skip the confirmation prompt
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

/// Every non-evergreen environment idle for at least `days`, sorted
/// most-idle first. Shared by `ls` (which shows all of them) and `prune`
/// (which only acts on the `done` subset).
fn compute_stale_entries<W: Write>(
    context: &CommandContext<W>,
    days: i64,
) -> anyhow::Result<Vec<StaleEntry>> {
    if days < 0 {
        bail!("--days must be zero or positive, got {days}");
    }
    let threshold_seconds = days
        .checked_mul(SECONDS_PER_DAY)
        .context("--days value is too large")?;

    let envs: Vec<Environment> = context.get_all_environments()?.into_values().collect();
    let meta_map =
        crate::usage_stats::collect_env_meta_map(&context.config.workspaces_directory, &envs);
    let now = crate::usage_stats::now_timestamp();

    let mut entries: Vec<StaleEntry> = envs
        .iter()
        .filter_map(|env| {
            let meta = meta_map.get(&env.name)?;
            if matches!(meta.status, Some(Status::Evergreen)) {
                return None;
            }
            let last_used = crate::usage_stats::last_touched(
                &context.config.workspaces_directory,
                &env.name,
                meta,
            )?;
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
    Ok(entries)
}

pub fn stale<W: Write>(context: &mut CommandContext<W>, args: StaleArgs) -> anyhow::Result<()> {
    match args.command {
        StaleCommand::Ls(args) => stale_ls(context, args),
        StaleCommand::Prune(args) => stale_prune(context, args),
    }
}

fn stale_ls<W: Write>(context: &mut CommandContext<W>, args: StaleLsArgs) -> anyhow::Result<()> {
    let entries = compute_stale_entries(context, args.days)?;

    if args.json {
        for entry in &entries {
            let line = serde_json::to_string(&entry).unwrap();
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

    Ok(())
}

fn stale_prune<W: Write>(
    context: &mut CommandContext<W>,
    args: StalePruneArgs,
) -> anyhow::Result<()> {
    let entries = compute_stale_entries(context, args.days)?;
    let done_names: Vec<&str> = entries
        .iter()
        .filter(|e| matches!(e.status, Some(Status::Done { .. })))
        .map(|e| e.name.as_str())
        .collect();

    if done_names.is_empty() {
        if entries.is_empty() {
            writeln!(
                context.writer,
                "No stale environments found (nothing idle for {} days).",
                args.days
            )
            .context("Could not write to output")?;
        } else {
            writeln!(
                context.writer,
                "{} stale environment(s) found, but none are `done` - nothing to prune. \
                 Run `enw stale ls` to see them.",
                entries.len()
            )
            .context("Could not write to output")?;
        }
        return Ok(());
    }

    if !args.yes {
        writeln!(
            context.writer,
            "The following {} done environment(s) will be removed:",
            done_names.len()
        )
        .context("Could not write to output")?;
        for name in &done_names {
            writeln!(context.writer, "  {name}").context("Could not write to output")?;
        }
        match crate::confirm::confirm("Remove them?") {
            Ok(true) => {}
            Ok(false) => {
                writeln!(context.writer, "Aborted.").context("Could not write to output")?;
                return Ok(());
            }
            Err(err) => {
                writeln!(context.writer, "{err}").context("Could not write to output")?;
                return Ok(());
            }
        }
    }

    let workspaces_directory = Path::new(&context.config.workspaces_directory).to_path_buf();
    let active_env = std::env::var(ENWIRO_ENV_VAR).ok();
    let mut tally = PruneTally::default();
    for name in done_names {
        if active_env.as_deref() == Some(name) {
            writeln!(context.writer, "- {name}  skipped (active)")
                .context("Could not write to output")?;
            tally.skipped += 1;
            continue;
        }
        // The confirmation above already covered the whole batch, so
        // `remove_env` is told `yes: true` here to skip its own per-item
        // prompt.
        match remove_env(
            &workspaces_directory,
            name,
            true,
            active_env.as_deref(),
            &context.cookbooks,
        ) {
            Ok(outcome) => {
                write_outcome(&mut context.writer, name, &outcome)?;
                tally.record(&outcome);
            }
            Err(err) => {
                // The env could not be removed at all, so it is still there.
                writeln!(context.writer, "✗ {name}  failed: {err:#}")
                    .context("Could not write to output")?;
                tally.kept += 1;
            }
        }
    }

    writeln!(
        context.writer,
        "Removed {}, kept {}, warnings {}, skipped {}.",
        tally.removed, tally.kept, tally.warnings, tally.skipped
    )
    .context("Could not write to output")?;
    context.exit_failure |= tally.kept > 0 || tally.warnings > 0;

    Ok(())
}

/// What happened to each of the environments `stale prune` went through,
/// for the closing summary line and the exit status.
#[derive(Default)]
struct PruneTally {
    removed: usize,
    /// Left in place: the cookbook kept its resource, or removal failed.
    kept: usize,
    /// Removed, but the cookbook's cleanup failed.
    warnings: usize,
    skipped: usize,
}

impl PruneTally {
    fn record(&mut self, outcome: &RemoveOutcome) {
        match outcome {
            RemoveOutcome::Removed => self.removed += 1,
            RemoveOutcome::Kept { .. } => self.kept += 1,
            RemoveOutcome::CleanupFailed { .. } => self.warnings += 1,
            // Unreachable here (`yes: true` never prompts); counted so a
            // future caller that prompts can't silently drop an env.
            RemoveOutcome::Aborted => self.skipped += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::test_utilities::{
        AdapterLog, FakeContext, FakeCookbook, NotificationLog, context_object,
    };
    use crate::usage_stats::EnvStats;
    use enwiro_daemon::meta::UserIntentSignals;
    use enwiro_sdk::cookbook::PruneOutcome;
    use rstest::rstest;

    fn write_meta(env_dir: &Path, meta: &EnvStats) {
        std::fs::write(
            env_dir.join("meta.json"),
            serde_json::to_string(meta).unwrap(),
        )
        .unwrap();
    }

    fn ls(days: i64, json: bool) -> StaleArgs {
        StaleArgs {
            command: StaleCommand::Ls(StaleLsArgs { days, json }),
        }
    }

    fn prune(days: i64, yes: bool) -> StaleArgs {
        StaleArgs {
            command: StaleCommand::Prune(StalePruneArgs { days, yes }),
        }
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

        stale(&mut context_object, ls(30, false)).unwrap();

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

        stale(&mut context_object, ls(30, false)).unwrap();

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

        stale(&mut context_object, ls(30, false)).unwrap();
        assert!(context_object.get_output().is_empty());

        stale(&mut context_object, ls(5, false)).unwrap();
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

        stale(&mut context_object, ls(30, false)).unwrap();

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

        stale(&mut context_object, ls(30, false)).unwrap();

        let output = context_object.get_output();
        assert!(output.contains("never-activated"), "got: {output}");
    }

    #[rstest]
    fn test_stale_does_not_misjudge_prep_only_environments_as_ancient(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("prep-only");

        let now = crate::usage_stats::now_timestamp();
        write_meta(
            &temp_dir.path().join("prep-only"),
            &EnvStats {
                signals: UserIntentSignals {
                    prep_buffer: vec![(now, 1.0)],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        // Writing meta.json above just bumped the directory's own mtime;
        // set it far in the past so the only way this test can pass is by
        // actually reading prep_buffer, not by coincidentally falling back
        // to a fresh directory mtime.
        let ancient = std::time::UNIX_EPOCH
            + std::time::Duration::from_secs((now - 200 * SECONDS_PER_DAY) as u64);
        std::fs::File::open(temp_dir.path().join("prep-only"))
            .unwrap()
            .set_modified(ancient)
            .unwrap();

        stale(&mut context_object, ls(30, false)).unwrap();

        let output = context_object.get_output();
        assert!(
            !output.contains("prep-only"),
            "an env whose only activity is prep_buffer must be judged by that \
             recent signal, not by an unrelated ancient directory mtime, got: {output}"
        );
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

        stale(&mut context_object, ls(30, false)).unwrap();

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

        stale(&mut context_object, ls(30, true)).unwrap();

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

        stale(&mut context_object, ls(30, false)).unwrap();

        let output = context_object.get_output();
        let idle_40_pos = output.find("idle-40").unwrap();
        let idle_100_pos = output.find("idle-100").unwrap();
        assert!(
            idle_100_pos < idle_40_pos,
            "most-idle env should be listed first, got: {output}"
        );
    }

    #[rstest]
    fn test_stale_prune_removes_done_environments(
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

        stale(&mut context_object, prune(30, true)).unwrap();

        assert!(
            !temp_dir.path().join("old-done").exists(),
            "done + stale env should be removed by prune"
        );
    }

    #[rstest]
    fn test_stale_prune_leaves_non_done_environments(
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

        stale(&mut context_object, prune(30, true)).unwrap();

        assert!(
            temp_dir.path().join("old-active").exists(),
            "prune must only remove environments whose status is done, \
             not merely stale ones"
        );
    }

    #[rstest]
    fn test_stale_prune_reports_when_nothing_is_stale(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;

        stale(&mut context_object, prune(30, true)).unwrap();

        let output = context_object.get_output();
        assert!(
            output.contains("No stale environments found"),
            "prune must say something even when there's nothing stale, got: {output}"
        );
    }

    #[rstest]
    fn test_stale_prune_reports_when_stale_but_none_are_done(
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

        stale(&mut context_object, prune(30, true)).unwrap();

        let output = context_object.get_output();
        assert!(
            output.contains("1 stale environment(s) found") && output.contains("enw stale ls"),
            "prune must explain why nothing was removed, got: {output}"
        );
    }

    #[rstest]
    fn test_stale_prune_without_yes_does_not_remove(
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

        stale(&mut context_object, prune(30, false)).unwrap();

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

    #[rstest]
    fn test_stale_prune_prints_preview_and_confirmation_error(
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

        stale(&mut context_object, prune(30, false)).unwrap();

        let output = context_object.get_output();
        assert!(
            output.contains("will be removed") && output.contains("old-done"),
            "should preview what prune is about to remove before prompting, got: {output}"
        );
    }

    #[rstest]
    fn test_stale_prune_reports_each_removal(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("done-a");
        context_object.create_mock_environment("done-b");

        let now = crate::usage_stats::now_timestamp();
        for name in ["done-a", "done-b"] {
            write_meta(
                &temp_dir.path().join(name),
                &EnvStats {
                    signals: UserIntentSignals {
                        activation_buffer: vec![(now - 60 * SECONDS_PER_DAY, 1.0)],
                        ..Default::default()
                    },
                    status: Some(Status::Done { outcome: None }),
                    ..Default::default()
                },
            );
        }

        stale(&mut context_object, prune(30, true)).unwrap();

        let output = context_object.get_output();
        assert!(
            output.contains("✓ done-a")
                && output.contains("✓ done-b")
                && output.contains("Removed 2, kept 0, warnings 0, skipped 0."),
            "successful removals must be reported, got: {output}"
        );
        assert!(
            !context_object.exit_failure,
            "a clean run must not request a failing exit status"
        );
    }

    #[rstest]
    fn test_stale_prune_reports_a_kept_env_honestly_and_fails_the_exit_status(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("clean");
        context_object.create_mock_environment("stuck");
        context_object.cookbooks.push(Box::new(
            FakeCookbook::new("git", vec![], vec![]).with_prune(PruneOutcome::Kept {
                reason: "uncommitted changes".to_string(),
            }),
        ));

        let now = crate::usage_stats::now_timestamp();
        for (name, cookbook) in [("clean", None), ("stuck", Some("git".to_string()))] {
            write_meta(
                &temp_dir.path().join(name),
                &EnvStats {
                    signals: UserIntentSignals {
                        activation_buffer: vec![(now - 60 * SECONDS_PER_DAY, 1.0)],
                        ..Default::default()
                    },
                    status: Some(Status::Done { outcome: None }),
                    cookbook,
                    recipe: Some(name.to_string()),
                    ..Default::default()
                },
            );
        }

        stale(&mut context_object, prune(30, true)).unwrap();

        assert!(!temp_dir.path().join("clean").exists());
        assert!(
            temp_dir.path().join("stuck").exists(),
            "an env whose cookbook kept its resource must survive so the prune can be retried"
        );
        let output = context_object.get_output();
        assert!(
            output.contains("✓ clean")
                && output.contains("✗ stuck  kept: uncommitted changes")
                && !output.contains("Removed 'stuck'")
                && output.contains("Removed 1, kept 1, warnings 0, skipped 0."),
            "a kept env must never be reported as removed, got: {output}"
        );
        assert!(
            context_object.exit_failure,
            "a kept env must request a failing exit status"
        );
    }

    #[rstest]
    fn test_stale_prune_skips_active_env_with_a_neutral_message(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("active-done");

        let now = crate::usage_stats::now_timestamp();
        write_meta(
            &temp_dir.path().join("active-done"),
            &EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![(now - 60 * SECONDS_PER_DAY, 1.0)],
                    ..Default::default()
                },
                status: Some(Status::Done { outcome: None }),
                ..Default::default()
            },
        );

        let prior = std::env::var(ENWIRO_ENV_VAR).ok();
        // SAFETY: serial within this file; no parallel readers of ENWIRO_ENV.
        unsafe {
            std::env::set_var(ENWIRO_ENV_VAR, "active-done");
        }
        let result = stale(&mut context_object, prune(30, true));
        unsafe {
            match &prior {
                Some(v) => std::env::set_var(ENWIRO_ENV_VAR, v),
                None => std::env::remove_var(ENWIRO_ENV_VAR),
            }
        }
        result.unwrap();

        assert!(
            temp_dir.path().join("active-done").exists(),
            "active env must survive prune"
        );
        let output = context_object.get_output();
        assert!(
            output.contains("- active-done  skipped (active)") && !output.contains("failed"),
            "active-env skip must read as expected behavior, not an error, got: {output}"
        );
        assert!(
            !context_object.exit_failure,
            "skipping the active env is expected, not a failure"
        );
    }

    #[rstest]
    fn test_stale_rejects_negative_days(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;

        let result = stale(&mut context_object, ls(-1, false));

        assert!(result.is_err(), "negative --days must be rejected");
    }

    #[rstest]
    fn test_stale_falls_back_to_symlink_mtime_not_target_mtime_for_legacy_envs(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;
        let workspaces = temp_dir.path();

        let target = workspaces.join("ancient-target");
        std::fs::create_dir(&target).unwrap();
        let ancient_time = std::time::UNIX_EPOCH
            + std::time::Duration::from_secs(
                (crate::usage_stats::now_timestamp() - 200 * SECONDS_PER_DAY) as u64,
            );
        std::fs::File::open(&target)
            .unwrap()
            .set_modified(ancient_time)
            .unwrap();

        // A legacy env is a bare symlink at `workspaces/<name>`, created
        // just now - its own mtime is recent even though its target is
        // ancient.
        std::os::unix::fs::symlink(&target, workspaces.join("legacy-fresh")).unwrap();

        stale(&mut context_object, ls(30, false)).unwrap();

        let output = context_object.get_output();
        assert!(
            !output.contains("legacy-fresh"),
            "a freshly-linked legacy env must not inherit its ancient target's mtime, got: {output}"
        );
    }
}
