use anyhow::{Context, anyhow};
use enwiro_sdk::rpc::{EnvMarkParams, EnwiroRpcClient};

use crate::{
    commands::adapter::{EnwiroAdapterExternal, EnwiroAdapterNone, EnwiroAdapterTrait},
    environments::Environment,
    notifier::{DesktopNotifier, Notifier},
};
use enwiro_daemon::ConfigurationValues;
use enwiro_sdk::client::{CachedRecipe, CookbookTrait, RpcCookbookClient};
use enwiro_sdk::plugin::{PluginKind, get_plugins};
use std::{collections::HashMap, io::Write, os::unix::fs::symlink, path::Path, path::PathBuf};

/// Per-invocation knobs for cooking an environment.
#[derive(Debug, Clone, Default)]
pub struct CookConfig {
    /// Skip firing garnish `run_on: [Cook]` cli entries. Gear files are still written.
    pub no_hooks: bool,
}

pub struct CommandContext<W: Write> {
    pub config: ConfigurationValues,
    pub writer: W,
    pub adapter: Box<dyn EnwiroAdapterTrait>,
    pub notifier: Box<dyn Notifier>,
    pub cookbooks: Vec<Box<dyn CookbookTrait>>,
    pub cache_dir: Option<PathBuf>,
    pub global_env: Option<String>,
}

impl<W: Write> CommandContext<W> {
    pub fn new(config: ConfigurationValues, writer: W) -> anyhow::Result<Self> {
        let adapter: Box<dyn EnwiroAdapterTrait> = match &config.adapter {
            None => {
                tracing::debug!("No adapter configured");
                Box::new(EnwiroAdapterNone {})
            }
            Some(adapter_name) => {
                tracing::debug!(adapter = %adapter_name, "Using adapter");
                Box::new(EnwiroAdapterExternal::new(adapter_name)?)
            }
        };

        let plugins = get_plugins(PluginKind::Cookbook);
        let mut cookbooks: Vec<Box<dyn CookbookTrait>> = plugins
            .into_iter()
            .map(|p| Box::new(RpcCookbookClient::new(p)) as Box<dyn CookbookTrait>)
            .collect();
        enwiro_sdk::client::sort_cookbooks(&mut cookbooks);

        tracing::debug!(count = cookbooks.len(), "Cookbooks loaded");

        let notifier: Box<dyn Notifier> = Box::new(DesktopNotifier);

        Ok(Self {
            config,
            writer,
            adapter,
            notifier,
            cookbooks,
            cache_dir: None,
            global_env: None,
        })
    }

    pub fn cook_environment(&self, name: &str, cfg: &CookConfig) -> anyhow::Result<Environment> {
        let (cookbook_name, description) = self.find_recipe_in_cache(name).ok_or_else(|| {
            tracing::error!(name = %name, "Recipe not in daemon cache");
            anyhow!(
                "No recipe '{}' in the daemon cache. \
                 Check: systemctl --user status enwiro-daemon.service",
                name
            )
        })?;

        let cookbook = self
            .cookbooks
            .iter()
            .find(|c| c.name() == cookbook_name)
            .ok_or_else(|| {
                anyhow!(
                    "Cache lists recipe '{}' under cookbook '{}', which is not installed",
                    name,
                    cookbook_name
                )
            })?;

        tracing::debug!(name = %name, cookbook = %cookbook_name, "Found recipe in cache");
        let env_path = cookbook.cook(name)?;
        let env = self.create_environment_symlink(name, &env_path)?;
        let flat_name = name.replace('/', "-");
        self.save_cook_metadata(&flat_name, &cookbook_name, name, description.as_deref());
        self.write_gear_if_present(cookbook.as_ref(), name, &flat_name);
        self.write_garnish_gear(&env_path, &flat_name, cfg);
        mark_via_daemon(&flat_name, "active");
        Ok(env)
    }

    pub fn find_recipe_in_cache_by_name(&self, recipe_name: &str) -> bool {
        self.find_recipe_in_cache(recipe_name).is_some()
    }

    fn find_recipe_in_cache(&self, recipe_name: &str) -> Option<(String, Option<String>)> {
        let cache = match &self.cache_dir {
            Some(dir) => enwiro_daemon::DaemonCache::with_runtime_dir(dir.clone()),
            None => enwiro_daemon::DaemonCache::open().ok()?,
        };
        let cached = cache.read_recipes().ok()??;
        for line in cached.lines() {
            if line.is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<CachedRecipe>(line)
                && entry.name == recipe_name
            {
                return Some((entry.cookbook, entry.description));
            }
        }
        None
    }

    fn save_cook_metadata(
        &self,
        env_name: &str,
        cookbook: &str,
        recipe: &str,
        description: Option<&str>,
    ) {
        let env_dir = Path::new(&self.config.workspaces_directory).join(env_name);
        crate::usage_stats::record_cook_metadata_per_env(&env_dir, cookbook, recipe, description);
    }

    /// Run every discovered Garnish plugin against the cooked project;
    /// write each contribution to `gear.d/garnish-<name>.json`, then
    /// fire any cli entry whose `run_on` contains `Cook` (unless
    /// `cfg.no_hooks` is set). Best-effort throughout — per-Garnish
    /// failures and autorun spawn failures are debug-logged and swallowed.
    fn write_garnish_gear(&self, project_dir: &str, flat_name: &str, cfg: &CookConfig) {
        let project_path = Path::new(project_dir);
        let env_dir = Path::new(&self.config.workspaces_directory).join(flat_name);
        let gear_dir = enwiro_sdk::gear::gear_dir(&env_dir);

        use enwiro_sdk::garnish::Garnish;
        for plugin in enwiro_sdk::plugin::get_plugins(enwiro_sdk::plugin::PluginKind::Garnish) {
            let garnish = enwiro_sdk::garnish::GarnishClient::new(plugin);
            let Some(data) = enwiro_sdk::garnish::run_garnish(&garnish, project_path) else {
                continue;
            };
            let path = gear_dir.join(garnish.filename());
            let result = serde_json::to_vec(&data)
                .map_err(anyhow::Error::from)
                .and_then(|bytes| enwiro_sdk::fs::atomic_write(&path, &bytes).map_err(Into::into));
            if let Err(e) = result {
                tracing::debug!(error = %e, garnish = garnish.name(), "garnish gear write failed, continuing");
                continue;
            }
            fire_hooks_if_enabled(cfg, &data, project_path);
        }
    }

    fn write_gear_if_present(&self, cookbook: &dyn CookbookTrait, recipe: &str, flat_name: &str) {
        match cookbook.gear(recipe) {
            Ok(Some(json)) => {
                let env_dir = Path::new(&self.config.workspaces_directory).join(flat_name);
                let gear_path = enwiro_sdk::gear::gear_dir(&env_dir)
                    .join(enwiro_sdk::gear::gear_filename(cookbook.name()));
                match serde_json::to_vec(&json) {
                    Ok(bytes) => {
                        if let Err(e) = enwiro_sdk::fs::atomic_write(&gear_path, &bytes) {
                            tracing::debug!(error = %e, "Failed to write gear file, continuing");
                        }
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "Failed to serialise gear JSON, continuing");
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::debug!(error = %e, "Cookbook gear() returned error, continuing");
            }
        }
    }

    fn create_environment_symlink(
        &self,
        name: &str,
        env_path: &str,
    ) -> anyhow::Result<Environment> {
        let flat_name = name.replace('/', "-");
        let env_dir = Path::new(&self.config.workspaces_directory).join(&flat_name);
        std::fs::create_dir_all(&env_dir)?;
        let inner_symlink = env_dir.join(&flat_name);
        tracing::info!(name = %name, target = %env_path, "Creating environment symlink");
        if inner_symlink.is_symlink() || inner_symlink.exists() {
            std::fs::remove_file(&inner_symlink)?;
        }
        symlink(Path::new(env_path), &inner_symlink)?;
        self.notifier
            .notify_success(&format!("Created environment: {}", name));
        Environment::get_one(&self.config.workspaces_directory, &flat_name)
    }

    fn resolve_environment_name(&self, name: &Option<String>) -> anyhow::Result<String> {
        if let Some(n) = name {
            return Ok(n.clone());
        }
        if let Some(n) = &self.global_env {
            return Ok(n.clone());
        }
        self.adapter
            .get_active_environment_name()
            .context("Could not determine active environment")
    }

    pub fn get_or_cook_environment(
        &self,
        name: &Option<String>,
        cfg: &CookConfig,
    ) -> anyhow::Result<Environment> {
        let explicitly_named = name.is_some() || self.global_env.is_some();
        let resolved = self.resolve_environment_name(name)?;
        let flat_name = resolved.replace('/', "-");
        match Environment::get_one(&self.config.workspaces_directory, &flat_name) {
            Ok(env) => Ok(env),
            Err(_) if explicitly_named => self
                .cook_environment(&resolved, cfg)
                .context("Could not cook environment"),
            Err(e) => Err(e),
        }
    }

    pub fn get_all_environments(&self) -> anyhow::Result<HashMap<String, Environment>> {
        Environment::get_all(&self.config.workspaces_directory)
    }
}

pub(crate) fn mark_via_daemon(env_name: &str, status: &str) {
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    let _ = rt.block_on(async {
        let client = enwiro_sdk::rpc::connect().await.ok()?;
        EnwiroRpcClient::env_mark(
            &client,
            EnvMarkParams {
                env_name: env_name.to_string(),
                status: status.to_string(),
            },
        )
        .await
        .ok()
    });
}

/// Gate over `fire_autorun_on_cook` that respects `CookConfig::no_hooks`.
/// Calling this is what every garnish-iteration site does — the gate is
/// extracted as its own function so `--no-hooks` can be tested behaviorally
/// without needing a real on-disk garnish.
fn fire_hooks_if_enabled(
    cfg: &CookConfig,
    data: &enwiro_sdk::gear::GearFileData,
    project_path: &Path,
) {
    if cfg.no_hooks {
        return;
    }
    fire_autorun_on_cook(data, project_path);
}

/// For every cli entry in `data` whose `run_on` contains `Cook`, spawn
/// it in `project_path`. Best-effort: empty commands and spawn failures
/// are debug-logged and skipped. Spawned children are not waited on —
/// the daemon never blocks on autorun.
fn fire_autorun_on_cook(data: &enwiro_sdk::gear::GearFileData, project_path: &Path) {
    use enwiro_sdk::gear::Hook;
    for (gear_name, gear) in &data.gear {
        for (entry_name, entry) in &gear.cli {
            if !entry.run_on.contains(&Hook::Cook) {
                continue;
            }
            // Autorun is non-interactive — no user to answer a prompt.
            if entry.require_confirmation {
                tracing::debug!(
                    gear = gear_name,
                    entry = entry_name,
                    "autorun cli entry requires confirmation; skipping"
                );
                continue;
            }
            let Some((bin, args)) = entry.command.split_first() else {
                tracing::debug!(
                    gear = gear_name,
                    entry = entry_name,
                    "autorun cli entry has empty command; skipping"
                );
                continue;
            };
            match std::process::Command::new(bin)
                .args(args)
                .current_dir(project_path)
                .spawn()
            {
                Ok(_) => tracing::debug!(gear = gear_name, entry = entry_name, "autorun fired"),
                Err(e) => tracing::debug!(
                    gear = gear_name,
                    entry = entry_name,
                    error = %e,
                    "autorun spawn failed; continuing"
                ),
            }
        }
    }
}

#[cfg(test)]
mod fire_autorun_tests {
    use super::fire_autorun_on_cook;
    use enwiro_sdk::gear::{CliEntry, Gear, GearFileData, Hook, SCHEMA_VERSION};
    use std::collections::HashMap;
    use std::path::Path;
    use std::time::{Duration, Instant};

    fn touch_command(path: &Path) -> Vec<String> {
        vec!["touch".into(), path.to_str().unwrap().into()]
    }

    /// Spawn is non-blocking, so we poll for the sentinel to appear.
    fn wait_for(path: &Path, max: Duration) -> bool {
        let deadline = Instant::now() + max;
        while Instant::now() < deadline {
            if path.exists() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        path.exists()
    }

    /// An entry that requires confirmation must not autorun — there's no
    /// user present at cook time to answer the prompt.
    #[test]
    fn skips_entries_that_require_confirmation() {
        let tmp = tempfile::tempdir().unwrap();
        let sentinel = tmp.path().join("must-not-fire");

        let mut cli = HashMap::new();
        cli.insert(
            "gated".to_owned(),
            CliEntry {
                description: None,
                command: touch_command(&sentinel),
                run_on: vec![Hook::Cook],
                require_confirmation: true,
                ..Default::default()
            },
        );
        let mut gear_map = HashMap::new();
        gear_map.insert(
            "g".to_owned(),
            Gear {
                description: "test".into(),
                cli,
                ..Default::default()
            },
        );
        let data = GearFileData {
            version: SCHEMA_VERSION,
            gear: gear_map,
        };

        fire_autorun_on_cook(&data, tmp.path());

        // Wait briefly so a buggy implementation that *did* spawn has
        // time to create the sentinel.
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !sentinel.exists(),
            "require_confirmation: true entry must NOT autorun (unexpected sentinel at {sentinel:?})"
        );
    }

    /// Fires Cook-tagged entries, skips entries without `run_on: [Cook]`.
    #[test]
    fn fires_cook_entries_and_skips_untagged() {
        let tmp = tempfile::tempdir().unwrap();
        let fires = tmp.path().join("fires");
        let skipped = tmp.path().join("skipped");

        let mut cli = HashMap::new();
        cli.insert(
            "should-fire".to_owned(),
            CliEntry {
                description: None,
                command: touch_command(&fires),
                run_on: vec![Hook::Cook],
                ..Default::default()
            },
        );
        cli.insert(
            "should-skip".to_owned(),
            CliEntry {
                description: None,
                command: touch_command(&skipped),
                run_on: vec![],
                ..Default::default()
            },
        );
        let mut gear_map = HashMap::new();
        gear_map.insert(
            "g".to_owned(),
            Gear {
                description: "test".into(),
                cli,
                ..Default::default()
            },
        );
        let data = GearFileData {
            version: SCHEMA_VERSION,
            gear: gear_map,
        };

        fire_autorun_on_cook(&data, tmp.path());

        assert!(
            wait_for(&fires, Duration::from_secs(2)),
            "Cook-tagged entry should have fired (sentinel at {fires:?} missing)"
        );
        assert!(
            !skipped.exists(),
            "Untagged entry must not fire (unexpected sentinel at {skipped:?})"
        );
    }

    /// Empty command must not panic or crash; the entry is just skipped.
    #[test]
    fn empty_command_is_skipped_silently() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cli = HashMap::new();
        cli.insert(
            "empty".to_owned(),
            CliEntry {
                description: None,
                command: vec![],
                run_on: vec![Hook::Cook],
                ..Default::default()
            },
        );
        let mut gear_map = HashMap::new();
        gear_map.insert(
            "g".to_owned(),
            Gear {
                description: "test".into(),
                cli,
                ..Default::default()
            },
        );
        let data = GearFileData {
            version: SCHEMA_VERSION,
            gear: gear_map,
        };

        fire_autorun_on_cook(&data, tmp.path()); // must not panic
    }

    fn single_cook_entry_gear(sentinel: &Path) -> GearFileData {
        let mut cli = HashMap::new();
        cli.insert(
            "fires".to_owned(),
            CliEntry {
                description: None,
                command: touch_command(sentinel),
                run_on: vec![Hook::Cook],
                ..Default::default()
            },
        );
        let mut gear_map = HashMap::new();
        gear_map.insert(
            "g".to_owned(),
            Gear {
                description: "test".into(),
                cli,
                ..Default::default()
            },
        );
        GearFileData {
            version: SCHEMA_VERSION,
            gear: gear_map,
        }
    }

    /// `fire_hooks_if_enabled` with `no_hooks: false` must call through to the
    /// spawn site — sanity check that the gate doesn't accidentally block the
    /// default path.
    #[test]
    fn gate_open_fires_autorun() {
        let tmp = tempfile::tempdir().unwrap();
        let sentinel = tmp.path().join("did-fire");
        let data = single_cook_entry_gear(&sentinel);

        super::fire_hooks_if_enabled(&super::CookConfig { no_hooks: false }, &data, tmp.path());

        assert!(
            wait_for(&sentinel, Duration::from_millis(500)),
            "expected sentinel at {sentinel:?} after fire_hooks_if_enabled with no_hooks=false"
        );
    }

    /// `fire_hooks_if_enabled` with `no_hooks: true` must NOT spawn the autorun.
    /// This is the load-bearing behavior of the `--no-hooks` flag on `prep` and
    /// `activate`: the flag flows through `CookConfig` to this gate.
    #[test]
    fn gate_closed_suppresses_autorun() {
        let tmp = tempfile::tempdir().unwrap();
        let sentinel = tmp.path().join("must-not-fire");
        let data = single_cook_entry_gear(&sentinel);

        super::fire_hooks_if_enabled(&super::CookConfig { no_hooks: true }, &data, tmp.path());

        // Wait briefly so a buggy implementation that *did* spawn has time
        // to create the sentinel.
        std::thread::sleep(Duration::from_millis(150));
        assert!(
            !sentinel.exists(),
            "no_hooks=true must suppress autorun (unexpected sentinel at {sentinel:?})"
        );
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use std::fs;

    use super::CookConfig;
    use crate::test_utils::test_utilities::{
        AdapterLog, FailingCookbook, FakeContext, FakeCookbook, NotificationLog, context_object,
    };

    #[rstest]
    fn test_cook_environment_creates_symlink_for_matching_recipe(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;

        // Create a real directory that the cookbook will "cook" (point to)
        let cooked_dir = temp_dir.path().join("cooked-target");
        fs::create_dir(&cooked_dir).unwrap();

        context_object.write_cache_entry("git", "my-project");
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["my-project"],
            vec![("my-project", cooked_dir.to_str().unwrap())],
        ))];

        let env = context_object
            .cook_environment("my-project", &CookConfig::default())
            .unwrap();
        assert_eq!(env.name, "my-project");

        // Verify directory with inner symlink was created
        let env_dir = temp_dir.path().join("my-project");
        assert!(env_dir.is_dir());
        let inner_link = env_dir.join("my-project");
        assert!(inner_link.is_symlink());
    }

    #[rstest]
    fn test_cook_environment_with_slash_in_name(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;

        let cooked_dir = temp_dir.path().join("cooked-target");
        fs::create_dir(&cooked_dir).unwrap();

        let recipe_name = "my-project@feature/my-thing";
        context_object.write_cache_entry("git", recipe_name);
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec![recipe_name],
            vec![(recipe_name, cooked_dir.to_str().unwrap())],
        ))];

        let env = context_object
            .cook_environment(recipe_name, &CookConfig::default())
            .unwrap();
        assert_eq!(env.name, "my-project@feature-my-thing");

        // Verify directory with inner symlink was created
        let env_dir = temp_dir.path().join("my-project@feature-my-thing");
        assert!(env_dir.is_dir());
        let inner_link = env_dir.join("my-project@feature-my-thing");
        assert!(inner_link.is_symlink());
    }

    #[rstest]
    fn test_get_or_cook_finds_existing_environment_with_slash_in_name(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;

        let cooked_dir = temp_dir.path().join("cooked-target");
        fs::create_dir(&cooked_dir).unwrap();

        let recipe_name = "my-project@feature/my-thing";
        context_object.write_cache_entry("git", recipe_name);
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec![recipe_name],
            vec![(recipe_name, cooked_dir.to_str().unwrap())],
        ))];

        // First call creates the environment
        let env1 = context_object
            .get_or_cook_environment(&Some(recipe_name.to_string()), &CookConfig::default())
            .unwrap();

        // Second call should find the existing environment, not try to cook again
        let env2 = context_object
            .get_or_cook_environment(&Some(recipe_name.to_string()), &CookConfig::default())
            .unwrap();

        assert_eq!(env1.name, env2.name);
    }

    #[rstest]
    fn test_cook_environment_errors_when_recipe_not_in_cache(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        // Cookbook can produce the recipe, but the cache does not list it.
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["my-project"],
            vec![],
        ))];

        let result = context_object.cook_environment("my-project", &CookConfig::default());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("daemon"),
            "Error should point at the daemon, got: {err}"
        );
    }

    #[rstest]
    fn test_cook_environment_uses_cache_to_skip_slow_cookbooks(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;

        let cooked_dir = temp_dir.path().join("cooked-target");
        fs::create_dir(&cooked_dir).unwrap();

        // Write a cache that knows about the recipe
        let cache_dir = context_object.cache_dir.as_ref().unwrap();
        fs::create_dir_all(cache_dir).unwrap();
        fs::write(
            cache_dir.join("recipes.cache"),
            "{\"cookbook\":\"git\",\"name\":\"my-project\"}\n",
        )
        .unwrap();

        // FailingCookbook simulates a slow cookbook (like GitHub) that would
        // block or error if list_recipes() is called. The working cookbook is second.
        context_object.cookbooks = vec![
            Box::new(FailingCookbook {
                cookbook_name: "github".into(),
            }),
            Box::new(FakeCookbook::new(
                "git",
                vec!["my-project"],
                vec![("my-project", cooked_dir.to_str().unwrap())],
            )),
        ];

        // With the cache available, cook_environment should find the recipe
        // without calling list_recipes() on the failing cookbook
        let env = context_object
            .cook_environment("my-project", &CookConfig::default())
            .unwrap();
        assert_eq!(env.name, "my-project");
    }

    #[rstest]
    fn test_cook_environment_cache_hit_with_description(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;

        let cooked_dir = temp_dir.path().join("cooked-target");
        fs::create_dir(&cooked_dir).unwrap();

        context_object.write_cache_entries(&[("github", "owner/repo#42", Some("Fix auth bug"))]);
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "github",
            vec!["owner/repo#42"],
            vec![("owner/repo#42", cooked_dir.to_str().unwrap())],
        ))];

        let env = context_object
            .cook_environment("owner/repo#42", &CookConfig::default())
            .unwrap();
        assert_eq!(env.name, "owner-repo#42");
    }

    #[rstest]
    fn test_cook_environment_errors_when_cache_references_uninstalled_cookbook(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;

        let cooked_dir = temp_dir.path().join("cooked-target");
        fs::create_dir(&cooked_dir).unwrap();

        // Cache says recipe belongs to "npm" cookbook, but only "git" is installed.
        context_object.write_cache_entry("npm", "my-project");
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["my-project"],
            vec![("my-project", cooked_dir.to_str().unwrap())],
        ))];

        let result = context_object.cook_environment("my-project", &CookConfig::default());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("npm") && err.contains("not installed"),
            "Error should name the missing cookbook, got: {err}"
        );
    }

    #[rstest]
    fn test_get_or_cook_returns_existing_environment(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("my-env");

        let env = context_object
            .get_or_cook_environment(&Some("my-env".to_string()), &CookConfig::default())
            .unwrap();
        assert_eq!(env.name, "my-env");
    }

    #[rstest]
    fn test_get_or_cook_falls_back_to_cooking(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;

        let cooked_dir = temp_dir.path().join("cooked-target");
        fs::create_dir(&cooked_dir).unwrap();

        context_object.write_cache_entry("git", "new-project");
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["new-project"],
            vec![("new-project", cooked_dir.to_str().unwrap())],
        ))];

        // "new-project" doesn't exist as an environment, so it should be cooked
        let env = context_object
            .get_or_cook_environment(&Some("new-project".to_string()), &CookConfig::default())
            .unwrap();
        assert_eq!(env.name, "new-project");
    }

    #[rstest]
    fn test_global_env_used_when_positional_absent(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("from-flag");
        context_object.global_env = Some("from-flag".to_string());

        let env = context_object
            .get_or_cook_environment(&None, &CookConfig::default())
            .unwrap();
        assert_eq!(env.name, "from-flag");
    }

    #[rstest]
    fn test_positional_wins_over_global_env(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("positional");
        context_object.create_mock_environment("from-flag");
        context_object.global_env = Some("from-flag".to_string());

        let env = context_object
            .get_or_cook_environment(&Some("positional".to_string()), &CookConfig::default())
            .unwrap();
        assert_eq!(env.name, "positional");
    }

    #[rstest]
    fn test_global_env_triggers_autocook(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;

        let cooked_dir = temp_dir.path().join("cooked-target");
        fs::create_dir(&cooked_dir).unwrap();

        context_object.write_cache_entry("git", "new-project");
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["new-project"],
            vec![("new-project", cooked_dir.to_str().unwrap())],
        ))];
        context_object.global_env = Some("new-project".to_string());

        let env = context_object
            .get_or_cook_environment(&None, &CookConfig::default())
            .unwrap();
        assert_eq!(env.name, "new-project");
    }

    #[rstest]
    fn test_get_or_cook_does_not_cook_when_name_from_adapter(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;

        // No matching environment for "foobaz" (adapter's workspace name).
        // FailingCookbook would error if cook_environment tries list_recipes().
        context_object.cookbooks = vec![Box::new(FailingCookbook {
            cookbook_name: "github".into(),
        })];

        // Name is None → resolved from adapter → should not try to cook
        let result = context_object.get_or_cook_environment(&None, &CookConfig::default());
        assert!(result.is_err());
        // Error should NOT be from FailingCookbook ("simulated failure").
        // Use Debug format to see the full anyhow error chain.
        let err_debug = format!("{:?}", result.unwrap_err());
        assert!(
            !err_debug.contains("simulated failure"),
            "Should not have called cook_environment, but got: {}",
            err_debug
        );
    }

    #[rstest]
    fn test_cook_environment_saves_cookbook_to_stats(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;

        let cooked_dir = temp_dir.path().join("cooked-target");
        fs::create_dir(&cooked_dir).unwrap();

        context_object.write_cache_entry("git", "my-project");
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["my-project"],
            vec![("my-project", cooked_dir.to_str().unwrap())],
        ))];

        context_object
            .cook_environment("my-project", &CookConfig::default())
            .unwrap();

        let env_dir = temp_dir.path().join("my-project");
        let meta = crate::usage_stats::load_env_meta(&env_dir);
        assert_eq!(meta.cookbook.as_deref(), Some("git"));
        assert_eq!(meta.recipe.as_deref(), Some("my-project"));
    }

    #[rstest]
    fn test_cook_environment_saves_unflattened_recipe_name(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;

        let cooked_dir = temp_dir.path().join("cooked-target");
        fs::create_dir(&cooked_dir).unwrap();

        let recipe_name = "owner/repo#42";
        context_object.write_cache_entry("github", recipe_name);
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "github",
            vec![recipe_name],
            vec![(recipe_name, cooked_dir.to_str().unwrap())],
        ))];

        context_object
            .cook_environment(recipe_name, &CookConfig::default())
            .unwrap();

        let env_dir = temp_dir.path().join("owner-repo#42");
        let meta = crate::usage_stats::load_env_meta(&env_dir);
        assert_eq!(
            meta.recipe.as_deref(),
            Some(recipe_name),
            "recipe must persist the unflattened id so refresh can re-run `cookbook gear <recipe>`"
        );
    }

    #[rstest]
    fn test_cook_environment_stats_keyed_by_flat_name(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;

        let cooked_dir = temp_dir.path().join("cooked-target");
        fs::create_dir(&cooked_dir).unwrap();

        context_object.write_cache_entries(&[("github", "owner/repo#42", Some("Fix auth bug"))]);
        context_object.cookbooks = vec![Box::new(FakeCookbook::new_with_descriptions(
            "github",
            vec![("owner/repo#42", Some("Fix auth bug"))],
            vec![("owner/repo#42", cooked_dir.to_str().unwrap())],
        ))];

        context_object
            .cook_environment("owner/repo#42", &CookConfig::default())
            .unwrap();

        // Meta should be stored in the flat-named env directory
        let env_dir = temp_dir.path().join("owner-repo#42");
        assert!(
            env_dir.is_dir(),
            "Env directory should exist with flat name"
        );
        let meta = crate::usage_stats::load_env_meta(&env_dir);
        assert_eq!(meta.description.as_deref(), Some("Fix auth bug"));
        assert_eq!(meta.cookbook.as_deref(), Some("github"));
    }

    #[rstest]
    fn test_cook_environment_sends_notification(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, notifications) = context_object;

        let cooked_dir = temp_dir.path().join("cooked-target");
        fs::create_dir(&cooked_dir).unwrap();

        context_object.write_cache_entry("git", "my-project");
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["my-project"],
            vec![("my-project", cooked_dir.to_str().unwrap())],
        ))];

        let result = context_object.cook_environment("my-project", &CookConfig::default());
        assert!(result.is_ok());

        let logs = notifications.borrow();
        assert_eq!(logs.len(), 1);
        assert!(logs[0].starts_with("SUCCESS:"));
        assert!(logs[0].contains("my-project"));
    }

    #[rstest]
    fn test_cook_environment_no_notification_on_failure(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, notifications) = context_object;

        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["other-project"],
            vec![],
        ))];

        let result = context_object.cook_environment("my-project", &CookConfig::default());
        assert!(result.is_err());

        let logs = notifications.borrow();
        assert_eq!(logs.len(), 0);
    }

    #[rstest]
    fn test_cook_environment_writes_gear_file_to_cookbook_named_path(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;

        let cooked_dir = temp_dir.path().join("cooked-target");
        fs::create_dir(&cooked_dir).unwrap();

        context_object.write_cache_entry("git", "my-project");
        let gear_data = serde_json::json!({"tool": "nvim", "lsp": "rust-analyzer"});
        context_object.cookbooks = vec![Box::new(
            FakeCookbook::new(
                "git",
                vec!["my-project"],
                vec![("my-project", cooked_dir.to_str().unwrap())],
            )
            .with_gear(gear_data.clone()),
        )];

        context_object
            .cook_environment("my-project", &CookConfig::default())
            .unwrap();

        let gear_path = temp_dir
            .path()
            .join("my-project")
            .join("gear.d")
            .join("cookbook-git.json");
        assert!(
            gear_path.exists(),
            "gear file should exist at gear.d/cookbook-<name>.json, expected {}",
            gear_path.display()
        );
        let written: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&gear_path).unwrap()).unwrap();
        assert_eq!(written, gear_data);
    }

    #[rstest]
    fn test_cook_environment_does_not_create_gear_dir_when_cookbook_returns_none(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;

        let cooked_dir = temp_dir.path().join("cooked-target");
        fs::create_dir(&cooked_dir).unwrap();

        context_object.write_cache_entry("git", "my-project");
        // FakeCookbook with no gear (default behaviour)
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["my-project"],
            vec![("my-project", cooked_dir.to_str().unwrap())],
        ))];

        context_object
            .cook_environment("my-project", &CookConfig::default())
            .unwrap();

        let gear_dir = temp_dir.path().join("my-project").join("gear.d");
        assert!(
            !gear_dir.exists(),
            "gear.d/ should NOT exist when cookbook returns no gear"
        );
    }
}
