use std::{collections::HashMap, path::Path};

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

fn build_repository_hashmap(config: &ConfigurationValues) -> HashMap<String, Repository> {
    let mut results: HashMap<String, Repository> = HashMap::new();
    for glob_from_config in config.repo_globs.iter() {
        glob::glob(glob_from_config)
            .expect("Could not parse glob")
            .for_each(|entry| {
                if let Ok(path) = entry {
                    if let Ok(repo) = Repository::open(path) {
                        let repo_path_string =
                            repo.path().to_str().unwrap().replace("/.git", "").clone();
                        let repo_name = Path::new(&repo_path_string.to_string())
                            .file_name()
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .to_string();

                        results.insert(repo_name, repo);
                    }
                }
            });
    }

    results
}

fn list_recipes(config: &ConfigurationValues) {
    for key in build_repository_hashmap(config).keys() {
        println!("{}", key);
    }
}

/// Cooks a recipe. It returns the path to the already existing local
/// clone of the repository.
fn cook(config: &ConfigurationValues, args: CookArgs) {
    let repositories = build_repository_hashmap(config);
    let selected_repo = repositories.get(&args.recipe_name);
    if let Some(repo) = selected_repo {
        println!("{}", repo.path().parent().unwrap().to_str().unwrap())
    } else {
        panic!("Could not find recipe {}", args.recipe_name);
    }
}

fn main() -> Result<(), ()> {
    let args = EnwiroCookbookGit::parse();
    let config: ConfigurationValues = match confy::load("enwiro", "cookbook-git") {
        Ok(x) => x,
        Err(x) => {
            panic!("Could not load configuration: {:?}", x);
        }
    };

    match args {
        EnwiroCookbookGit::ListRecipes(_) => {
            list_recipes(&config);
        }
        EnwiroCookbookGit::Cook(args) => {
            cook(&config, args);
        }
    };

    Ok(())
}
