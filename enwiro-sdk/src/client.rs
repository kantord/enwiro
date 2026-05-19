use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::process::{Command, Output, Stdio};

use crate::cookbook::{CookbookMetadata, CookbookPayload, Recipe};
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
    config: serde_json::Value,
}

impl CookbookClient {
    /// Construct a client whose config is resolved with the project-level
    /// walker rooted at the current working directory. Used by the `enw`
    /// CLI: the user invokes `enw` from a project shell, so cwd identifies
    /// the project context.
    pub fn new(plugin: Plugin) -> Self {
        let metadata = Self::fetch_metadata(&plugin.executable);
        let config = resolve_config_with_walker(&plugin, &metadata);
        Self {
            plugin,
            metadata,
            config,
        }
    }

    /// Construct a client whose config is resolved from user-level files
    /// only — no project-layer walk. Used by the enwiro daemon: it's a
    /// single long-running process serving many projects with no concept
    /// of "current project," so per-project overrides can't be applied
    /// meaningfully. The daemon's recipe cache is correspondingly
    /// project-independent.
    pub fn new_user_level_only(plugin: Plugin) -> Self {
        let metadata = Self::fetch_metadata(&plugin.executable);
        let config = resolve_user_level_only(&plugin);
        Self {
            plugin,
            metadata,
            config,
        }
    }

    #[cfg(test)]
    fn with_metadata(plugin: Plugin, metadata: CookbookMetadata) -> Self {
        Self::with_metadata_and_config(
            plugin,
            metadata,
            serde_json::Value::Object(Default::default()),
        )
    }

    #[cfg(test)]
    fn with_metadata_and_config(
        plugin: Plugin,
        metadata: CookbookMetadata,
        config: serde_json::Value,
    ) -> Self {
        Self {
            plugin,
            metadata,
            config,
        }
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

    /// Spawn the cookbook with the given subcommand args, write the
    /// resolved `CookbookPayload` to its stdin, and collect output.
    /// Centralizes the stdin pipe so every subcommand carries the same
    /// payload.
    fn spawn_with_payload(&self, args: &[&str]) -> anyhow::Result<Output> {
        let mut child = Command::new(&self.plugin.executable)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Failed to spawn cookbook")?;
        if let Some(mut stdin) = child.stdin.take() {
            let payload = CookbookPayload::new(self.config.clone());
            let bytes =
                serde_json::to_vec(&payload).context("Failed to serialize cookbook payload")?;
            stdin
                .write_all(&bytes)
                .context("Failed to write cookbook payload to stdin")?;
        }
        child.wait_with_output().context("Cookbook process failed")
    }
}

/// Resolve a cookbook's config with the project-layer walker rooted at
/// the current working directory, filtered through the cookbook's
/// `project_overridable` allowlist. Falls back to an empty JSON object
/// (with `warn` log) on error so a single misconfigured cookbook
/// doesn't break the whole CLI.
fn resolve_config_with_walker(plugin: &Plugin, metadata: &CookbookMetadata) -> serde_json::Value {
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "Could not determine cwd; cookbook config defaults to empty");
            return serde_json::Value::Object(Default::default());
        }
    };
    let scope = scope_for(plugin);
    let allowlist: Vec<&str> = metadata
        .project_overridable
        .iter()
        .map(String::as_str)
        .collect();
    match crate::config::build_cookbook_config(&cwd, &scope, &allowlist) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(cookbook = %plugin.name, error = %e, "Failed to resolve cookbook config; using empty config");
            serde_json::Value::Object(Default::default())
        }
    }
}

/// Resolve a cookbook's config from the user-level file only. Used by
/// the daemon, which has no meaningful "current project" cwd.
fn resolve_user_level_only(plugin: &Plugin) -> serde_json::Value {
    let scope = scope_for(plugin);
    match crate::config::load_user_config(&scope) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(cookbook = %plugin.name, error = %e, "Failed to load user-level cookbook config; using empty config");
            serde_json::Value::Object(Default::default())
        }
    }
}

fn scope_for(plugin: &Plugin) -> String {
    format!("cookbook-{}", plugin.name)
}

impl CookbookTrait for CookbookClient {
    fn list_recipes(&self) -> anyhow::Result<Vec<Recipe>> {
        tracing::debug!(cookbook = %self.plugin.name, "Listing recipes from cookbook");
        let output = self.spawn_with_payload(&["list-recipes"])?;

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
        let output = self.spawn_with_payload(&["cook", recipe])?;

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
        let output = match self.spawn_with_payload(&["gear", recipe]) {
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
                project_overridable: vec![],
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

    /// Subcommand spawn pipes `CookbookPayload` JSON to the child's stdin
    /// so cookbooks can read their resolved config from there.
    #[test]
    fn test_cook_pipes_payload_to_stdin() {
        let (_dir, client) = cookbook_client_from_script(
            r#"#!/bin/sh
payload=$(cat)
echo "$payload"
"#,
        );
        let stdout = client.cook("anything").expect("cook returns stdout");
        let payload: CookbookPayload =
            serde_json::from_str(&stdout).expect("cookbook saw a valid CookbookPayload on stdin");
        assert_eq!(payload.version, 1, "payload version should be 1");
    }

    /// End-to-end integration: a project-level `.enwiro.toml` is found by
    /// the SDK loader, filtered through a cookbook's `project_overridable`
    /// allowlist, merged on top of the user-level config, and piped into a
    /// language-agnostic (shell-script) cookbook over stdin. This satisfies
    /// the ADR/AC requirement of one integration test exercising a
    /// shell-script cookbook with the full project-walker pipeline.
    #[test]
    fn test_shell_cookbook_receives_merged_project_layer_config() {
        use std::os::unix::fs::PermissionsExt;

        let tempdir = tempfile::tempdir().expect("tempdir");
        let home_dir = tempdir.path().join("home");
        let project_dir = tempdir.path().join("proj");
        std::fs::create_dir_all(&home_dir).expect("mkdir home");
        std::fs::create_dir_all(&project_dir).expect("mkdir project");

        // User-level config for the fake cookbook (scope `cookbook-fake`).
        let user_config_dir = home_dir.join(".config/enwiro");
        std::fs::create_dir_all(&user_config_dir).expect("mkdir user config dir");
        std::fs::write(
            user_config_dir.join("cookbook-fake.toml"),
            "repo_globs = [\"from-user\"]\n",
        )
        .expect("write user config");

        // Project-level `.enwiro.toml` overrides `repo_globs` (allowlisted)
        // and tries to set `not_allowed` (should be dropped).
        std::fs::write(
            project_dir.join(".enwiro.toml"),
            "[cookbook-fake]\nrepo_globs = [\"from-project\"]\nnot_allowed = \"x\"\n",
        )
        .expect("write project config");

        // Fake shell-script cookbook that echoes the payload on `cook`.
        let script = project_dir.join("fake-cookbook");
        std::fs::write(
            &script,
            r#"#!/bin/sh
payload=$(cat)
echo "$payload"
"#,
        )
        .expect("write script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("chmod script");

        // Resolve config via the SDK's loader with the project as cwd.
        let config = crate::config::ConfigLoader::with_home(home_dir.clone())
            .build_cookbook_config(&project_dir, "cookbook-fake", &["repo_globs"])
            .expect("build_cookbook_config succeeds");

        let metadata = CookbookMetadata {
            default_priority: Some(99),
            project_overridable: vec!["repo_globs".to_string()],
        };
        let plugin = Plugin {
            name: "fake".to_string(),
            kind: PluginKind::Cookbook,
            executable: script.to_string_lossy().into_owned(),
        };
        let client = CookbookClient::with_metadata_and_config(plugin, metadata, config);

        let stdout = client.cook("anything").expect("cook returns stdout");
        let payload: CookbookPayload =
            serde_json::from_str(&stdout).expect("cookbook saw a valid CookbookPayload on stdin");

        assert_eq!(payload.version, 1, "payload version should be 1");
        assert_eq!(
            payload.config["repo_globs"],
            serde_json::json!(["from-project"]),
            "project layer must win over user layer for the allowlisted key"
        );
        assert!(
            payload.config.get("not_allowed").is_none(),
            "non-allowlisted key must be dropped before reaching the cookbook; got {:?}",
            payload.config
        );
    }
}
