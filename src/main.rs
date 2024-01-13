use clap::Parser;
use serde_derive::{Deserialize, Serialize};
use std::collections::HashMap;
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
    id: String,
    path: String,
    name: String,
    type_: EnvironmentType,
}

fn get_environments(source_directory: &str) -> HashMap<String, Environment> {
    let mut results: HashMap<String, Environment> = HashMap::new();
    let directory_entries = fs::read_dir(source_directory).expect("Could not read workspaces directory. Make sure that the path is a directory you have permissions to access.");

    for directory_entry in directory_entries {
        let path = directory_entry.unwrap().path();
        let id = path.file_name().unwrap().to_str().unwrap().to_string();

        if path.is_dir() {
            let new_environment = Environment {
                id: id.clone(),
                path: path.to_str().unwrap().to_string(),
                name: id.clone(),
                type_: EnvironmentType::Simple,
            };

            results.insert(id.clone(), new_environment);
        }
    }

    results
}

#[derive(Parser)]
enum EnwiroCli {
    ListEnvironments(ListEnvironmentsArgs),
    ShowPath(ShowPathArgs),
}

#[derive(clap::Args)]
#[command(author, version, about)]
struct ListEnvironmentsArgs {}

#[derive(clap::Args)]
#[command(author, version, about)]
struct ShowPathArgs {
    environment_name: String,
}

fn ensure_can_run(config: &ConfigurationValues) {
    let environments_directory = Path::new(&config.workspaces_directory);
    if !environments_directory.exists() {
        create_dir(environments_directory)
            .expect("Workspace directory does not exist and could not be automatically created.");
    }
}

fn list_environments(config: &ConfigurationValues) {
    let environments = get_environments(&config.workspaces_directory);

    for environment in environments.values() {
        println!("{}", environment.name);
    }
}

fn show_path(config: &ConfigurationValues, args: ShowPathArgs) {
    let environments = get_environments(&config.workspaces_directory);
    let selected_environment = environments
        .get(&args.environment_name)
        .expect("Environment not found");

    println!("{}", selected_environment.path);
}

fn main() {
    let args = EnwiroCli::parse();

    let config: ConfigurationValues =
        confy::load("enwiro", None).expect("Configuration file must be present");

    ensure_can_run(&config);

    match args {
        EnwiroCli::ListEnvironments(args) => list_environments(&config),
        EnwiroCli::ShowPath(args) => show_path(&config, args),
    }

    dbg!(config);
}
