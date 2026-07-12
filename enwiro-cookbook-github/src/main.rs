use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use enwiro_sdk::cli::{CookArgs, CookbookCore};
use enwiro_sdk::cookbook::CookbookCapability;
use enwiro_sdk::metadata::DeclaredCapabilities;
use enwiro_sdk::{CookbookMetadata, CookbookPayload, PatternRecipe, Recipe, RecipeItem};
use serde_derive::{Deserialize, Serialize};

const LISTEN_POLL_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct ConfigurationValues {
    pub worktree_dir: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RepoConfig {
    pub repo: String,
    pub local_path: PathBuf,
}

/// Minimal representation of the git cookbook's configuration.
/// This intentionally couples to the git cookbook's config schema
/// (config scope "cookbook-git", field "repo_globs"). If the git cookbook
/// renames these, this struct must be updated to match.
#[derive(Debug, Serialize, Deserialize, Default)]
struct GitCookbookConfig {
    repo_globs: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum GithubItemKind {
    PullRequest { head_ref_name: String },
    Issue,
}

#[derive(Debug, Clone)]
pub struct GithubItem {
    pub number: u64,
    pub title: String,
    pub repo: String,
    pub kind: GithubItemKind,
    pub updated_at: String,
}

#[derive(Parser)]
enum EnwiroCookbookGithub {
    #[command(flatten)]
    Core(CookbookCore),
    Gear(GearArgs),
    ExternalPaths(ExternalPathsArgs),
    Listen,
}

#[derive(clap::Args)]
pub struct GearArgs {
    recipe_name: String,
}

#[derive(clap::Args)]
pub struct ExternalPathsArgs {
    recipe_name: String,
}

fn short_path_hash(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(path.to_string_lossy().as_bytes());
    hex::encode(hash)[..8].to_string()
}

fn default_worktree_dir() -> anyhow::Result<PathBuf> {
    let base = dirs::data_dir().context("Could not determine data directory (is $HOME set?)")?;
    Ok(base.join("enwiro").join("worktrees").join("pr"))
}

fn worktree_base_dir(config: &ConfigurationValues) -> anyhow::Result<PathBuf> {
    match &config.worktree_dir {
        Some(dir) => Ok(PathBuf::from(dir)),
        None => default_worktree_dir(),
    }
}

/// Parse a GitHub remote URL and extract "owner/repo".
/// Returns None for non-GitHub remotes.
fn parse_github_remote(url: &str) -> Option<String> {
    let url = url.trim();

    // SSH format: git@github.com:owner/repo.git
    if let Some(rest) = url.strip_prefix("git@github.com:") {
        let repo = rest.strip_suffix(".git").unwrap_or(rest);
        return if repo.contains('/') {
            Some(repo.to_string())
        } else {
            None
        };
    }

    // URL formats: https://github.com/..., ssh://git@github.com/..., http://github.com/...
    let path = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))?;

    let repo = path.strip_suffix(".git").unwrap_or(path);
    if repo.contains('/') {
        Some(repo.to_string())
    } else {
        None
    }
}

fn discover_github_repos_from_config(
    git_config: &GitCookbookConfig,
) -> anyhow::Result<Vec<RepoConfig>> {
    let mut results = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for glob_pattern in &git_config.repo_globs {
        let paths = glob::glob(glob_pattern)
            .with_context(|| format!("Could not parse glob pattern: {}", glob_pattern))?;

        for path in paths.flatten() {
            let repo = match git2::Repository::open(&path) {
                Ok(r) => r,
                Err(_) => continue,
            };

            let origin = match repo.find_remote("origin") {
                Ok(r) => r,
                Err(_) => continue,
            };

            let url = match origin.url() {
                Ok(u) => u.to_string(),
                Err(_) => continue,
            };

            if let Some(github_repo) = parse_github_remote(&url)
                && seen.insert(github_repo.clone())
            {
                let canonical_path = path.canonicalize().unwrap_or(path);
                tracing::debug!(repo = %github_repo, path = %canonical_path.display(), "Discovered GitHub repo");
                results.push(RepoConfig {
                    repo: github_repo,
                    local_path: canonical_path,
                });
            }
        }
    }

    Ok(results)
}

fn discover_github_repos() -> anyhow::Result<Vec<RepoConfig>> {
    let git_config_json = enwiro_sdk::config::load_user_config("cookbook-git")
        .context("Could not load git cookbook configuration")?;
    let git_config: GitCookbookConfig = serde_json::from_value(git_config_json)
        .context("Could not deserialize git cookbook configuration")?;
    discover_github_repos_from_config(&git_config)
}

/// The only recognized goal-variant suffix (#756): `repo#42@fix-ci` cooks
/// the same PR as `repo#42`, just recorded with a "fix CI" goal instead of
/// "work on it". `@` is this codebase's documented convention for "pins a
/// ref or variant" (see `enwiro_sdk::recipe_expr`).
const FIX_CI_VARIANT: &str = "fix-ci";

/// Parse a recipe name like "repo#123" or "repo#123@fix-ci" into
/// ("repo", 123, is_fix_ci_variant).
fn parse_recipe_name(name: &str) -> anyhow::Result<(&str, u64, bool)> {
    let (repo, number_str) = name
        .rsplit_once('#')
        .context("Recipe name must contain '#' (expected format: repo#123)")?;
    let (number_str, is_fix_ci_variant) = match number_str.split_once('@') {
        Some((number_str, variant)) => {
            anyhow::ensure!(
                variant == FIX_CI_VARIANT,
                "Unrecognized goal variant '@{}' (expected '@{}')",
                variant,
                FIX_CI_VARIANT
            );
            (number_str, true)
        }
        None => (number_str, false),
    };
    let number = number_str
        .parse::<u64>()
        .with_context(|| format!("Invalid issue/PR number: {}", number_str))?;
    Ok((repo, number, is_fix_ci_variant))
}

/// Build a GitHub search query string.
/// `type_filter` controls the item type, e.g.:
/// - `"is:pr is:open"` for pull requests
/// - `"is:issue is:open assignee:@me"` for assigned issues
fn build_search_query(repos: &[String], type_filter: &str) -> String {
    let repo_filters: Vec<String> = repos.iter().map(|r| format!("repo:{}", r)).collect();
    let date_qualifier = if type_filter.contains("is:issue") {
        String::new()
    } else {
        let cutoff = chrono::Utc::now() - chrono::Duration::days(30);
        format!(" updated:>{}", cutoff.format("%Y-%m-%d"))
    };
    format!(
        "{} {}{} sort:updated-desc",
        type_filter,
        repo_filters.join(" "),
        date_qualifier,
    )
}

/// Serde structs for the GraphQL search response.
/// Both PR and Issue nodes are deserialized into `GraphQlNode`;
/// PRs have `head_ref_name` set, issues don't.
#[derive(Deserialize)]
struct GraphQlResponse {
    data: GraphQlData,
}

#[derive(Deserialize)]
struct GraphQlData {
    search: GraphQlSearch,
}

#[derive(Deserialize)]
struct GraphQlSearch {
    nodes: Vec<GraphQlNode>,
}

#[derive(Deserialize)]
struct GraphQlNode {
    number: u64,
    title: String,
    #[serde(rename = "headRefName", default)]
    head_ref_name: Option<String>,
    #[serde(rename = "updatedAt", default)]
    updated_at: Option<String>,
    repository: GraphQlRepo,
}

#[derive(Deserialize)]
struct GraphQlRepo {
    #[serde(rename = "nameWithOwner")]
    name_with_owner: String,
}

fn extract_short_repo_name(name_with_owner: String) -> String {
    name_with_owner
        .rsplit_once('/')
        .map(|(_, name)| name.to_string())
        .unwrap_or(name_with_owner)
}

fn parse_search_response(json: &str) -> anyhow::Result<Vec<GithubItem>> {
    let response: GraphQlResponse =
        serde_json::from_str(json).context("Could not parse GraphQL response")?;
    Ok(response
        .data
        .search
        .nodes
        .into_iter()
        .map(|node| {
            let repo = extract_short_repo_name(node.repository.name_with_owner);
            let kind = match node.head_ref_name {
                Some(head_ref_name) => GithubItemKind::PullRequest { head_ref_name },
                None => GithubItemKind::Issue,
            };
            GithubItem {
                number: node.number,
                title: node.title,
                repo,
                kind,
                updated_at: node.updated_at.unwrap_or_default(),
            }
        })
        .collect())
}

/// Single GraphQL query with both PR and Issue fragments.
/// GraphQL matches only the appropriate fragment per node type.
const SEARCH_QUERY: &str = r#"query($searchQuery: String!) {
  search(query: $searchQuery, type: ISSUE, first: 100) {
    nodes {
      ... on PullRequest {
        number
        title
        headRefName
        updatedAt
        repository { nameWithOwner }
      }
      ... on Issue {
        number
        title
        updatedAt
        repository { nameWithOwner }
      }
    }
  }
}"#;

fn interpret_gh_output(
    stdout: &[u8],
    stderr: &[u8],
    success: bool,
) -> anyhow::Result<Vec<GithubItem>> {
    if !success {
        let stderr_str = String::from_utf8_lossy(stderr);
        if stderr_str.contains("GitHub search returned 100 results (the maximum)") {
            tracing::warn!(
                "GitHub search returned 100 results (the maximum). Some results may be missing."
            );
        } else {
            anyhow::bail!(
                "gh api graphql failed: {}. Is gh authenticated? (try: gh auth login)",
                stderr_str
            );
        }
    }

    let stdout_str = String::from_utf8(stdout.to_vec()).context("gh produced invalid UTF-8")?;
    parse_search_response(&stdout_str)
}

fn search_github(repos: &[String], type_filter: &str) -> anyhow::Result<Vec<GithubItem>> {
    if repos.is_empty() {
        return Ok(Vec::new());
    }

    let search_query = build_search_query(repos, type_filter);

    let output = Command::new("gh")
        .args([
            "api",
            "graphql",
            "-F",
            &format!("searchQuery={}", search_query),
            "-f",
            &format!("query={}", SEARCH_QUERY),
        ])
        .output()
        .context(
            "Failed to run gh CLI. Is it installed and authenticated? \
             (https://cli.github.com/, then run: gh auth login)",
        )?;

    interpret_gh_output(&output.stdout, &output.stderr, output.status.success())
}

fn sort_items_by_date(items: &mut [GithubItem]) {
    items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
}

fn compute_sort_order(index: usize, total: usize) -> u32 {
    if total <= 1 {
        0
    } else {
        ((index * 100) / (total - 1)) as u32
    }
}

/// The git cookbook's display name for a repo: the basename of its local
/// working-directory path. The github cookbook reproduces it so its
/// `equivalent_to` alias matches the git cookbook's `repo@<branch>` recipe for
/// the same worktree.
fn git_repo_display_name(local_path: &Path) -> Option<String> {
    local_path
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
}

/// The branch the cookbook checks out when cooking this item: `pr-<n>` for a
/// pull request, `issue-<n>` for an issue (see `cook_pr` / `cook_issue`).
fn cooked_branch_name(item: &GithubItem) -> String {
    match item.kind {
        GithubItemKind::PullRequest { .. } => format!("pr-{}", item.number),
        GithubItemKind::Issue => format!("issue-{}", item.number),
    }
}

/// One goal-detail payload shared by every recipe derived from `item`:
/// `{repo, number}`, enough for a future prompt generator to look the item
/// back up without re-deriving it from the recipe name (#756).
fn goal_detail(kind: &str, label: String, item: &GithubItem) -> enwiro_sdk::goal::GoalDetail {
    enwiro_sdk::goal::GoalDetail {
        kind: kind.to_string(),
        label,
        detail: Some(serde_json::json!({"repo": item.repo, "number": item.number})),
    }
}

/// The recipe(s) one search result expands to: one for an issue
/// (`repo#N`, goal `github_issue`), two for a PR - `repo#N` (goal
/// `work_on`, unchanged from before goal-variants existed) and
/// `repo#N@fix-ci` (goal `fix_ci`) - both cooking the same worktree, so
/// they share `equivalent_to` (#756).
fn recipes_for_item(
    item: &GithubItem,
    index: usize,
    total: usize,
    display_names: &std::collections::HashMap<String, String>,
) -> Vec<Recipe> {
    let safe_title = item.title.replace(['\n', '\0', '\x1f'], " ");
    let sort_order = compute_sort_order(index, total);
    let equivalent_to = display_names
        .get(&item.repo)
        .map(|display| vec![format!("{}@{}", display, cooked_branch_name(item))])
        .unwrap_or_default();
    let base_name = format!("{}#{}", item.repo, item.number);

    match &item.kind {
        GithubItemKind::Issue => {
            let mut recipe = Recipe::with_description(base_name, format!("[issue] {}", safe_title));
            recipe.sort_order = sort_order;
            recipe.equivalent_to = equivalent_to;
            recipe.goal = Some(goal_detail("github_issue", safe_title, item));
            vec![recipe]
        }
        GithubItemKind::PullRequest { .. } => {
            let mut work_on =
                Recipe::with_description(base_name.clone(), format!("[PR] {}", safe_title));
            work_on.sort_order = sort_order;
            work_on.equivalent_to = equivalent_to.clone();
            work_on.goal = Some(goal_detail("work_on", safe_title.clone(), item));

            let fix_ci_label = format!("Fix CI for {}", safe_title);
            let mut fix_ci = Recipe::with_description(
                format!("{base_name}@{FIX_CI_VARIANT}"),
                format!("[PR] {}", fix_ci_label),
            );
            fix_ci.sort_order = sort_order;
            fix_ci.equivalent_to = equivalent_to;
            fix_ci.goal = Some(goal_detail("fix_ci", fix_ci_label, item));

            vec![work_on, fix_ci]
        }
    }
}

fn collect_recipes() -> Vec<Recipe> {
    let Ok(repos) = discover_github_repos() else {
        return Vec::new();
    };
    let repo_names: Vec<String> = repos.iter().map(|r| r.repo.clone()).collect();
    // Map a search result's short repo name to the local clone's display name,
    // so each recipe can declare the git cookbook's equivalent `repo@<branch>`.
    let display_names: std::collections::HashMap<String, String> = repos
        .iter()
        .filter_map(|r| {
            git_repo_display_name(&r.local_path)
                .map(|name| (extract_short_repo_name(r.repo.clone()), name))
        })
        .collect();

    let prs = search_github(&repo_names, "is:pr is:open").unwrap_or_default();
    // Issues are scoped to `assignee:@me` so only actionable work appears,
    // unlike PRs which show all open PRs on configured repos.
    let issues = search_github(&repo_names, "is:issue is:open assignee:@me").unwrap_or_default();

    let mut items: Vec<GithubItem> = prs.into_iter().chain(issues).collect();
    sort_items_by_date(&mut items);

    let total = items.len();
    items
        .iter()
        .enumerate()
        .flat_map(|(index, item)| recipes_for_item(item, index, total, &display_names))
        .collect()
}

fn list_recipes() -> anyhow::Result<()> {
    for recipe in collect_recipes() {
        println!("{}", recipe.to_jsonl());
    }
    Ok(())
}

/// One claim per configured repo covering every `repo#<number>` name.
/// The searches behind `collect_recipes` are scoped (assigned issues, open
/// PRs), but `cook` probes the forge for what a number is - so any issue or
/// PR is cookable, not just the listed ones. Emitted unanchored; the daemon
/// anchors them (see `enwiro_sdk::recipe_pattern`).
///
/// Each claim also carries a URL rule mapping the repo's PR/issue pages
/// (including subpages such as `/pull/42/files`) to the claimed name, so the
/// browser extension can activate straight from a GitHub page. Claims are
/// deduplicated by short name to match `cook`'s resolution; when two
/// configured repos share a short name, the URL rule points at the
/// lexicographically first full name.
fn item_pattern_recipes(repos: &[RepoConfig]) -> Vec<RecipeItem> {
    let mut full_names: Vec<String> = repos.iter().map(|r| r.repo.clone()).collect();
    full_names.sort();
    full_names.dedup();
    let mut full_by_short: BTreeMap<String, String> = BTreeMap::new();
    for full_name in full_names {
        full_by_short
            .entry(extract_short_repo_name(full_name.clone()))
            .or_insert(full_name);
    }
    full_by_short
        .into_iter()
        .map(|(short_name, full_name)| {
            RecipeItem::Pattern(PatternRecipe {
                // [0-9]{1,19}, not \d+: the regex crate's \d is Unicode and
                // unbounded, which would claim names whose number
                // parse_recipe_name's u64 parse then rejects. The trailing
                // optional group claims the fix-ci goal variant (#756) for
                // not-yet-listed numbers too, e.g. `repo#42@fix-ci`.
                pattern: format!(
                    "{}#(?P<number>[0-9]{{1,19}})(?:@{FIX_CI_VARIANT})?",
                    enwiro_sdk::recipe_pattern::escape(&short_name)
                ),
                description: Some(format!(
                    "Work on PR or issue #{{number}} in {}",
                    enwiro_sdk::recipe_pattern::escape_template(&short_name)
                )),
                url: Some(github_url_rule(&short_name, &full_name)),
            })
        })
        .collect()
}

/// The URL rule routing a repo's PR/issue pages (including subpages such as
/// `/pull/42/files`) to its `repo#<number>` recipe. GitHub owner and repo
/// names are limited to `[A-Za-z0-9_.-]`, none of which is URLPattern
/// syntax, so the full name embeds literally. The URL regex is `[0-9]+`
/// rather than the claim's `[0-9]{1,19}`: an overlong number derives a name
/// the anchored claim then rejects, which consumers already handle.
fn github_url_rule(short_name: &str, full_name: &str) -> enwiro_sdk::url_rule::UrlRule {
    enwiro_sdk::url_rule::UrlRule {
        pattern: format!(
            "https://github.com/{}/:kind(pull|issues)/:number([0-9]+){{/*}}?",
            full_name
        ),
        recipe: format!(
            "{}#{{number}}",
            enwiro_sdk::recipe_pattern::escape_template(short_name)
        ),
    }
}

fn collect_recipe_items() -> Vec<RecipeItem> {
    let mut items: Vec<RecipeItem> = collect_recipes()
        .into_iter()
        .map(RecipeItem::Concrete)
        .collect();
    if let Ok(repos) = discover_github_repos() {
        items.extend(item_pattern_recipes(&repos));
    }
    items
}

/// Auto-detected `done` events for the PR/issue envs this cookbook has
/// actually cooked (#302). We only check items that *could* need closing --
/// the cookbook's own worktrees -- never a broad forge search (GitHub's
/// search API is score-rate-limited). `done` is asked of the forge (`gh`),
/// which is authoritative. `done_cache` carries already-confirmed-done recipes
/// across listen ticks so we stop re-querying `gh` for them.
fn collect_status_events(
    config: &ConfigurationValues,
    done_cache: &mut std::collections::HashSet<String>,
) -> Vec<enwiro_sdk::listen::RecipeUpdate> {
    let Ok(repos) = discover_github_repos() else {
        return Vec::new();
    };
    let mut events = Vec::new();
    for repo_config in &repos {
        events.extend(repo_status_events(config, repo_config, done_cache));
    }
    events
}

/// `status_changed{done}` events for one repo's cooked PR/issue worktrees.
fn repo_status_events(
    config: &ConfigurationValues,
    repo_config: &RepoConfig,
    done_cache: &mut std::collections::HashSet<String>,
) -> Vec<enwiro_sdk::listen::RecipeUpdate> {
    use enwiro_sdk::listen::RecipeUpdate;
    use enwiro_sdk::status::{DoneOutcome, Status};

    // Worktrees are stored under the SHORT repo name (the recipe name `cook`
    // uses), not the full `owner/repo`.
    let short_repo = extract_short_repo_name(repo_config.repo.clone());
    let Ok(repo_dir) = repo_worktree_dir(config, repo_config, &short_repo) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&repo_dir) else {
        return Vec::new(); // nothing cooked for this repo yet
    };

    let mut events = Vec::new();
    for entry in entries.flatten() {
        let Some((kind, number)) = parse_cooked_env_dir(&entry.file_name().to_string_lossy())
        else {
            continue;
        };
        let recipe = format!("{short_repo}#{number}");
        // The forge is authoritative for "is it merged/closed?" (a GitHub
        // squash-merge is undetectable from local git history once the default
        // branch moves on, #302). A `done` result never reverts, so once we've
        // confirmed it we cache the recipe and stop calling `gh` for it - that
        // re-query of every cooked env each tick is the bulk of the cost.
        if !done_cache.contains(&recipe) {
            if !forge_item_is_done(&repo_config.repo, kind, number) {
                continue;
            }
            done_cache.insert(recipe.clone());
        }
        events.push(RecipeUpdate::StatusChanged {
            recipe,
            status: Status::Done {
                outcome: Some(DoneOutcome::Completed),
            },
        });
    }
    events
}

/// Parse a cooked-env directory name (`pr-123` / `issue-45`) into
/// `(kind, number)`. `None` for anything else (e.g. stray files).
fn parse_cooked_env_dir(dir_name: &str) -> Option<(&'static str, u64)> {
    for (prefix, kind) in [("pr-", "pr"), ("issue-", "issue")] {
        if let Some(rest) = dir_name.strip_prefix(prefix)
            && let Ok(number) = rest.parse::<u64>()
        {
            return Some((kind, number));
        }
    }
    None
}

/// Targeted, single-item forge state check via `gh` REST (not search).
/// A PR counts as done when merged; an issue when closed. Any error ->
/// `false` (stay silent; the daemon won't auto-mark).
fn forge_item_is_done(repo: &str, kind: &str, number: u64) -> bool {
    let (subcommand, done_state) = match kind {
        "pr" => ("pr", "MERGED"),
        "issue" => ("issue", "CLOSED"),
        _ => return false,
    };
    let output = Command::new("gh")
        .args([
            subcommand,
            "view",
            &number.to_string(),
            "--repo",
            repo,
            "--json",
            "state",
            "-q",
            ".state",
        ])
        .output();
    match output {
        Ok(out) if out.status.success() => {
            String::from_utf8_lossy(&out.stdout).trim() == done_state
        }
        _ => false,
    }
}

fn resolve_repo_config(repo_str: &str) -> anyhow::Result<RepoConfig> {
    let repos = discover_github_repos()?;
    let matching: Vec<_> = repos
        .into_iter()
        .filter(|r| {
            r.repo
                .rsplit_once('/')
                .map(|(_, name)| name)
                .unwrap_or(&r.repo)
                == repo_str
        })
        .collect();
    anyhow::ensure!(
        matching.len() <= 1,
        "Ambiguous repo name '{}': matches {} configured repos. Use a more specific name.",
        repo_str,
        matching.len()
    );
    let repo_config = matching
        .into_iter()
        .next()
        .with_context(|| format!("No configured repo matching '{}'", repo_str))?;
    anyhow::ensure!(
        repo_config.local_path.exists(),
        "Local clone not found at {}. Please clone the repo first.",
        repo_config.local_path.display()
    );
    Ok(repo_config)
}

fn print_worktree_path(wt_path: &Path) -> anyhow::Result<()> {
    println!(
        "{}",
        wt_path
            .to_str()
            .context("Could not convert worktree path to string")?
    );
    Ok(())
}

/// The directory under which a repo's cooked PR/issue worktrees live:
/// `<worktree_base>/<repo_str>-<path_hash>`. `repo_str` is the recipe's
/// short repo name (e.g. `enwiro`).
fn repo_worktree_dir(
    config: &ConfigurationValues,
    repo_config: &RepoConfig,
    repo_str: &str,
) -> anyhow::Result<PathBuf> {
    let wt_base = worktree_base_dir(config)?;
    let path_hash = short_path_hash(&repo_config.local_path);
    Ok(wt_base.join(format!("{}-{}", repo_str, path_hash)))
}

fn worktree_path(
    config: &ConfigurationValues,
    repo_config: &RepoConfig,
    repo_str: &str,
    prefix: &str,
    number: u64,
) -> anyhow::Result<PathBuf> {
    Ok(repo_worktree_dir(config, repo_config, repo_str)?.join(format!("{}-{}", prefix, number)))
}

/// Create a worktree for a PR. Assumes the ref `pr-{number}` was already
/// fetched and that no existing worktree was found (caller checks both).
fn cook_pr(
    config: &ConfigurationValues,
    repo_config: &RepoConfig,
    repo_str: &str,
    number: u64,
) -> anyhow::Result<()> {
    let wt_path = worktree_path(config, repo_config, repo_str, "pr", number)?;

    std::fs::create_dir_all(wt_path.parent().unwrap())
        .context("Could not create worktree directory")?;

    let ref_name = format!("pr-{}", number);
    let repo = git2::Repository::open(&repo_config.local_path)
        .context("Could not open repository for worktree creation")?;
    let branch = repo
        .find_branch(&ref_name, git2::BranchType::Local)
        .with_context(|| format!("Could not find branch {}", ref_name))?;
    let reference = branch.into_reference();

    let wt_name = format!("enwiro-pr-{}", number);
    let mut opts = git2::WorktreeAddOptions::new();
    opts.reference(Some(&reference));
    repo.worktree(&wt_name, &wt_path, Some(&opts))
        .with_context(|| format!("Could not create worktree for PR #{}", number))?;

    tracing::debug!(path = %wt_path.display(), pr = number, "Created worktree for PR");
    print_worktree_path(&wt_path)
}

/// Unlike the git cookbook, issue-branch creation has no local-HEAD
/// fallback: these repos are GitHub clones by definition, so a missing
/// remote default is a config problem worth an actionable error.
fn get_default_branch(repo: &git2::Repository) -> anyhow::Result<String> {
    enwiro_sdk::git::remote_default_branch(repo).ok_or_else(|| {
        anyhow::anyhow!(
            "Could not determine default branch: origin/HEAD is not set and \
             neither origin/main nor origin/master exist. \
             Try running: git remote set-head origin --auto"
        )
    })
}

/// Create a worktree for an issue. Assumes no existing worktree was found
/// (caller checks). Creates a new branch `issue-{number}` from the default
/// branch, or reuses the branch if it already exists (e.g., worktree was
/// manually deleted but the branch was left behind).
fn cook_issue(
    config: &ConfigurationValues,
    repo_config: &RepoConfig,
    repo_str: &str,
    number: u64,
) -> anyhow::Result<()> {
    let wt_path = worktree_path(config, repo_config, repo_str, "issue", number)?;

    std::fs::create_dir_all(wt_path.parent().unwrap())
        .context("Could not create worktree directory")?;

    let local_path_str = repo_config
        .local_path
        .to_str()
        .context("Could not convert local path to string")?;

    // Fetch latest state of default branch
    let fetch_status = Command::new("git")
        .args(["-C", local_path_str, "fetch", "origin"])
        .status()
        .context("Failed to run git fetch")?;

    if !fetch_status.success() {
        anyhow::bail!("Failed to fetch from {}", repo_config.repo);
    }

    let repo = git2::Repository::open(&repo_config.local_path)
        .context("Could not open repository for worktree creation")?;

    let default_branch = get_default_branch(&repo)?;

    let branch_name = format!("issue-{}", number);

    // Reuse existing branch if present (e.g., worktree was manually deleted
    // but the branch was left behind), otherwise create from default branch.
    let branch = match repo.find_branch(&branch_name, git2::BranchType::Local) {
        Ok(existing) => {
            tracing::debug!(branch = %branch_name, "Reusing existing issue branch");
            existing
        }
        Err(_) => {
            let origin_ref = format!("origin/{}", default_branch);
            let origin_commit = repo
                .find_reference(&format!("refs/remotes/{}", origin_ref))
                .with_context(|| format!("Could not find ref {}", origin_ref))?
                .peel_to_commit()
                .with_context(|| format!("Could not resolve {} to a commit", origin_ref))?;

            repo.branch(&branch_name, &origin_commit, false)
                .with_context(|| format!("Could not create branch {}", branch_name))?
        }
    };
    let reference = branch.into_reference();

    let wt_name = format!("enwiro-issue-{}", number);
    let mut opts = git2::WorktreeAddOptions::new();
    opts.reference(Some(&reference));
    repo.worktree(&wt_name, &wt_path, Some(&opts))
        .with_context(|| format!("Could not create worktree for issue #{}", number))?;

    tracing::debug!(path = %wt_path.display(), issue = number, "Created worktree for issue");
    print_worktree_path(&wt_path)
}

/// A goal-variant number resolved to an issue, not a PR - the variant only
/// makes sense for PRs (#756).
fn reject_fix_ci_on_issue(is_fix_ci_variant: bool, number: u64) -> anyhow::Result<()> {
    anyhow::ensure!(
        !is_fix_ci_variant,
        "'@{FIX_CI_VARIANT}' only applies to pull requests, but #{number} is an issue"
    );
    Ok(())
}

fn cook(config: &ConfigurationValues, args: CookArgs) -> anyhow::Result<()> {
    let (repo_str, number, is_fix_ci_variant) = parse_recipe_name(&args.recipe_name)?;
    let repo_config = resolve_repo_config(repo_str)?;

    // Check if a worktree already exists for either PR or issue
    let pr_wt_path = worktree_path(config, &repo_config, repo_str, "pr", number)?;
    let issue_wt_path = worktree_path(config, &repo_config, repo_str, "issue", number)?;

    if pr_wt_path.exists() {
        return print_worktree_path(&pr_wt_path);
    }
    if issue_wt_path.exists() {
        reject_fix_ci_on_issue(is_fix_ci_variant, number)?;
        return print_worktree_path(&issue_wt_path);
    }

    // Also check old worktree path format for backward compatibility (PR only)
    let old_repo_name = repo_config.repo.replace('/', "-");
    let old_pr_wt_path =
        repo_worktree_dir(config, &repo_config, &old_repo_name)?.join(format!("pr-{}", number));
    if old_pr_wt_path.exists() {
        return print_worktree_path(&old_pr_wt_path);
    }

    // Try fetching as a PR first. If the ref doesn't exist, treat as an issue.
    // If fetch fails for another reason (network error), bail instead of
    // silently creating an issue branch.
    let local_path_str = repo_config
        .local_path
        .to_str()
        .context("Could not convert local path to string")?;
    let fetch_refspec = format!("pull/{}/head:pr-{}", number, number);
    let fetch_output = Command::new("git")
        .args(["-C", local_path_str, "fetch", "origin", &fetch_refspec])
        .output()
        .context("Failed to run git fetch")?;

    if fetch_output.status.success() {
        return cook_pr(config, &repo_config, repo_str, number);
    }

    let stderr = String::from_utf8_lossy(&fetch_output.stderr);
    // "not found" / "couldn't find remote ref" indicate the number is an
    // issue, not a PR. Any other failure is a real error (network, auth, etc.)
    if stderr.contains("not found") || stderr.contains("couldn't find remote ref") {
        reject_fix_ci_on_issue(is_fix_ci_variant, number)?;
        cook_issue(config, &repo_config, repo_str, number)
    } else {
        anyhow::bail!(
            "Failed to fetch #{} from {}: {}",
            number,
            repo_config.repo,
            stderr.trim()
        )
    }
}

/// Per-kind constants for the gear emitter. `worktree_subdir` doubles as
/// the gear name in the emitted schema (e.g. `pr` worktrees → `"pr"` gear,
/// `issue` worktrees → `"issue"` gear). `url_subdir` is the GitHub URL
/// path segment (`pull` vs `issues`).
struct GearKind {
    worktree_subdir: &'static str,
    description_prefix: &'static str,
    page_description: &'static str,
    url_subdir: &'static str,
}

const PR_KIND: GearKind = GearKind {
    worktree_subdir: "pr",
    description_prefix: "Pull request",
    page_description: "Open the PR page",
    url_subdir: "pull",
};

const ISSUE_KIND: GearKind = GearKind {
    worktree_subdir: "issue",
    description_prefix: "Issue",
    page_description: "Open the issue page",
    url_subdir: "issues",
};

fn build_gear_file_for_kind(
    kind: &GearKind,
    repo: &str,
    number: u64,
) -> enwiro_sdk::gear::GearFileData {
    use enwiro_sdk::gear::{Gear, GearFileData, SCHEMA_VERSION, WebEntry};
    use std::collections::HashMap;

    let page = WebEntry {
        description: kind.page_description.to_string(),
        url: format!("https://github.com/{repo}/{}/{number}", kind.url_subdir),
    };
    let gear_entry = Gear {
        description: format!("{} #{number} on {repo}", kind.description_prefix),
        web: HashMap::from([("page".to_string(), page)]),
        ..Default::default()
    };
    GearFileData {
        version: SCHEMA_VERSION,
        gear: HashMap::from([(kind.worktree_subdir.to_string(), gear_entry)]),
    }
}

fn gear_with_writer<W: Write>(
    config: &ConfigurationValues,
    repo_config: &RepoConfig,
    repo_str: &str,
    number: u64,
    writer: &mut W,
) -> anyhow::Result<()> {
    for kind in [&PR_KIND, &ISSUE_KIND] {
        let path = worktree_path(config, repo_config, repo_str, kind.worktree_subdir, number)?;
        if path.exists() {
            let file = build_gear_file_for_kind(kind, &repo_config.repo, number);
            serde_json::to_writer(writer, &file)?;
            return Ok(());
        }
    }
    anyhow::bail!("No worktree found for {}#{}", repo_str, number)
}

fn gear(config: &ConfigurationValues, args: GearArgs) -> anyhow::Result<()> {
    let (repo_str, number, _is_fix_ci_variant) = parse_recipe_name(&args.recipe_name)?;
    let repo_config = resolve_repo_config(repo_str)?;
    gear_with_writer(
        config,
        &repo_config,
        repo_str,
        number,
        &mut std::io::stdout(),
    )
}

/// Every recipe here cooks to a git worktree (`cook_pr`/`cook_issue`); its
/// `.git` is a pointer into `repo_config.local_path`'s own
/// `.git/worktrees/<name>`, which holds the shared object database and refs
/// the worktree depends on. Report that path so the isolation layer can
/// mount it alongside the worktree -- this cookbook has no notion of *why*
/// that's needed (containers, or anything else). Unlike the plain-git
/// cookbook, there's no "base repo" recipe here to special-case: every PR
/// and issue recipe is a worktree.
fn resolve_external_paths(recipe_name: &str) -> anyhow::Result<Vec<String>> {
    let (repo_str, _number, _is_fix_ci_variant) = parse_recipe_name(recipe_name)?;
    let repo_config = resolve_repo_config(repo_str)?;
    Ok(vec![
        repo_config
            .local_path
            .to_str()
            .context("Could not convert repo local path to string")?
            .to_string(),
    ])
}

fn external_paths(args: ExternalPathsArgs) -> anyhow::Result<()> {
    let paths = resolve_external_paths(&args.recipe_name)?;
    println!("{}", serde_json::to_string(&paths)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn parse_cooked_env_dir_examples() {
        assert_eq!(parse_cooked_env_dir("pr-123"), Some(("pr", 123)));
        assert_eq!(parse_cooked_env_dir("issue-45"), Some(("issue", 45)));
        assert_eq!(parse_cooked_env_dir("issue-0"), Some(("issue", 0)));
        // Rejected: missing number, non-numeric, wrong prefix, overflow, junk.
        assert_eq!(parse_cooked_env_dir("pr-"), None);
        assert_eq!(parse_cooked_env_dir("pr-abc"), None);
        assert_eq!(parse_cooked_env_dir("pr-12x"), None);
        assert_eq!(parse_cooked_env_dir("random"), None);
        assert_eq!(parse_cooked_env_dir("branch-7"), None);
        assert_eq!(parse_cooked_env_dir("pr-99999999999999999999999999"), None);
    }

    proptest! {
        // P2: total over arbitrary input (never panics, incl. overflow/unicode).
        #[test]
        fn parse_cooked_env_dir_never_panics(s in ".*") {
            let _ = parse_cooked_env_dir(&s);
        }

        // P2: round-trips with the worktree dir naming (`{prefix}-{number}`).
        #[test]
        fn parse_cooked_env_dir_round_trips(n in any::<u64>(), is_pr in any::<bool>()) {
            let kind = if is_pr { "pr" } else { "issue" };
            let dir = format!("{kind}-{n}");
            prop_assert_eq!(parse_cooked_env_dir(&dir), Some((kind, n)));
        }
    }

    #[test]
    fn test_parse_recipe_name_valid() {
        let (repo, number, is_fix_ci_variant) = parse_recipe_name("enwiro#42").unwrap();
        assert_eq!(repo, "enwiro");
        assert_eq!(number, 42);
        assert!(!is_fix_ci_variant);
    }

    #[test]
    fn test_parse_recipe_name_fix_ci_variant() {
        let (repo, number, is_fix_ci_variant) = parse_recipe_name("owner/repo#42@fix-ci").unwrap();
        assert_eq!(repo, "owner/repo");
        assert_eq!(number, 42);
        assert!(is_fix_ci_variant);
    }

    #[test]
    fn test_parse_recipe_name_rejects_unknown_variant() {
        let result = parse_recipe_name("owner/repo#42@bogus");
        assert!(result.is_err());
    }

    #[test]
    fn test_item_pattern_recipes_claim_any_number_per_repo() {
        let repos = vec![
            RepoConfig {
                repo: "kantord/enwiro".to_string(),
                local_path: PathBuf::from("/tmp/enwiro"),
            },
            RepoConfig {
                repo: "vercel/next.js".to_string(),
                local_path: PathBuf::from("/tmp/next.js"),
            },
        ];

        let items = item_pattern_recipes(&repos);

        let patterns: Vec<&PatternRecipe> = items
            .iter()
            .map(|item| match item {
                RecipeItem::Pattern(p) => p,
                RecipeItem::Concrete(_) => panic!("expected only pattern items"),
            })
            .collect();
        assert_eq!(patterns.len(), 2);

        for pattern in &patterns {
            enwiro_sdk::recipe_pattern::validate(&pattern.pattern, pattern.description.as_deref())
                .expect("emitted pattern must pass daemon validation");
        }

        // Short repo name, any number - and nothing else. `next.js` must be
        // escaped: the dot may not match arbitrary characters.
        let anchored = enwiro_sdk::recipe_pattern::anchor(&patterns[1].pattern);
        assert!(enwiro_sdk::recipe_pattern::match_name(&anchored, None, "next.js#123").is_some());
        assert!(enwiro_sdk::recipe_pattern::match_name(&anchored, None, "next-js#123").is_none());
        assert!(enwiro_sdk::recipe_pattern::match_name(&anchored, None, "next.js#abc").is_none());
        // Only ASCII digits that fit in u64: the claim must not cover names
        // parse_recipe_name later rejects.
        assert!(enwiro_sdk::recipe_pattern::match_name(&anchored, None, "next.js#٤٢").is_none());
        assert!(
            enwiro_sdk::recipe_pattern::match_name(
                &anchored,
                None,
                "next.js#99999999999999999999999"
            )
            .is_none()
        );
        assert!(
            enwiro_sdk::recipe_pattern::match_name(&anchored, None, "other/next.js#5").is_none()
        );

        let enwiro_anchored = enwiro_sdk::recipe_pattern::anchor(&patterns[0].pattern);
        let matched = enwiro_sdk::recipe_pattern::match_name(
            &enwiro_anchored,
            patterns[0].description.as_deref(),
            "enwiro#997",
        )
        .unwrap();
        assert_eq!(
            matched.description.as_deref(),
            Some("Work on PR or issue #997 in enwiro")
        );

        // The fix-ci goal variant (#756) must also be claimed by the same
        // pattern, for a not-yet-listed PR number.
        assert!(
            enwiro_sdk::recipe_pattern::match_name(&enwiro_anchored, None, "enwiro#997@fix-ci")
                .is_some()
        );
        assert!(
            enwiro_sdk::recipe_pattern::match_name(&enwiro_anchored, None, "enwiro#997@bogus")
                .is_none()
        );
    }

    #[test]
    fn test_item_pattern_recipes_carry_a_valid_url_rule() {
        // URL-matching behavior for this exact rule shape is covered by the
        // extension's router tests (the only production matcher); here we
        // pin the emitted strings and that the daemon's gate accepts them.
        let repos = vec![RepoConfig {
            repo: "kantord/enwiro".to_string(),
            local_path: PathBuf::from("/tmp/enwiro"),
        }];

        let items = item_pattern_recipes(&repos);
        let RecipeItem::Pattern(pattern) = &items[0] else {
            panic!("expected a pattern item");
        };
        let rule = pattern.url.as_ref().expect("pattern must carry a URL rule");
        enwiro_sdk::url_rule::validate(rule).expect("emitted URL rule must pass daemon validation");
        assert_eq!(
            rule.pattern,
            "https://github.com/kantord/enwiro/:kind(pull|issues)/:number([0-9]+){/*}?"
        );
        assert_eq!(rule.recipe, "enwiro#{number}");

        // A name rendered from the template satisfies the name claim.
        let anchored = enwiro_sdk::recipe_pattern::anchor(&pattern.pattern);
        assert!(enwiro_sdk::recipe_pattern::match_name(&anchored, None, "enwiro#42").is_some());
    }

    #[test]
    fn test_item_pattern_recipes_share_short_name_single_claim() {
        let repos = vec![
            RepoConfig {
                repo: "kantord/tool".to_string(),
                local_path: PathBuf::from("/tmp/tool"),
            },
            RepoConfig {
                repo: "acme/tool".to_string(),
                local_path: PathBuf::from("/tmp/tool2"),
            },
        ];

        let items = item_pattern_recipes(&repos);
        assert_eq!(items.len(), 1, "claims stay deduplicated by short name");
        let RecipeItem::Pattern(pattern) = &items[0] else {
            panic!("expected a pattern item");
        };
        let rule = pattern.url.as_ref().unwrap();
        assert!(rule.pattern.contains("github.com/acme/tool"));
    }

    #[test]
    fn test_parse_recipe_name_large_number() {
        let (repo, number, _is_fix_ci_variant) = parse_recipe_name("next.js#12345").unwrap();
        assert_eq!(repo, "next.js");
        assert_eq!(number, 12345);
    }

    #[test]
    fn test_parse_recipe_name_no_hash() {
        let result = parse_recipe_name("enwiro");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_recipe_name_invalid_number() {
        let result = parse_recipe_name("enwiro#abc");
        assert!(result.is_err());
    }

    #[test]
    fn test_build_search_query_pr() {
        let repos = vec!["kantord/enwiro".to_string()];
        let query = build_search_query(&repos, "is:pr is:open");
        assert!(query.contains("repo:kantord/enwiro"));
        assert!(query.contains("is:pr"));
        assert!(query.contains("is:open"));
        assert!(query.contains("sort:updated-desc"));
        assert!(
            query.contains("updated:>"),
            "Should contain date filter, got: {}",
            query
        );
    }

    #[test]
    fn test_build_search_query_issue() {
        let repos = vec!["kantord/enwiro".to_string()];
        let query = build_search_query(&repos, "is:issue is:open assignee:@me");
        assert!(query.contains("repo:kantord/enwiro"));
        assert!(query.contains("is:issue"));
        assert!(query.contains("is:open"));
        assert!(query.contains("assignee:@me"));
        assert!(query.contains("sort:updated-desc"));
    }

    #[test]
    fn test_build_search_query_issue_omits_date_filter() {
        // Assigned issues should never be silently excluded by a date filter -
        // old issues that are still assigned should always appear.
        let repos = vec!["kantord/enwiro".to_string()];
        let query = build_search_query(&repos, "is:issue is:open assignee:@me");
        assert!(
            !query.contains("updated:>"),
            "Issue query must NOT include a date filter, but got: {}",
            query
        );
    }

    #[test]
    fn test_build_search_query_pr_retains_date_filter() {
        // PRs are expected to be recent; keep the 30-day staleness window so
        // the result set stays manageable.
        let repos = vec!["kantord/enwiro".to_string()];
        let query = build_search_query(&repos, "is:pr is:open");
        assert!(
            query.contains("updated:>"),
            "PR query MUST include a date filter, but got: {}",
            query
        );
    }

    #[test]
    fn test_build_search_query_multiple_repos() {
        let repos = vec![
            "kantord/enwiro".to_string(),
            "expressjs/express".to_string(),
        ];
        let query = build_search_query(&repos, "is:pr is:open");
        assert!(query.contains("repo:kantord/enwiro"));
        assert!(query.contains("repo:expressjs/express"));
    }

    #[test]
    fn test_parse_search_response_prs() {
        let json = r#"{
            "data": {
                "search": {
                    "nodes": [
                        {
                            "number": 42,
                            "title": "Fix the thing",
                            "headRefName": "fix-thing",
                            "updatedAt": "2026-02-14T13:10:29Z",
                            "repository": { "nameWithOwner": "kantord/enwiro" }
                        },
                        {
                            "number": 99,
                            "title": "Add feature",
                            "headRefName": "feature/add-stuff",
                            "updatedAt": "2026-02-13T10:00:00Z",
                            "repository": { "nameWithOwner": "expressjs/express" }
                        }
                    ]
                }
            }
        }"#;
        let items = parse_search_response(json).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].number, 42);
        assert_eq!(items[0].title, "Fix the thing");
        assert!(matches!(
            &items[0].kind,
            GithubItemKind::PullRequest { head_ref_name } if head_ref_name == "fix-thing"
        ));
        assert_eq!(items[0].repo, "enwiro");
        assert_eq!(items[1].number, 99);
        assert_eq!(items[1].repo, "express");
    }

    #[test]
    fn test_parse_search_response_issues() {
        let json = r#"{
            "data": {
                "search": {
                    "nodes": [
                        {
                            "number": 225,
                            "title": "Discover GitHub Issues",
                            "updatedAt": "2026-02-14T13:10:29Z",
                            "repository": { "nameWithOwner": "kantord/enwiro" }
                        },
                        {
                            "number": 100,
                            "title": "Fix login bug",
                            "updatedAt": "2026-02-13T10:00:00Z",
                            "repository": { "nameWithOwner": "expressjs/express" }
                        }
                    ]
                }
            }
        }"#;
        let items = parse_search_response(json).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].number, 225);
        assert_eq!(items[0].title, "Discover GitHub Issues");
        assert!(matches!(&items[0].kind, GithubItemKind::Issue));
        assert_eq!(items[0].repo, "enwiro");
        assert_eq!(items[1].number, 100);
        assert!(matches!(&items[1].kind, GithubItemKind::Issue));
    }

    #[test]
    fn test_parse_search_response_empty_nodes() {
        let json = r#"{"data": {"search": {"nodes": []}}}"#;
        let items = parse_search_response(json).unwrap();
        assert!(items.is_empty());
    }

    #[test]
    fn test_parse_github_remote_ssh() {
        assert_eq!(
            parse_github_remote("git@github.com:kantord/enwiro.git"),
            Some("kantord/enwiro".to_string())
        );
    }

    #[test]
    fn test_parse_github_remote_https_with_git_suffix() {
        assert_eq!(
            parse_github_remote("https://github.com/kantord/enwiro.git"),
            Some("kantord/enwiro".to_string())
        );
    }

    #[test]
    fn test_parse_github_remote_https_without_git_suffix() {
        assert_eq!(
            parse_github_remote("https://github.com/kantord/enwiro"),
            Some("kantord/enwiro".to_string())
        );
    }

    #[test]
    fn test_parse_github_remote_ssh_protocol() {
        assert_eq!(
            parse_github_remote("ssh://git@github.com/kantord/enwiro.git"),
            Some("kantord/enwiro".to_string())
        );
    }

    #[test]
    fn test_parse_github_remote_gitlab_returns_none() {
        assert_eq!(
            parse_github_remote("git@gitlab.com:kantord/project.git"),
            None
        );
    }

    #[test]
    fn test_parse_github_remote_empty_string() {
        assert_eq!(parse_github_remote(""), None);
    }

    #[test]
    fn test_discover_finds_github_repo() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_path = tmp.path().join("enwiro");
        std::fs::create_dir(&repo_path).unwrap();
        let repo = git2::Repository::init(&repo_path).unwrap();
        repo.remote("origin", "git@github.com:kantord/enwiro.git")
            .unwrap();

        let git_config = GitCookbookConfig {
            repo_globs: vec![tmp.path().join("*").to_str().unwrap().to_string()],
        };

        let repos = discover_github_repos_from_config(&git_config).unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].repo, "kantord/enwiro");
        assert_eq!(repos[0].local_path, repo_path.canonicalize().unwrap());
    }

    #[test]
    fn test_discover_skips_non_github_repo() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_path = tmp.path().join("project");
        std::fs::create_dir(&repo_path).unwrap();
        let repo = git2::Repository::init(&repo_path).unwrap();
        repo.remote("origin", "git@gitlab.com:kantord/project.git")
            .unwrap();

        let git_config = GitCookbookConfig {
            repo_globs: vec![tmp.path().join("*").to_str().unwrap().to_string()],
        };

        let repos = discover_github_repos_from_config(&git_config).unwrap();
        assert_eq!(repos.len(), 0);
    }

    #[test]
    fn test_discover_skips_repo_without_origin() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_path = tmp.path().join("project");
        std::fs::create_dir(&repo_path).unwrap();
        git2::Repository::init(&repo_path).unwrap();

        let git_config = GitCookbookConfig {
            repo_globs: vec![tmp.path().join("*").to_str().unwrap().to_string()],
        };

        let repos = discover_github_repos_from_config(&git_config).unwrap();
        assert_eq!(repos.len(), 0);
    }

    #[test]
    fn test_discover_skips_non_repo_directory() {
        let tmp = tempfile::TempDir::new().unwrap();
        let not_a_repo = tmp.path().join("just-a-folder");
        std::fs::create_dir(&not_a_repo).unwrap();

        let git_config = GitCookbookConfig {
            repo_globs: vec![tmp.path().join("*").to_str().unwrap().to_string()],
        };

        let repos = discover_github_repos_from_config(&git_config).unwrap();
        assert_eq!(repos.len(), 0);
    }

    #[test]
    fn test_discover_deduplicates_repos() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_path = tmp.path().join("enwiro");
        std::fs::create_dir(&repo_path).unwrap();
        let repo = git2::Repository::init(&repo_path).unwrap();
        repo.remote("origin", "git@github.com:kantord/enwiro.git")
            .unwrap();

        let git_config = GitCookbookConfig {
            repo_globs: vec![
                tmp.path().join("*").to_str().unwrap().to_string(),
                repo_path.to_str().unwrap().to_string(),
            ],
        };

        let repos = discover_github_repos_from_config(&git_config).unwrap();
        assert_eq!(repos.len(), 1);
    }

    #[test]
    fn test_default_worktree_dir_is_absolute() {
        let dir = default_worktree_dir().unwrap();
        assert!(
            dir.is_absolute(),
            "default_worktree_dir should return an absolute path, got: {:?}",
            dir
        );
    }

    #[test]
    fn test_worktree_base_dir_uses_config() {
        let config = ConfigurationValues {
            worktree_dir: Some("/tmp/my-pr-worktrees".to_string()),
            ..Default::default()
        };
        let dir = worktree_base_dir(&config).unwrap();
        assert_eq!(dir, PathBuf::from("/tmp/my-pr-worktrees"));
    }

    #[test]
    fn test_reject_fix_ci_on_issue_errors_for_fix_ci_variant() {
        let result = reject_fix_ci_on_issue(true, 42);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("fix-ci"));
    }

    #[test]
    fn test_reject_fix_ci_on_issue_allows_plain_variant() {
        assert!(reject_fix_ci_on_issue(false, 42).is_ok());
    }

    #[test]
    fn test_cook_creates_worktree_for_pr() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_path = tmp.path().join("my-project");
        std::fs::create_dir(&repo_path).unwrap();

        // Create a repo with an initial commit
        let repo = git2::Repository::init(&repo_path).unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();

        // Create a branch simulating a fetched PR ref
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("pr-42", &head, false).unwrap();

        let wt_dir = tmp.path().join("worktrees");
        let repo_config = RepoConfig {
            repo: "kantord/enwiro".to_string(),
            local_path: repo_path.clone(),
        };

        // Simulate what cook does after the fetch step
        let path_hash = short_path_hash(&repo_config.local_path);
        let wt_path = wt_dir
            .join(format!("kantord-enwiro-{}", path_hash))
            .join("pr-42");

        std::fs::create_dir_all(wt_path.parent().unwrap()).unwrap();

        let branch = repo.find_branch("pr-42", git2::BranchType::Local).unwrap();
        let reference = branch.into_reference();
        let mut opts = git2::WorktreeAddOptions::new();
        opts.reference(Some(&reference));
        repo.worktree("enwiro-pr-42", &wt_path, Some(&opts))
            .unwrap();

        assert!(wt_path.exists(), "Worktree path should exist on disk");
        let wt_repo = git2::Repository::open(&wt_path).unwrap();
        assert!(wt_repo.is_worktree(), "Should be a git worktree");
    }

    #[test]
    fn test_cook_creates_worktree_for_issue() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_path = tmp.path().join("my-project");
        std::fs::create_dir(&repo_path).unwrap();

        // Create a repo with an initial commit on "main"
        let repo = git2::Repository::init(&repo_path).unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let commit_oid = repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();

        // Create an issue branch from the initial commit
        let commit = repo.find_commit(commit_oid).unwrap();
        repo.branch("issue-225", &commit, false).unwrap();

        let wt_dir = tmp.path().join("worktrees");
        let path_hash = short_path_hash(&repo_path);
        let wt_path = wt_dir
            .join(format!("my-project-{}", path_hash))
            .join("issue-225");

        std::fs::create_dir_all(wt_path.parent().unwrap()).unwrap();

        let branch = repo
            .find_branch("issue-225", git2::BranchType::Local)
            .unwrap();
        let reference = branch.into_reference();
        let mut opts = git2::WorktreeAddOptions::new();
        opts.reference(Some(&reference));
        repo.worktree("enwiro-issue-225", &wt_path, Some(&opts))
            .unwrap();

        assert!(wt_path.exists(), "Worktree path should exist on disk");
        let wt_repo = git2::Repository::open(&wt_path).unwrap();
        assert!(wt_repo.is_worktree(), "Should be a git worktree");
    }

    /// Helper: create a repo with an initial commit and a remote "origin"
    /// pointing at `origin_path`. Returns the cloned repo.
    fn setup_repo_with_origin(local_path: &Path, origin_path: &Path) -> git2::Repository {
        // Create the "origin" bare repo
        let origin = git2::Repository::init_bare(origin_path).unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let tree_id = origin.index().unwrap().write_tree().unwrap();
        let tree = origin.find_tree(tree_id).unwrap();
        origin
            .commit(Some("refs/heads/main"), &sig, &sig, "initial", &tree, &[])
            .unwrap();

        // Clone it
        let repo = git2::build::RepoBuilder::new()
            .clone(origin_path.to_str().unwrap(), local_path)
            .unwrap();
        repo
    }

    #[test]
    fn test_get_default_branch_uses_origin_head() {
        let tmp = tempfile::TempDir::new().unwrap();
        let origin_path = tmp.path().join("origin.git");
        let local_path = tmp.path().join("local");
        let repo = setup_repo_with_origin(&local_path, &origin_path);

        // Clone sets origin/HEAD automatically
        let branch = get_default_branch(&repo).unwrap();
        assert_eq!(branch, "main");
    }

    #[test]
    fn test_get_default_branch_falls_back_to_main() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_path = tmp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        let repo = git2::Repository::init(&repo_path).unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();

        // Add a remote with a "main" branch ref but no HEAD
        repo.remote("origin", "https://example.com/fake.git")
            .unwrap();
        let head_commit = repo.head().unwrap().peel_to_commit().unwrap();
        repo.reference(
            "refs/remotes/origin/main",
            head_commit.id(),
            false,
            "fake remote ref",
        )
        .unwrap();

        let branch = get_default_branch(&repo).unwrap();
        assert_eq!(branch, "main");
    }

    #[test]
    fn test_get_default_branch_falls_back_to_master() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_path = tmp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        let repo = git2::Repository::init(&repo_path).unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();

        // Add a remote with only a "master" branch ref (no HEAD, no main)
        repo.remote("origin", "https://example.com/fake.git")
            .unwrap();
        let head_commit = repo.head().unwrap().peel_to_commit().unwrap();
        repo.reference(
            "refs/remotes/origin/master",
            head_commit.id(),
            false,
            "fake remote ref",
        )
        .unwrap();

        let branch = get_default_branch(&repo).unwrap();
        assert_eq!(branch, "master");
    }

    #[test]
    fn test_parse_search_response_captures_updated_at() {
        let json = r#"{
            "data": {
                "search": {
                    "nodes": [
                        {
                            "number": 42,
                            "title": "Fix the thing",
                            "headRefName": "fix-thing",
                            "updatedAt": "2026-02-14T13:10:29Z",
                            "repository": { "nameWithOwner": "kantord/enwiro" }
                        },
                        {
                            "number": 225,
                            "title": "Bug report",
                            "updatedAt": "2026-02-12T09:00:00Z",
                            "repository": { "nameWithOwner": "kantord/enwiro" }
                        }
                    ]
                }
            }
        }"#;
        let items = parse_search_response(json).unwrap();
        assert_eq!(items[0].updated_at, "2026-02-14T13:10:29Z");
        assert_eq!(items[1].updated_at, "2026-02-12T09:00:00Z");
    }

    #[test]
    fn test_list_recipes_sorts_combined_items_by_date() {
        // When PRs and issues are combined, they should be sorted by
        // updated_at descending (newest first), not grouped by type.
        let mut items = vec![
            GithubItem {
                number: 10,
                title: "Old PR".to_string(),
                repo: "enwiro".to_string(),
                kind: GithubItemKind::PullRequest {
                    head_ref_name: "old-pr".to_string(),
                },
                updated_at: "2026-02-01T00:00:00Z".to_string(),
            },
            GithubItem {
                number: 20,
                title: "Recent issue".to_string(),
                repo: "enwiro".to_string(),
                kind: GithubItemKind::Issue,
                updated_at: "2026-02-15T00:00:00Z".to_string(),
            },
            GithubItem {
                number: 30,
                title: "Newest PR".to_string(),
                repo: "enwiro".to_string(),
                kind: GithubItemKind::PullRequest {
                    head_ref_name: "newest-pr".to_string(),
                },
                updated_at: "2026-02-18T00:00:00Z".to_string(),
            },
        ];

        sort_items_by_date(&mut items);

        assert_eq!(items[0].number, 30, "Newest PR should be first");
        assert_eq!(items[1].number, 20, "Recent issue should be second");
        assert_eq!(items[2].number, 10, "Old PR should be last");
    }

    #[test]
    fn test_cooked_branch_name_matches_worktree_branch() {
        let pr = GithubItem {
            number: 42,
            title: "x".to_string(),
            repo: "enwiro".to_string(),
            kind: GithubItemKind::PullRequest {
                head_ref_name: "feature-x".to_string(),
            },
            updated_at: String::new(),
        };
        let issue = GithubItem {
            number: 7,
            title: "x".to_string(),
            repo: "enwiro".to_string(),
            kind: GithubItemKind::Issue,
            updated_at: String::new(),
        };
        // Must equal the branch cook_pr/cook_issue check out, so the git
        // cookbook's `repo@<branch>` recipe matches the equivalence alias.
        assert_eq!(cooked_branch_name(&pr), "pr-42");
        assert_eq!(cooked_branch_name(&issue), "issue-7");
    }

    #[test]
    fn test_recipes_for_item_issue_yields_one_recipe_with_github_issue_goal() {
        let item = GithubItem {
            number: 42,
            title: "Fix auth bug".to_string(),
            repo: "owner/repo".to_string(),
            kind: GithubItemKind::Issue,
            updated_at: String::new(),
        };
        let recipes = recipes_for_item(&item, 0, 1, &std::collections::HashMap::new());
        assert_eq!(recipes.len(), 1);
        assert_eq!(recipes[0].name, "owner/repo#42");
        assert_eq!(
            recipes[0].goal.as_ref().map(|g| g.kind.as_str()),
            Some("github_issue")
        );
    }

    #[test]
    fn test_recipes_for_item_pr_yields_work_on_and_fix_ci_variants() {
        let item = GithubItem {
            number: 42,
            title: "Add feature".to_string(),
            repo: "owner/repo".to_string(),
            kind: GithubItemKind::PullRequest {
                head_ref_name: "feature".to_string(),
            },
            updated_at: String::new(),
        };
        let recipes = recipes_for_item(&item, 0, 1, &std::collections::HashMap::new());
        assert_eq!(recipes.len(), 2);
        assert_eq!(recipes[0].name, "owner/repo#42");
        assert_eq!(
            recipes[0].goal.as_ref().map(|g| g.kind.as_str()),
            Some("work_on")
        );
        assert_eq!(recipes[1].name, "owner/repo#42@fix-ci");
        assert_eq!(
            recipes[1].goal.as_ref().map(|g| g.kind.as_str()),
            Some("fix_ci")
        );
    }

    #[test]
    fn test_recipes_for_item_pr_variants_share_equivalent_to() {
        let item = GithubItem {
            number: 42,
            title: "Add feature".to_string(),
            repo: "owner/repo".to_string(),
            kind: GithubItemKind::PullRequest {
                head_ref_name: "feature".to_string(),
            },
            updated_at: String::new(),
        };
        let mut display_names = std::collections::HashMap::new();
        display_names.insert("owner/repo".to_string(), "repo".to_string());

        let recipes = recipes_for_item(&item, 0, 1, &display_names);
        assert_eq!(recipes.len(), 2);
        assert_eq!(recipes[0].equivalent_to, vec!["repo@pr-42".to_string()]);
        assert_eq!(recipes[1].equivalent_to, recipes[0].equivalent_to);
    }

    #[test]
    fn test_git_repo_display_name_is_path_basename() {
        assert_eq!(
            git_repo_display_name(Path::new("/home/me/code/enwiro")).as_deref(),
            Some("enwiro")
        );
    }

    #[test]
    fn test_get_default_branch_errors_when_no_candidates() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo_path = tmp.path().join("repo");
        std::fs::create_dir(&repo_path).unwrap();
        let repo = git2::Repository::init(&repo_path).unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();

        // Remote exists but has no refs at all
        repo.remote("origin", "https://example.com/fake.git")
            .unwrap();

        let result = get_default_branch(&repo);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Could not determine default branch"),
            "Expected helpful error, got: {}",
            err
        );
    }

    #[test]
    fn test_compute_sort_order_single_item() {
        assert_eq!(compute_sort_order(0, 1), 0);
    }

    #[test]
    fn test_compute_sort_order_two_items() {
        assert_eq!(compute_sort_order(0, 2), 0);
        assert_eq!(compute_sort_order(1, 2), 100);
    }

    #[test]
    fn test_compute_sort_order_three_items() {
        assert_eq!(compute_sort_order(0, 3), 0);
        assert_eq!(compute_sort_order(1, 3), 50);
        assert_eq!(compute_sort_order(2, 3), 100);
    }

    mod interpret_gh_output_tests {
        use super::*;

        /// Minimal valid GraphQL JSON with one PR node, used as a fixture for the
        /// truncation-warning tests below.
        fn gh_output_with_one_pr() -> Vec<u8> {
            r#"{
            "data": {
                "search": {
                    "nodes": [
                        {
                            "number": 42,
                            "title": "Fix the thing",
                            "headRefName": "fix-thing",
                            "updatedAt": "2026-02-14T13:10:29Z",
                            "repository": { "nameWithOwner": "kantord/enwiro" }
                        }
                    ]
                }
            }
        }"#
            .as_bytes()
            .to_vec()
        }

        /// When gh exits non-zero AND stderr contains the 100-result truncation
        /// warning AND stdout is valid JSON, `interpret_gh_output` must return the
        /// parsed items instead of an error.
        #[test]
        fn test_interpret_gh_output_truncation_warning_returns_partial_results() {
            let stdout = gh_output_with_one_pr();
            let stderr =
                b"GitHub search returned 100 results (the maximum). Some results may be missing."
                    .to_vec();

            // success = false simulates a non-zero exit code
            let result = interpret_gh_output(&stdout, &stderr, false);

            assert!(
                result.is_ok(),
                "Expected Ok with partial results, got Err: {:?}",
                result.unwrap_err()
            );
            let items = result.unwrap();
            assert_eq!(
                items.len(),
                1,
                "Expected 1 parsed item from partial results, got {}",
                items.len()
            );
            assert_eq!(items[0].number, 42);
        }

        /// When gh exits non-zero AND stderr does NOT contain the truncation
        /// warning, `interpret_gh_output` must still return an error (the original
        /// bail! behaviour must be preserved for real failures).
        #[test]
        fn test_interpret_gh_output_real_failure_still_errors() {
            let stdout = gh_output_with_one_pr();
            let stderr = b"some other gh error: authentication failed".to_vec();

            let result = interpret_gh_output(&stdout, &stderr, false);

            assert!(
                result.is_err(),
                "Expected Err for a real gh failure, but got Ok"
            );
        }
    }

    mod gear_subcommand {
        use super::*;

        /// Set up a `(config, repo_config, tmp)` triple for gear tests. The
        /// returned tempdir keeps `config.worktree_dir` and the local repo
        /// path alive for the test's lifetime.
        fn setup_gear_test() -> (ConfigurationValues, RepoConfig, tempfile::TempDir) {
            let tmp = tempfile::TempDir::new().unwrap();
            let local_path = tmp.path().join("enwiro");
            std::fs::create_dir(&local_path).unwrap();
            let repo_config = RepoConfig {
                repo: "kantord/enwiro".to_string(),
                local_path,
            };
            let config = ConfigurationValues {
                worktree_dir: Some(tmp.path().join("worktrees").to_str().unwrap().to_string()),
            };
            (config, repo_config, tmp)
        }

        /// Run `gear_with_writer` with a worktree of `kind` (`"pr"` or
        /// `"issue"`) pre-created, and assert the emitted JSON matches
        /// `expected`. Captures the full happy-path shape so callers stay
        /// declarative.
        fn assert_gear_emits(kind: &str, number: u64, expected: serde_json::Value) {
            let (config, repo_config, _tmp) = setup_gear_test();
            let path = worktree_path(&config, &repo_config, "enwiro", kind, number).unwrap();
            std::fs::create_dir_all(&path).unwrap();

            let mut output = Vec::new();
            gear_with_writer(&config, &repo_config, "enwiro", number, &mut output).unwrap();

            let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
            assert_eq!(json, expected);
        }

        #[test]
        fn outputs_pull_request_url_when_pr_worktree_exists() {
            assert_gear_emits(
                "pr",
                42,
                serde_json::json!({
                    "version": 1,
                    "gear": {
                        "pr": {
                            "description": "Pull request #42 on kantord/enwiro",
                            "web": {
                                "page": {
                                    "description": "Open the PR page",
                                    "url": "https://github.com/kantord/enwiro/pull/42"
                                }
                            }
                        }
                    }
                }),
            );
        }

        #[test]
        fn outputs_issue_url_when_issue_worktree_exists() {
            assert_gear_emits(
                "issue",
                309,
                serde_json::json!({
                    "version": 1,
                    "gear": {
                        "issue": {
                            "description": "Issue #309 on kantord/enwiro",
                            "web": {
                                "page": {
                                    "description": "Open the issue page",
                                    "url": "https://github.com/kantord/enwiro/issues/309"
                                }
                            }
                        }
                    }
                }),
            );
        }

        #[test]
        fn errors_when_no_worktree_exists() {
            let (config, repo_config, _tmp) = setup_gear_test();
            let mut output = Vec::new();
            let result = gear_with_writer(&config, &repo_config, "enwiro", 42, &mut output);
            assert!(
                result.is_err(),
                "Expected error when no worktree exists, but got Ok"
            );
        }
    }
}

fn read_config() -> anyhow::Result<ConfigurationValues> {
    let payload =
        CookbookPayload::read_from_stdin().context("Could not read cookbook payload from stdin")?;
    let config: ConfigurationValues = serde_json::from_value(payload.config)
        .context("Could not deserialize cookbook-github configuration")?;
    tracing::debug!("Config loaded, repos will be auto-discovered from git cookbook");
    Ok(config)
}

fn main() -> anyhow::Result<()> {
    let _guard = enwiro_sdk::init_logging("enwiro-cookbook-github.log");

    let args = EnwiroCookbookGithub::parse();

    match args {
        EnwiroCookbookGithub::Core(CookbookCore::ListRecipes(_)) => {
            list_recipes()?;
        }
        EnwiroCookbookGithub::Core(CookbookCore::Cook(args)) => {
            let config = read_config()?;
            cook(&config, args)?;
        }
        EnwiroCookbookGithub::Gear(args) => {
            let config = read_config()?;
            gear(&config, args)?;
        }
        EnwiroCookbookGithub::ExternalPaths(args) => {
            external_paths(args)?;
        }
        EnwiroCookbookGithub::Core(CookbookCore::Metadata) => {
            println!(
                "{}",
                CookbookMetadata {
                    capabilities: DeclaredCapabilities::declare([CookbookCapability::Listen]),
                    default_priority: Some(30),
                    project_overridable: vec![],
                }
                .to_json()
            );
        }
        EnwiroCookbookGithub::Listen => {
            let payload = CookbookPayload::read_first_line_from_stdin()
                .context("Could not read cookbook payload from stdin")?;
            let config: ConfigurationValues = serde_json::from_value(payload.config)
                .context("Could not deserialize cookbook-github configuration")?;
            // Recipes already confirmed done are cached so we stop re-querying
            // `gh` for them on every tick (#302).
            let mut done_cache: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            enwiro_sdk::listen::serve_updates(LISTEN_POLL_INTERVAL, move || {
                let mut updates = vec![enwiro_sdk::listen::RecipeUpdate::Recipes {
                    data: collect_recipe_items(),
                }];
                updates.extend(collect_status_events(&config, &mut done_cache));
                updates
            });
        }
    };

    Ok(())
}
