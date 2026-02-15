use anyhow::Context;
use std::io::Write;

use crate::context::CommandContext;
use crate::daemon;

#[derive(clap::Args)]
#[command(
    author,
    version,
    about = "list all existing environments as well as recipes to create environments"
)]
pub struct ListAllArgs {}

pub fn list_all<W: Write>(context: &mut CommandContext<W>) -> anyhow::Result<()> {
    // 1. Always list environments (instant — local directory listing), sorted by frecency
    let mut envs: Vec<_> = context.get_all_environments()?.into_values().collect();
    let stats = match &context.stats_path {
        Some(path) => crate::usage_stats::load_stats(path),
        None => crate::usage_stats::load_stats_default(),
    };
    let now = crate::usage_stats::now_timestamp();
    envs.sort_by(|a, b| {
        let score_a = stats
            .envs
            .get(&a.name)
            .map(|s| crate::usage_stats::frecency_score(s, now))
            .unwrap_or(0.0);
        let score_b = stats
            .envs
            .get(&b.name)
            .map(|s| crate::usage_stats::frecency_score(s, now))
            .unwrap_or(0.0);
        score_b
            .partial_cmp(&score_a)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });
    for env in &envs {
        let line = match stats
            .envs
            .get(&env.name)
            .and_then(|s| s.description.as_deref())
        {
            Some(desc) => format!("_: {}\t{}\n", env.name, desc),
            None => format!("_: {}\n", env.name),
        };
        context
            .writer
            .write_all(line.as_bytes())
            .context("Could not write to output")?;
    }

    // 2. Resolve runtime directory (test-injectable via cache_dir)
    let runtime_dir = match &context.cache_dir {
        Some(dir) => dir.clone(),
        None => daemon::runtime_dir()?,
    };

    // 3. Ensure daemon is running (spawns if needed; skip in test mode)
    if context.cache_dir.is_none() {
        match daemon::ensure_daemon_running(&runtime_dir) {
            Ok(true) => {
                tracing::info!("Started background recipe cache daemon");
                context
                    .notifier
                    .notify_success("Recipe cache daemon started");
            }
            Ok(false) => {
                tracing::debug!("Daemon already running");
            }
            Err(e) => {
                tracing::warn!(error = %e, "Could not ensure daemon is running");
            }
        }
    }

    // 4. Read from cache if available, otherwise synchronous fallback
    match daemon::read_cached_recipes(&runtime_dir) {
        Ok(Some(cached)) => {
            let _ = daemon::touch_heartbeat(&runtime_dir);
            context
                .writer
                .write_all(cached.as_bytes())
                .context("Could not write cached recipes to output")?;
        }
        Ok(None) => {
            tracing::debug!("No cache available, falling back to synchronous recipe collection");
            let recipes = daemon::collect_all_recipes(&context.cookbooks);
            context
                .writer
                .write_all(recipes.as_bytes())
                .context("Could not write recipes to output")?;
        }
        Err(e) => {
            tracing::warn!(error = %e, "Could not read cache, falling back to sync");
            let recipes = daemon::collect_all_recipes(&context.cookbooks);
            context
                .writer
                .write_all(recipes.as_bytes())
                .context("Could not write recipes to output")?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    use crate::test_utils::test_utilities::{
        AdapterLog, FakeContext, FakeCookbook, NotificationLog, context_object,
    };

    #[rstest]
    fn test_list_all_shows_environments_and_recipes(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("my-env");
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["repo-a", "repo-b"],
            vec![],
        ))];

        list_all(&mut context_object).unwrap();

        let output = context_object.get_output();
        assert!(output.contains("_: my-env"));
        assert!(output.contains("git: repo-a"));
        assert!(output.contains("git: repo-b"));
    }

    #[rstest]
    fn test_list_all_with_no_cookbooks(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("env-a");
        context_object.create_mock_environment("env-b");

        list_all(&mut context_object).unwrap();

        let output = context_object.get_output();
        assert!(output.contains("_: env-a"));
        assert!(output.contains("_: env-b"));
        assert!(!output.contains("git:"));
    }

    #[rstest]
    fn test_list_all_with_no_environments_but_has_recipes(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["some-repo"],
            vec![],
        ))];

        list_all(&mut context_object).unwrap();

        let output = context_object.get_output();
        assert!(output.contains("git: some-repo"));
        assert!(!output.contains("_:"));
    }

    #[rstest]
    fn test_list_all_with_multiple_cookbooks(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.cookbooks = vec![
            Box::new(FakeCookbook::new("git", vec!["repo-a"], vec![])),
            Box::new(FakeCookbook::new("npm", vec!["pkg-x", "pkg-y"], vec![])),
        ];

        list_all(&mut context_object).unwrap();

        let output = context_object.get_output();
        assert!(output.contains("git: repo-a"));
        assert!(output.contains("npm: pkg-x"));
        assert!(output.contains("npm: pkg-y"));
    }

    #[rstest]
    fn test_list_all_reads_from_cache_when_available(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        let cache_dir = context_object.cache_dir.clone().unwrap();

        // Pre-populate cache
        daemon::write_cache_atomic(&cache_dir, "git: cached-repo\n").unwrap();

        // No cookbooks — if it falls back to sync, output would be empty
        context_object.cookbooks = vec![];

        list_all(&mut context_object).unwrap();

        let output = context_object.get_output();
        assert!(
            output.contains("git: cached-repo"),
            "Should read from cache, got: {}",
            output
        );
    }

    #[rstest]
    fn test_list_all_sorts_environments_by_frecency(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("rarely-used");
        context_object.create_mock_environment("often-used");
        context_object.create_mock_environment("never-used");

        // Write stats giving "often-used" a high score and "rarely-used" a low score
        let stats_path = context_object.stats_path.as_ref().unwrap();
        let now = crate::usage_stats::now_timestamp();
        let stats = crate::usage_stats::UsageStats {
            envs: [
                (
                    "often-used".to_string(),
                    crate::usage_stats::EnvStats {
                        last_activated: now,
                        activation_count: 50,
                        ..Default::default()
                    },
                ),
                (
                    "rarely-used".to_string(),
                    crate::usage_stats::EnvStats {
                        last_activated: now - 700_000,
                        activation_count: 2,
                        ..Default::default()
                    },
                ),
            ]
            .into(),
        };
        std::fs::write(stats_path, serde_json::to_string(&stats).unwrap()).unwrap();

        list_all(&mut context_object).unwrap();

        let output = context_object.get_output();
        let env_lines: Vec<&str> = output.lines().filter(|l| l.starts_with("_: ")).collect();
        assert_eq!(env_lines[0], "_: often-used");
        assert_eq!(env_lines[1], "_: rarely-used");
        assert_eq!(env_lines[2], "_: never-used");
    }

    #[rstest]
    fn test_list_all_shows_description_for_environments(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("owner-repo#42");

        let stats_path = context_object.stats_path.as_ref().unwrap();
        let now = crate::usage_stats::now_timestamp();
        let stats = crate::usage_stats::UsageStats {
            envs: [(
                "owner-repo#42".to_string(),
                crate::usage_stats::EnvStats {
                    last_activated: now,
                    activation_count: 1,
                    description: Some("Fix auth bug".to_string()),
                    cookbook: Some("github".to_string()),
                },
            )]
            .into(),
        };
        std::fs::write(stats_path, serde_json::to_string(&stats).unwrap()).unwrap();

        list_all(&mut context_object).unwrap();

        let output = context_object.get_output();
        assert!(
            output.contains("_: owner-repo#42\tFix auth bug"),
            "Expected description in environment listing, got: {}",
            output
        );
    }

    #[rstest]
    fn test_list_all_falls_back_to_sync_when_no_cache(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["sync-repo"],
            vec![],
        ))];

        list_all(&mut context_object).unwrap();

        let output = context_object.get_output();
        assert!(
            output.contains("git: sync-repo"),
            "Should fall back to sync, got: {}",
            output
        );
    }
}
