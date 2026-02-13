use std::{collections::HashMap, path::Path};

use anyhow::{Context, bail};
use clap::Parser;
use git2::Repository;
use serde_derive::{Deserialize, Serialize};
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct ConfigurationValues {
    pub repo_globs: Vec<String>,
}

#[derive(Parser)]
enum EnwiroCookbookGit {
    ListRecipes(ListRecipesArgs),
    Cook(CookArgs),
}

#[derive(clap::Args)]
pub struct ListRecipesArgs {}

#[derive(clap::Args)]
pub struct CookArgs {
    recipe_name: String,
}

fn build_repository_hashmap(
    config: &ConfigurationValues,
) -> anyhow::Result<HashMap<String, Repository>> {
    let mut results: HashMap<String, Repository> = HashMap::new();
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

                // Discover worktrees and add them as repo_name@worktree_name
                if let Ok(worktrees) = repo.worktrees() {
                    for wt_name in worktrees.iter().flatten() {
                        if let Ok(wt) = repo.find_worktree(wt_name)
                            && let Ok(wt_repo) = Repository::open(wt.path())
                        {
                            let compound_name = format!("{}@{}", repo_name, wt_name);
                            tracing::debug!(name = %compound_name, "Found git worktree");
                            results.insert(compound_name, wt_repo);
                        }
                    }
                }

                if repo.is_bare() {
                    tracing::debug!(name = %repo_name, "Skipping bare repo (no working directory)");
                } else if repo.is_worktree() {
                    tracing::debug!(name = %repo_name, "Skipping standalone worktree (discovered via parent)");
                } else {
                    tracing::debug!(name = %repo_name, path = %repo_path_string, "Found git repository");
                    results.insert(repo_name, repo);
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
/// clone of the repository.
fn cook(config: &ConfigurationValues, args: CookArgs) -> anyhow::Result<()> {
    tracing::debug!(recipe = %args.recipe_name, "Cooking recipe");
    let repositories = build_repository_hashmap(config)?;
    let selected_repo = repositories.get(&args.recipe_name);
    if let Some(repo) = selected_repo {
        let workdir = repo
            .workdir()
            .context("Could not get working directory of repo")?;
        println!(
            "{}",
            workdir
                .to_str()
                .context("Could not convert repo path to string")?
        );
    } else {
        tracing::error!(recipe = %args.recipe_name, "Recipe not found");
        bail!("Could not find recipe {}", args.recipe_name);
    }
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

        let wt_repo = repos.get("my-project@feature-branch").unwrap();
        let cooked_path = wt_repo.workdir().unwrap();

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
    };

    Ok(())
}
