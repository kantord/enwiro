use anyhow::{Context, anyhow};
use enwiro_sdk::rpc::{EnvMarkParams, EnwiroRpcClient};

use crate::{
    commands::adapter::{EnwiroAdapterExternal, EnwiroAdapterNone, EnwiroAdapterTrait},
    environments::Environment,
    notifier::{DesktopNotifier, Notifier},
};
use enwiro_daemon::ConfigurationValues;
use enwiro_sdk::client::{CachedEntry, CookbookTrait, RpcCookbookClient};
use enwiro_sdk::plugin::{PluginKind, get_plugins};
use enwiro_sdk::recipe_expr::RecipeExpr;
use std::{collections::HashMap, io::Write, os::unix::fs::symlink, path::Path, path::PathBuf};

/// Per-invocation knobs for cooking an environment.
#[derive(Debug, Clone, Default)]
pub struct CookConfig {
    /// Skip firing garnish `run_on: [Cook]` cli entries. Gear files are still written.
    pub no_hooks: bool,
}

struct ResolvedRecipe {
    cookbook: String,
    description: Option<String>,
    via_pattern: bool,
}

/// One cooked recipe: the cookbook that cooked it, the project path it
/// produced, and the description the cache resolved for it.
struct CookedRecipe<'a> {
    cookbook: &'a dyn CookbookTrait,
    path: String,
    description: Option<String>,
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

    pub fn cook_environment(
        &self,
        env_name: &str,
        recipe_name: &str,
        cfg: &CookConfig,
    ) -> anyhow::Result<Environment> {
        match enwiro_sdk::recipe_expr::parse(recipe_name)? {
            RecipeExpr::Name(name) => self.cook_plain_environment(env_name, &name, cfg),
            RecipeExpr::Composition(parts) => {
                self.cook_composed_environment(env_name, recipe_name, &parts)
            }
        }
    }

    fn cook_plain_environment(
        &self,
        env_name: &str,
        recipe_name: &str,
        cfg: &CookConfig,
    ) -> anyhow::Result<Environment> {
        self.notifier
            .notify_info(env_name, &format!("Preparing environment: {}", env_name));
        let cooked = self.resolve_and_cook(env_name, recipe_name)?;
        let flat_name = env_name.replace('/', "-");
        // main_folder must be written before create_environment_symlink resolves its
        // return value (via Environment::get_one) -- otherwise a re-cook could read a
        // stale main_folder left over from a previous cook of the same env.
        self.save_cook_metadata(
            &flat_name,
            cooked.cookbook.name(),
            recipe_name,
            cooked.description.as_deref(),
        );
        let env = self.create_environment_symlink(env_name, &cooked.path)?;
        self.write_gear_if_present(cooked.cookbook, recipe_name, &flat_name);
        self.write_external_paths_if_present(cooked.cookbook, recipe_name, &flat_name);
        self.write_garnish_gear(&cooked.path, &flat_name, cfg);
        mark_via_daemon(&flat_name, "active", enwiro_sdk::rpc::MarkSource::Auto);
        Ok(env)
    }

    /// Cook `a+b(+...)` (#375): cook every part through normal resolution,
    /// then build one env whose project directory is a real folder (the
    /// "wrapper") holding a symlink per part - entering the env shows the
    /// parts side by side. meta.json records the whole expression under the
    /// reserved cookbook name `composed`. Gear and garnish hooks are not
    /// collected: their commands assume the project directory is their own
    /// part, which the wrapper is not.
    fn cook_composed_environment(
        &self,
        env_name: &str,
        expression: &str,
        parts: &[String],
    ) -> anyhow::Result<Environment> {
        self.notifier.notify_info(
            env_name,
            &format!("Preparing composed environment: {}", env_name),
        );
        let flat_parts: Vec<String> = parts.iter().map(|p| p.replace('/', "-")).collect();
        let mut seen = std::collections::HashSet::new();
        for (part, flat_part) in parts.iter().zip(&flat_parts) {
            if !seen.insert(flat_part) {
                anyhow::bail!(
                    "Recipe '{}' appears more than once in '{}'",
                    part,
                    expression
                );
            }
        }

        // Cook every part before touching the env directory: a failing part
        // must not leave a half-composed environment behind. Already-cooked
        // parts are not wasted - cooking is idempotent, so a retry (or
        // cooking a part standalone) reuses them.
        let mut cooked_parts: Vec<CookedRecipe> = Vec::new();
        for part in parts {
            let cooked = self
                .resolve_and_cook(env_name, part)
                .with_context(|| format!("Could not cook part '{}' of '{}'", part, expression))?;
            cooked_parts.push(cooked);
        }

        let flat_name = env_name.replace('/', "-");
        self.save_cook_metadata(
            &flat_name,
            enwiro_sdk::recipe_expr::COMPOSED_COOKBOOK_NAME,
            expression,
            None,
        );
        let env = self.create_composed_wrapper(env_name, &flat_parts, &cooked_parts)?;
        self.write_composed_external_paths(&flat_name, parts, &cooked_parts);
        mark_via_daemon(&flat_name, "active", enwiro_sdk::rpc::MarkSource::Auto);
        Ok(env)
    }

    /// Resolve `recipe_name` in the daemon cache and cook it with the owning
    /// cookbook. Shared by plain and composed cooks.
    fn resolve_and_cook(
        &self,
        env_name: &str,
        recipe_name: &str,
    ) -> anyhow::Result<CookedRecipe<'_>> {
        let resolved = self.find_recipe_in_cache(recipe_name).ok_or_else(|| {
            tracing::error!(name = %recipe_name, "Recipe not in daemon cache");
            anyhow!(
                "No recipe '{}' in the daemon cache. \
                 Check: systemctl --user status enwiro-daemon.service",
                recipe_name
            )
        })?;
        let ResolvedRecipe {
            cookbook: cookbook_name,
            description,
            via_pattern,
        } = resolved;

        let cookbook = self
            .cookbooks
            .iter()
            .find(|c| c.name() == cookbook_name)
            .ok_or_else(|| {
                anyhow!(
                    "Cache lists recipe '{}' under cookbook '{}', which is not installed",
                    recipe_name,
                    cookbook_name
                )
            })?;

        // A pattern-routed cook does something the recipe list never showed
        // (e.g. creating a new branch) - surface the rendered description so
        // a typo'd name is noticed instead of silently becoming a branch.
        if via_pattern && let Some(rendered) = &description {
            self.notifier.notify_info(env_name, rendered);
        }

        tracing::debug!(env = %env_name, recipe = %recipe_name, cookbook = %cookbook_name, "Found recipe in cache");
        let path = cookbook.cook(recipe_name)?;
        Ok(CookedRecipe {
            cookbook: cookbook.as_ref(),
            path,
            description,
        })
    }

    pub fn find_recipe_in_cache_by_name(&self, recipe_name: &str) -> bool {
        self.find_recipe_in_cache(recipe_name).is_some()
    }

    /// Every parseable entry of the daemon's recipe cache, in its
    /// priority-sorted file order. Errors when the cache file is missing -
    /// callers that treat that as "no match" use `.ok()`.
    pub fn read_cached_entries(&self) -> anyhow::Result<Vec<CachedEntry>> {
        let cache = match &self.cache_dir {
            Some(dir) => enwiro_daemon::DaemonCache::with_runtime_dir(dir.clone()),
            None => enwiro_daemon::DaemonCache::open()?,
        };
        let cached = cache
            .read_recipes()?
            .context("No recipe cache found - is enwiro-daemon running?")?;
        Ok(cached
            .lines()
            .filter(|line| !line.is_empty())
            .filter_map(|line| serde_json::from_str::<CachedEntry>(line).ok())
            .collect())
    }

    /// Exact matches shadow pattern claims, so the exact pass scans the
    /// whole cache before any pattern is tried. The cache is priority-sorted,
    /// so the first pattern match wins - the same arbitration duplicate
    /// concrete names already get.
    fn find_recipe_in_cache(&self, recipe_name: &str) -> Option<ResolvedRecipe> {
        let entries = self.read_cached_entries().ok()?;

        for entry in &entries {
            if let CachedEntry::Concrete(concrete) = entry
                && concrete.name == recipe_name
            {
                return Some(ResolvedRecipe {
                    cookbook: concrete.cookbook.clone(),
                    description: concrete.description.clone(),
                    via_pattern: false,
                });
            }
        }
        for entry in &entries {
            if let CachedEntry::Pattern(pattern) = entry
                && let Some(matched) = enwiro_sdk::recipe_pattern::match_name(
                    &pattern.pattern,
                    pattern.description.as_deref(),
                    recipe_name,
                )
            {
                return Some(ResolvedRecipe {
                    cookbook: pattern.cookbook.clone(),
                    description: matched.description,
                    via_pattern: true,
                });
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
        crate::usage_stats::record_cook_metadata_per_env(
            &env_dir,
            cookbook,
            recipe,
            description,
            env_name,
        );
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
            if !enwiro_sdk::dropin::write_json_file(&path, &data) {
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
                enwiro_sdk::dropin::write_json_file(&gear_path, &json);
            }
            Ok(None) => {}
            Err(e) => {
                tracing::debug!(error = %e, "Cookbook gear() returned error, continuing");
            }
        }
    }

    fn write_external_paths_if_present(
        &self,
        cookbook: &dyn CookbookTrait,
        recipe: &str,
        flat_name: &str,
    ) {
        match cookbook.external_paths(recipe) {
            Ok(paths) if !paths.is_empty() => {
                let env_dir = Path::new(&self.config.workspaces_directory).join(flat_name);
                let file_path = enwiro_sdk::external_paths::external_paths_dir(&env_dir).join(
                    enwiro_sdk::external_paths::external_paths_filename(cookbook.name()),
                );
                let data = enwiro_sdk::external_paths::ExternalPathsFileData {
                    version: enwiro_sdk::external_paths::SCHEMA_VERSION,
                    paths,
                };
                enwiro_sdk::dropin::write_json_file(&file_path, &data);
            }
            Ok(_) => {}
            Err(e) => {
                tracing::debug!(error = %e, "Cookbook external_paths() returned error, continuing");
            }
        }
    }

    /// Build the composed env's project directory: `<env>/<flat_name>/` is
    /// a real directory holding one symlink per part, named after the
    /// part's flattened recipe name. `main_folder` (already written) points
    /// at it, so entering the env lands where every part is visible.
    fn create_composed_wrapper(
        &self,
        name: &str,
        flat_parts: &[String],
        cooked_parts: &[CookedRecipe],
    ) -> anyhow::Result<Environment> {
        let flat_name = name.replace('/', "-");
        let env_dir = Path::new(&self.config.workspaces_directory).join(&flat_name);
        let wrapper = env_dir.join(&flat_name);
        tracing::info!(name = %name, "Creating composed environment wrapper");
        if wrapper.is_symlink() {
            // The env was previously cooked from a plain recipe; its project
            // symlink gives way to the wrapper directory.
            std::fs::remove_file(&wrapper)?;
        }
        std::fs::create_dir_all(&wrapper)?;
        for (flat_part, cooked) in flat_parts.iter().zip(cooked_parts) {
            let link = wrapper.join(flat_part);
            if link.is_symlink() || link.exists() {
                std::fs::remove_file(&link)?;
            }
            symlink(Path::new(&cooked.path), &link)?;
        }
        self.notifier
            .notify_success(name, &format!("Created environment: {}", name));
        Environment::get_one(&self.config.workspaces_directory, &flat_name)
    }

    /// The union of every part's isolation needs, written as one file under
    /// the `composed` contributor name: each part's cooked project path (the
    /// wrapper's symlinks point outside the env) plus whatever the part's
    /// own cookbook declares (e.g. a worktree's base repo).
    fn write_composed_external_paths(
        &self,
        flat_name: &str,
        parts: &[String],
        cooked_parts: &[CookedRecipe],
    ) {
        let mut paths: Vec<String> = Vec::new();
        for (part, cooked) in parts.iter().zip(cooked_parts) {
            paths.push(cooked.path.clone());
            match cooked.cookbook.external_paths(part) {
                Ok(extra) => paths.extend(extra),
                Err(e) => {
                    tracing::debug!(error = %e, "Cookbook external_paths() returned error, continuing");
                }
            }
        }
        paths.sort();
        paths.dedup();
        let env_dir = Path::new(&self.config.workspaces_directory).join(flat_name);
        let file_path = enwiro_sdk::external_paths::external_paths_dir(&env_dir).join(
            enwiro_sdk::external_paths::external_paths_filename(
                enwiro_sdk::recipe_expr::COMPOSED_COOKBOOK_NAME,
            ),
        );
        let data = enwiro_sdk::external_paths::ExternalPathsFileData {
            version: enwiro_sdk::external_paths::SCHEMA_VERSION,
            paths,
        };
        enwiro_sdk::dropin::write_json_file(&file_path, &data);
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
            .notify_success(name, &format!("Created environment: {}", name));
        Environment::get_one(&self.config.workspaces_directory, &flat_name)
    }

    pub(crate) fn resolve_environment_name(&self, name: &Option<String>) -> anyhow::Result<String> {
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
                .cook_environment(&resolved, &resolved, cfg)
                .context("Could not cook environment"),
            Err(e) => Err(e),
        }
    }

    /// Like `get_or_cook_environment`, but the environment name and the recipe
    /// it is cooked from may differ (recipe aliasing, `enw activate foo=x`).
    /// If the environment already exists, it is returned as-is and the recipe
    /// is ignored.
    pub fn get_or_cook_environment_as(
        &self,
        env_name: &str,
        recipe_name: &str,
        cfg: &CookConfig,
    ) -> anyhow::Result<Environment> {
        let flat_name = env_name.replace('/', "-");
        match Environment::get_one(&self.config.workspaces_directory, &flat_name) {
            Ok(env) => Ok(env),
            Err(_) => self
                .cook_environment(env_name, recipe_name, cfg)
                .context("Could not cook environment"),
        }
    }

    pub fn get_all_environments(&self) -> anyhow::Result<HashMap<String, Environment>> {
        Environment::get_all(&self.config.workspaces_directory)
    }
}

pub(crate) fn mark_via_daemon(env_name: &str, status: &str, source: enwiro_sdk::rpc::MarkSource) {
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
                source,
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
            .cook_environment("my-project", "my-project", &CookConfig::default())
            .unwrap();
        assert_eq!(env.name, "my-project");

        // Verify directory with inner symlink was created
        let env_dir = temp_dir.path().join("my-project");
        assert!(env_dir.is_dir());
        let inner_link = env_dir.join("my-project");
        assert!(inner_link.is_symlink());
    }

    fn concrete_cache_line(cookbook: &str, name: &str) -> String {
        serde_json::to_string(&enwiro_sdk::client::CachedRecipe {
            cookbook: cookbook.to_string(),
            name: name.to_string(),
            description: None,
            sort_order: 0,
            equivalent_to: Vec::new(),
            scores: None,
        })
        .unwrap()
    }

    /// A pattern cache line as the daemon stores it: already anchored.
    fn pattern_cache_line(cookbook: &str, pattern: &str, description: Option<&str>) -> String {
        serde_json::to_string(&enwiro_sdk::client::CachedPatternRecipe {
            cookbook: cookbook.to_string(),
            pattern: enwiro_sdk::recipe_pattern::anchor(pattern),
            description: description.map(str::to_string),
            url: None,
        })
        .unwrap()
    }

    #[rstest]
    fn test_cook_environment_via_pattern_match_creates_env_and_notifies(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, notifications) = context_object;

        let cooked_dir = temp_dir.path().join("cooked-target");
        fs::create_dir(&cooked_dir).unwrap();

        context_object.write_cache_lines(&[
            concrete_cache_line("git", "my-project"),
            pattern_cache_line(
                "git",
                "my-project@(?P<branch>.+)",
                Some("Create new branch '{branch}' in my-project"),
            ),
        ]);
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["my-project"],
            vec![("my-project@new-idea", cooked_dir.to_str().unwrap())],
        ))];

        let env = context_object
            .cook_environment(
                "my-project@new-idea",
                "my-project@new-idea",
                &CookConfig::default(),
            )
            .unwrap();
        assert_eq!(env.name, "my-project@new-idea");

        // The rendered pattern description is surfaced as a notification so
        // an unintended branch creation is visible.
        assert!(
            notifications
                .borrow()
                .iter()
                .any(|n| n.contains("Create new branch 'new-idea' in my-project")),
            "notifications: {:?}",
            notifications.borrow()
        );

        // ... and persisted as the env description.
        let env_dir = temp_dir.path().join("my-project@new-idea");
        let meta = crate::usage_stats::load_env_meta(&env_dir);
        assert_eq!(
            meta.description.as_deref(),
            Some("Create new branch 'new-idea' in my-project")
        );
    }

    #[rstest]
    fn test_exact_cache_entry_shadows_pattern_match(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, context_object, _, _) = context_object;

        // The pattern line comes FIRST and claims everything; the exact
        // concrete entry must still win.
        context_object.write_cache_lines(&[
            pattern_cache_line("greedy", "(?P<anything>.+)", None),
            concrete_cache_line("git", "my-project"),
        ]);

        let resolved = context_object.find_recipe_in_cache("my-project").unwrap();
        assert_eq!(resolved.cookbook, "git");
        assert!(!resolved.via_pattern);
    }

    #[rstest]
    fn test_pattern_matches_resolve_in_cache_order(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, context_object, _, _) = context_object;

        // Both patterns match; the cache is priority-sorted, so the first
        // line wins.
        context_object.write_cache_lines(&[
            pattern_cache_line("first", "my-project@(?P<branch>.+)", None),
            pattern_cache_line("second", "(?P<anything>.+)", None),
        ]);

        let resolved = context_object
            .find_recipe_in_cache("my-project@feature")
            .unwrap();
        assert_eq!(resolved.cookbook, "first");
        assert!(resolved.via_pattern);
    }

    #[rstest]
    fn test_name_matching_no_entry_is_not_found(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, context_object, _, _) = context_object;

        context_object.write_cache_lines(&[
            concrete_cache_line("git", "my-project"),
            pattern_cache_line("git", "my-project@(?P<branch>.+)", None),
        ]);

        // Typo in the repo part: neither the exact entry nor the pattern
        // claims it.
        assert!(
            context_object
                .find_recipe_in_cache("my-porject@feature")
                .is_none()
        );
    }

    #[rstest]
    fn test_cook_composed_environment_builds_wrapper_with_part_symlinks(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;

        let foo_target = temp_dir.path().join("foo-target");
        let bar_target = temp_dir.path().join("bar-target");
        fs::create_dir(&foo_target).unwrap();
        fs::create_dir(&bar_target).unwrap();

        context_object.write_cache_entries(&[("git", "foo", None), ("git", "bar", None)]);
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["foo", "bar"],
            vec![
                ("foo", foo_target.to_str().unwrap()),
                ("bar", bar_target.to_str().unwrap()),
            ],
        ))];

        let env = context_object
            .cook_environment("foo+bar", "foo+bar", &CookConfig::default())
            .unwrap();

        // The project directory is the wrapper folder, showing both parts.
        let env_dir = temp_dir.path().join("foo+bar");
        let wrapper = env_dir.join("foo+bar");
        assert_eq!(env.path, wrapper.to_str().unwrap());
        assert!(wrapper.is_dir() && !wrapper.is_symlink());
        assert_eq!(wrapper.join("foo").read_link().unwrap(), foo_target);
        assert_eq!(wrapper.join("bar").read_link().unwrap(), bar_target);

        let meta = crate::usage_stats::load_env_meta(&env_dir);
        assert_eq!(meta.cookbook.as_deref(), Some("composed"));
        assert_eq!(meta.recipe.as_deref(), Some("foo+bar"));
        assert_eq!(meta.main_folder.as_deref(), Some("foo+bar"));
    }

    #[rstest]
    fn test_cook_composed_environment_unions_external_paths(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;

        let foo_target = temp_dir.path().join("foo-target");
        let bar_target = temp_dir.path().join("bar-target");
        fs::create_dir(&foo_target).unwrap();
        fs::create_dir(&bar_target).unwrap();

        context_object.write_cache_entries(&[("git", "foo", None), ("git", "bar", None)]);
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["foo", "bar"],
            vec![
                ("foo", foo_target.to_str().unwrap()),
                ("bar", bar_target.to_str().unwrap()),
            ],
        ))];

        context_object
            .cook_environment("foo+bar", "foo+bar", &CookConfig::default())
            .unwrap();

        // The wrapper's symlinks point outside the env, so the parts' cooked
        // paths must be declared as external for isolation to reach them.
        let file = temp_dir
            .path()
            .join("foo+bar")
            .join("external-paths.d")
            .join("cookbook-composed.json");
        let data: enwiro_sdk::external_paths::ExternalPathsFileData =
            serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert!(
            data.paths
                .contains(&foo_target.to_str().unwrap().to_string())
        );
        assert!(
            data.paths
                .contains(&bar_target.to_str().unwrap().to_string())
        );
    }

    #[rstest]
    fn test_cook_composed_environment_failing_part_leaves_no_env(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;

        let foo_target = temp_dir.path().join("foo-target");
        fs::create_dir(&foo_target).unwrap();

        // 'bar' is in the cache but its cook fails (no cook result for it).
        context_object.write_cache_entries(&[("git", "foo", None), ("git", "bar", None)]);
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["foo", "bar"],
            vec![("foo", foo_target.to_str().unwrap())],
        ))];

        let result = context_object.cook_environment("foo+bar", "foo+bar", &CookConfig::default());

        assert!(result.is_err());
        assert!(
            !temp_dir.path().join("foo+bar").exists(),
            "a failing part must not leave a half-composed env behind"
        );
    }

    #[rstest]
    fn test_cook_composed_environment_rejects_duplicate_parts(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;

        let foo_target = temp_dir.path().join("foo-target");
        fs::create_dir(&foo_target).unwrap();
        context_object.write_cache_entry("git", "foo");
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["foo"],
            vec![("foo", foo_target.to_str().unwrap())],
        ))];

        let err = context_object
            .cook_environment("foo+foo", "foo+foo", &CookConfig::default())
            .unwrap_err();

        assert!(err.to_string().contains("more than once"), "{err}");
        assert!(!temp_dir.path().join("foo+foo").exists());
    }

    #[rstest]
    fn test_cook_composed_environment_under_an_alias(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;

        let foo_target = temp_dir.path().join("foo-target");
        let bar_target = temp_dir.path().join("bar-target");
        fs::create_dir(&foo_target).unwrap();
        fs::create_dir(&bar_target).unwrap();

        context_object.write_cache_entries(&[("git", "foo", None), ("git", "bar", None)]);
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["foo", "bar"],
            vec![
                ("foo", foo_target.to_str().unwrap()),
                ("bar", bar_target.to_str().unwrap()),
            ],
        ))];

        let env = context_object
            .cook_environment("work", "foo+bar", &CookConfig::default())
            .unwrap();

        let wrapper = temp_dir.path().join("work").join("work");
        assert_eq!(env.path, wrapper.to_str().unwrap());
        assert!(wrapper.join("foo").is_symlink());
        assert!(wrapper.join("bar").is_symlink());
        let meta = crate::usage_stats::load_env_meta(&temp_dir.path().join("work"));
        assert_eq!(meta.recipe.as_deref(), Some("foo+bar"));
    }

    #[rstest]
    fn test_cook_environment_rejects_invalid_recipe_grammar(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, context_object, _, _) = context_object;

        let err = context_object
            .cook_environment("foo+", "foo+", &CookConfig::default())
            .unwrap_err();

        assert!(err.to_string().contains("expected a recipe name"), "{err}");
    }

    #[rstest]
    fn test_cook_composed_environment_flattens_slashes_in_part_links(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;

        let a_target = temp_dir.path().join("a-target");
        let b_target = temp_dir.path().join("b-target");
        fs::create_dir(&a_target).unwrap();
        fs::create_dir(&b_target).unwrap();

        context_object
            .write_cache_entries(&[("github", "owner/repo#4", None), ("git", "bar", None)]);
        context_object.cookbooks = vec![
            Box::new(FakeCookbook::new(
                "github",
                vec!["owner/repo#4"],
                vec![("owner/repo#4", a_target.to_str().unwrap())],
            )),
            Box::new(FakeCookbook::new(
                "git",
                vec!["bar"],
                vec![("bar", b_target.to_str().unwrap())],
            )),
        ];

        context_object
            .cook_environment(
                "owner/repo#4+bar",
                "owner/repo#4+bar",
                &CookConfig::default(),
            )
            .unwrap();

        let wrapper = temp_dir
            .path()
            .join("owner-repo#4+bar")
            .join("owner-repo#4+bar");
        assert!(wrapper.join("owner-repo#4").is_symlink());
        assert!(wrapper.join("bar").is_symlink());
    }

    #[rstest]
    fn test_cook_environment_writes_main_folder_to_meta(
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
            .cook_environment("my-project", "my-project", &CookConfig::default())
            .unwrap();

        let env_dir = temp_dir.path().join("my-project");
        let meta = crate::usage_stats::load_env_meta(&env_dir);
        assert_eq!(meta.main_folder.as_deref(), Some("my-project"));
    }

    #[rstest]
    fn test_recooking_does_not_resolve_a_stale_main_folder(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;

        let first_target = temp_dir.path().join("first-target");
        fs::create_dir(&first_target).unwrap();
        context_object.write_cache_entry("git", "my-project");
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["my-project"],
            vec![("my-project", first_target.to_str().unwrap())],
        ))];
        context_object
            .cook_environment("my-project", "my-project", &CookConfig::default())
            .unwrap();

        // Simulate a leftover main_folder from a hypothetical composed env
        // (#375) pointing at a sibling symlink that still exists on disk.
        let env_dir = temp_dir.path().join("my-project");
        let stale_target = temp_dir.path().join("stale-target");
        fs::create_dir(&stale_target).unwrap();
        std::os::unix::fs::symlink(&stale_target, env_dir.join("stale-folder")).unwrap();
        let mut meta = crate::usage_stats::load_env_meta(&env_dir);
        meta.main_folder = Some("stale-folder".to_string());
        enwiro_daemon::meta::save_env_meta(&env_dir, &meta).unwrap();

        let second_target = temp_dir.path().join("second-target");
        fs::create_dir(&second_target).unwrap();
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["my-project"],
            vec![("my-project", second_target.to_str().unwrap())],
        ))];

        let env = context_object
            .cook_environment("my-project", "my-project", &CookConfig::default())
            .unwrap();

        assert_eq!(env.path, env_dir.join("my-project").to_str().unwrap());
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
            .cook_environment(recipe_name, recipe_name, &CookConfig::default())
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

        let result =
            context_object.cook_environment("my-project", "my-project", &CookConfig::default());
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
                cookbook_name: enwiro_sdk::plugin::PluginName::new("github").unwrap(),
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
            .cook_environment("my-project", "my-project", &CookConfig::default())
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
            .cook_environment("owner/repo#42", "owner/repo#42", &CookConfig::default())
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

        let result =
            context_object.cook_environment("my-project", "my-project", &CookConfig::default());
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
            cookbook_name: enwiro_sdk::plugin::PluginName::new("github").unwrap(),
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
            .cook_environment("my-project", "my-project", &CookConfig::default())
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
            .cook_environment(recipe_name, recipe_name, &CookConfig::default())
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
    fn test_cook_environment_alias_flattens_env_name_only(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;

        let cooked_dir = temp_dir.path().join("cooked-target");
        fs::create_dir(&cooked_dir).unwrap();

        let recipe_name = "owner/repo#42";
        context_object.write_cache_entry("github", recipe_name);
        let gear_data = serde_json::json!({"tool": "nvim"});
        context_object.cookbooks = vec![Box::new(
            FakeCookbook::new(
                "github",
                vec![recipe_name],
                vec![(recipe_name, cooked_dir.to_str().unwrap())],
            )
            .with_gear(gear_data.clone()),
        )];

        let env = context_object
            .cook_environment("team/foo", recipe_name, &CookConfig::default())
            .unwrap();
        assert_eq!(env.name, "team-foo");

        // Env directory and inner symlink use the flattened env name.
        let env_dir = temp_dir.path().join("team-foo");
        assert!(env_dir.is_dir());
        assert!(env_dir.join("team-foo").is_symlink());

        // Meta records the unflattened recipe id, distinct from the env name.
        let meta = crate::usage_stats::load_env_meta(&env_dir);
        assert_eq!(meta.recipe.as_deref(), Some(recipe_name));
        assert_eq!(meta.main_folder.as_deref(), Some("team-foo"));

        // Gear was queried for the recipe but written under the env dir.
        let gear_path = env_dir.join("gear.d").join("cookbook-github.json");
        assert!(gear_path.exists());
        let written: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&gear_path).unwrap()).unwrap();
        assert_eq!(written, gear_data);
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
            .cook_environment("owner/repo#42", "owner/repo#42", &CookConfig::default())
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

        let result =
            context_object.cook_environment("my-project", "my-project", &CookConfig::default());
        assert!(result.is_ok());

        let logs = notifications.borrow();
        assert_eq!(logs.len(), 2);
        assert!(logs[0].starts_with("INFO:"));
        assert!(logs[0].contains("my-project"));
        assert!(logs[1].starts_with("SUCCESS:"));
        assert!(logs[1].contains("my-project"));
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

        let result =
            context_object.cook_environment("my-project", "my-project", &CookConfig::default());
        assert!(result.is_err());

        let logs = notifications.borrow();
        assert_eq!(logs.len(), 1);
        assert!(
            logs[0].starts_with("INFO:"),
            "only the info notification should fire on failure"
        );
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
            .cook_environment("my-project", "my-project", &CookConfig::default())
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
            .cook_environment("my-project", "my-project", &CookConfig::default())
            .unwrap();

        let gear_dir = temp_dir.path().join("my-project").join("gear.d");
        assert!(
            !gear_dir.exists(),
            "gear.d/ should NOT exist when cookbook returns no gear"
        );
    }
}
