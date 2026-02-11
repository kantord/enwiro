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
        let paths = glob::glob(glob_from_config).context("Could not parse glob")?;
        for path in paths.flatten() {
            if let Ok(repo) = Repository::open(path) {
                let repo_path_string = repo
                    .path()
                    .to_str()
                    .context("Failed to convert repo path to string")?
                    .replace("/.git", "");
                let repo_name = Path::new(&repo_path_string)
                    .file_name()
                    .context("Failed to get repo file name")?
                    .to_str()
                    .context("Failed to convert repo name to string")?
                    .to_string();

                results.insert(repo_name, repo);
            }
        }
    }

    Ok(results)
}

fn list_recipes(config: &ConfigurationValues) -> anyhow::Result<()> {
    for key in build_repository_hashmap(config)?.keys() {
        println!("{}", key);
    }
    Ok(())
}

/// Cooks a recipe. It returns the path to the already existing local
/// clone of the repository.
fn cook(config: &ConfigurationValues, args: CookArgs) -> anyhow::Result<()> {
    let repositories = build_repository_hashmap(config)?;
    let selected_repo = repositories.get(&args.recipe_name);
    if let Some(repo) = selected_repo {
        let parent = repo
            .path()
            .parent()
            .context("Could not get parent directory of repo")?;
        println!(
            "{}",
            parent
                .to_str()
                .context("Could not convert repo path to string")?
        );
    } else {
        bail!("Could not find recipe {}", args.recipe_name);
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args = EnwiroCookbookGit::parse();
    let config: ConfigurationValues =
        confy::load("enwiro", "cookbook-git").context("Could not load configuration")?;

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
