use anyhow::{Context, bail};
use serde::Deserialize;
use std::process::Command;

use crate::plugin::Plugin;

const DEFAULT_PRIORITY: u32 = 50;

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CookbookMetadata {
    pub description: Option<String>,
    pub default_priority: Option<u32>,
}

pub fn parse_metadata(json: &str) -> anyhow::Result<CookbookMetadata> {
    serde_json::from_str(json).context("Failed to parse cookbook metadata")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipe {
    pub name: String,
    pub description: Option<String>,
}

impl Recipe {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
        }
    }

    pub fn with_description(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: Some(description.into()),
        }
    }
}

pub trait CookbookTrait {
    fn list_recipes(&self) -> anyhow::Result<Vec<Recipe>>;
    fn cook(&self, recipe: &str) -> anyhow::Result<String>;
    fn name(&self) -> &str;
    /// Lower values = higher priority. Used to sort cookbooks for display and
    /// recipe resolution order. Default is 50.
    fn priority(&self) -> u32 {
        50
    }
}

pub struct CookbookClient {
    plugin: Plugin,
    metadata: CookbookMetadata,
}

impl CookbookClient {
    pub fn new(plugin: Plugin) -> Self {
        let metadata = Self::fetch_metadata(&plugin.executable);
        Self { plugin, metadata }
    }

    fn fetch_metadata(executable: &str) -> CookbookMetadata {
        let result = (|| -> anyhow::Result<CookbookMetadata> {
            let output = Command::new(executable)
                .arg("metadata")
                .output()
                .context("Failed to run cookbook metadata command")?;
            if !output.status.success() {
                bail!("Cookbook does not support metadata subcommand");
            }
            let stdout = String::from_utf8(output.stdout)
                .context("Cookbook metadata produced invalid UTF-8")?;
            parse_metadata(&stdout)
        })();
        match result {
            Ok(meta) => meta,
            Err(e) => {
                tracing::debug!(error = %e, "Could not fetch cookbook metadata, using defaults");
                CookbookMetadata::default()
            }
        }
    }
}

impl CookbookTrait for CookbookClient {
    fn list_recipes(&self) -> anyhow::Result<Vec<Recipe>> {
        tracing::debug!(cookbook = %self.plugin.name, "Listing recipes from cookbook");
        let output = Command::new(&self.plugin.executable)
            .arg("list-recipes")
            .output()
            .context("Cookbook failed to list recipes")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::error!(cookbook = %self.plugin.name, %stderr, "Cookbook failed to list recipes");
            bail!(
                "Cookbook '{}' failed to list recipes: {}",
                self.plugin.name,
                stderr
            );
        }

        let stdout =
            String::from_utf8(output.stdout).context("Cookbook produced invalid UTF-8 output")?;
        Ok(stdout
            .lines()
            .map(|line| match line.split_once('\t') {
                Some((name, desc)) => Recipe::with_description(name, desc),
                None => Recipe::new(line),
            })
            .collect())
    }

    fn cook(&self, recipe: &str) -> anyhow::Result<String> {
        tracing::debug!(cookbook = %self.plugin.name, recipe = %recipe, "Cooking recipe");
        let output = Command::new(&self.plugin.executable)
            .arg("cook")
            .arg(recipe)
            .output()
            .context("Failed to cook recipe")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::error!(cookbook = %self.plugin.name, recipe = %recipe, %stderr, "Cookbook failed to cook recipe");
            bail!(
                "Cookbook '{}' failed to cook '{}': {}",
                self.plugin.name,
                recipe,
                stderr
            );
        }

        let stdout =
            String::from_utf8(output.stdout).context("Cookbook produced invalid UTF-8 output")?;
        Ok(stdout.trim().to_string())
    }

    fn name(&self) -> &str {
        &self.plugin.name
    }

    fn priority(&self) -> u32 {
        self.metadata.default_priority.unwrap_or(DEFAULT_PRIORITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::PluginKind;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn test_parse_metadata_valid_json() {
        let json = r#"{
            "description": "Discovers git repositories",
            "defaultPriority": 10
        }"#;
        let meta = parse_metadata(json).unwrap();
        assert_eq!(
            meta.description.as_deref(),
            Some("Discovers git repositories")
        );
        assert_eq!(meta.default_priority, Some(10));
    }

    #[test]
    fn test_parse_metadata_empty_object() {
        let json = r#"{}"#;
        let meta = parse_metadata(json).unwrap();
        assert_eq!(meta.description, None);
        assert_eq!(meta.default_priority, None);
    }

    #[test]
    fn test_parse_metadata_unknown_fields_ignored() {
        let json = r#"{
            "defaultPriority": 20,
            "someFutureField": "hello"
        }"#;
        let meta = parse_metadata(json).unwrap();
        assert_eq!(meta.default_priority, Some(20));
    }

    #[test]
    fn test_parse_metadata_invalid_json() {
        assert!(parse_metadata("not json").is_err());
    }

    fn make_mock_plugin(dir: &std::path::Path, script: &str) -> Plugin {
        let bin_path = dir.join("enwiro-cookbook-mock");
        fs::write(&bin_path, script).unwrap();
        fs::set_permissions(&bin_path, fs::Permissions::from_mode(0o755)).unwrap();
        Plugin {
            name: "mock".to_string(),
            kind: PluginKind::Cookbook,
            executable: bin_path.to_string_lossy().to_string(),
        }
    }

    #[test]
    fn test_cookbook_client_uses_priority_from_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = make_mock_plugin(
            dir.path(),
            r#"#!/bin/sh
case "$1" in
  metadata) echo '{"defaultPriority":10}' ;;
  list-recipes) echo "" ;;
  *) exit 1 ;;
esac
"#,
        );
        let client = CookbookClient::new(plugin);
        assert_eq!(client.priority(), 10);
    }

    #[test]
    fn test_cookbook_client_default_priority_when_metadata_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = make_mock_plugin(
            dir.path(),
            r#"#!/bin/sh
case "$1" in
  list-recipes) echo "" ;;
  *) exit 1 ;;
esac
"#,
        );
        let client = CookbookClient::new(plugin);
        assert_eq!(client.priority(), DEFAULT_PRIORITY);
    }

    #[test]
    fn test_cookbook_client_name_from_plugin_filename() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = make_mock_plugin(
            dir.path(),
            r#"#!/bin/sh
case "$1" in
  metadata) echo '{}' ;;
  list-recipes) echo "" ;;
  *) exit 1 ;;
esac
"#,
        );
        let client = CookbookClient::new(plugin);
        assert_eq!(client.name(), "mock");
    }
}
