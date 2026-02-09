use serde_derive::{Deserialize, Serialize};

use crate::plugin::{get_plugins, PluginKind};

#[derive(Debug, Serialize, Deserialize)]
pub struct ConfigurationValues {
    pub workspaces_directory: String,
    pub adapter: Option<String>,
}

impl ::std::default::Default for ConfigurationValues {
    fn default() -> Self {
        let home_dir = home::home_dir().expect("User home directory not found");
        let default_workspaces_directory = home_dir.join(".enwiro_envs");
        let mut adapter: Option<String> = None;
        let mut available_adapters = get_plugins(PluginKind::Adapter);
        if available_adapters.len() == 1 {
            adapter = Some(available_adapters.drain().next().unwrap().name);
        }

        Self {
            workspaces_directory: default_workspaces_directory.to_str().unwrap().to_string(),
            adapter,
        }
    }
}
