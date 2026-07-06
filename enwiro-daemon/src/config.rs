use serde::{Deserialize, Serialize};

use enwiro_sdk::plugin::{PluginKind, get_plugins};

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ConfigurationValues {
    pub workspaces_directory: String,
    pub adapter: Option<String>,
    /// OCI runtime to pass as `--runtime` for every container launch (e.g.
    /// `/usr/bin/krun` for microVM isolation via libkrun). `None` uses
    /// whatever the container engine defaults to. Global for now -- not
    /// per-environment/per-app policy, which stays north-star (issue #540).
    pub container_runtime: Option<String>,
}

impl ::std::default::Default for ConfigurationValues {
    fn default() -> Self {
        let home_dir = home::home_dir().expect("User home directory not found");
        let default_workspaces_directory = home_dir.join(".enwiro_envs");
        let mut adapter: Option<String> = None;
        let mut available_adapters = get_plugins(PluginKind::Adapter);
        if available_adapters.len() == 1 {
            adapter = Some(available_adapters.drain().next().unwrap().name.to_string());
        }

        match &adapter {
            Some(name) => tracing::info!(adapter = %name, "Auto-selected adapter"),
            None => tracing::debug!("No adapter auto-selected"),
        }

        Self {
            workspaces_directory: default_workspaces_directory.to_str().unwrap().to_string(),
            adapter,
            container_runtime: None,
        }
    }
}
