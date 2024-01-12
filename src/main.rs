use clap::Parser;
use serde_derive::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::fs::create_dir;
use std::path::Path;

#[derive(Debug, Serialize, Deserialize)]
struct ConfigurationValues {
    workspaces_directory: String,
}

impl ::std::default::Default for ConfigurationValues {
    fn default() -> Self {
        let home_dir = env::home_dir().expect("User home directory not found");
        let default_workspaces_directory = home_dir.join(".enwiro_envs");

        Self {
            workspaces_directory: default_workspaces_directory.to_str().unwrap().to_string(),
        }
    }
}

#[derive(Debug)]
enum EnvironmentType {
    Simple,
}

#[derive(Debug)]
struct Environment {
    path: String,
    name: String,
    type_: EnvironmentType,
}

fn get_basic_environments(source_directory: &str) -> Vec<Environment> {
    let mut results: Vec<Environment> = vec![];
    let directory_entries = fs::read_dir(source_directory).expect("Could not read workspaces directory. Make sure that the path is a directory you have permissions to access.");

    for directory_entry in directory_entries {
        let path = directory_entry.unwrap().path();

        if path.is_dir() {
            results.push(Environment {
                path: path.to_str().unwrap().to_string(),
                name: path.file_name().unwrap().to_str().unwrap().to_string(),
                type_: EnvironmentType::Simple,
            })
        }
    }

    results
}

#[derive(Parser)]
enum EnwiroCli {
    ListEnvironments(ListEnvironmentsArgs),
}

#[derive(clap::Args)]
#[command(author, version, about)]
struct ListEnvironmentsArgs {}

fn ensure_can_run(config: &ConfigurationValues) {
    let environments_directory = Path::new(&config.workspaces_directory);
    if !environments_directory.exists() {
        create_dir(environments_directory)
            .expect("Workspace directory does not exist and could not be automatically created.");
    }
}

fn main() {
    let EnwiroCli::ListEnvironments(_) = EnwiroCli::parse();

    let config: ConfigurationValues =
        confy::load("enwiro", None).expect("Configuration file must be present");

    ensure_can_run(&config);

    let mut environments = get_basic_environments(&config.workspaces_directory);

    dbg!(config);
    dbg!(environments);
}
