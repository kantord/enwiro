use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Context;
use clap::Parser;
use enwiro_sdk::cli::{CookArgs, CookbookCore};
use enwiro_sdk::cookbook::CookbookCapability;
use enwiro_sdk::metadata::DeclaredCapabilities;
use enwiro_sdk::{CookbookMetadata, CookbookPayload, PatternRecipe, Recipe, RecipeItem};
use git2::Repository;
use serde_derive::{Deserialize, Serialize};
#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ConfigurationValues {
    pub repo_globs: Vec<String>,
    pub worktree_dir: Option<String>,
}

fn short_path_hash(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(path.to_string_lossy().as_bytes());
    hex::encode(hash)[..8].to_string()
}

fn default_worktree_dir() -> anyhow::Result<PathBuf> {
    let base = dirs::data_dir().context("Could not determine data directory (is $HOME set?)")?;
    Ok(base.join("enwiro").join("worktrees"))
}

fn worktree_base_dir(config: &ConfigurationValues) -> anyhow::Result<PathBuf> {
    match &config.worktree_dir {
        Some(dir) => Ok(PathBuf::from(dir)),
        None => default_worktree_dir(),
    }
}

fn resolve_recipe_path(config: &ConfigurationValues, recipe_name: &str) -> anyhow::Result<PathBuf> {
    let recipes = build_repository_hashmap(config)?;
    match recipes.get(recipe_name) {
        Some(RecipeInfo::ExistingRepo { repo, .. }) => {
            let workdir = repo
                .workdir()
                .context("Could not get working directory of repo")?;
            Ok(workdir.to_path_buf())
        }
        Some(RecipeInfo::Branch {
            repo_path,
            branch_name,
            is_remote,
            ..
        }) => resolve_branch_worktree(config, repo_path, branch_name, *is_remote),
        None => cook_new_branch(config, &recipes, recipe_name),
    }
}

/// Pattern-routed cook (#246): `repo@branch` names discovery didn't list -
/// usually because the branch doesn't exist yet, but also the stale-cache
/// race where it appeared after discovery ran.
fn cook_new_branch(
    config: &ConfigurationValues,
    recipes: &HashMap<String, RecipeInfo>,
    recipe_name: &str,
) -> anyhow::Result<PathBuf> {
    let (repo_name, branch_name) = recipe_name
        .split_once('@')
        .with_context(|| format!("Could not find recipe {}", recipe_name))?;
    let Some(RecipeInfo::ExistingRepo { repo, .. }) = recipes.get(repo_name) else {
        anyhow::bail!(
            "Could not find recipe {} (no repository named '{}')",
            recipe_name,
            repo_name
        );
    };
    // Canonicalize: `workdir()` carries a trailing separator, and the
    // worktree layout hashes the path string, so it must match the
    // canonicalized form branch recipes use.
    let repo_path = repo
        .workdir()
        .context("Could not get working directory of repo")?
        .canonicalize()
        .context("Could not canonicalize repo path")?;
    ensure_local_branch(repo, branch_name)?;
    resolve_branch_worktree(config, &repo_path, branch_name, false)
}

fn ensure_local_branch(repo: &Repository, branch_name: &str) -> anyhow::Result<()> {
    if repo
        .find_branch(branch_name, git2::BranchType::Local)
        .is_ok()
    {
        tracing::debug!(branch = %branch_name, "Reusing existing branch");
        return Ok(());
    }
    let fork_point = fork_point_commit(repo)?;
    repo.branch(branch_name, &fork_point, false)
        .with_context(|| format!("Could not create branch {}", branch_name))?;
    tracing::debug!(branch = %branch_name, "Created new branch");
    Ok(())
}

/// No `git fetch` first (unlike the github cookbook's issue-branch path):
/// this cookbook never touches the network, so the fork point is the local
/// view of the remote default branch - or plain HEAD for remote-less repos.
fn fork_point_commit(repo: &Repository) -> anyhow::Result<git2::Commit<'_>> {
    if let Some(default_branch) = enwiro_sdk::git::remote_default_branch(repo)
        && let Ok(reference) =
            repo.find_reference(&format!("refs/remotes/origin/{}", default_branch))
        && let Ok(commit) = reference.peel_to_commit()
    {
        return Ok(commit);
    }
    repo.head()
        .context("Could not resolve repo HEAD")?
        .peel_to_commit()
        .context("Could not resolve HEAD to a commit")
}

/// The short branch name (remote prefix stripped) and the worktree path a
/// branch recipe cooks to. Worktrees live at
/// `<base>/<repo>-<path_hash>/<branch>-<branch_hash>`; the hashes disambiguate
/// same-named repos in different directories and slash-vs-dash branch names.
fn branch_worktree_layout(
    config: &ConfigurationValues,
    repo_path: &Path,
    branch_name: &str,
    is_remote: bool,
) -> anyhow::Result<(String, PathBuf)> {
    let repo_name = repo_path
        .file_name()
        .context("Could not get repo directory name")?
        .to_str()
        .context("Could not convert repo name to string")?;
    let path_hash = short_path_hash(repo_path);

    let short_name = if is_remote {
        branch_name.split('/').skip(1).collect::<Vec<_>>().join("/")
    } else {
        branch_name.to_string()
    };
    let branch_hash = short_path_hash(Path::new(&short_name));
    let flat_name = format!("{}-{}", short_name.replace('/', "-"), branch_hash);

    let wt_path = worktree_base_dir(config)?
        .join(format!("{}-{}", repo_name, path_hash))
        .join(&flat_name);
    Ok((short_name, wt_path))
}

/// Resolve the git reference a new worktree should check out, creating a local
/// branch from the remote tracking branch when the recipe is a remote branch.
fn resolve_branch_reference<'repo>(
    repo: &'repo Repository,
    branch_name: &str,
    short_name: &str,
    is_remote: bool,
) -> anyhow::Result<git2::Reference<'repo>> {
    if is_remote {
        let remote_branch = repo
            .find_branch(branch_name, git2::BranchType::Remote)
            .with_context(|| format!("Could not find remote branch {}", branch_name))?;
        let commit = remote_branch
            .get()
            .peel_to_commit()
            .context("Could not resolve remote branch to commit")?;
        let local_branch = repo
            .branch(short_name, &commit, false)
            .with_context(|| format!("Could not create local branch {}", short_name))?;
        Ok(local_branch.into_reference())
    } else {
        let branch = repo
            .find_branch(branch_name, git2::BranchType::Local)
            .with_context(|| format!("Could not find branch {}", branch_name))?;
        Ok(branch.into_reference())
    }
}

/// Cook a branch recipe: return the existing worktree path, or create a new
/// worktree for the branch and return its path.
fn resolve_branch_worktree(
    config: &ConfigurationValues,
    repo_path: &Path,
    branch_name: &str,
    is_remote: bool,
) -> anyhow::Result<PathBuf> {
    let repo =
        Repository::open(repo_path).context("Could not open repository for worktree creation")?;
    let (short_name, wt_path) = branch_worktree_layout(config, repo_path, branch_name, is_remote)?;

    if wt_path.exists() {
        return Ok(wt_path);
    }
    std::fs::create_dir_all(wt_path.parent().unwrap())
        .context("Could not create worktree directory")?;

    let reference = resolve_branch_reference(&repo, branch_name, &short_name, is_remote)?;

    // Worktree name must be unique within the repo; mirror the flat dir name.
    let wt_name = format!("enwiro-{}", wt_path.file_name().unwrap().to_string_lossy());
    let mut opts = git2::WorktreeAddOptions::new();
    opts.reference(Some(&reference));
    repo.worktree(&wt_name, &wt_path, Some(&opts))
        .map_err(|err| {
            if err.class() == git2::ErrorClass::Worktree
                && err.message().contains("already checked out")
            {
                anyhow::anyhow!(
                    "Branch '{}' is already checked out in another worktree - switch away from it first",
                    branch_name
                )
            } else {
                anyhow::anyhow!(err)
                    .context(format!("Could not create worktree for branch {}", branch_name))
            }
        })?;

    tracing::debug!(path = %wt_path.display(), branch = %branch_name, "Created worktree");
    Ok(wt_path)
}

enum RecipeInfo {
    ExistingRepo {
        repo: Repository,
        commit_epoch: i64,
    },
    Branch {
        repo_path: PathBuf,
        branch_name: String,
        is_remote: bool,
        commit_epoch: i64,
    },
}

impl RecipeInfo {
    fn commit_epoch(&self) -> i64 {
        match self {
            RecipeInfo::ExistingRepo { commit_epoch, .. }
            | RecipeInfo::Branch { commit_epoch, .. } => *commit_epoch,
        }
    }
}

fn head_commit_epoch(repo: &Repository) -> i64 {
    repo.head()
        .ok()
        .and_then(|r| r.peel_to_commit().ok())
        .map(|c| c.time().seconds())
        .unwrap_or(0)
}

/// Return the most recent modification time among local git metadata files.
/// Checks `.git/index` (updated by add, checkout, merge, commit) and
/// `.git/HEAD` (updated by init, branch switch).  This reflects actual
/// user activity rather than upstream commit timestamps.
fn repo_activity_epoch(repo: &Repository) -> i64 {
    let git_dir = repo.path();
    [git_dir.join("index"), git_dir.join("HEAD")]
        .iter()
        .filter_map(|p| p.metadata().ok())
        .filter_map(|m| m.modified().ok())
        .filter_map(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .max()
        .unwrap_or(0)
}

#[derive(Parser)]
enum EnwiroCookbookGit {
    #[command(flatten)]
    Core(CookbookCore),
    ExternalPaths(ExternalPathsArgs),
    Listen,
}

const LISTEN_POLL_INTERVAL: Duration = Duration::from_secs(30);

#[derive(clap::Args)]
pub struct ExternalPathsArgs {
    recipe_name: String,
}

/// Insert a `repo@<worktree>` recipe for every non-enwiro worktree of `repo`.
/// enwiro-managed worktrees are implementation details behind branch recipes
/// and stay invisible.
fn discover_worktree_recipes(
    repo: &Repository,
    repo_name: &str,
    results: &mut HashMap<String, RecipeInfo>,
) {
    let Ok(worktrees) = repo.worktrees() else {
        return;
    };
    for wt_name in worktrees.iter().flatten().flatten() {
        if wt_name.starts_with("enwiro-") {
            tracing::debug!(worktree = %wt_name, "Skipping enwiro-managed worktree");
            continue;
        }
        match repo.find_worktree(wt_name) {
            Ok(wt) => match Repository::open(wt.path()) {
                Ok(wt_repo) => {
                    let compound_name = format!("{}@{}", repo_name, wt_name);
                    tracing::debug!(name = %compound_name, "Found git worktree");
                    let epoch = head_commit_epoch(&wt_repo);
                    results.insert(
                        compound_name,
                        RecipeInfo::ExistingRepo {
                            repo: wt_repo,
                            commit_epoch: epoch,
                        },
                    );
                }
                Err(e) => {
                    tracing::debug!(worktree = %wt_name, error = %e, "Failed to open worktree as repository");
                }
            },
            Err(e) => {
                tracing::debug!(worktree = %wt_name, error = %e, "Failed to find worktree");
            }
        }
    }
}

/// Short names of branches already checked out in the main working tree or any
/// worktree, including enwiro-managed ones. git refuses to create a worktree
/// for a branch that is already checked out elsewhere, so a recipe for such a
/// branch could never be cooked - it would only ever hit the "already checked
/// out" error in `resolve_branch_worktree`. Branch discovery skips these so we
/// never offer a predictably un-cookable recipe. (This is also what collapses
/// the duplicate left behind once enwiro cooks a branch: the cooked worktree
/// holds the branch, so the branch recipe drops out on its own.)
fn checked_out_branch_names(repo: &Repository) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();

    // The main working tree's own branch is checked out too, but it is not
    // enumerated by `worktrees()`, so account for it explicitly.
    if let Ok(head) = repo.head()
        && head.is_branch()
        && let Ok(name) = head.shorthand()
    {
        names.insert(name.to_string());
    }

    let Ok(worktrees) = repo.worktrees() else {
        return names;
    };
    for wt_name in worktrees.iter().flatten().flatten() {
        if let Ok(wt) = repo.find_worktree(wt_name)
            && let Ok(wt_repo) = Repository::open(wt.path())
            && let Ok(wt_head) = wt_repo.head()
            && let Ok(name) = wt_head.shorthand()
        {
            names.insert(name.to_string());
        }
    }
    names
}

/// Insert a `repo@<branch>` recipe for every branch not already checked out.
/// Local branches are iterated first so they take priority over remote
/// tracking branches with the same short name.
fn discover_branch_recipes(
    repo: &Repository,
    repo_name: &str,
    repo_abs_path: &Path,
    checked_out_branches: &std::collections::HashSet<String>,
    results: &mut HashMap<String, RecipeInfo>,
) {
    for &bt in &[git2::BranchType::Local, git2::BranchType::Remote] {
        let Ok(branches) = repo.branches(Some(bt)) else {
            continue;
        };
        for (branch, branch_type) in branches.flatten() {
            let Ok(Some(name)) = branch.name() else {
                continue;
            };
            // Skip symbolic refs like origin/HEAD.
            if name.ends_with("/HEAD") || name == "HEAD" {
                continue;
            }
            let short_name = match branch_type {
                // Strip remote prefix (e.g. "origin/feature" -> "feature").
                git2::BranchType::Remote => name.split('/').skip(1).collect::<Vec<_>>().join("/"),
                git2::BranchType::Local => name.to_string(),
            };
            if checked_out_branches.contains(&short_name) {
                continue;
            }
            let epoch = branch
                .get()
                .peel_to_commit()
                .map(|c| c.time().seconds())
                .unwrap_or(0);
            let compound_name = format!("{}@{}", repo_name, short_name);
            // Don't overwrite an existing entry (a worktree, or a local branch
            // that shadows a remote one).
            results
                .entry(compound_name)
                .or_insert_with(|| RecipeInfo::Branch {
                    repo_path: repo_abs_path.to_path_buf(),
                    branch_name: name.to_string(),
                    is_remote: branch_type == git2::BranchType::Remote,
                    commit_epoch: epoch,
                });
        }
    }
}

/// Add every recipe a single repo path contributes: its worktrees, its
/// cookable branches, and (unless bare) the base repo itself. A path that
/// isn't a repo, or is a standalone worktree, contributes nothing.
/// A repo's display name: its directory name with the trailing `.git` (the
/// git dir) stripped. `None` if the path has no usable file name.
fn repo_display_name(repo: &Repository) -> Option<String> {
    let repo_path_string = repo
        .path()
        .to_str()?
        .replace("/.git", "")
        .replace("/.git/", "");
    Path::new(&repo_path_string)
        .file_name()?
        .to_str()
        .map(str::to_string)
}

fn register_path_recipes(
    path: &Path,
    results: &mut HashMap<String, RecipeInfo>,
) -> anyhow::Result<()> {
    let Ok(repo) = Repository::open(path) else {
        tracing::debug!(path = %path.display(), "Skipping non-repo path");
        return Ok(());
    };

    let repo_name = repo_display_name(&repo).context("Failed to determine repo name")?;

    // Standalone worktrees are discovered via their parent repo.
    if repo.is_worktree() {
        tracing::debug!(name = %repo_name, "Skipping standalone worktree (discovered via parent)");
        return Ok(());
    }

    let repo_abs_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    discover_worktree_recipes(&repo, &repo_name, results);
    let checked_out_branches = checked_out_branch_names(&repo);
    discover_branch_recipes(
        &repo,
        &repo_name,
        &repo_abs_path,
        &checked_out_branches,
        results,
    );

    if repo.is_bare() {
        tracing::debug!(name = %repo_name, "Skipping bare repo (no working directory)");
    } else {
        tracing::debug!(name = %repo_name, path = %repo.path().display(), "Found git repository");
        // Use .git/index mtime so repos rank by local activity rather than
        // upstream commit timestamps.
        let activity_epoch = repo_activity_epoch(&repo);
        results.insert(
            repo_name,
            RecipeInfo::ExistingRepo {
                repo,
                commit_epoch: activity_epoch,
            },
        );
    }
    Ok(())
}

fn build_repository_hashmap(
    config: &ConfigurationValues,
) -> anyhow::Result<HashMap<String, RecipeInfo>> {
    let mut results: HashMap<String, RecipeInfo> = HashMap::new();
    for glob_from_config in config.repo_globs.iter() {
        tracing::debug!(pattern = %glob_from_config, "Processing glob pattern");
        let paths = glob::glob(glob_from_config).context("Could not parse glob")?;
        for path in paths.flatten() {
            register_path_recipes(&path, &mut results)?;
        }
    }

    Ok(results)
}

fn compute_sort_order(index: usize, total: usize) -> u32 {
    if total <= 1 {
        0
    } else {
        ((index * 100) / (total - 1)) as u32
    }
}

/// Build the sorted recipe list with sort_order values.
/// Returns (name, sort_order) pairs in output order.
fn build_sorted_recipes(repos: &HashMap<String, RecipeInfo>) -> Vec<(&String, u32)> {
    let mut sorted: Vec<_> = repos.iter().collect();
    sorted.sort_by(|a, b| {
        b.1.commit_epoch()
            .cmp(&a.1.commit_epoch())
            .then_with(|| a.0.cmp(b.0))
    });
    let total = sorted.len();

    sorted
        .into_iter()
        .enumerate()
        .map(|(i, (key, _))| (key, compute_sort_order(i, total)))
        .collect()
}

fn sorted_concrete_recipes(repos: &HashMap<String, RecipeInfo>) -> Vec<Recipe> {
    build_sorted_recipes(repos)
        .into_iter()
        .map(|(key, sort_order)| {
            let mut recipe = Recipe::new(key);
            recipe.sort_order = sort_order;
            recipe
        })
        .collect()
}

fn list_recipes(config: &ConfigurationValues) -> anyhow::Result<()> {
    let repos = build_repository_hashmap(config)?;
    tracing::debug!(count = repos.len(), "Listing recipes");

    for recipe in sorted_concrete_recipes(&repos) {
        println!("{}", recipe.to_jsonl());
    }
    Ok(())
}

/// Emitted unanchored; the daemon anchors them (see `enwiro_sdk::recipe_pattern`).
fn branch_pattern_recipes(repos: &HashMap<String, RecipeInfo>) -> Vec<RecipeItem> {
    let mut repo_names: Vec<&String> = repos.keys().filter(|n| is_base_repo_recipe(n)).collect();
    repo_names.sort();
    repo_names
        .into_iter()
        .map(|repo_name| {
            RecipeItem::Pattern(PatternRecipe {
                pattern: format!(
                    "{}@(?P<branch>.+)",
                    enwiro_sdk::recipe_pattern::escape(repo_name)
                ),
                description: Some(format!(
                    "Create new branch '{{branch}}' in {}",
                    enwiro_sdk::recipe_pattern::escape_template(repo_name)
                )),
                url: None,
            })
        })
        .collect()
}

fn collect_recipe_items(config: &ConfigurationValues) -> Vec<RecipeItem> {
    let Ok(repos) = build_repository_hashmap(config) else {
        return Vec::new();
    };
    let mut items: Vec<RecipeItem> = sorted_concrete_recipes(&repos)
        .into_iter()
        .map(RecipeItem::Concrete)
        .collect();
    items.extend(branch_pattern_recipes(&repos));
    items
}

/// Auto-detected status for git recipes (#302): a base repo is a standing
/// workspace, so it is `evergreen`. Branch recipes get no auto-status here -
/// "is this branch merged?" is answered by the forge (the github cookbook's
/// `gh` check), because a squash-merge is undetectable from local history once
/// the default branch moves on and walking large repos on a timer is too slow.
fn collect_status_events(config: &ConfigurationValues) -> Vec<enwiro_sdk::listen::RecipeUpdate> {
    use enwiro_sdk::listen::RecipeUpdate;
    use enwiro_sdk::status::Status;

    let Ok(repos) = build_repository_hashmap(config) else {
        return Vec::new();
    };
    repos
        .keys()
        .filter(|name| is_base_repo_recipe(name))
        .map(|name| RecipeUpdate::StatusChanged {
            recipe: name.clone(),
            status: Status::Evergreen,
        })
        .collect()
}

/// A base-repo recipe (the repository itself) rather than a `repo@branch`
/// worktree recipe.
fn is_base_repo_recipe(recipe: &str) -> bool {
    !recipe.contains('@')
}

/// Cooks a recipe. It returns the path to the already existing local
/// clone of the repository, or creates a worktree for a branch recipe.
fn cook(config: &ConfigurationValues, args: CookArgs) -> anyhow::Result<()> {
    tracing::debug!(recipe = %args.recipe_name, "Cooking recipe");
    let path = resolve_recipe_path(config, &args.recipe_name)?;
    println!(
        "{}",
        path.to_str()
            .context("Could not convert recipe path to string")?
    );
    Ok(())
}

/// A branch recipe's env is a git worktree; its `.git` is a pointer into
/// the base repo's own `.git/worktrees/<name>`, which holds the shared
/// object database and refs the worktree depends on. Report the base repo's
/// path so the isolation layer can mount it alongside the worktree -- this
/// cookbook has no notion of *why* that's needed (containers, or anything
/// else). A base repo recipe's env already *is* the repo, so it needs
/// nothing extra.
///
/// This is called as a fresh process invocation strictly *after* `cook` has
/// already created the worktree (see `write_external_paths_if_present` in
/// the host CLI), by which point the branch is checked out -- and a checked-
/// out branch's compound `repo@branch` key is deliberately absent from
/// `build_repository_hashmap` (`discover_branch_recipes` skips already-
/// checked-out branches, `discover_worktree_recipes` skips the `enwiro-`-
/// prefixed worktree itself), so looking up `recipe_name` directly there
/// would always fail. The base repo's own key has no such lifecycle and is
/// always resolvable, so resolve through that instead.
fn resolve_external_paths(
    config: &ConfigurationValues,
    recipe_name: &str,
) -> anyhow::Result<Vec<String>> {
    if is_base_repo_recipe(recipe_name) {
        return Ok(Vec::new());
    }
    let (repo_name, _branch) = recipe_name
        .split_once('@')
        .context("Branch recipe name must contain '@'")?;
    let recipes = build_repository_hashmap(config)?;
    let repo_recipe = recipes
        .get(repo_name)
        .with_context(|| format!("Could not find base repo recipe {}", repo_name))?;
    let RecipeInfo::ExistingRepo { repo, .. } = repo_recipe else {
        anyhow::bail!(
            "Expected '{}' to be a base repo recipe, found a branch recipe",
            repo_name
        );
    };
    let workdir = repo
        .workdir()
        .context("Could not get working directory of repo")?
        .to_str()
        .context("Could not convert repo path to string")?;
    // `Repository::workdir()` always returns a trailing separator; trim it
    // so this matches the plain, separator-free form the daemon's own
    // `environment_path` mount uses (same absolute path, just normalized).
    Ok(vec![workdir.trim_end_matches('/').to_string()])
}

fn external_paths(config: &ConfigurationValues, args: ExternalPathsArgs) -> anyhow::Result<()> {
    let paths = resolve_external_paths(config, &args.recipe_name)?;
    println!("{}", serde_json::to_string(&paths)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Create a non-bare git repo with an initial commit so worktrees can be added.
    fn create_repo_with_commit(path: &std::path::Path) -> Repository {
        let repo = Repository::init(path).expect("Failed to init repo");
        // Need an initial commit before we can create worktrees
        let sig = repo
            .signature()
            .unwrap_or_else(|_| git2::Signature::now("Test", "test@test.com").unwrap());
        {
            let tree_id = repo.index().unwrap().write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
        }
        repo
    }

    fn config_for_glob(glob: &str) -> ConfigurationValues {
        ConfigurationValues {
            repo_globs: vec![glob.to_string()],
            worktree_dir: None,
        }
    }

    fn config_with_worktree_dir(glob: &str, worktree_dir: &str) -> ConfigurationValues {
        ConfigurationValues {
            repo_globs: vec![glob.to_string()],
            worktree_dir: Some(worktree_dir.to_string()),
        }
    }

    #[test]
    fn test_discovers_regular_repo() {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().join("my-project");
        fs::create_dir(&repo_path).unwrap();
        Repository::init(&repo_path).unwrap();

        let config = config_for_glob(tmp.path().join("*").to_str().unwrap());
        let repos = build_repository_hashmap(&config).unwrap();

        assert!(
            repos.contains_key("my-project"),
            "Expected 'my-project' in keys: {:?}",
            repos.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_discovers_worktrees() {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().join("my-project");
        fs::create_dir(&repo_path).unwrap();
        let repo = create_repo_with_commit(&repo_path);

        // Create a worktree
        let wt_path = tmp.path().join("my-worktree-dir");
        repo.worktree("feature-branch", wt_path.as_path(), None)
            .unwrap();

        let config = config_for_glob(tmp.path().join("*").to_str().unwrap());
        let repos = build_repository_hashmap(&config).unwrap();

        assert!(
            repos.contains_key("my-project"),
            "Expected 'my-project' in keys: {:?}",
            repos.keys().collect::<Vec<_>>()
        );
        assert!(
            repos.contains_key("my-project@feature-branch"),
            "Expected 'my-project@feature-branch' in keys: {:?}",
            repos.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_worktree_cook_returns_worktree_path() {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().join("my-project");
        fs::create_dir(&repo_path).unwrap();
        let repo = create_repo_with_commit(&repo_path);

        let wt_path = tmp.path().join("my-worktree-dir");
        repo.worktree("feature-branch", wt_path.as_path(), None)
            .unwrap();

        let config = config_for_glob(tmp.path().join("*").to_str().unwrap());
        let repos = build_repository_hashmap(&config).unwrap();

        let recipe = repos.get("my-project@feature-branch").unwrap();
        let cooked_path = match recipe {
            RecipeInfo::ExistingRepo { repo, .. } => repo.workdir().unwrap().to_path_buf(),
            _ => panic!("Expected ExistingRepo variant"),
        };

        assert_eq!(
            cooked_path.canonicalize().unwrap(),
            wt_path.canonicalize().unwrap(),
            "Worktree recipe should resolve to the worktree directory"
        );
    }

    #[test]
    fn test_bare_repo_worktrees_discovered() {
        let tmp = TempDir::new().unwrap();
        let bare_path = tmp.path().join("my-project.git");
        fs::create_dir(&bare_path).unwrap();
        let repo = Repository::init_bare(&bare_path).unwrap();

        // Create an initial commit on the bare repo
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let tree_id = repo.treebuilder(None).unwrap().write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();

        // Add a worktree
        let wt_path = tmp.path().join("my-worktree-dir");
        repo.worktree("main-wt", wt_path.as_path(), None).unwrap();

        let config = config_for_glob(tmp.path().join("*").to_str().unwrap());
        let repos = build_repository_hashmap(&config).unwrap();

        // Bare repo itself should NOT be a recipe
        assert!(
            !repos.contains_key("my-project.git"),
            "Bare repo should not appear as a recipe: {:?}",
            repos.keys().collect::<Vec<_>>()
        );
        // But its worktree should
        assert!(
            repos.contains_key("my-project.git@main-wt"),
            "Expected 'my-project.git@main-wt' in keys: {:?}",
            repos.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_list_recipes_includes_branches() {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().join("my-project");
        fs::create_dir(&repo_path).unwrap();
        let repo = create_repo_with_commit(&repo_path);

        // Create a second branch
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("feature-x", &head, false).unwrap();

        let config = config_for_glob(tmp.path().join("*").to_str().unwrap());
        let recipes = build_repository_hashmap(&config).unwrap();

        assert!(
            recipes.contains_key("my-project@feature-x"),
            "Expected 'my-project@feature-x' in keys: {:?}",
            recipes.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_cook_branch_creates_worktree() {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().join("my-project");
        fs::create_dir(&repo_path).unwrap();
        let repo = create_repo_with_commit(&repo_path);

        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("feature-x", &head, false).unwrap();

        let wt_dir = tmp.path().join("worktrees");
        let config = config_with_worktree_dir(
            tmp.path().join("*").to_str().unwrap(),
            wt_dir.to_str().unwrap(),
        );

        let result = resolve_recipe_path(&config, "my-project@feature-x").unwrap();

        assert!(result.exists(), "Worktree path should exist on disk");
        let wt_repo = Repository::open(&result).unwrap();
        assert!(wt_repo.is_worktree(), "Should be a git worktree");
    }

    // Matches real production ordering (external_paths runs as a fresh
    // process after cook already checked the branch out) -- see
    // resolve_external_paths's doc comment for why that timing matters.
    #[test]
    fn test_external_paths_reports_the_main_repo_for_an_already_cooked_branch_recipe() {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().join("my-project");
        fs::create_dir(&repo_path).unwrap();
        let repo = create_repo_with_commit(&repo_path);

        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("feature-x", &head, false).unwrap();

        let wt_dir = tmp.path().join("worktrees");
        let config = config_with_worktree_dir(
            tmp.path().join("*").to_str().unwrap(),
            wt_dir.to_str().unwrap(),
        );

        // Cook first, exactly like the real `cook` subcommand does.
        resolve_recipe_path(&config, "my-project@feature-x").unwrap();

        let paths = resolve_external_paths(&config, "my-project@feature-x").unwrap();

        // `repo.workdir()` resolves symlinks (libgit2 canonicalizes the repo
        // path internally), so compare against the canonical form -- on
        // macOS, `TempDir`'s path lives under `/var/folders/...`, itself a
        // symlink to `/private/var/folders/...`, and the raw, unresolved
        // path would never match. Same convention as
        // `enwiro-cookbook-github`'s `test_discovers_regular_repo`.
        let expected_repo_path = repo_path.canonicalize().unwrap();
        assert_eq!(
            paths,
            vec![expected_repo_path.to_str().unwrap().to_string()]
        );
    }

    #[test]
    fn test_external_paths_is_empty_for_a_base_repo_recipe() {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().join("my-project");
        fs::create_dir(&repo_path).unwrap();
        Repository::init(&repo_path).unwrap();

        let config = config_for_glob(tmp.path().join("*").to_str().unwrap());

        let paths = resolve_external_paths(&config, "my-project").unwrap();

        assert!(paths.is_empty(), "{paths:?}");
    }

    #[test]
    fn test_existing_worktree_not_duplicated() {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().join("my-project");
        fs::create_dir(&repo_path).unwrap();
        let repo = create_repo_with_commit(&repo_path);

        // Create a worktree (this also creates the feature-x branch)
        let wt_path = tmp.path().join("feature-x-wt");
        repo.worktree("feature-x", wt_path.as_path(), None).unwrap();

        let config = config_for_glob(tmp.path().join("*").to_str().unwrap());
        let recipes = build_repository_hashmap(&config).unwrap();

        // Should have the worktree entry, but only once
        let matching_keys: Vec<_> = recipes.keys().filter(|k| k.contains("feature-x")).collect();
        assert_eq!(
            matching_keys.len(),
            1,
            "Branch with existing worktree should not be duplicated: {:?}",
            matching_keys
        );
    }

    #[test]
    fn test_remote_branches_listed() {
        let tmp = TempDir::new().unwrap();

        // Create a "remote" repo with a branch
        let remote_path = tmp.path().join("remote-repo");
        fs::create_dir(&remote_path).unwrap();
        let remote_repo = create_repo_with_commit(&remote_path);
        let head = remote_repo.head().unwrap().peel_to_commit().unwrap();
        remote_repo.branch("feature-remote", &head, false).unwrap();

        // Create a "local" repo that fetches from the remote
        let local_path = tmp.path().join("local-repo");
        fs::create_dir(&local_path).unwrap();
        let local_repo = create_repo_with_commit(&local_path);
        local_repo
            .remote("origin", remote_path.to_str().unwrap())
            .unwrap();
        local_repo
            .find_remote("origin")
            .unwrap()
            .fetch(&["refs/heads/*:refs/remotes/origin/*"], None, None)
            .unwrap();

        // Glob only matches the local repo
        let config = config_for_glob(local_path.to_str().unwrap());
        let recipes = build_repository_hashmap(&config).unwrap();

        assert!(
            recipes.contains_key("local-repo@feature-remote"),
            "Expected 'local-repo@feature-remote' in keys: {:?}",
            recipes.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_remote_head_excluded_from_recipes() {
        let tmp = TempDir::new().unwrap();

        // Create a "remote" repo
        let remote_path = tmp.path().join("remote-repo");
        fs::create_dir(&remote_path).unwrap();
        create_repo_with_commit(&remote_path);

        // Clone it (this creates origin/HEAD, unlike manual remote + fetch)
        let local_path = tmp.path().join("local-repo");
        Repository::clone(remote_path.to_str().unwrap(), &local_path).unwrap();

        let config = config_for_glob(local_path.to_str().unwrap());
        let recipes = build_repository_hashmap(&config).unwrap();

        assert!(
            !recipes.contains_key("local-repo@HEAD"),
            "origin/HEAD should not appear as a recipe: {:?}",
            recipes.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_worktree_dir_uses_hash_for_disambiguation() {
        let tmp = TempDir::new().unwrap();

        // Create two repos with the same name in different directories
        let dir_a = tmp.path().join("a").join("my-project");
        let dir_b = tmp.path().join("b").join("my-project");
        fs::create_dir_all(&dir_a).unwrap();
        fs::create_dir_all(&dir_b).unwrap();

        let hash_a = short_path_hash(&dir_a.canonicalize().unwrap());
        let hash_b = short_path_hash(&dir_b.canonicalize().unwrap());

        assert_ne!(
            hash_a, hash_b,
            "Same-named repos in different dirs should get different hashes"
        );
    }

    #[test]
    fn test_cooked_branch_recipe_drops_out_of_list() {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().join("my-project");
        fs::create_dir(&repo_path).unwrap();
        let repo = create_repo_with_commit(&repo_path);

        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("feature-x", &head, false).unwrap();

        let wt_dir = tmp.path().join("worktrees");
        let config = config_with_worktree_dir(
            tmp.path().join("*").to_str().unwrap(),
            wt_dir.to_str().unwrap(),
        );

        // Before cooking, the branch is offered as a recipe.
        let before = build_repository_hashmap(&config).unwrap();
        assert!(before.contains_key("my-project@feature-x"));

        // Cooking checks the branch out into an enwiro-managed worktree.
        let result = resolve_recipe_path(&config, "my-project@feature-x").unwrap();
        assert!(result.exists());

        // Now the branch is checked out, so cooking it again would fail with
        // "already checked out". The recipe must drop out of the list rather
        // than be offered as something that predictably cannot be built.
        let after = build_repository_hashmap(&config).unwrap();
        assert!(
            !after.contains_key("my-project@feature-x"),
            "Cooked (checked-out) branch must not be offered again, got: {:?}",
            after.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_branch_with_slash_creates_flat_directory() {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().join("my-project");
        fs::create_dir(&repo_path).unwrap();
        let repo = create_repo_with_commit(&repo_path);

        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("feature/my-thing", &head, false).unwrap();

        let wt_dir = tmp.path().join("worktrees");
        let config = config_with_worktree_dir(
            tmp.path().join("*").to_str().unwrap(),
            wt_dir.to_str().unwrap(),
        );

        let result = resolve_recipe_path(&config, "my-project@feature/my-thing").unwrap();

        assert!(result.exists(), "Worktree path should exist on disk");
        // The worktree should be directly inside the repo folder, not nested
        let repo_folder = result.parent().unwrap();
        assert_eq!(
            repo_folder.parent().unwrap().canonicalize().unwrap(),
            wt_dir.canonicalize().unwrap(),
            "Worktree should be at depth worktrees/<repo>/<branch>, not deeper"
        );
    }

    #[test]
    fn test_head_branch_not_listed_as_recipe() {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().join("my-project");
        fs::create_dir(&repo_path).unwrap();
        let repo = create_repo_with_commit(&repo_path);

        // HEAD points to "master" (or "main" depending on git config).
        // Create another branch so there's something else to list.
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("feature-x", &head, false).unwrap();

        let head_branch_name = repo.head().unwrap().shorthand().unwrap().to_string();

        let config = config_for_glob(tmp.path().join("*").to_str().unwrap());
        let recipes = build_repository_hashmap(&config).unwrap();

        // The HEAD branch is checked out in the main working tree, so cooking it
        // would fail with "already checked out"; it must not be offered.
        let head_recipe_key = format!("my-project@{}", head_branch_name);
        assert!(
            !recipes.contains_key(&head_recipe_key),
            "Checked-out HEAD branch '{}' must not appear as a recipe: {:?}",
            head_branch_name,
            recipes.keys().collect::<Vec<_>>()
        );

        // The repo entry and the un-checked-out branch are still offered.
        assert!(recipes.contains_key("my-project"));
        assert!(recipes.contains_key("my-project@feature-x"));
    }

    #[test]
    fn test_branch_checked_out_in_worktree_not_listed_as_branch_recipe() {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().join("my-project");
        fs::create_dir(&repo_path).unwrap();
        let repo = create_repo_with_commit(&repo_path);

        // Create a branch and a worktree for it (with a different worktree name)
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let branch = repo.branch("feature-x", &head, false).unwrap();
        let wt_path = tmp.path().join("wt-for-feature-x");
        let reference = branch.into_reference();
        let mut opts = git2::WorktreeAddOptions::new();
        opts.reference(Some(&reference));
        repo.worktree("my-wt", &wt_path, Some(&opts)).unwrap();

        // Also create a branch that is NOT checked out anywhere
        repo.branch("feature-y", &head, false).unwrap();

        let config = config_for_glob(tmp.path().join("*").to_str().unwrap());
        let recipes = build_repository_hashmap(&config).unwrap();

        // The worktree entry should exist
        assert!(
            recipes.contains_key("my-project@my-wt"),
            "Worktree entry should exist: {:?}",
            recipes.keys().collect::<Vec<_>>()
        );

        // feature-x should NOT appear as a branch recipe (it's checked out in a worktree)
        assert!(
            !recipes.contains_key("my-project@feature-x"),
            "Branch checked out in a worktree should not appear as a separate recipe: {:?}",
            recipes.keys().collect::<Vec<_>>()
        );

        // feature-y (not checked out) should still be listed
        assert!(
            recipes.contains_key("my-project@feature-y"),
            "Unchecked-out branch should appear: {:?}",
            recipes.keys().collect::<Vec<_>>()
        );
    }

    /// When `cook()` is called for a branch that is already checked out in the
    /// main worktree (i.e. the current HEAD branch), the cookbook must surface
    /// a human-readable error that explicitly names the branch and tells the user
    /// it is already checked out — not just a raw git2 internal error string.
    ///
    /// git2 already says "reference is already checked out" internally, but the
    /// cookbook must wrap this with an explicit context message of the form
    /// "Branch '<name>' is already checked out" so the branch name is
    /// prominently visible in the top-level error message without requiring the
    /// user to parse the full chain or the technical `refs/heads/...` path.
    ///
    /// This test fails today because the wrapper context is
    /// "Could not create worktree for branch <name>" — which does not say
    /// "already checked out".  The outermost error (shown with `{}`) must
    /// already contain that phrase.
    #[test]
    fn test_cook_already_checked_out_branch_gives_actionable_error() {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().join("my-project");
        fs::create_dir(&repo_path).unwrap();
        let repo = create_repo_with_commit(&repo_path);

        // Determine the name of the HEAD branch (commonly "master" or "main").
        let head_branch_name = repo.head().unwrap().shorthand().unwrap().to_string();

        // The HEAD branch is already checked out in the main worktree.  Attempting to
        // create a new worktree for the same branch must fail — and the outermost
        // error message (displayed with `{}`, NOT `{:#}`) must immediately tell the
        // user both the branch name and that it is already checked out.
        let wt_dir = tmp.path().join("worktrees");
        let config = config_with_worktree_dir(
            tmp.path().join("*").to_str().unwrap(),
            wt_dir.to_str().unwrap(),
        );

        // Branch discovery excludes already-checked-out branches, so this path
        // is only reachable defensively (e.g. a branch checked out between
        // listing and cooking). Exercise the worktree-creation step directly so
        // we still guarantee the error message it produces is actionable.
        let result = resolve_branch_worktree(&config, &repo_path, &head_branch_name, false);

        assert!(
            result.is_err(),
            "cooking an already-checked-out branch must return an error"
        );

        let err = result.unwrap_err();

        // The OUTERMOST error message (shown to the user by default, `{}` format)
        // must include both the branch name and "already checked out".
        // Using `{:#}` (full chain) is not enough — the outermost layer must be
        // self-explanatory so that the notification (which uses `{}`) is actionable.
        let outermost_msg = err.to_string();
        assert!(
            outermost_msg.to_lowercase().contains("already checked out"),
            "the outermost error message (displayed with `{{}}`) must say 'already \
             checked out' so the user sees it immediately without inspecting the full \
             chain; got outermost: {outermost_msg:?}\n\
             Hint: detect the 'already checked out' condition from git2 and return \
             an explicit error like \"Branch '{}' is already checked out\"",
            head_branch_name
        );
        assert!(
            outermost_msg.contains(&head_branch_name),
            "the outermost error message must include the branch name '{}'; \
             got: {outermost_msg:?}",
            head_branch_name
        );
    }

    #[test]
    fn test_cook_nonexistent_branch_creates_branch_and_worktree() {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().join("my-project");
        fs::create_dir(&repo_path).unwrap();
        let repo = create_repo_with_commit(&repo_path);
        let head_commit = repo.head().unwrap().peel_to_commit().unwrap().id();

        let wt_dir = tmp.path().join("worktrees");
        let config = config_with_worktree_dir(
            tmp.path().join("*").to_str().unwrap(),
            wt_dir.to_str().unwrap(),
        );

        // The branch is not listed anywhere - this is the pattern-routed path.
        let result = resolve_recipe_path(&config, "my-project@brand-new-branch").unwrap();

        assert!(result.exists(), "Worktree path should exist on disk");
        let wt_repo = Repository::open(&result).unwrap();
        assert!(wt_repo.is_worktree());
        let branch = repo
            .find_branch("brand-new-branch", git2::BranchType::Local)
            .expect("branch should have been created");
        // Local-only repo (no remote): fork point is local HEAD.
        assert_eq!(
            branch.get().peel_to_commit().unwrap().id(),
            head_commit,
            "new branch should fork from local HEAD when there is no remote"
        );
    }

    #[test]
    fn test_cook_nonexistent_branch_forks_from_origin_default() {
        let tmp = TempDir::new().unwrap();

        // Remote with one commit; clone it (sets origin/HEAD).
        let remote_path = tmp.path().join("remote-repo");
        fs::create_dir(&remote_path).unwrap();
        let remote_repo = create_repo_with_commit(&remote_path);
        let origin_tip = remote_repo.head().unwrap().peel_to_commit().unwrap().id();

        let local_path = tmp.path().join("local-repo");
        let local_repo = Repository::clone(remote_path.to_str().unwrap(), &local_path).unwrap();

        // Move the local HEAD ahead so it differs from the origin default tip.
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let tree_id = local_repo.index().unwrap().write_tree().unwrap();
        let tree = local_repo.find_tree(tree_id).unwrap();
        let parent = local_repo.head().unwrap().peel_to_commit().unwrap();
        let local_tip = local_repo
            .commit(Some("HEAD"), &sig, &sig, "local-only", &tree, &[&parent])
            .unwrap();
        assert_ne!(local_tip, origin_tip);

        let wt_dir = tmp.path().join("worktrees");
        let config =
            config_with_worktree_dir(local_path.to_str().unwrap(), wt_dir.to_str().unwrap());

        resolve_recipe_path(&config, "local-repo@fresh-branch").unwrap();

        let branch = local_repo
            .find_branch("fresh-branch", git2::BranchType::Local)
            .unwrap();
        assert_eq!(
            branch.get().peel_to_commit().unwrap().id(),
            origin_tip,
            "new branch should fork from the origin default branch, not local HEAD"
        );
    }

    #[test]
    fn test_cook_nonexistent_branch_with_slash() {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().join("my-project");
        fs::create_dir(&repo_path).unwrap();
        create_repo_with_commit(&repo_path);

        let wt_dir = tmp.path().join("worktrees");
        let config = config_with_worktree_dir(
            tmp.path().join("*").to_str().unwrap(),
            wt_dir.to_str().unwrap(),
        );

        let result = resolve_recipe_path(&config, "my-project@feat/new-thing").unwrap();
        assert!(result.exists());
    }

    #[test]
    fn test_cook_unknown_repo_fails() {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().join("my-project");
        fs::create_dir(&repo_path).unwrap();
        create_repo_with_commit(&repo_path);

        let wt_dir = tmp.path().join("worktrees");
        let config = config_with_worktree_dir(
            tmp.path().join("*").to_str().unwrap(),
            wt_dir.to_str().unwrap(),
        );

        let err = resolve_recipe_path(&config, "my-porject@branch").unwrap_err();
        assert!(err.to_string().contains("Could not find recipe"), "{err}");
    }

    #[test]
    fn test_ensure_local_branch_reuses_existing_branch() {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().join("my-project");
        fs::create_dir(&repo_path).unwrap();
        let repo = create_repo_with_commit(&repo_path);
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let existing_tip = head.id();
        repo.branch("feature-x", &head, false).unwrap();

        // Stale-cache race: the branch appeared between discovery and cook.
        ensure_local_branch(&repo, "feature-x").unwrap();

        let branch = repo
            .find_branch("feature-x", git2::BranchType::Local)
            .unwrap();
        assert_eq!(branch.get().peel_to_commit().unwrap().id(), existing_tip);
    }

    #[test]
    fn test_collect_recipe_items_includes_branch_pattern_per_repo() {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().join("my-project");
        fs::create_dir(&repo_path).unwrap();
        create_repo_with_commit(&repo_path);

        let config = config_for_glob(tmp.path().join("*").to_str().unwrap());
        let items = collect_recipe_items(&config);

        let patterns: Vec<&PatternRecipe> = items
            .iter()
            .filter_map(|item| match item {
                RecipeItem::Pattern(p) => Some(p),
                RecipeItem::Concrete(_) => None,
            })
            .collect();
        assert_eq!(patterns.len(), 1, "one pattern claim per base repo");
        assert_eq!(
            patterns[0].pattern,
            format!(
                "{}@(?P<branch>.+)",
                enwiro_sdk::recipe_pattern::escape("my-project")
            )
        );
        assert_eq!(
            patterns[0].description.as_deref(),
            Some("Create new branch '{branch}' in my-project")
        );
        enwiro_sdk::recipe_pattern::validate(
            &patterns[0].pattern,
            patterns[0].description.as_deref(),
        )
        .expect("emitted pattern must pass daemon validation");
    }

    #[test]
    fn test_default_worktree_dir_is_absolute() {
        // default_worktree_dir must return an absolute path (never
        // fall back to a relative "." path).  On a normal system
        // dirs::data_dir() succeeds, so this verifies the happy path
        // returns an absolute path.
        let dir = default_worktree_dir().unwrap();
        assert!(
            dir.is_absolute(),
            "default_worktree_dir should return an absolute path, got: {:?}",
            dir
        );
    }

    #[test]
    fn test_worktree_base_dir_errors_without_config_or_data_dir() {
        // When worktree_dir is configured, it should be used regardless
        // of whether dirs::data_dir() would work.
        let config = ConfigurationValues {
            repo_globs: vec![],
            worktree_dir: Some("/tmp/my-worktrees".to_string()),
        };
        let dir = worktree_base_dir(&config).unwrap();
        assert_eq!(dir, PathBuf::from("/tmp/my-worktrees"));
    }

    #[test]
    fn test_slash_and_dash_branches_do_not_collide() {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().join("my-project");
        fs::create_dir(&repo_path).unwrap();
        let repo = create_repo_with_commit(&repo_path);

        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("feature/foo", &head, false).unwrap();
        repo.branch("feature-foo", &head, false).unwrap();

        let wt_dir = tmp.path().join("worktrees");
        let config = config_with_worktree_dir(
            tmp.path().join("*").to_str().unwrap(),
            wt_dir.to_str().unwrap(),
        );

        let path_a = resolve_recipe_path(&config, "my-project@feature/foo").unwrap();
        let path_b = resolve_recipe_path(&config, "my-project@feature-foo").unwrap();

        assert!(path_a.exists(), "feature/foo worktree should exist");
        assert!(path_b.exists(), "feature-foo worktree should exist");
        assert_ne!(
            path_a, path_b,
            "Branches feature/foo and feature-foo should have different worktree paths"
        );
    }

    /// Helper: create a repo with two branches at different known commit times.
    /// Returns (repo, old_epoch, new_epoch).
    fn create_repo_with_timed_branches(repo_path: &Path) -> (Repository, i64, i64) {
        let repo = Repository::init(repo_path).expect("Failed to init repo");
        let old_epoch: i64 = 1_000_000;
        let new_epoch: i64 = 2_000_000;

        let tree_id = repo.index().unwrap().write_tree().unwrap();

        let old_oid = {
            let old_time = git2::Time::new(old_epoch, 0);
            let old_sig = git2::Signature::new("Test", "test@test.com", &old_time).unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            let oid = repo
                .commit(Some("HEAD"), &old_sig, &old_sig, "old commit", &tree, &[])
                .unwrap();
            let commit = repo.find_commit(oid).unwrap();
            repo.branch("old-branch", &commit, false).unwrap();
            oid
        };

        {
            let new_time = git2::Time::new(new_epoch, 0);
            let new_sig = git2::Signature::new("Test", "test@test.com", &new_time).unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            let old_commit = repo.find_commit(old_oid).unwrap();
            let oid = repo
                .commit(
                    Some("HEAD"),
                    &new_sig,
                    &new_sig,
                    "new commit",
                    &tree,
                    &[&old_commit],
                )
                .unwrap();
            let commit = repo.find_commit(oid).unwrap();
            repo.branch("new-branch", &commit, false).unwrap();
        }

        (repo, old_epoch, new_epoch)
    }

    #[test]
    fn test_branch_recipe_has_correct_commit_epoch() {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().join("my-project");
        fs::create_dir(&repo_path).unwrap();
        let (_repo, old_epoch, new_epoch) = create_repo_with_timed_branches(&repo_path);

        let config = config_for_glob(tmp.path().join("*").to_str().unwrap());
        let recipes = build_repository_hashmap(&config).unwrap();

        let old_recipe = recipes
            .get("my-project@old-branch")
            .expect("old-branch not found");
        let new_recipe = recipes
            .get("my-project@new-branch")
            .expect("new-branch not found");

        assert_eq!(
            old_recipe.commit_epoch(),
            old_epoch,
            "old-branch should have epoch {}",
            old_epoch
        );
        assert_eq!(
            new_recipe.commit_epoch(),
            new_epoch,
            "new-branch should have epoch {}",
            new_epoch
        );
    }

    #[test]
    fn test_list_recipes_sorted_newest_first() {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().join("my-project");
        fs::create_dir(&repo_path).unwrap();
        let (_repo, _old_epoch, _new_epoch) = create_repo_with_timed_branches(&repo_path);

        let config = config_for_glob(tmp.path().join("*").to_str().unwrap());
        let recipes = build_repository_hashmap(&config).unwrap();

        let mut recipe_list: Vec<_> = recipes.iter().collect();
        recipe_list.sort_by(|a, b| {
            b.1.commit_epoch()
                .cmp(&a.1.commit_epoch())
                .then_with(|| a.0.cmp(b.0))
        });
        let names: Vec<&str> = recipe_list.iter().map(|(k, _)| k.as_str()).collect();

        let new_idx = names
            .iter()
            .position(|n| *n == "my-project@new-branch")
            .unwrap();
        let old_idx = names
            .iter()
            .position(|n| *n == "my-project@old-branch")
            .unwrap();

        assert!(
            new_idx < old_idx,
            "new-branch should come before old-branch, got order: {:?}",
            names
        );
    }

    #[test]
    fn test_recently_active_repo_sorts_before_old_branches() {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().join("my-project");
        fs::create_dir(&repo_path).unwrap();
        let (_repo, _old_epoch, _new_epoch) = create_repo_with_timed_branches(&repo_path);

        // Branches have commit epochs in the distant past (1_000_000 and 2_000_000)
        // while the main repo's .git/index mtime is "now" (wall-clock time).
        // The main repo should sort before all old branches.
        let config = config_for_glob(tmp.path().join("*").to_str().unwrap());
        let recipes = build_repository_hashmap(&config).unwrap();

        let mut recipe_list: Vec<_> = recipes.iter().collect();
        recipe_list.sort_by(|a, b| {
            b.1.commit_epoch()
                .cmp(&a.1.commit_epoch())
                .then_with(|| a.0.cmp(b.0))
        });
        let names: Vec<&str> = recipe_list.iter().map(|(k, _)| k.as_str()).collect();

        let repo_idx = names.iter().position(|n| *n == "my-project").unwrap();

        // The main repo entry uses .git/index mtime (current wall-clock time),
        // which is much more recent than any test commit epoch, so it sorts first.
        assert_eq!(
            repo_idx, 0,
            "Recently active repo should sort first, got order: {:?}",
            names
        );
    }

    #[test]
    fn test_same_epoch_branches_sorted_alphabetically() {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path().join("my-project");
        fs::create_dir(&repo_path).unwrap();
        let repo = Repository::init(&repo_path).expect("Failed to init repo");

        // Create a single commit — all branches will point here (same epoch).
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        {
            let time = git2::Time::new(1_000_000, 0);
            let sig = git2::Signature::new("Test", "test@test.com", &time).unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            let oid = repo
                .commit(Some("HEAD"), &sig, &sig, "single commit", &tree, &[])
                .unwrap();
            let commit = repo.find_commit(oid).unwrap();
            // Names chosen so alphabetical order differs from insertion order
            repo.branch("zebra", &commit, false).unwrap();
            repo.branch("alpha", &commit, false).unwrap();
            repo.branch("middle", &commit, false).unwrap();
        }

        let config = config_for_glob(tmp.path().join("*").to_str().unwrap());
        let recipes = build_repository_hashmap(&config).unwrap();

        let mut recipe_list: Vec<_> = recipes.iter().collect();
        recipe_list.sort_by(|a, b| {
            b.1.commit_epoch()
                .cmp(&a.1.commit_epoch())
                .then_with(|| a.0.cmp(b.0))
        });
        let branch_names: Vec<&str> = recipe_list
            .iter()
            .map(|(k, _)| k.as_str())
            .filter(|k| k.contains('@'))
            .collect();

        // All branches share the same epoch, so tiebreaker should be alphabetical
        let alpha_idx = branch_names
            .iter()
            .position(|n| *n == "my-project@alpha")
            .unwrap();
        let middle_idx = branch_names
            .iter()
            .position(|n| *n == "my-project@middle")
            .unwrap();
        let zebra_idx = branch_names
            .iter()
            .position(|n| *n == "my-project@zebra")
            .unwrap();

        assert!(
            alpha_idx < middle_idx && middle_idx < zebra_idx,
            "Same-epoch branches should sort alphabetically, got: {:?}",
            branch_names
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

    #[test]
    fn test_compute_sort_order_five_items() {
        assert_eq!(compute_sort_order(0, 5), 0);
        assert_eq!(compute_sort_order(1, 5), 25);
        assert_eq!(compute_sort_order(2, 5), 50);
        assert_eq!(compute_sort_order(3, 5), 75);
        assert_eq!(compute_sort_order(4, 5), 100);
    }

    #[test]
    fn test_main_repo_without_branches_keeps_position_sort_order() {
        let tmp = TempDir::new().unwrap();
        // Create two repos so total > 1 and sort_order isn't trivially 0.
        // Use Repository::init() without commits so there are no branches.
        let repo_a_path = tmp.path().join("aaa-project");
        let repo_b_path = tmp.path().join("zzz-project");
        fs::create_dir(&repo_a_path).unwrap();
        fs::create_dir(&repo_b_path).unwrap();
        Repository::init(&repo_a_path).unwrap();
        // Sleep briefly so the two repos get different .git/HEAD mtimes
        std::thread::sleep(std::time::Duration::from_millis(50));
        Repository::init(&repo_b_path).unwrap();

        let config = config_for_glob(tmp.path().join("*").to_str().unwrap());
        let repos = build_repository_hashmap(&config).unwrap();
        let sorted = build_sorted_recipes(&repos);

        // Both repos have no branches, so they keep position-based
        // sort_order from the linear mapping.
        assert_eq!(sorted.len(), 2);
        assert_eq!(sorted[0].1, 0, "First repo gets sort_order=0 from position");
        assert_eq!(
            sorted[1].1, 100,
            "Second repo gets sort_order=100 from position"
        );
    }
}

fn read_config() -> anyhow::Result<ConfigurationValues> {
    let payload =
        CookbookPayload::read_from_stdin().context("Could not read cookbook payload from stdin")?;
    let config: ConfigurationValues = serde_json::from_value(payload.config)
        .context("Could not deserialize cookbook-git configuration")?;
    tracing::debug!(globs = config.repo_globs.len(), "Config loaded");
    Ok(config)
}

fn main() -> anyhow::Result<()> {
    let _guard = enwiro_sdk::init_logging("enwiro-cookbook-git.log");

    let args = EnwiroCookbookGit::parse();

    match args {
        EnwiroCookbookGit::Core(CookbookCore::ListRecipes(_)) => {
            let config = read_config()?;
            list_recipes(&config)?;
        }
        EnwiroCookbookGit::Core(CookbookCore::Cook(args)) => {
            let config = read_config()?;
            cook(&config, args)?;
        }
        EnwiroCookbookGit::ExternalPaths(args) => {
            let config = read_config()?;
            external_paths(&config, args)?;
        }
        EnwiroCookbookGit::Core(CookbookCore::Metadata) => {
            println!(
                "{}",
                CookbookMetadata {
                    capabilities: DeclaredCapabilities::declare([CookbookCapability::Listen]),
                    default_priority: Some(10),
                    project_overridable: vec!["repo_globs".to_string()],
                }
                .to_json()
            );
        }
        EnwiroCookbookGit::Listen => {
            let payload = CookbookPayload::read_first_line_from_stdin()
                .context("Could not read cookbook payload from stdin")?;
            let config: ConfigurationValues = serde_json::from_value(payload.config)
                .context("Could not deserialize cookbook-git configuration")?;
            enwiro_sdk::listen::serve_updates(LISTEN_POLL_INTERVAL, || {
                let mut updates = vec![enwiro_sdk::listen::RecipeUpdate::Recipes {
                    data: collect_recipe_items(&config),
                }];
                updates.extend(collect_status_events(&config));
                updates
            });
        }
    };

    Ok(())
}
