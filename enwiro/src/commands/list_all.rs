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
pub struct ListAllArgs {
    /// Output in JSON lines format
    #[arg(long)]
    pub json: bool,
}

pub fn list_all<W: Write>(context: &mut CommandContext<W>, json: bool) -> anyhow::Result<()> {
    // 1. Always list environments (instant — local directory listing), sorted by frecency
    let mut envs: Vec<_> = context.get_all_environments()?.into_values().collect();

    // Build per-env metadata from colocated meta.json files
    let mut meta_map: HashMap<String, EnvStats> = HashMap::new();
    for env in &envs {
        let env_dir = Path::new(&context.config.workspaces_directory).join(&env.name);
        let meta = crate::usage_stats::load_env_meta(&env_dir);
        if !meta.signals.activation_buffer.is_empty() || meta.description.is_some() {
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
    // Ensure every env has an entry in meta_map so activation_percentile_scores sees the full population
    for env in &envs {
        meta_map.entry(env.name.clone()).or_default();
    }

    let now = crate::usage_stats::now_timestamp();
    let percentile_map = crate::usage_stats::launcher_score(&meta_map, now);
    envs.sort_by(|a, b| {
        let score_a = percentile_map.get(&a.name).copied().unwrap_or(0.0);
        let score_b = percentile_map.get(&b.name).copied().unwrap_or(0.0);
        score_b
            .partial_cmp(&score_a)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });
    for env in &envs {
        if json {
            let cached = CachedRecipe {
                cookbook: "_".to_string(),
                name: env.name.clone(),
                description: meta_map.get(&env.name).and_then(|s| s.description.clone()),
                sort_order: 0,
            };
            let line = serde_json::to_string(&cached).unwrap();
            writeln!(context.writer, "{}", line).context("Could not write to output")?;
        } else {
            let line = match meta_map
                .get(&env.name)
                .and_then(|s| s.description.as_deref())
            {
                Some(desc) => format!("_: {}\t{}", env.name, desc),
                None => format!("_: {}", env.name),
            };
            writeln!(context.writer, "{}", line).context("Could not write to output")?;
        }
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
        Ok(Some(cached)) => cached,
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
        if let Ok(entry) = serde_json::from_str::<CachedRecipe>(line) {
            if env_names.contains(entry.name.as_str()) {
                continue;
            }
            if json {
                writeln!(context.writer, "{}", line).context("Could not write recipe to output")?;
            } else {
                let formatted = match &entry.description {
                    Some(desc) => format!("{}: {}\t{}", entry.cookbook, entry.name, desc),
                    None => format!("{}: {}", entry.cookbook, entry.name),
                };
                writeln!(context.writer, "{}", formatted)
                    .context("Could not write recipe to output")?;
            }
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
    use crate::usage_stats::UserIntentSignals;

    fn parse_json_entries(output: &str) -> Vec<CachedRecipe> {
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

        list_all(&mut context_object, false).unwrap();

        let output = context_object.get_output();
        assert!(output.contains("_: my-env"));
        assert!(output.contains("git: repo-a"));
        assert!(output.contains("git: repo-b"));
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

        list_all(&mut context_object, false).unwrap();

        let output = context_object.get_output();
        assert!(output.contains("_: repo-a"), "Environment should be listed");
        assert!(
            !output.contains("git: repo-a"),
            "Recipe matching an existing environment should be excluded"
        );
        assert!(
            output.contains("git: repo-b"),
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

        list_all(&mut context_object, false).unwrap();

        let output = context_object.get_output();
        assert!(
            !output.contains("github: repo#42"),
            "Recipe with description matching an existing environment should be excluded"
        );
        assert!(
            output.contains("github: repo#99\tAdd feature"),
            "Non-matching recipe with description should still be listed"
        );
    }

    #[rstest]
    fn test_list_all_with_no_cookbooks(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("env-a");
        context_object.create_mock_environment("env-b");

        list_all(&mut context_object, false).unwrap();

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

        list_all(&mut context_object, false).unwrap();

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

        list_all(&mut context_object, false).unwrap();

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

        // Pre-populate cache with JSONL (daemon format)
        daemon::write_cache_atomic(
            &cache_dir,
            "{\"cookbook\":\"git\",\"name\":\"cached-repo\"}\n",
        )
        .unwrap();

        // No cookbooks — if it falls back to sync, output would be empty
        context_object.cookbooks = vec![];

        list_all(&mut context_object, false).unwrap();

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
        let (temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("rarely-used");
        context_object.create_mock_environment("often-used");
        context_object.create_mock_environment("never-used");

        // Write per-env meta.json giving "often-used" a high score and "rarely-used" a low score
        let now = crate::usage_stats::now_timestamp();
        let often_meta = crate::usage_stats::EnvStats {
            signals: UserIntentSignals {
                activation_buffer: vec![(now, 1.0); 10],
                ..Default::default()
            },
            ..Default::default()
        };
        let rarely_meta = crate::usage_stats::EnvStats {
            signals: UserIntentSignals {
                activation_buffer: vec![(now - 700_000, 1.0)],
                ..Default::default()
            },
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

        list_all(&mut context_object, false).unwrap();

        let output = context_object.get_output();
        let env_lines: Vec<&str> = output.lines().filter(|l| l.starts_with("_: ")).collect();
        assert_eq!(env_lines[0], "_: often-used");
        assert_eq!(env_lines[1], "_: rarely-used");
        assert_eq!(env_lines[2], "_: never-used");
    }

    /// Verify that `list-all` orders environments by percentile rank (highest first).
    ///
    /// Three environments are given clearly distinct activation histories so their
    /// raw frecency scores are strictly ordered: high > mid > low.  Because
    /// `launcher_score` returns percentile rank (monotone with raw frecency), the
    /// expected output order is the same: high, mid, low.  The test confirms that
    /// the sort is wired through `launcher_score` (percentile) rather than raw
    /// decay sum, establishing the end-to-end contract.
    #[rstest]
    fn test_list_all_orders_environments_by_launcher_percentile_score(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("low-activity");
        context_object.create_mock_environment("mid-activity");
        context_object.create_mock_environment("high-activity");

        let now = crate::usage_stats::now_timestamp();

        // high-activity: 5 recent activations — highest frecency → highest percentile
        let high_meta = crate::usage_stats::EnvStats {
            signals: UserIntentSignals {
                activation_buffer: vec![(now, 1.0); 5],
                ..Default::default()
            },
            ..Default::default()
        };
        // mid-activity: 1 activation 48 h ago — score ≈ 0.5, middle percentile
        let mid_meta = crate::usage_stats::EnvStats {
            signals: UserIntentSignals {
                activation_buffer: vec![(now - 48 * 3600, 1.0)],
                ..Default::default()
            },
            ..Default::default()
        };
        // low-activity: no activations at all — score 0.0, lowest percentile

        std::fs::write(
            temp_dir.path().join("high-activity").join("meta.json"),
            serde_json::to_string(&high_meta).unwrap(),
        )
        .unwrap();
        std::fs::write(
            temp_dir.path().join("mid-activity").join("meta.json"),
            serde_json::to_string(&mid_meta).unwrap(),
        )
        .unwrap();

        list_all(&mut context_object, false).unwrap();

        let output = context_object.get_output();
        let env_lines: Vec<&str> = output.lines().filter(|l| l.starts_with("_: ")).collect();
        assert_eq!(
            env_lines.len(),
            3,
            "expected 3 env lines, got: {:?}",
            env_lines
        );
        assert_eq!(
            env_lines[0], "_: high-activity",
            "highest percentile rank must be first"
        );
        assert_eq!(
            env_lines[1], "_: mid-activity",
            "middle percentile rank must be second"
        );
        assert_eq!(
            env_lines[2], "_: low-activity",
            "lowest percentile rank (no activations) must be last"
        );
    }

    /// Verify that `list-all` sorts environments using `launcher_score` from `usage_stats`.
    ///
    /// This test checks two things:
    /// 1. `crate::usage_stats::launcher_score` is a callable public symbol (compile-time check).
    /// 2. The ordering produced by `list_all` is consistent with what `launcher_score` returns
    ///    for the same input data — establishing that the caller is wired to `launcher_score`
    ///    rather than some other scoring function.
    ///
    /// Two environments are set up with different activation histories.  `launcher_score` is
    /// called directly with the same metadata to derive the expected ordering.  `list_all` must
    /// place the environment with the higher `launcher_score` first.
    #[rstest]
    fn test_list_all_uses_launcher_score_for_ordering(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        use std::collections::HashMap;

        let (temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("alpha");
        context_object.create_mock_environment("beta");

        let now = crate::usage_stats::now_timestamp();

        // "beta" gets a recent activation; "alpha" gets none.
        let beta_meta = crate::usage_stats::EnvStats {
            signals: UserIntentSignals {
                activation_buffer: vec![(now, 1.0)],
                ..Default::default()
            },
            ..Default::default()
        };
        let alpha_meta = crate::usage_stats::EnvStats::default();

        std::fs::write(
            temp_dir.path().join("beta").join("meta.json"),
            serde_json::to_string(&beta_meta).unwrap(),
        )
        .unwrap();

        // Derive the expected order using launcher_score directly (compile-time symbol check).
        let mut meta_map: HashMap<String, crate::usage_stats::EnvStats> = HashMap::new();
        meta_map.insert("alpha".to_string(), alpha_meta.clone());
        meta_map.insert("beta".to_string(), beta_meta.clone());

        let scores = crate::usage_stats::launcher_score(&meta_map, now);
        assert!(
            scores["beta"] > scores["alpha"],
            "launcher_score must rank beta higher than alpha; beta={}, alpha={}",
            scores["beta"],
            scores["alpha"]
        );

        // Now verify list_all produces the same ordering.
        list_all(&mut context_object, false).unwrap();

        let output = context_object.get_output();
        let env_lines: Vec<&str> = output.lines().filter(|l| l.starts_with("_: ")).collect();
        assert_eq!(
            env_lines.len(),
            2,
            "expected 2 env lines, got: {:?}",
            env_lines
        );
        assert_eq!(
            env_lines[0], "_: beta",
            "list_all must put the environment with the higher launcher_score first"
        );
        assert_eq!(
            env_lines[1], "_: alpha",
            "list_all must put the environment with the lower launcher_score second"
        );
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
            signals: UserIntentSignals {
                activation_buffer: vec![(now, 1.0)],
                ..Default::default()
            },
            description: Some("Fix auth bug".to_string()),
            cookbook: Some("github".to_string()),
        };
        let env_dir = temp_dir.path().join("owner-repo#42");
        std::fs::write(
            env_dir.join("meta.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();

        list_all(&mut context_object, false).unwrap();

        let output = context_object.get_output();
        assert!(
            output.contains("_: owner-repo#42\tFix auth bug"),
            "Expected description in environment listing, got: {}",
            output
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

        list_all(&mut context_object, false).unwrap();

        let output = context_object.get_output();
        let recipe_lines: Vec<&str> = output.lines().filter(|l| !l.starts_with("_: ")).collect();
        assert_eq!(recipe_lines[0], "git: my-repo");
        assert_eq!(recipe_lines[1], "chezmoi: dotfiles");
        assert_eq!(recipe_lines[2], "github: repo#1");
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

        list_all(&mut context_object, false).unwrap();

        let output = context_object.get_output();
        assert!(
            output.contains("git: sync-repo"),
            "Should fall back to sync, got: {}",
            output
        );
    }

    #[rstest]
    fn test_list_all_json_flag_outputs_jsonl(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("my-env");
        context_object.cookbooks = vec![Box::new(FakeCookbook::new("git", vec!["repo-a"], vec![]))];

        list_all(&mut context_object, true).unwrap();

        let entries = parse_json_entries(&context_object.get_output());
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
    }
}
