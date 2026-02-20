use anyhow::Context;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::Path;

use crate::client::CachedRecipe;
use crate::context::CommandContext;
use crate::daemon;
use crate::usage_stats::EnvStats;

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

    // Build per-env metadata from colocated meta.json files
    let mut meta_map: HashMap<String, EnvStats> = HashMap::new();
    for env in &envs {
        let env_dir = Path::new(&context.config.workspaces_directory).join(&env.name);
        let meta = crate::usage_stats::load_env_meta(&env_dir);
        if meta.activation_count > 0 || meta.description.is_some() {
            meta_map.insert(env.name.clone(), meta);
        }
    }
    // Legacy fallback: check centralized stats for envs without per-env metadata
    let legacy_stats = crate::usage_stats::load_stats_default();
    for env in &envs {
        if !meta_map.contains_key(&env.name)
            && let Some(s) = legacy_stats.envs.get(&env.name)
        {
            meta_map.insert(env.name.clone(), s.clone());
        }
    }

    let now = crate::usage_stats::now_timestamp();
    envs.sort_by(|a, b| {
        let score_a = meta_map
            .get(&a.name)
            .map(|s| crate::usage_stats::frecency_score(s, now))
            .unwrap_or(0.0);
        let score_b = meta_map
            .get(&b.name)
            .map(|s| crate::usage_stats::frecency_score(s, now))
            .unwrap_or(0.0);
        score_b
            .partial_cmp(&score_a)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });
    for env in &envs {
        let cached = CachedRecipe {
            cookbook: "_".to_string(),
            name: env.name.clone(),
            description: meta_map.get(&env.name).and_then(|s| s.description.clone()),
        };
        let line = serde_json::to_string(&cached).unwrap();
        writeln!(context.writer, "{}", line).context("Could not write to output")?;
    }

    // Collect environment names to filter out duplicate recipes
    let env_names: HashSet<&str> = envs.iter().map(|e| e.name.as_str()).collect();

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
    let recipes = match daemon::read_cached_recipes(&runtime_dir) {
        Ok(Some(cached)) => {
            let _ = daemon::touch_heartbeat(&runtime_dir);
            cached
        }
        Ok(None) => {
            tracing::debug!("No cache available, falling back to synchronous recipe collection");
            daemon::collect_all_recipes(&context.cookbooks)
        }
        Err(e) => {
            tracing::warn!(error = %e, "Could not read cache, falling back to sync");
            daemon::collect_all_recipes(&context.cookbooks)
        }
    };

    // 5. Write recipes, excluding any that match an existing environment
    for line in recipes.lines() {
        if line.is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<CachedRecipe>(line)
            && env_names.contains(entry.name.as_str())
        {
            continue;
        }
        writeln!(context.writer, "{}", line).context("Could not write recipe to output")?;
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

    fn parse_output_entries(output: &str) -> Vec<CachedRecipe> {
        output
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

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

        let entries = parse_output_entries(&context_object.get_output());
        assert!(
            entries
                .iter()
                .any(|e| e.cookbook == "_" && e.name == "my-env")
        );
        assert!(
            entries
                .iter()
                .any(|e| e.cookbook == "git" && e.name == "repo-a")
        );
        assert!(
            entries
                .iter()
                .any(|e| e.cookbook == "git" && e.name == "repo-b")
        );
    }

    #[rstest]
    fn test_list_all_excludes_recipes_that_match_existing_environments(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("repo-a");
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["repo-a", "repo-b"],
            vec![],
        ))];

        list_all(&mut context_object).unwrap();

        let entries = parse_output_entries(&context_object.get_output());
        assert!(
            entries
                .iter()
                .any(|e| e.cookbook == "_" && e.name == "repo-a"),
            "Environment should be listed"
        );
        assert!(
            !entries
                .iter()
                .any(|e| e.cookbook == "git" && e.name == "repo-a"),
            "Recipe matching an existing environment should be excluded"
        );
        assert!(
            entries
                .iter()
                .any(|e| e.cookbook == "git" && e.name == "repo-b"),
            "Recipe without a matching environment should still be listed"
        );
    }

    #[rstest]
    fn test_list_all_excludes_recipes_with_descriptions_that_match_existing_environments(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("repo#42");
        context_object.cookbooks = vec![Box::new(FakeCookbook::new_with_descriptions(
            "github",
            vec![
                ("repo#42", Some("Fix auth bug")),
                ("repo#99", Some("Add feature")),
            ],
            vec![],
        ))];

        list_all(&mut context_object).unwrap();

        let entries = parse_output_entries(&context_object.get_output());
        assert!(
            !entries
                .iter()
                .any(|e| e.cookbook == "github" && e.name == "repo#42"),
            "Recipe with description matching an existing environment should be excluded"
        );
        let repo99 = entries
            .iter()
            .find(|e| e.cookbook == "github" && e.name == "repo#99")
            .expect("Non-matching recipe should still be listed");
        assert_eq!(repo99.description.as_deref(), Some("Add feature"));
    }

    #[rstest]
    fn test_list_all_with_no_cookbooks(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("env-a");
        context_object.create_mock_environment("env-b");

        list_all(&mut context_object).unwrap();

        let entries = parse_output_entries(&context_object.get_output());
        assert!(
            entries
                .iter()
                .any(|e| e.cookbook == "_" && e.name == "env-a")
        );
        assert!(
            entries
                .iter()
                .any(|e| e.cookbook == "_" && e.name == "env-b")
        );
        assert!(!entries.iter().any(|e| e.cookbook == "git"));
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

        let entries = parse_output_entries(&context_object.get_output());
        assert!(
            entries
                .iter()
                .any(|e| e.cookbook == "git" && e.name == "some-repo")
        );
        assert!(!entries.iter().any(|e| e.cookbook == "_"));
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

        let entries = parse_output_entries(&context_object.get_output());
        assert!(
            entries
                .iter()
                .any(|e| e.cookbook == "git" && e.name == "repo-a")
        );
        assert!(
            entries
                .iter()
                .any(|e| e.cookbook == "npm" && e.name == "pkg-x")
        );
        assert!(
            entries
                .iter()
                .any(|e| e.cookbook == "npm" && e.name == "pkg-y")
        );
    }

    #[rstest]
    fn test_list_all_reads_from_cache_when_available(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        let cache_dir = context_object.cache_dir.clone().unwrap();

        // Pre-populate cache with JSON
        daemon::write_cache_atomic(
            &cache_dir,
            "{\"cookbook\":\"git\",\"name\":\"cached-repo\"}\n",
        )
        .unwrap();

        // No cookbooks — if it falls back to sync, output would be empty
        context_object.cookbooks = vec![];

        list_all(&mut context_object).unwrap();

        let entries = parse_output_entries(&context_object.get_output());
        assert!(
            entries
                .iter()
                .any(|e| e.cookbook == "git" && e.name == "cached-repo"),
            "Should read from cache, got: {}",
            context_object.get_output()
        );
    }

    #[rstest]
    fn test_list_all_sorts_environments_by_frecency(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("rarely-used");
        context_object.create_mock_environment("often-used");
        context_object.create_mock_environment("never-used");

        // Write per-env meta.json giving "often-used" a high score and "rarely-used" a low score
        let now = crate::usage_stats::now_timestamp();
        let often_meta = crate::usage_stats::EnvStats {
            last_activated: now,
            activation_count: 50,
            ..Default::default()
        };
        let rarely_meta = crate::usage_stats::EnvStats {
            last_activated: now - 700_000,
            activation_count: 2,
            ..Default::default()
        };
        let often_dir = temp_dir.path().join("often-used");
        let rarely_dir = temp_dir.path().join("rarely-used");
        std::fs::write(
            often_dir.join("meta.json"),
            serde_json::to_string(&often_meta).unwrap(),
        )
        .unwrap();
        std::fs::write(
            rarely_dir.join("meta.json"),
            serde_json::to_string(&rarely_meta).unwrap(),
        )
        .unwrap();

        list_all(&mut context_object).unwrap();

        let entries = parse_output_entries(&context_object.get_output());
        let env_entries: Vec<&CachedRecipe> =
            entries.iter().filter(|e| e.cookbook == "_").collect();
        assert_eq!(env_entries[0].name, "often-used");
        assert_eq!(env_entries[1].name, "rarely-used");
        assert_eq!(env_entries[2].name, "never-used");
    }

    #[rstest]
    fn test_list_all_shows_description_for_environments(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("owner-repo#42");

        // Write per-env meta.json with description
        let now = crate::usage_stats::now_timestamp();
        let meta = crate::usage_stats::EnvStats {
            last_activated: now,
            activation_count: 1,
            description: Some("Fix auth bug".to_string()),
            cookbook: Some("github".to_string()),
        };
        let env_dir = temp_dir.path().join("owner-repo#42");
        std::fs::write(
            env_dir.join("meta.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();

        list_all(&mut context_object).unwrap();

        let entries = parse_output_entries(&context_object.get_output());
        let env = entries
            .iter()
            .find(|e| e.cookbook == "_" && e.name == "owner-repo#42")
            .expect("Expected environment in listing");
        assert_eq!(
            env.description.as_deref(),
            Some("Fix auth bug"),
            "Expected description in environment listing"
        );
    }

    #[rstest]
    fn test_list_all_sorts_recipes_by_cookbook_priority(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.cookbooks = vec![
            Box::new(FakeCookbook::new("github", vec!["repo#1"], vec![]).with_priority(30)),
            Box::new(FakeCookbook::new("chezmoi", vec!["dotfiles"], vec![]).with_priority(20)),
            Box::new(FakeCookbook::new("git", vec!["my-repo"], vec![]).with_priority(10)),
        ];

        list_all(&mut context_object).unwrap();

        let entries = parse_output_entries(&context_object.get_output());
        let recipe_entries: Vec<&CachedRecipe> =
            entries.iter().filter(|e| e.cookbook != "_").collect();
        assert_eq!(recipe_entries[0].cookbook, "git");
        assert_eq!(recipe_entries[0].name, "my-repo");
        assert_eq!(recipe_entries[1].cookbook, "chezmoi");
        assert_eq!(recipe_entries[1].name, "dotfiles");
        assert_eq!(recipe_entries[2].cookbook, "github");
        assert_eq!(recipe_entries[2].name, "repo#1");
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

        let entries = parse_output_entries(&context_object.get_output());
        assert!(
            entries
                .iter()
                .any(|e| e.cookbook == "git" && e.name == "sync-repo"),
            "Should fall back to sync, got: {}",
            context_object.get_output()
        );
    }
}
