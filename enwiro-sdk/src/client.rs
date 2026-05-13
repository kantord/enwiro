use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use std::process::Command;

use crate::cookbook::{CookbookMetadata, Recipe};
use crate::plugin::Plugin;

const DEFAULT_PRIORITY: u32 = 50;

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct EnvScores {
    pub launcher: f64,
    pub slot: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedRecipe {
    pub cookbook: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub sort_order: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scores: Option<EnvScores>,
}

pub trait CookbookTrait {
    fn list_recipes(&self) -> anyhow::Result<Vec<Recipe>>;
    fn cook(&self, recipe: &str) -> anyhow::Result<String>;
    fn name(&self) -> &str;
    /// Controls display and resolution order. Lower values appear first.
    /// Built-in range: git=10, chezmoi=20, github=30. Third-party plugins
    /// that don't provide metadata default to 50.
    fn priority(&self) -> u32 {
        DEFAULT_PRIORITY
    }
    /// Return optional gear configuration JSON for the given recipe.
    /// If `Some(json)` is returned after cooking, it is written to
    /// `<env>/gear.d/cookbook-<name>.json` (one file per cookbook),
    /// where the env-side reader (`enwiro_sdk::gear::LoadedGear`)
    /// merges every cookbook's contribution into one keyed map.
    fn gear(&self, _recipe: &str) -> anyhow::Result<Option<serde_json::Value>> {
        Ok(None)
    }
}

/// Sort cookbooks by priority (lower first), then alphabetically by name.
pub fn sort_cookbooks(cookbooks: &mut [Box<dyn CookbookTrait>]) {
    cookbooks.sort_by(|a, b| {
        a.priority()
            .cmp(&b.priority())
            .then_with(|| a.name().cmp(b.name()))
    });
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

    #[cfg(test)]
    fn with_metadata(plugin: Plugin, metadata: CookbookMetadata) -> Self {
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
            CookbookMetadata::from_json(&stdout)
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
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_str::<Recipe>(line).unwrap_or_else(|_| Recipe::new(line)))
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

    /// Invoke the cookbook binary's optional `gear <recipe>` subcommand and
    /// parse its stdout as JSON. Returns `Ok(None)` if the subcommand fails
    /// for any reason (old cookbook that doesn't implement `gear`, exec
    /// error, malformed JSON) so a missing or broken `gear` never blocks
    /// cooking. Best-effort by design.
    fn gear(&self, recipe: &str) -> anyhow::Result<Option<serde_json::Value>> {
        let output = match Command::new(&self.plugin.executable)
            .arg("gear")
            .arg(recipe)
            .output()
        {
            Ok(o) => o,
            Err(e) => {
                tracing::debug!(cookbook = %self.plugin.name, error = %e, "Cookbook gear exec failed");
                return Ok(None);
            }
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::debug!(cookbook = %self.plugin.name, recipe = %recipe, %stderr, "Cookbook gear subcommand returned non-zero");
            return Ok(None);
        }
        match serde_json::from_slice::<serde_json::Value>(&output.stdout) {
            Ok(json) => Ok(Some(json)),
            Err(e) => {
                tracing::debug!(cookbook = %self.plugin.name, error = %e, "Cookbook gear stdout was not valid JSON");
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::PluginKind;

    fn mock_plugin(name: &str) -> Plugin {
        Plugin {
            name: name.to_string(),
            kind: PluginKind::Cookbook,
            executable: String::new(),
        }
    }

    #[test]
    fn test_cookbook_client_uses_priority_from_metadata() {
        let client = CookbookClient::with_metadata(
            mock_plugin("git"),
            CookbookMetadata {
                default_priority: Some(10),
            },
        );
        assert_eq!(client.priority(), 10);
    }

    #[test]
    fn test_cookbook_client_default_priority_when_no_metadata() {
        let client = CookbookClient::with_metadata(mock_plugin("git"), CookbookMetadata::default());
        assert_eq!(client.priority(), DEFAULT_PRIORITY);
    }

    #[test]
    fn test_cookbook_client_name_from_plugin() {
        let client =
            CookbookClient::with_metadata(mock_plugin("my-cookbook"), CookbookMetadata::default());
        assert_eq!(client.name(), "my-cookbook");
    }

    /// Build a CookbookClient backed by a temp-dir shell script that responds
    /// to subcommands by echoing fixture text. The caller supplies the script
    /// body; tempdir is returned so it stays alive for the lifetime of the test.
    fn cookbook_client_from_script(script_body: &str) -> (tempfile::TempDir, CookbookClient) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("fake-cookbook");
        std::fs::write(&script, script_body).expect("write script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("chmod script");
        let plugin = Plugin {
            name: "fake".to_string(),
            kind: PluginKind::Cookbook,
            executable: script.to_string_lossy().into_owned(),
        };
        let client = CookbookClient::with_metadata(plugin, CookbookMetadata::default());
        (dir, client)
    }

    /// `CookbookClient::gear` runs the binary's `gear <recipe>` subcommand
    /// and parses stdout as JSON. Without this override, the trait default
    /// returns `Ok(None)` and silently drops every cookbook's gear.
    #[test]
    fn test_gear_invokes_cookbook_subcommand_and_parses_stdout() {
        let (_dir, client) = cookbook_client_from_script(
            r#"#!/bin/sh
case "$1" in
    gear) echo '{"version":1,"gear":{"x":{"description":"y","web":{"p":{"description":"z","url":"https://example.com"}}}}}' ;;
    *) echo "unexpected subcommand: $1" >&2; exit 1 ;;
esac
"#,
        );
        let value = client
            .gear("some-recipe")
            .expect("gear() should succeed when cookbook returns valid JSON")
            .expect("gear() must return Some(json) when cookbook emits a payload");
        assert_eq!(value["version"], 1);
        assert_eq!(
            value["gear"]["x"]["web"]["p"]["url"], "https://example.com",
            "the cookbook's stdout must reach the caller verbatim - got {value}"
        );
    }

    /// A cookbook that doesn't implement the `gear` subcommand must yield
    /// `Ok(None)` (best-effort, no error), so old cookbooks keep working.
    #[test]
    fn test_gear_returns_none_when_subcommand_fails() {
        let (_dir, client) = cookbook_client_from_script(
            r#"#!/bin/sh
echo "unsupported subcommand: $1" >&2
exit 1
"#,
        );
        let result = client
            .gear("some-recipe")
            .expect("gear() must not return Err for an unsupported subcommand");
        assert!(
            result.is_none(),
            "old cookbooks (no gear subcommand) must surface as Ok(None); got {result:?}"
        );
    }
}
