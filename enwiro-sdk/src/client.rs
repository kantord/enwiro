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
    /// See `Recipe::equivalent_to`: carried through the daemon cache so `ls`
    /// can hide a recipe once an equivalent one has been cooked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub equivalent_to: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scores: Option<EnvScores>,
}

/// Cache-file counterpart of [`crate::cookbook::PatternRecipe`]: a pattern
/// claim with its owning cookbook. The pattern is stored anchored and the
/// description template pre-validated (see [`crate::recipe_pattern`]). Pattern
/// lines have no `name`, so cache consumers that predate patterns skip them
/// as unparseable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedPatternRecipe {
    pub cookbook: String,
    pub pattern: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// One line of the daemon recipe cache. Untagged: pattern lines carry
/// `pattern` instead of `name`, concrete lines are unchanged [`CachedRecipe`]s.
/// `Concrete` stays first for the same reason as [`crate::cookbook::RecipeItem`]:
/// a stray `pattern` field must not flip a named entry into a claim.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CachedEntry {
    Concrete(CachedRecipe),
    Pattern(CachedPatternRecipe),
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
    /// Return host paths (beyond the env's own project directory) this
    /// recipe's environment depends on to function -- e.g. a git worktree
    /// needs its main repo's `.git` alongside it. The cookbook reports
    /// plain paths only: it has no notion of *why* a consumer needs them
    /// (bind-mounting into a container, or nothing at all on the host
    /// path). If non-empty after cooking, written to
    /// `<env>/external-paths.d/cookbook-<name>.json`, where the env-side
    /// reader (`enwiro_sdk::external_paths::load_external_paths`) merges
    /// every cookbook's contribution.
    fn external_paths(&self, _recipe: &str) -> anyhow::Result<Vec<String>> {
        Ok(Vec::new())
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

/// Parse `invoke_result`'s stdout as JSON `T`, or `None` for any failure
/// (the RPC call itself failing, or malformed JSON). Shared by every
/// cookbook-declared-metadata subcommand invoked via the daemon RPC (`gear`,
/// `external-paths`, and future ones): all of them are best-effort by the
/// same contract, so callers turn `None` into whatever "nothing declared"
/// looks like for their own return type.
fn best_effort_json_via_rpc<T: serde::de::DeserializeOwned>(
    invoke_result: anyhow::Result<String>,
    cookbook: &str,
    subcommand: &str,
) -> Option<T> {
    let stdout = match invoke_result {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(%cookbook, %subcommand, error = %e, "Cookbook RPC failed");
            return None;
        }
    };
    match serde_json::from_str(&stdout) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::debug!(%cookbook, %subcommand, error = %e, "Cookbook stdout was not valid JSON");
            None
        }
    }
}

/// Same contract as [`best_effort_json_via_rpc`], for the `CookbookClient`
/// (direct subprocess) path: also checks the exit status before parsing,
/// since a spawned subcommand can fail without ever erroring on spawn.
fn best_effort_json_via_subprocess<T: serde::de::DeserializeOwned>(
    spawn_result: anyhow::Result<Output>,
    cookbook: &str,
    recipe: &str,
    subcommand: &str,
) -> Option<T> {
    let output = match spawn_result {
        Ok(o) => o,
        Err(e) => {
            tracing::debug!(%cookbook, %subcommand, error = %e, "Cookbook exec failed");
            return None;
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::debug!(%cookbook, %subcommand, %recipe, %stderr, "Cookbook subcommand returned non-zero");
        return None;
    }
    match serde_json::from_slice(&output.stdout) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::debug!(%cookbook, %subcommand, error = %e, "Cookbook stdout was not valid JSON");
            None
        }
    }
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

    /// The resolved cookbook config (typed as `serde_json::Value`).
    /// Used by callers (e.g. the daemon) that need to forward the same
    /// payload to a long-running `listen` subprocess over stdin.
    pub fn config(&self) -> &serde_json::Value {
        &self.config
    }

    /// The cookbook's probed metadata. Used by the daemon to gate
    /// capability-specific behavior (e.g. spawning `listen`) on what the
    /// cookbook actually declares.
    pub fn metadata(&self) -> &CookbookMetadata {
        &self.metadata
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

    pub(crate) fn fetch_metadata(executable: &str) -> CookbookMetadata {
        crate::metadata::fetch_metadata(executable)
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

/// A `CookbookTrait` implementation that delegates every operation to the
/// running `enwiro-daemon` over the JSON-RPC IPC defined in ADR-0002.
///
/// The construction side (metadata fetch + project-layer config resolution)
/// stays on the caller's side because the daemon has no concept of the
/// caller's cwd; the daemon only spawns the cookbook with the resolved
/// payload that the caller pre-computed. On each operation the resolved
/// `payload` is sent through the RPC to the daemon, which writes it to the
/// cookbook's stdin as a `CookbookPayload`.
///
/// Sync façade over the async `Client`: holds a current-thread tokio runtime
/// per cookbook client. The cost is one runtime per cookbook on construction;
/// `enw` constructs a handful of cookbook clients in `CommandContext::new`
/// and then reuses them for the lifetime of the process, so this is fine.
pub struct RpcCookbookClient {
    plugin: Plugin,
    metadata: CookbookMetadata,
    config: serde_json::Value,
    runtime: tokio::runtime::Runtime,
}

impl RpcCookbookClient {
    /// Construct an RPC-backed cookbook client. Behaves like
    /// `CookbookClient::new` from the caller's POV: resolves project-layer
    /// config using `current_dir()` and fetches metadata directly via
    /// subprocess (metadata is a one-shot read on construction, not a hot
    /// path, and doesn't need the daemon).
    pub fn new(plugin: Plugin) -> Self {
        let metadata = CookbookClient::fetch_metadata(&plugin.executable);
        let config = resolve_config_with_walker(&plugin, &metadata);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread tokio runtime for RpcCookbookClient");
        Self {
            plugin,
            metadata,
            config,
            runtime,
        }
    }

    /// Run one `cookbook.invoke` RPC and return the cookbook's raw stdout.
    /// Connects fresh on every call — cheap for our usage pattern (one or
    /// two cooks per `enw` invocation), and avoids long-held connections.
    fn invoke(&self, op: &str, args: Vec<String>) -> anyhow::Result<String> {
        let cookbook = self.plugin.name.to_string();
        let op = op.to_string();
        let payload = self.config.clone();
        let call_chain = current_call_chain();

        self.runtime.block_on(async move {
            use crate::rpc::EnwiroRpcClient;
            let client = crate::rpc::connect()
                .await
                .context("connect to enwiro-daemon")?;
            let result = client
                .cookbook_invoke(crate::rpc::CookbookInvokeParams {
                    cookbook,
                    op,
                    args,
                    payload,
                    call_chain,
                })
                .await
                .context("rpc cookbook.invoke")?;
            Ok(result.stdout)
        })
    }
}

/// Parse `$ENWIRO_RPC_CALL_CHAIN` (colon-separated cookbook names, set by
/// the daemon when one cookbook invokes another) into an ordered list.
/// Empty / unset env var yields an empty chain.
fn current_call_chain() -> Vec<String> {
    std::env::var(crate::rpc::CALL_CHAIN_ENV_VAR)
        .unwrap_or_default()
        .split(':')
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned)
        .collect()
}

impl CookbookTrait for RpcCookbookClient {
    fn list_recipes(&self) -> anyhow::Result<Vec<Recipe>> {
        let cookbook: &str = self.plugin.name.as_str();
        tracing::debug!(%cookbook, "Listing recipes via daemon RPC");
        let stdout = self
            .invoke("list-recipes", vec![])
            .with_context(|| format!("cookbook '{cookbook}' failed during 'list-recipes'"))?;
        Ok(stdout
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| {
                serde_json::from_str::<Recipe>(line).unwrap_or_else(|e| {
                    tracing::warn!(
                        %cookbook, error = %e, %line,
                        "cookbook list-recipes produced non-Recipe-JSON line; treating as a bare name"
                    );
                    Recipe::new(line)
                })
            })
            .collect())
    }

    fn cook(&self, recipe: &str) -> anyhow::Result<String> {
        let cookbook = self.plugin.name.as_str();
        tracing::debug!(%cookbook, %recipe, "Cooking recipe via daemon RPC");
        let stdout = self
            .invoke("cook", vec![recipe.to_string()])
            .with_context(|| format!("cookbook '{cookbook}' failed during 'cook {recipe}'"))?;
        Ok(stdout.trim().to_string())
    }

    fn name(&self) -> &str {
        self.plugin.name.as_str()
    }

    fn priority(&self) -> u32 {
        self.metadata.default_priority.unwrap_or(DEFAULT_PRIORITY)
    }

    /// Best-effort `gear <recipe>` via the daemon -- see
    /// [`best_effort_json_via_rpc`] for the shared failure contract.
    fn gear(&self, recipe: &str) -> anyhow::Result<Option<serde_json::Value>> {
        Ok(best_effort_json_via_rpc(
            self.invoke("gear", vec![recipe.to_string()]),
            self.plugin.name.as_str(),
            "gear",
        ))
    }

    /// Best-effort `external-paths <recipe>` via the daemon -- see
    /// [`best_effort_json_via_rpc`] for the shared failure contract.
    fn external_paths(&self, recipe: &str) -> anyhow::Result<Vec<String>> {
        Ok(best_effort_json_via_rpc(
            self.invoke("external-paths", vec![recipe.to_string()]),
            self.plugin.name.as_str(),
            "external-paths",
        )
        .unwrap_or_default())
    }
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
        self.plugin.name.as_str()
    }

    fn priority(&self) -> u32 {
        self.metadata.default_priority.unwrap_or(DEFAULT_PRIORITY)
    }

    /// Invoke the cookbook binary's optional `gear <recipe>` subcommand --
    /// see [`best_effort_json_via_subprocess`] for the shared failure
    /// contract.
    fn gear(&self, recipe: &str) -> anyhow::Result<Option<serde_json::Value>> {
        Ok(best_effort_json_via_subprocess(
            self.spawn_with_payload(&["gear", recipe]),
            self.plugin.name.as_str(),
            recipe,
            "gear",
        ))
    }

    /// Invoke the cookbook binary's optional `external-paths <recipe>`
    /// subcommand -- see [`best_effort_json_via_subprocess`] for the shared
    /// failure contract.
    fn external_paths(&self, recipe: &str) -> anyhow::Result<Vec<String>> {
        Ok(best_effort_json_via_subprocess(
            self.spawn_with_payload(&["external-paths", recipe]),
            self.plugin.name.as_str(),
            recipe,
            "external-paths",
        )
        .unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::PluginKind;

    fn mock_plugin(name: &str) -> Plugin {
        Plugin {
            name: crate::plugin::PluginName::new(name).unwrap(),
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
                ..Default::default()
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

    /// Regression guard for AC #1: "without `.enwiro.toml` present, behavior
    /// is identical to today (user-level files still loaded; no regression)."
    /// With no user-level TOML and no project-level TOML, the SDK must
    /// produce an empty-object config so cookbook structs with
    /// `#[serde(default)]` can deserialize to defaults instead of erroring
    /// on missing fields. Also exercises the shell-script cookbook path
    /// (proves the language-agnostic protocol works with no payload).
    #[test]
    fn test_shell_cookbook_receives_defaults_when_no_user_or_project_config() {
        use std::os::unix::fs::PermissionsExt;

        let tempdir = tempfile::tempdir().expect("tempdir");
        let home_dir = tempdir.path().join("home");
        let project_dir = tempdir.path().join("proj");
        std::fs::create_dir_all(&home_dir).expect("mkdir home");
        std::fs::create_dir_all(&project_dir).expect("mkdir project");

        // Deliberately do NOT write any user or project config files.

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

        let config = crate::config::ConfigLoader::with_home(home_dir.clone())
            .build_cookbook_config(&project_dir, "cookbook-fake", &["repo_globs"])
            .expect("no-files build_cookbook_config succeeds");

        // The loader must give the cookbook an object (not null) so that
        // `serde_json::from_value::<CookbookConfig>(payload.config)` against
        // a struct with `#[serde(default)]` deserializes to defaults.
        assert!(
            config.is_object(),
            "config from a no-files load must be a JSON object so #[serde(default)] structs deserialize cleanly; got {config:?}"
        );

        let plugin = Plugin {
            name: crate::plugin::PluginName::new("fake").unwrap(),
            kind: PluginKind::Cookbook,
            executable: script.to_string_lossy().into_owned(),
        };
        let client =
            CookbookClient::with_metadata_and_config(plugin, CookbookMetadata::default(), config);

        let stdout = client.cook("anything").expect("cook returns stdout");
        let payload: CookbookPayload =
            serde_json::from_str(&stdout).expect("cookbook saw a valid CookbookPayload on stdin");
        assert!(
            payload.config.is_object(),
            "cookbook must see config as an object (not null) so its #[serde(default)] struct can deserialize; got {:?}",
            payload.config
        );
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
            ..Default::default()
        };
        let plugin = Plugin {
            name: crate::plugin::PluginName::new("fake").unwrap(),
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
