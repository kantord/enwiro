use anyhow::{Context, anyhow};
use colored::Colorize;
use std::collections::{HashMap, HashSet};
use std::io::{IsTerminal, Write};
use std::path::Path;

use crate::context::CommandContext;
use crate::environments::Environment;
use crate::usage_stats::EnvStats;
use enwiro_daemon::meta::{CookedPhase, Status};
use enwiro_sdk::client::{CachedRecipe, EnvScores};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Scope {
    All,
    Envs,
    Recipes,
}

#[derive(clap::ValueEnum, Clone, Debug)]
pub enum StatusFilter {
    Ready,
    Active,
    Waiting,
    Done,
    Evergreen,
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
    /// Filter environments by status
    #[arg(long, value_enum)]
    pub status: Option<StatusFilter>,
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

#[derive(serde::Serialize)]
struct EnvEntry {
    cookbook: Option<String>,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<Status>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scores: Option<EnvScores>,
}

fn status_label(status: Option<&Status>) -> &'static str {
    match status {
        Some(Status::Cooked {
            phase: Some(CookedPhase::Active),
            ..
        }) => "active",
        Some(Status::Cooked {
            phase: Some(CookedPhase::Waiting),
            ..
        }) => "waiting",
        Some(Status::Cooked { phase: None, .. }) => "ready",
        Some(Status::Done { .. }) => "done",
        Some(Status::Evergreen) => "evergreen",
        Some(Status::Uncooked) | None => "-",
    }
}

fn colorize_status(label: &str) -> String {
    match label {
        "active" => label.green().to_string(),
        "waiting" => label.yellow().to_string(),
        "ready" => label.cyan().to_string(),
        "done" => label.dimmed().to_string(),
        "evergreen" => label.blue().to_string(),
        _ => label.dimmed().to_string(),
    }
}

fn matches_filter(status: Option<&Status>, filter: &StatusFilter) -> bool {
    match filter {
        StatusFilter::Active => matches!(
            status,
            Some(Status::Cooked {
                phase: Some(CookedPhase::Active),
                ..
            })
        ),
        StatusFilter::Waiting => matches!(
            status,
            Some(Status::Cooked {
                phase: Some(CookedPhase::Waiting),
                ..
            })
        ),
        StatusFilter::Ready => matches!(status, Some(Status::Cooked { phase: None, .. })),
        StatusFilter::Done => matches!(status, Some(Status::Done { .. })),
        StatusFilter::Evergreen => matches!(status, Some(Status::Evergreen)),
    }
}

pub fn ls<W: Write>(
    context: &mut CommandContext<W>,
    scope: Scope,
    json: bool,
    status_filter: Option<StatusFilter>,
) -> anyhow::Result<()> {
    let env_names = match scope {
        Scope::All | Scope::Envs => write_envs(context, json, status_filter.as_ref())?,
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

fn format_env_text(entries: &[(String, Option<Status>, String)]) -> String {
    let status_width = entries
        .iter()
        .map(|(_, s, _)| status_label(s.as_ref()).len())
        .max()
        .unwrap_or(1);
    let has_any_desc = entries.iter().any(|(_, _, d)| !d.trim().is_empty());
    let name_width = if has_any_desc {
        entries.iter().map(|(n, _, _)| n.len()).max().unwrap_or(1)
    } else {
        0
    };

    let mut out = String::new();
    for (name, status, desc) in entries {
        let label = status_label(status.as_ref());
        let colored_label = colorize_status(label);
        let padded_label = format!(
            "{}{}",
            colored_label,
            " ".repeat(status_width.saturating_sub(label.len()))
        );
        let trimmed_desc = desc.trim();
        if trimmed_desc.is_empty() {
            out.push_str(&format!("{}: {}\n", padded_label, name));
        } else {
            out.push_str(&format!(
                "{}: {:<name_width$}  {}\n",
                padded_label, name, trimmed_desc,
            ));
        }
    }
    out
}

fn write_envs<W: Write>(
    context: &mut CommandContext<W>,
    json: bool,
    status_filter: Option<&StatusFilter>,
) -> anyhow::Result<HashSet<String>> {
    let mut envs: Vec<Environment> = context.get_all_environments()?.into_values().collect();

    let mut meta_map: HashMap<String, EnvStats> = HashMap::new();
    for env in &envs {
        let env_dir = Path::new(&context.config.workspaces_directory).join(&env.name);
        let meta = crate::usage_stats::load_env_meta(&env_dir);
        if !meta.signals.activation_buffer.is_empty()
            || meta.description.is_some()
            || meta.status.is_some()
            || meta.cookbook.is_some()
        {
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

    if let Some(filter) = status_filter {
        envs.retain(|env| {
            let status = meta_map.get(&env.name).and_then(|m| m.status.as_ref());
            matches_filter(status, filter)
        });
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

    if json {
        for env in &envs {
            let launcher = percentile_map.get(&env.name).copied().unwrap_or(0.0);
            let slot = slot_map.get(&env.name).copied().unwrap_or(0.0);
            let meta = meta_map.get(&env.name);
            let entry = EnvEntry {
                cookbook: meta.and_then(|m| m.cookbook.clone()),
                name: env.name.clone(),
                description: meta.and_then(|m| m.description.clone()),
                status: meta.and_then(|m| m.status.clone()),
                scores: Some(EnvScores { launcher, slot }),
            };
            let line = serde_json::to_string(&entry).unwrap();
            writeln!(context.writer, "{}", line).context("Could not write to output")?;
        }
    } else {
        if std::io::stdout().is_terminal() {
            colored::control::set_override(true);
        }

        let entries: Vec<_> = envs
            .iter()
            .map(|env| {
                let meta = meta_map.get(&env.name);
                let status = meta.and_then(|m| m.status.as_ref());
                let desc = meta
                    .and_then(|m| m.description.as_deref())
                    .unwrap_or("")
                    .to_string();
                (env.name.clone(), status.cloned(), desc)
            })
            .collect();

        let text = format_env_text(&entries);
        context
            .writer
            .write_all(text.as_bytes())
            .context("Could not write to output")?;
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

    let mut filtered: Vec<CachedRecipe> = Vec::new();
    let mut raw_lines: Vec<String> = Vec::new();
    for line in recipes.lines() {
        if line.is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<CachedRecipe>(line) {
            if env_names.contains(entry.name.as_str()) {
                continue;
            }
            raw_lines.push(line.to_string());
            filtered.push(entry);
        }
    }

    if json {
        for line in &raw_lines {
            writeln!(context.writer, "{}", line).context("Could not write recipe to output")?;
        }
    } else {
        let cookbook_width = filtered.iter().map(|e| e.cookbook.len()).max().unwrap_or(1);
        let has_any_desc = filtered.iter().any(|e| e.description.is_some());
        let name_width = if has_any_desc {
            filtered.iter().map(|e| e.name.len()).max().unwrap_or(1)
        } else {
            0
        };

        for entry in &filtered {
            let desc = entry.description.as_deref().unwrap_or("").trim();
            if desc.is_empty() {
                writeln!(
                    context.writer,
                    "{:<cookbook_width$}: {}",
                    entry.cookbook, entry.name,
                )
                .context("Could not write recipe to output")?;
            } else {
                writeln!(
                    context.writer,
                    "{:<cookbook_width$}: {:<name_width$}  {}",
                    entry.cookbook, entry.name, desc,
                )
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

    fn env_text_lines(output: &str) -> Vec<&str> {
        output
            .lines()
            .filter(|l| {
                l.contains(": ")
                    && !l.starts_with("git:")
                    && !l.starts_with("npm:")
                    && !l.starts_with("github:")
                    && !l.starts_with("chezmoi:")
            })
            .collect()
    }

    #[rstest]
    fn test_ls_shows_environments_and_recipes(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("my-env");
        context_object.write_cache_entries(&[("git", "repo-a", None), ("git", "repo-b", None)]);

        ls(&mut context_object, Scope::All, false, None).unwrap();

        let output = context_object.get_output();
        assert!(output.contains("my-env"));
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

        ls(&mut context_object, Scope::All, false, None).unwrap();

        let output = context_object.get_output();
        assert!(output.contains("repo-a"), "Environment should be listed");
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

        ls(&mut context_object, Scope::All, false, None).unwrap();

        let output = context_object.get_output();
        assert!(
            !output.contains("github: repo#42"),
            "Recipe with description matching an existing environment should be excluded"
        );
        assert!(
            output.contains("repo#99") && output.contains("Add feature"),
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

        ls(&mut context_object, Scope::All, false, None).unwrap();

        let output = context_object.get_output();
        assert!(output.contains("env-a"));
        assert!(output.contains("env-b"));
        assert!(!output.contains("git:"));
    }

    #[rstest]
    fn test_ls_with_no_environments_but_has_recipes(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.write_cache_entries(&[("git", "some-repo", None)]);

        ls(&mut context_object, Scope::All, false, None).unwrap();

        let output = context_object.get_output();
        assert!(output.contains("git: some-repo"));
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

        ls(&mut context_object, Scope::All, false, None).unwrap();

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

        ls(&mut context_object, Scope::All, false, None).unwrap();

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

        ls(&mut context_object, Scope::All, false, None).unwrap();

        let output = context_object.get_output();
        let env_lines = env_text_lines(&output);
        assert!(env_lines[0].contains("often-used"));
        assert!(env_lines[1].contains("rarely-used"));
        assert!(env_lines[2].contains("never-used"));
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

        ls(&mut context_object, Scope::All, false, None).unwrap();

        let output = context_object.get_output();
        assert!(
            output.contains("owner-repo#42") && output.contains("Fix auth bug"),
            "Expected env name and description in output, got: {}",
            output
        );
    }

    #[rstest]
    fn test_ls_json_includes_status(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("my-env");
        context_object.write_cache_entries(&[]);

        let now = crate::usage_stats::now_timestamp();
        let meta = crate::usage_stats::EnvStats {
            signals: UserIntentSignals {
                activation_buffer: vec![(now, 1.0)],
                ..Default::default()
            },
            status: Some(Status::Cooked {
                phase: Some(CookedPhase::Active),
                detail: None,
            }),
            ..Default::default()
        };
        std::fs::write(
            temp_dir.path().join("my-env").join("meta.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();

        ls(&mut context_object, Scope::All, true, None).unwrap();

        let output = context_object.get_output();
        let entry: serde_json::Value =
            serde_json::from_str(output.lines().next().unwrap()).unwrap();
        assert_eq!(entry["status"]["type"], "cooked");
        assert_eq!(entry["status"]["phase"], "active");
    }

    #[rstest]
    fn test_ls_json_env_has_real_cookbook_name(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("my-env");
        context_object.write_cache_entries(&[]);

        let meta = crate::usage_stats::EnvStats {
            cookbook: Some("github".to_string()),
            ..Default::default()
        };
        std::fs::write(
            temp_dir.path().join("my-env").join("meta.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();

        ls(&mut context_object, Scope::All, true, None).unwrap();

        let output = context_object.get_output();
        let entry: serde_json::Value =
            serde_json::from_str(output.lines().next().unwrap()).unwrap();
        assert_eq!(entry["cookbook"], "github");
    }

    #[rstest]
    fn test_ls_status_filter_active(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("active-env");
        context_object.create_mock_environment("done-env");
        context_object.write_cache_entries(&[]);

        let active_meta = crate::usage_stats::EnvStats {
            status: Some(Status::Cooked {
                phase: Some(CookedPhase::Active),
                detail: None,
            }),
            ..Default::default()
        };
        let done_meta = crate::usage_stats::EnvStats {
            status: Some(Status::Done { outcome: None }),
            ..Default::default()
        };
        std::fs::write(
            temp_dir.path().join("active-env").join("meta.json"),
            serde_json::to_string(&active_meta).unwrap(),
        )
        .unwrap();
        std::fs::write(
            temp_dir.path().join("done-env").join("meta.json"),
            serde_json::to_string(&done_meta).unwrap(),
        )
        .unwrap();

        ls(
            &mut context_object,
            Scope::Envs,
            false,
            Some(StatusFilter::Active),
        )
        .unwrap();

        let output = context_object.get_output();
        assert!(output.contains("active-env"));
        assert!(!output.contains("done-env"));
    }

    #[rstest]
    fn test_ls_text_shows_status_label(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("my-env");
        context_object.write_cache_entries(&[]);

        let meta = crate::usage_stats::EnvStats {
            status: Some(Status::Cooked {
                phase: Some(CookedPhase::Active),
                detail: None,
            }),
            ..Default::default()
        };
        std::fs::write(
            temp_dir.path().join("my-env").join("meta.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();

        ls(&mut context_object, Scope::Envs, false, None).unwrap();

        let output = context_object.get_output();
        assert!(
            output.contains("active") && output.contains("my-env"),
            "Expected 'active' status label in output, got: {}",
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

        ls(&mut context_object, Scope::All, false, None).unwrap();

        let output = context_object.get_output();
        let recipe_lines: Vec<&str> = output
            .lines()
            .filter(|l| l.contains("my-repo") || l.contains("dotfiles") || l.contains("repo#1"))
            .collect();
        assert!(recipe_lines[0].contains("my-repo"));
        assert!(recipe_lines[1].contains("dotfiles"));
        assert!(recipe_lines[2].contains("repo#1"));
    }

    #[rstest]
    fn test_ls_errors_when_cache_unavailable(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;

        let result = ls(&mut context_object, Scope::All, false, None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("daemon"),
            "Error should point at the daemon, got: {err}"
        );
    }

    #[rstest]
    fn test_ls_json_env_entry_has_scores_object(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("my-env");
        context_object.write_cache_entries(&[]);

        ls(&mut context_object, Scope::All, true, None).unwrap();

        let output = context_object.get_output();
        let entries: Vec<serde_json::Value> = output
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();

        let env_entry = entries
            .iter()
            .find(|e| e["name"] == "my-env")
            .expect("expected an entry for my-env");

        let scores = env_entry
            .get("scores")
            .expect("env entry must have a 'scores' field");

        assert!(scores.get("launcher").is_some());
        assert!(scores.get("slot").is_some());
    }

    #[rstest]
    fn test_ls_envs_does_not_require_daemon_cache(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("env-a");
        context_object.create_mock_environment("env-b");

        ls(&mut context_object, Scope::Envs, false, None).unwrap();

        let output = context_object.get_output();
        assert!(output.contains("env-a"));
        assert!(output.contains("env-b"));
    }

    #[rstest]
    fn test_ls_envs_omits_recipes(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("my-env");
        context_object.write_cache_entries(&[("git", "repo-a", None)]);

        ls(&mut context_object, Scope::Envs, false, None).unwrap();

        let output = context_object.get_output();
        assert!(output.contains("my-env"));
        assert!(!output.contains("git: repo-a"));
    }

    #[rstest]
    fn test_ls_recipes_omits_environments(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("my-env");
        context_object.write_cache_entries(&[("git", "repo-a", None)]);

        ls(&mut context_object, Scope::Recipes, false, None).unwrap();

        let output = context_object.get_output();
        assert!(output.contains("git: repo-a"));
    }

    #[rstest]
    fn test_ls_recipes_errors_when_cache_unavailable(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;

        let result = ls(&mut context_object, Scope::Recipes, false, None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("daemon"));
    }
}

#[cfg(test)]
mod prop_tests {
    use super::*;
    use enwiro_daemon::meta::{CookedPhase, DoneOutcome, Status, StatusDetail};
    use proptest::prelude::*;

    fn arb_status() -> impl Strategy<Value = Option<Status>> {
        prop_oneof![
            Just(None),
            Just(Some(Status::Uncooked)),
            Just(Some(Status::Cooked {
                phase: None,
                detail: None
            })),
            Just(Some(Status::Cooked {
                phase: Some(CookedPhase::Active),
                detail: None
            })),
            Just(Some(Status::Cooked {
                phase: Some(CookedPhase::Waiting),
                detail: None
            })),
            Just(Some(Status::Cooked {
                phase: Some(CookedPhase::Active),
                detail: Some(StatusDetail {
                    source: "test".into(),
                    label: "testing".into(),
                    info: None
                })
            })),
            Just(Some(Status::Done { outcome: None })),
            Just(Some(Status::Done {
                outcome: Some(DoneOutcome::Completed)
            })),
            Just(Some(Status::Done {
                outcome: Some(DoneOutcome::Abandoned)
            })),
            Just(Some(Status::Evergreen)),
        ]
    }

    fn arb_env_entry() -> impl Strategy<Value = (String, Option<Status>, String)> {
        (
            "[a-zA-Z0-9#@_]{1,50}",
            arb_status(),
            prop_oneof![Just(String::new()), "[a-zA-Z0-9]{1,80}"],
        )
    }

    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut in_escape = false;
        for c in s.chars() {
            if c == '\x1b' {
                in_escape = true;
            } else if in_escape {
                if c == 'm' {
                    in_escape = false;
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    proptest! {
        #[test]
        fn text_output_no_trailing_whitespace(
            entries in proptest::collection::vec(arb_env_entry(), 0..20)
        ) {
            colored::control::set_override(false);
            let text = format_env_text(&entries);
            for line in text.lines() {
                prop_assert!(
                    !line.ends_with(' '),
                    "trailing whitespace in: {:?}", line
                );
            }
        }

        #[test]
        fn text_output_line_count_matches_entries(
            entries in proptest::collection::vec(arb_env_entry(), 0..20)
        ) {
            colored::control::set_override(false);
            let text = format_env_text(&entries);
            let line_count = text.lines().count();
            prop_assert_eq!(line_count, entries.len());
        }

        #[test]
        fn text_output_every_name_appears(
            entries in proptest::collection::vec(arb_env_entry(), 0..20)
        ) {
            colored::control::set_override(false);
            let text = format_env_text(&entries);
            for (name, _, _) in &entries {
                prop_assert!(
                    text.contains(name.as_str()),
                    "name {:?} missing from output:\n{}", name, text
                );
            }
        }

        #[test]
        fn text_output_separator_aligned_no_color(
            entries in proptest::collection::vec(arb_env_entry(), 1..20)
        ) {
            colored::control::set_override(false);
            let text = format_env_text(&entries);
            let stripped = strip_ansi(&text);
            let positions: Vec<usize> = stripped
                .lines()
                .filter_map(|l| l.find(": "))
                .collect();
            if let Some(&first) = positions.first() {
                for (i, &pos) in positions.iter().enumerate() {
                    prop_assert_eq!(
                        pos, first,
                        "line {} has separator at col {} but expected {}\noutput:\n{}",
                        i, pos, first, stripped
                    );
                }
            }
        }

        #[test]
        fn text_output_separator_aligned_with_color(
            entries in proptest::collection::vec(arb_env_entry(), 1..20)
        ) {
            colored::control::set_override(true);
            let text = format_env_text(&entries);
            let stripped = strip_ansi(&text);
            let positions: Vec<usize> = stripped
                .lines()
                .filter_map(|l| l.find(": "))
                .collect();
            if let Some(&first) = positions.first() {
                for (i, &pos) in positions.iter().enumerate() {
                    prop_assert_eq!(
                        pos, first,
                        "line {} has separator at col {} but expected {} (after stripping ANSI)\nstripped:\n{}",
                        i, pos, first, stripped
                    );
                }
            }
            colored::control::set_override(false);
        }

        #[test]
        fn text_output_lines_with_desc_are_longer_than_without(
            entries in proptest::collection::vec(arb_env_entry(), 1..20)
        ) {
            colored::control::set_override(false);
            let text = format_env_text(&entries);
            let lines: Vec<&str> = text.lines().collect();
            let no_desc_max_len = lines.iter()
                .zip(entries.iter())
                .filter(|(_, (_, _, desc))| desc.trim().is_empty())
                .map(|(line, _)| line.len())
                .max();
            if let Some(max_no_desc) = no_desc_max_len {
                for (line, (_, _, desc)) in lines.iter().zip(entries.iter()) {
                    if !desc.trim().is_empty() {
                        prop_assert!(
                            line.len() > max_no_desc,
                            "line with desc {:?} should be longer than lines without desc (len {} <= {})\noutput:\n{}",
                            desc.trim(), line.len(), max_no_desc, text
                        );
                    }
                }
            }
        }

        fn text_output_lines_with_desc_end_with_desc(
            entries in proptest::collection::vec(arb_env_entry(), 0..20)
        ) {
            colored::control::set_override(false);
            let text = format_env_text(&entries);
            for (line, (_, _, desc)) in text.lines().zip(entries.iter()) {
                let trimmed = desc.trim();
                if !trimmed.is_empty() {
                    prop_assert!(
                        line.ends_with(trimmed),
                        "line should end with description {:?}, got {:?}",
                        trimmed, line
                    );
                }
            }
        }

        #[test]
        fn json_output_always_valid(
            entries in proptest::collection::vec(arb_env_entry(), 0..20)
        ) {
            for (name, status, desc) in &entries {
                let entry = EnvEntry {
                    cookbook: Some("test".into()),
                    name: name.clone(),
                    description: if desc.is_empty() { None } else { Some(desc.clone()) },
                    status: status.clone(),
                    scores: Some(EnvScores { launcher: 0.5, slot: 0.3 }),
                };
                let json = serde_json::to_string(&entry).unwrap();
                let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
                prop_assert_eq!(parsed["name"].as_str().unwrap(), name.as_str());
            }
        }
    }
}
