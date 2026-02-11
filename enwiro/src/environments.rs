use anyhow::{Context, bail};
use std::collections::HashMap;
use std::fs;

#[derive(Debug)]
pub struct Environment {
    // Actual path to the environment
    pub path: String,

    // Name should be short enough to be displayed
    pub name: String,
}

impl Environment {
    pub fn get_all(source_directory: &str) -> anyhow::Result<HashMap<String, Environment>> {
        let mut results: HashMap<String, Environment> = HashMap::new();
        let directory_entries = fs::read_dir(source_directory)?;

        for directory_entry in directory_entries {
            let entry = directory_entry.context("Failed to read directory entry")?;
            let path = entry.path();
            let id = path
                .file_name()
                .context("Failed to get file name")?
                .to_str()
                .context("Failed to convert file name to string")?
                .to_string();

            if path.is_dir() {
                let new_environment = Environment {
                    path: path
                        .to_str()
                        .context("Failed to convert path to string")?
                        .to_string(),
                    name: id.clone(),
                };

                results.insert(id.clone(), new_environment);
            }
        }

        Ok(results)
    }

    pub fn get_one(source_directory: &str, name: &str) -> anyhow::Result<Environment> {
        let mut environments = Self::get_all(source_directory)?;

        match environments.remove(name) {
            Some(x) => Ok(x),
            None => bail!("Environment \"{}\" does not exist", name),
        }
    }
}
