use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::Context;
use clap::Parser;
use git2::Repository;
use serde_derive::{Deserialize, Serialize};
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct ConfigurationValues {
    pub repo_globs: Vec<String>,
    pub worktree_dir: Option<String>,
}

fn short_path_hash(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(path.to_string_lossy().as_bytes());
    format!("{:x}", hash)[..8].to_string()
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
    let recipe = recipes
        .get(recipe_name)
        .with_context(|| format!("Could not find recipe {}", recipe_name))?;
    match recipe {
        RecipeInfo::ExistingRepo(repo) => {
            let workdir = repo
                .workdir()
                .context("Could not get working directory of repo")?;
            Ok(workdir.to_path_buf())
        }
        RecipeInfo::Branch {
            repo_path,
            branch_name,
            is_remote,
        } => {
            let repo = Repository::open(repo_path)
                .context("Could not open repository for worktree creation")?;
            let repo_name = repo_path
                .file_name()
                .context("Could not get repo directory name")?
                .to_str()
                .context("Could not convert repo name to string")?;
            let path_hash = short_path_hash(repo_path);

            // Strip remote prefix for the directory/worktree name
            let short_name = if *is_remote {
                branch_name.split('/').skip(1).collect::<Vec<_>>().join("/")
            } else {
                branch_name.clone()
            };

            let branch_hash = short_path_hash(Path::new(&short_name));
            let flat_name = format!("{}-{}", short_name.replace('/', "-"), branch_hash);

            let wt_base = worktree_base_dir(config)?;
            let wt_path = wt_base
                .join(format!("{}-{}", repo_name, path_hash))
                .join(&flat_name);

            // If worktree already exists, just return the path
            if wt_path.exists() {
                return Ok(wt_path);
            }

            // Create parent directories
            std::fs::create_dir_all(wt_path.parent().unwrap())
                .context("Could not create worktree directory")?;

            // Resolve the branch reference
            let reference = if *is_remote {
                // For remote branches, find the remote tracking ref
                let remote_branch = repo
                    .find_branch(branch_name, git2::BranchType::Remote)
                    .with_context(|| format!("Could not find remote branch {}", branch_name))?;
                let commit = remote_branch
                    .get()
                    .peel_to_commit()
                    .context("Could not resolve remote branch to commit")?;
                // Create a local branch from the remote tracking branch
                let local_branch = repo
                    .branch(&short_name, &commit, false)
                    .with_context(|| format!("Could not create local branch {}", short_name))?;
                local_branch.into_reference()
            } else {
                let branch = repo
                    .find_branch(branch_name, git2::BranchType::Local)
                    .with_context(|| format!("Could not find branch {}", branch_name))?;
                branch.into_reference()
            };

            // Worktree name must be unique within the repo
            let wt_name = format!("enwiro-{}", flat_name);
            let mut opts = git2::WorktreeAddOptions::new();
            opts.reference(Some(&reference));
            repo.worktree(&wt_name, &wt_path, Some(&opts))
                .with_context(|| format!("Could not create worktree for branch {}", branch_name))?;

            tracing::debug!(path = %wt_path.display(), branch = %branch_name, "Created worktree");
            Ok(wt_path)
        }
    }
}

enum RecipeInfo {
    ExistingRepo(Repository),
    Branch {
        repo_path: PathBuf,
        branch_name: String,
        is_remote: bool,
    },
}

#[derive(Parser)]
enum EnwiroCookbookGit {
    ListRecipes(ListRecipesArgs),
    Cook(CookArgs),
    Metadata,
}

#[derive(clap::Args)]
pub struct ListRecipesArgs {}

#[derive(clap::Args)]
pub struct CookArgs {
    recipe_name: String,
}

fn build_repository_hashmap(
    config: &ConfigurationValues,
) -> anyhow::Result<HashMap<String, RecipeInfo>> {
    let mut results: HashMap<String, RecipeInfo> = HashMap::new();
    for glob_from_config in config.repo_globs.iter() {
        tracing::debug!(pattern = %glob_from_config, "Processing glob pattern");
        let paths = glob::glob(glob_from_config).context("Could not parse glob")?;
        for path in paths.flatten() {
            if let Ok(repo) = Repository::open(&path) {
                let repo_path_string = repo
                    .path()
                    .to_str()
                    .context("Failed to convert repo path to string")?
                    .replace("/.git", "")
                    .replace("/.git/", "");
                let repo_name = Path::new(&repo_path_string)
                    .file_name()
                    .context("Failed to get repo file name")?
                    .to_str()
                    .context("Failed to convert repo name to string")?
                    .to_string();

                // Skip standalone worktrees (they're discovered via their parent)
                if repo.is_worktree() {
                    tracing::debug!(name = %repo_name, "Skipping standalone worktree (discovered via parent)");
                    continue;
                }

                let repo_abs_path = path.canonicalize().unwrap_or(path.clone());

                // Discover existing worktrees
                if let Ok(worktrees) = repo.worktrees() {
                    for wt_name in worktrees.iter().flatten() {
                        // Skip enwiro-managed worktrees — they are implementation
                        // details behind branch recipes and should stay invisible.
                        if wt_name.starts_with("enwiro-") {
                            tracing::debug!(worktree = %wt_name, "Skipping enwiro-managed worktree");
                            continue;
                        }
                        match repo.find_worktree(wt_name) {
                            Ok(wt) => match Repository::open(wt.path()) {
                                Ok(wt_repo) => {
                                    let compound_name = format!("{}@{}", repo_name, wt_name);
                                    tracing::debug!(name = %compound_name, "Found git worktree");
                                    results
                                        .insert(compound_name, RecipeInfo::ExistingRepo(wt_repo));
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

                // Collect branches that are already checked out (in main
                // working tree or any worktree) so we can skip them during
                // branch discovery — creating a worktree for an already
                // checked-out branch would fail.
                let mut checked_out_branches: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                if let Ok(worktrees) = repo.worktrees() {
                    for wt_name in worktrees.iter().flatten() {
                        // Skip enwiro-managed worktrees — their branches
                        // should remain as cookable branch recipes.
                        if wt_name.starts_with("enwiro-") {
                            continue;
                        }
                        if let Ok(wt) = repo.find_worktree(wt_name)
                            && let Ok(wt_repo) = Repository::open(wt.path())
                            && let Ok(wt_head) = wt_repo.head()
                            && let Some(name) = wt_head.shorthand()
                        {
                            checked_out_branches.insert(name.to_string());
                        }
                    }
                }

                // Discover branches and add them as potential worktree recipes.
                // Local branches are iterated first so they take priority over
                // remote tracking branches with the same short name.
                let branch_types = [git2::BranchType::Local, git2::BranchType::Remote];
                for &bt in &branch_types {
                    if let Ok(branches) = repo.branches(Some(bt)) {
                        for branch_result in branches.flatten() {
                            let (branch, branch_type) = branch_result;
                            if let Ok(Some(name)) = branch.name() {
                                // Skip symbolic refs like origin/HEAD
                                if name.ends_with("/HEAD") || name == "HEAD" {
                                    continue;
                                }
                                let short_name = match branch_type {
                                    git2::BranchType::Remote => {
                                        // Strip remote prefix (e.g. "origin/feature" -> "feature")
                                        name.split('/').skip(1).collect::<Vec<_>>().join("/")
                                    }
                                    git2::BranchType::Local => name.to_string(),
                                };
                                // Skip branches already checked out in main or a worktree
                                if checked_out_branches.contains(&short_name) {
                                    continue;
                                }
                                let compound_name = format!("{}@{}", repo_name, short_name);
                                // Don't overwrite existing entries (worktrees or local branches)
                                results.entry(compound_name).or_insert_with(|| {
                                    RecipeInfo::Branch {
                                        repo_path: repo_abs_path.clone(),
                                        branch_name: name.to_string(),
                                        is_remote: branch_type == git2::BranchType::Remote,
                                    }
                                });
                            }
                        }
                    }
                }

                if repo.is_bare() {
                    tracing::debug!(name = %repo_name, "Skipping bare repo (no working directory)");
                } else {
                    tracing::debug!(name = %repo_name, path = %repo_path_string, "Found git repository");
                    results.insert(repo_name, RecipeInfo::ExistingRepo(repo));
                }
            } else {
                tracing::debug!(path = %path.display(), "Skipping non-repo path");
            }
        }
    }

    Ok(results)
}

fn list_recipes(config: &ConfigurationValues) -> anyhow::Result<()> {
    let repos = build_repository_hashmap(config)?;
    tracing::debug!(count = repos.len(), "Listing recipes");
    for key in repos.keys() {
        println!("{}", key);
    }
    Ok(())
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
            RecipeInfo::ExistingRepo(repo) => repo.workdir().unwrap().to_path_buf(),
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
    fn test_cooked_branch_recipe_remains_in_list() {
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

        // Cook the branch recipe (creates a worktree)
        let result = resolve_recipe_path(&config, "my-project@feature-x").unwrap();
        assert!(result.exists());

        // The recipe should still appear in the list after cooking
        let recipes = build_repository_hashmap(&config).unwrap();
        assert!(
            recipes.contains_key("my-project@feature-x"),
            "Branch recipe should remain in list after cooking, got: {:?}",
            recipes.keys().collect::<Vec<_>>()
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
    fn test_head_branch_listed_as_recipe() {
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

        // The HEAD branch should appear as a branch recipe even though it's checked out
        let head_recipe_key = format!("my-project@{}", head_branch_name);
        assert!(
            recipes.contains_key(&head_recipe_key),
            "HEAD branch '{}' should appear as a recipe: {:?}",
            head_branch_name,
            recipes.keys().collect::<Vec<_>>()
        );

        // The main repo entry and the other branch should still be there
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
}

fn main() -> anyhow::Result<()> {
    let _guard = enwiro_logging::init_logging("enwiro-cookbook-git.log");

    let args = EnwiroCookbookGit::parse();
    let config: ConfigurationValues =
        confy::load("enwiro", "cookbook-git").context("Could not load configuration")?;
    tracing::debug!(globs = config.repo_globs.len(), "Config loaded");

    match args {
        EnwiroCookbookGit::ListRecipes(_) => {
            list_recipes(&config)?;
        }
        EnwiroCookbookGit::Cook(args) => {
            cook(&config, args)?;
        }
        EnwiroCookbookGit::Metadata => {
            println!(r#"{{"defaultPriority":10}}"#);
        }
    };

    Ok(())
}
