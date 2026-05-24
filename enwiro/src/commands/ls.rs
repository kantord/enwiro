use anyhow::{Context, anyhow};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::Path;

use crate::context::CommandContext;
use crate::environments::Environment;
use crate::usage_stats::EnvStats;
use enwiro_sdk::client::{CachedRecipe, EnvScores};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Scope {
    All,
    Envs,
    Recipes,
}

#[derive(clap::Args)]
#[command(
    author,
    version,
    about = "list existing environments and/or available recipes"
)]
pub struct LsArgs {
    /// Show both environments and recipes (default)
    #[arg(long, group = "scope")]
    pub all: bool,
    /// Show only existing environments (does not require the daemon)
    #[arg(long, group = "scope")]
    pub envs: bool,
    /// Show only available recipes (requires the daemon cache)
    #[arg(long, group = "scope")]
    pub recipes: bool,
    /// Output in JSON lines format
    #[arg(long)]
    pub json: bool,
}

impl LsArgs {
    pub fn scope(&self) -> Scope {
        if self.envs {
            Scope::Envs
        } else if self.recipes {
            Scope::Recipes
        } else {
            Scope::All
        }
    }
}

pub fn ls<W: Write>(
    context: &mut CommandContext<W>,
    scope: Scope,
    json: bool,
) -> anyhow::Result<()> {
    let env_names = match scope {
        Scope::All | Scope::Envs => write_envs(context, json)?,
        Scope::Recipes => collect_env_names(context)?,
    };
    if scope == Scope::Envs {
        return Ok(());
    }
    write_recipes(context, json, &env_names)
}

fn collect_env_names<W: Write>(context: &CommandContext<W>) -> anyhow::Result<HashSet<String>> {
    Ok(context
        .get_all_environments()?
        .into_values()
        .map(|e| e.name)
        .collect())
}

fn write_envs<W: Write>(
    context: &mut CommandContext<W>,
    json: bool,
) -> anyhow::Result<HashSet<String>> {
    let mut envs: Vec<Environment> = context.get_all_environments()?.into_values().collect();

    let mut meta_map: HashMap<String, EnvStats> = HashMap::new();
    for env in &envs {
        let env_dir = Path::new(&context.config.workspaces_directory).join(&env.name);
        let meta = crate::usage_stats::load_env_meta(&env_dir);
        if !meta.signals.activation_buffer.is_empty() || meta.description.is_some() {
            meta_map.insert(env.name.clone(), meta);
        }
    }
    let legacy_stats = crate::usage_stats::load_stats_default();
    for env in &envs {
        if !meta_map.contains_key(&env.name)
            && let Some(s) = legacy_stats.envs.get(&env.name)
        {
            meta_map.insert(env.name.clone(), s.clone());
        }
    }
    for env in &envs {
        meta_map.entry(env.name.clone()).or_default();
    }

    let now = crate::usage_stats::now_timestamp();
    let percentile_map = crate::usage_stats::launcher_score(&meta_map, now);
    let slot_map = crate::usage_stats::slot_scores(&meta_map, now);
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
            let launcher = percentile_map.get(&env.name).copied().unwrap_or(0.0);
            let slot = slot_map.get(&env.name).copied().unwrap_or(0.0);
            let cached = CachedRecipe {
                cookbook: "_".to_string(),
                name: env.name.clone(),
                description: meta_map.get(&env.name).and_then(|s| s.description.clone()),
                sort_order: 0,
                scores: Some(EnvScores { launcher, slot }),
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

    Ok(envs.into_iter().map(|e| e.name).collect())
}

fn write_recipes<W: Write>(
    context: &mut CommandContext<W>,
    json: bool,
    env_names: &HashSet<String>,
) -> anyhow::Result<()> {
    let cache = match &context.cache_dir {
        Some(dir) => enwiro_daemon::DaemonCache::with_runtime_dir(dir.clone()),
        None => enwiro_daemon::DaemonCache::open()?,
    };

    let recipes = cache
        .read_recipes()
        .context("Could not read the daemon cache")?
        .ok_or_else(|| {
            anyhow!(
                "Daemon cache is not available. \
                 Check: systemctl --user status enwiro-daemon.service"
            )
        })?;

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
        AdapterLog, FakeContext, NotificationLog, context_object,
    };
    use enwiro_daemon::meta::UserIntentSignals;

    fn parse_json_entries(output: &str) -> Vec<CachedRecipe> {
        output
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[rstest]
    fn test_ls_shows_environments_and_recipes(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("my-env");
        context_object.write_cache_entries(&[("git", "repo-a", None), ("git", "repo-b", None)]);

        ls(&mut context_object, Scope::All, false).unwrap();

        let output = context_object.get_output();
        assert!(output.contains("_: my-env"));
        assert!(output.contains("git: repo-a"));
        assert!(output.contains("git: repo-b"));
    }

    #[rstest]
    fn test_ls_excludes_recipes_that_match_existing_environments(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("repo-a");
        context_object.write_cache_entries(&[("git", "repo-a", None), ("git", "repo-b", None)]);

        ls(&mut context_object, Scope::All, false).unwrap();

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
    fn test_ls_excludes_recipes_with_descriptions_that_match_existing_environments(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("repo#42");
        context_object.write_cache_entries(&[
            ("github", "repo#42", Some("Fix auth bug")),
            ("github", "repo#99", Some("Add feature")),
        ]);

        ls(&mut context_object, Scope::All, false).unwrap();

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
    fn test_ls_with_no_recipes_in_cache(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("env-a");
        context_object.create_mock_environment("env-b");
        context_object.write_cache_entries(&[]);

        ls(&mut context_object, Scope::All, false).unwrap();

        let output = context_object.get_output();
        assert!(output.contains("_: env-a"));
        assert!(output.contains("_: env-b"));
        assert!(!output.contains("git:"));
    }

    #[rstest]
    fn test_ls_with_no_environments_but_has_recipes(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.write_cache_entries(&[("git", "some-repo", None)]);

        ls(&mut context_object, Scope::All, false).unwrap();

        let output = context_object.get_output();
        assert!(output.contains("git: some-repo"));
        assert!(!output.contains("_:"));
    }

    #[rstest]
    fn test_ls_with_multiple_cookbooks(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.write_cache_entries(&[
            ("git", "repo-a", None),
            ("npm", "pkg-x", None),
            ("npm", "pkg-y", None),
        ]);

        ls(&mut context_object, Scope::All, false).unwrap();

        let output = context_object.get_output();
        assert!(output.contains("git: repo-a"));
        assert!(output.contains("npm: pkg-x"));
        assert!(output.contains("npm: pkg-y"));
    }

    #[rstest]
    fn test_ls_reads_from_cache_when_available(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        let cache_dir = context_object.cache_dir.clone().unwrap();

        std::fs::create_dir_all(&cache_dir).unwrap();
        std::fs::write(
            cache_dir.join("recipes.cache"),
            "{\"cookbook\":\"git\",\"name\":\"cached-repo\"}\n",
        )
        .unwrap();

        context_object.cookbooks = vec![];

        ls(&mut context_object, Scope::All, false).unwrap();

        let output = context_object.get_output();
        assert!(
            output.contains("git: cached-repo"),
            "Should read from cache, got: {}",
            output
        );
    }

    #[rstest]
    fn test_ls_sorts_environments_by_frecency(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("rarely-used");
        context_object.create_mock_environment("often-used");
        context_object.create_mock_environment("never-used");
        context_object.write_cache_entries(&[]);

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

        ls(&mut context_object, Scope::All, false).unwrap();

        let output = context_object.get_output();
        let env_lines: Vec<&str> = output.lines().filter(|l| l.starts_with("_: ")).collect();
        assert_eq!(env_lines[0], "_: often-used");
        assert_eq!(env_lines[1], "_: rarely-used");
        assert_eq!(env_lines[2], "_: never-used");
    }

    /// Verify that `ls` orders environments by percentile rank (highest first).
    #[rstest]
    fn test_ls_orders_environments_by_launcher_percentile_score(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("low-activity");
        context_object.create_mock_environment("mid-activity");
        context_object.create_mock_environment("high-activity");
        context_object.write_cache_entries(&[]);

        let now = crate::usage_stats::now_timestamp();

        let high_meta = crate::usage_stats::EnvStats {
            signals: UserIntentSignals {
                activation_buffer: vec![(now, 1.0); 5],
                ..Default::default()
            },
            ..Default::default()
        };
        let mid_meta = crate::usage_stats::EnvStats {
            signals: UserIntentSignals {
                activation_buffer: vec![(now - 48 * 3600, 1.0)],
                ..Default::default()
            },
            ..Default::default()
        };

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

        ls(&mut context_object, Scope::All, false).unwrap();

        let output = context_object.get_output();
        let env_lines: Vec<&str> = output.lines().filter(|l| l.starts_with("_: ")).collect();
        assert_eq!(env_lines.len(), 3);
        assert_eq!(env_lines[0], "_: high-activity");
        assert_eq!(env_lines[1], "_: mid-activity");
        assert_eq!(env_lines[2], "_: low-activity");
    }

    #[rstest]
    fn test_ls_uses_launcher_score_for_ordering(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("alpha");
        context_object.create_mock_environment("beta");
        context_object.write_cache_entries(&[]);

        let now = crate::usage_stats::now_timestamp();

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

        let mut meta_map: HashMap<String, crate::usage_stats::EnvStats> = HashMap::new();
        meta_map.insert("alpha".to_string(), alpha_meta);
        meta_map.insert("beta".to_string(), beta_meta);

        let scores = crate::usage_stats::launcher_score(&meta_map, now);
        assert!(scores["beta"] > scores["alpha"]);

        ls(&mut context_object, Scope::All, false).unwrap();

        let output = context_object.get_output();
        let env_lines: Vec<&str> = output.lines().filter(|l| l.starts_with("_: ")).collect();
        assert_eq!(env_lines.len(), 2);
        assert_eq!(env_lines[0], "_: beta");
        assert_eq!(env_lines[1], "_: alpha");
    }

    #[rstest]
    fn test_ls_shows_description_for_environments(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("owner-repo#42");
        context_object.write_cache_entries(&[]);

        let now = crate::usage_stats::now_timestamp();
        let meta = crate::usage_stats::EnvStats {
            signals: UserIntentSignals {
                activation_buffer: vec![(now, 1.0)],
                ..Default::default()
            },
            description: Some("Fix auth bug".to_string()),
            cookbook: Some("github".to_string()),
            recipe: Some("owner/repo#42".to_string()),
            ..Default::default()
        };
        let env_dir = temp_dir.path().join("owner-repo#42");
        std::fs::write(
            env_dir.join("meta.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();

        ls(&mut context_object, Scope::All, false).unwrap();

        let output = context_object.get_output();
        assert!(
            output.contains("_: owner-repo#42\tFix auth bug"),
            "Expected description in environment listing, got: {}",
            output
        );
    }

    #[rstest]
    fn test_ls_preserves_cache_order(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.write_cache_entries(&[
            ("git", "my-repo", None),
            ("chezmoi", "dotfiles", None),
            ("github", "repo#1", None),
        ]);

        ls(&mut context_object, Scope::All, false).unwrap();

        let output = context_object.get_output();
        let recipe_lines: Vec<&str> = output.lines().filter(|l| !l.starts_with("_: ")).collect();
        assert_eq!(recipe_lines[0], "git: my-repo");
        assert_eq!(recipe_lines[1], "chezmoi: dotfiles");
        assert_eq!(recipe_lines[2], "github: repo#1");
    }

    #[rstest]
    fn test_ls_errors_when_cache_unavailable(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;

        let result = ls(&mut context_object, Scope::All, false);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("daemon"),
            "Error should point at the daemon, got: {err}"
        );
    }

    #[rstest]
    fn test_ls_json_flag_outputs_jsonl(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("my-env");
        context_object.write_cache_entries(&[("git", "repo-a", None)]);

        ls(&mut context_object, Scope::All, true).unwrap();

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

    #[rstest]
    fn test_ls_json_env_entry_has_scores_object(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("my-env");
        context_object.write_cache_entries(&[]);

        ls(&mut context_object, Scope::All, true).unwrap();

        let output = context_object.get_output();
        let entries: Vec<serde_json::Value> = output
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();

        let env_entry = entries
            .iter()
            .find(|e| e["cookbook"] == "_" && e["name"] == "my-env")
            .expect("expected an entry for my-env with cookbook=_");

        let scores = env_entry
            .get("scores")
            .expect("env entry must have a 'scores' field");

        assert!(scores.get("launcher").is_some());
        assert!(scores["launcher"].is_f64() || scores["launcher"].is_number());

        assert!(scores.get("slot").is_some());
        assert!(scores["slot"].is_f64() || scores["slot"].is_number());
    }

    #[rstest]
    fn test_ls_json_recipe_entry_has_no_scores_field(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.write_cache_entries(&[("git", "repo-a", None)]);

        ls(&mut context_object, Scope::All, true).unwrap();

        let output = context_object.get_output();
        let entries: Vec<serde_json::Value> = output
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();

        let recipe_entry = entries
            .iter()
            .find(|e| e["cookbook"] == "git" && e["name"] == "repo-a")
            .expect("expected a recipe entry for git: repo-a");

        assert!(recipe_entry.get("scores").is_none());
    }

    #[rstest]
    fn test_ls_json_higher_frecency_env_has_higher_scores(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("often-used");
        context_object.create_mock_environment("never-used");
        context_object.write_cache_entries(&[]);

        let now = crate::usage_stats::now_timestamp();
        let often_meta = crate::usage_stats::EnvStats {
            signals: UserIntentSignals {
                activation_buffer: vec![(now, 1.0); 5],
                ..Default::default()
            },
            ..Default::default()
        };
        let often_dir = temp_dir.path().join("often-used");
        std::fs::write(
            often_dir.join("meta.json"),
            serde_json::to_string(&often_meta).unwrap(),
        )
        .unwrap();

        ls(&mut context_object, Scope::All, true).unwrap();

        let output = context_object.get_output();
        let entries: Vec<serde_json::Value> = output
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();

        let often_entry = entries
            .iter()
            .find(|e| e["cookbook"] == "_" && e["name"] == "often-used")
            .expect("expected entry for often-used");
        let never_entry = entries
            .iter()
            .find(|e| e["cookbook"] == "_" && e["name"] == "never-used")
            .expect("expected entry for never-used");

        let often_launcher = often_entry["scores"]["launcher"].as_f64().unwrap();
        let never_launcher = never_entry["scores"]["launcher"].as_f64().unwrap();
        assert!(often_launcher > never_launcher);

        let often_slot = often_entry["scores"]["slot"].as_f64().unwrap();
        let never_slot = never_entry["scores"]["slot"].as_f64().unwrap();
        assert!(often_slot >= never_slot);
    }

    /// `Scope::Envs` lists only environments and does not touch the daemon cache,
    /// so it must succeed even when no cache file exists.
    #[rstest]
    fn test_ls_envs_does_not_require_daemon_cache(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("env-a");
        context_object.create_mock_environment("env-b");

        ls(&mut context_object, Scope::Envs, false).unwrap();

        let output = context_object.get_output();
        assert!(output.contains("_: env-a"));
        assert!(output.contains("_: env-b"));
        for line in output.lines().filter(|l| !l.is_empty()) {
            assert!(
                line.starts_with("_: "),
                "unexpected non-env line {line:?} in {output:?}"
            );
        }
    }

    #[rstest]
    fn test_ls_envs_omits_recipes(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("my-env");
        context_object.write_cache_entries(&[("git", "repo-a", None)]);

        ls(&mut context_object, Scope::Envs, false).unwrap();

        let output = context_object.get_output();
        assert!(output.contains("_: my-env"));
        assert!(!output.contains("git: repo-a"));
    }

    #[rstest]
    fn test_ls_envs_json_emits_env_entries_with_scores(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("my-env");

        ls(&mut context_object, Scope::Envs, true).unwrap();

        let output = context_object.get_output();
        let entries: Vec<serde_json::Value> = output
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["cookbook"], "_");
        assert_eq!(entries[0]["name"], "my-env");
        assert!(entries[0]["scores"].is_object());
    }

    #[rstest]
    fn test_ls_recipes_omits_environments(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("my-env");
        context_object.write_cache_entries(&[("git", "repo-a", None)]);

        ls(&mut context_object, Scope::Recipes, false).unwrap();

        let output = context_object.get_output();
        assert!(!output.contains("_: my-env"));
        assert!(output.contains("git: repo-a"));
    }

    #[rstest]
    fn test_ls_recipes_still_filters_recipes_matching_existing_environments(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("repo-a");
        context_object.write_cache_entries(&[("git", "repo-a", None), ("git", "repo-b", None)]);

        ls(&mut context_object, Scope::Recipes, false).unwrap();

        let output = context_object.get_output();
        assert!(!output.contains("git: repo-a"));
        assert!(output.contains("git: repo-b"));
    }

    #[rstest]
    fn test_ls_recipes_json_emits_only_recipes(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("my-env");
        context_object.write_cache_entries(&[("git", "repo-a", None)]);

        ls(&mut context_object, Scope::Recipes, true).unwrap();

        let entries = parse_json_entries(&context_object.get_output());
        assert!(entries.iter().all(|e| e.cookbook != "_"));
        assert!(
            entries
                .iter()
                .any(|e| e.cookbook == "git" && e.name == "repo-a")
        );
    }

    #[rstest]
    fn test_ls_recipes_errors_when_cache_unavailable(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;

        let result = ls(&mut context_object, Scope::Recipes, false);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("daemon"));
    }
}
