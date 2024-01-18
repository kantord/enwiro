use serde_derive::{Deserialize, Serialize};
use std::env;

#[derive(Debug, Serialize, Deserialize)]
pub struct ConfigurationValues {
    pub workspaces_directory: String,
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
