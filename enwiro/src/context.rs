use anyhow::{Context, bail};

use crate::{
    client::{CookbookClient, CookbookTrait},
    commands::adapter::{EnwiroAdapterExternal, EnwiroAdapterNone, EnwiroAdapterTrait},
    config::ConfigurationValues,
    daemon,
    environments::Environment,
    notifier::{DesktopNotifier, Notifier},
    plugin::{PluginKind, get_plugins},
};
use std::{collections::HashMap, io::Write, os::unix::fs::symlink, path::Path, path::PathBuf};

pub struct CommandContext<W: Write> {
    pub config: ConfigurationValues,
    pub writer: W,
    pub adapter: Box<dyn EnwiroAdapterTrait>,
    pub notifier: Box<dyn Notifier>,
    pub cookbooks: Vec<Box<dyn CookbookTrait>>,
    pub cache_dir: Option<PathBuf>,
    pub stats_path: Option<PathBuf>,
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
        let cookbooks: Vec<Box<dyn CookbookTrait>> = plugins
            .into_iter()
            .map(|p| Box::new(CookbookClient::new(p)) as Box<dyn CookbookTrait>)
            .collect();

        tracing::debug!(count = cookbooks.len(), "Cookbooks loaded");

        let notifier: Box<dyn Notifier> = Box::new(DesktopNotifier);

        Ok(Self {
            config,
            writer,
            adapter,
            notifier,
            cookbooks,
            cache_dir: None,
            stats_path: None,
        })
    }

    pub fn cook_environment(&self, name: &str) -> anyhow::Result<Environment> {
        // Check the cache to avoid calling list_recipes() on every cookbook
        // (which can be slow for API-based cookbooks like GitHub)
        match self.find_recipe_in_cache(name) {
            Some(Some(cookbook_name)) => {
                // Cache hit: cook directly via the right cookbook
                if let Some(cookbook) = self.cookbooks.iter().find(|c| c.name() == cookbook_name) {
                    tracing::debug!(name = %name, cookbook = %cookbook_name, "Found recipe in cache");
                    let env_path = cookbook.cook(name)?;
                    return self.create_environment_symlink(name, &env_path);
                }
                tracing::warn!(name = %name, cookbook = %cookbook_name, "Cache references cookbook not found in plugins");
            }
            Some(None) => {
                // Cache is fresh but recipe not in it — fall through to slow path
                // in case the cache is slightly stale (up to CACHE_MAX_AGE)
                tracing::debug!(name = %name, "Recipe not found in cache, falling back to slow path");
            }
            None => {
                // No cache available — fall through to slow path
            }
        }

        // Fallback: iterate cookbooks and call list_recipes()
        for cookbook in &self.cookbooks {
            let recipes = cookbook.list_recipes()?;
            for recipe in recipes.into_iter() {
                if recipe.name != name {
                    continue;
                }
                let env_path = cookbook.cook(&recipe.name)?;
                return self.create_environment_symlink(name, &env_path);
            }
        }

        tracing::error!(name = %name, "No recipe available to cook environment");
        bail!("No recipe available to cook this environment.")
    }

    /// Look up a recipe in the daemon cache.
    /// Returns:
    /// - `Some(Some(cookbook_name))` — cache is fresh and recipe was found
    /// - `Some(None)` — cache is fresh but recipe is NOT in it
    /// - `None` — no cache available (missing, stale, or error)
    fn find_recipe_in_cache(&self, recipe_name: &str) -> Option<Option<String>> {
        let runtime_dir = match &self.cache_dir {
            Some(dir) => dir.clone(),
            None => daemon::runtime_dir().ok()?,
        };
        let cached = daemon::read_cached_recipes(&runtime_dir).ok()??;
        // Cache format: "cookbook: recipe\tdescription\n" (see daemon::collect_all_recipes)
        for line in cached.lines() {
            if let Some((cookbook_name, rest)) = line.split_once(": ") {
                let name = rest.split_once('\t').map_or(rest, |(n, _)| n);
                if name == recipe_name {
                    return Some(Some(cookbook_name.to_string()));
                }
            }
        }
        Some(None) // cache exists but recipe not found
    }

    fn create_environment_symlink(
        &self,
        name: &str,
        env_path: &str,
    ) -> anyhow::Result<Environment> {
        let flat_name = name.replace('/', "-");
        let target_path = Path::new(&self.config.workspaces_directory).join(&flat_name);
        tracing::info!(name = %name, target = %env_path, "Creating environment symlink");
        symlink(Path::new(env_path), target_path)?;
        self.notifier
            .notify_success(&format!("Created environment: {}", name));
        Environment::get_one(&self.config.workspaces_directory, &flat_name)
    }

    fn resolve_environment_name(&self, name: &Option<String>) -> anyhow::Result<String> {
        match name {
            Some(n) => Ok(n.clone()),
            None => self
                .adapter
                .get_active_environment_name()
                .context("Could not determine active environment"),
        }
    }

    pub fn get_or_cook_environment(&self, name: &Option<String>) -> anyhow::Result<Environment> {
        let resolved = self.resolve_environment_name(name)?;
        let flat_name = resolved.replace('/', "-");
        match Environment::get_one(&self.config.workspaces_directory, &flat_name) {
            Ok(env) => Ok(env),
            Err(_) if name.is_some() => self
                .cook_environment(&resolved)
                .context("Could not cook environment"),
            Err(e) => Err(e),
        }
    }

    pub fn get_all_environments(&self) -> anyhow::Result<HashMap<String, Environment>> {
        Environment::get_all(&self.config.workspaces_directory)
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use std::fs;

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

        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["my-project"],
            vec![("my-project", cooked_dir.to_str().unwrap())],
        ))];

        let env = context_object.cook_environment("my-project").unwrap();
        assert_eq!(env.name, "my-project");

        // Verify symlink was created
        let link_path = temp_dir.path().join("my-project");
        assert!(link_path.is_symlink());
    }

    #[rstest]
    fn test_cook_environment_with_slash_in_name(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;

        let cooked_dir = temp_dir.path().join("cooked-target");
        fs::create_dir(&cooked_dir).unwrap();

        let recipe_name = "my-project@feature/my-thing";
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec![recipe_name],
            vec![(recipe_name, cooked_dir.to_str().unwrap())],
        ))];

        let env = context_object.cook_environment(recipe_name).unwrap();
        assert_eq!(env.name, "my-project@feature-my-thing");

        // Verify symlink was created (not nested due to the slash)
        assert!(
            temp_dir.path().read_dir().unwrap().any(|entry| {
                let entry = entry.unwrap();
                entry.path().is_symlink()
            }),
            "A symlink should exist in the workspaces directory"
        );
    }

    #[rstest]
    fn test_get_or_cook_finds_existing_environment_with_slash_in_name(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;

        let cooked_dir = temp_dir.path().join("cooked-target");
        fs::create_dir(&cooked_dir).unwrap();

        let recipe_name = "my-project@feature/my-thing";
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec![recipe_name],
            vec![(recipe_name, cooked_dir.to_str().unwrap())],
        ))];

        // First call creates the environment
        let env1 = context_object
            .get_or_cook_environment(&Some(recipe_name.to_string()))
            .unwrap();

        // Second call should find the existing environment, not try to cook again
        let env2 = context_object
            .get_or_cook_environment(&Some(recipe_name.to_string()))
            .unwrap();

        assert_eq!(env1.name, env2.name);
    }

    #[rstest]
    fn test_cook_environment_finds_recipe_in_second_cookbook(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;

        let cooked_dir = temp_dir.path().join("cooked-target");
        fs::create_dir(&cooked_dir).unwrap();

        context_object.cookbooks = vec![
            Box::new(FakeCookbook::new("npm", vec!["unrelated-project"], vec![])),
            Box::new(FakeCookbook::new(
                "git",
                vec!["my-project"],
                vec![("my-project", cooked_dir.to_str().unwrap())],
            )),
        ];

        let env = context_object.cook_environment("my-project").unwrap();
        assert_eq!(env.name, "my-project");
    }

    #[rstest]
    fn test_cook_environment_errors_when_no_recipe_matches(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["other-project"],
            vec![],
        ))];

        let result = context_object.cook_environment("my-project");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("No recipe available")
        );
    }

    #[rstest]
    fn test_cook_environment_errors_when_no_cookbooks(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, context_object, _, _) = context_object;

        let result = context_object.cook_environment("anything");
        assert!(result.is_err());
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
        fs::write(cache_dir.join("recipes.cache"), "git: my-project\n").unwrap();

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
        let env = context_object.cook_environment("my-project").unwrap();
        assert_eq!(env.name, "my-project");
    }

    #[rstest]
    fn test_cook_environment_falls_through_when_recipe_not_in_cache(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;

        let cooked_dir = temp_dir.path().join("cooked-target");
        fs::create_dir(&cooked_dir).unwrap();

        // Write a fresh cache that does NOT contain the requested recipe
        let cache_dir = context_object.cache_dir.as_ref().unwrap();
        fs::create_dir_all(cache_dir).unwrap();
        fs::write(cache_dir.join("recipes.cache"), "git: other-project\n").unwrap();

        // The recipe isn't in the cache, but the cookbook has it.
        // cook_environment should fall through to the slow path and find it.
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["new-branch"],
            vec![("new-branch", cooked_dir.to_str().unwrap())],
        ))];

        let env = context_object.cook_environment("new-branch").unwrap();
        assert_eq!(env.name, "new-branch");
    }

    #[rstest]
    fn test_cook_environment_cache_hit_with_description(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;

        let cooked_dir = temp_dir.path().join("cooked-target");
        fs::create_dir(&cooked_dir).unwrap();

        // Cache entry has a tab-separated description
        let cache_dir = context_object.cache_dir.as_ref().unwrap();
        fs::create_dir_all(cache_dir).unwrap();
        fs::write(
            cache_dir.join("recipes.cache"),
            "github: owner/repo#42\tFix auth bug\n",
        )
        .unwrap();

        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "github",
            vec!["owner/repo#42"],
            vec![("owner/repo#42", cooked_dir.to_str().unwrap())],
        ))];

        let env = context_object.cook_environment("owner/repo#42").unwrap();
        assert_eq!(env.name, "owner-repo#42");
    }

    #[rstest]
    fn test_cook_environment_falls_through_when_cached_cookbook_missing(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;

        let cooked_dir = temp_dir.path().join("cooked-target");
        fs::create_dir(&cooked_dir).unwrap();

        // Cache says recipe belongs to "npm" cookbook, but only "git" is installed
        let cache_dir = context_object.cache_dir.as_ref().unwrap();
        fs::create_dir_all(cache_dir).unwrap();
        fs::write(cache_dir.join("recipes.cache"), "npm: my-project\n").unwrap();

        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["my-project"],
            vec![("my-project", cooked_dir.to_str().unwrap())],
        ))];

        // Should fall through to slow path and find it via git cookbook
        let env = context_object.cook_environment("my-project").unwrap();
        assert_eq!(env.name, "my-project");
    }

    #[rstest]
    fn test_get_or_cook_returns_existing_environment(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context_object, _, _) = context_object;
        context_object.create_mock_environment("my-env");

        let env = context_object
            .get_or_cook_environment(&Some("my-env".to_string()))
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

        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["new-project"],
            vec![("new-project", cooked_dir.to_str().unwrap())],
        ))];

        // "new-project" doesn't exist as an environment, so it should be cooked
        let env = context_object
            .get_or_cook_environment(&Some("new-project".to_string()))
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
        let result = context_object.get_or_cook_environment(&None);
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
    fn test_cook_environment_sends_notification(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, notifications) = context_object;

        let cooked_dir = temp_dir.path().join("cooked-target");
        fs::create_dir(&cooked_dir).unwrap();

        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["my-project"],
            vec![("my-project", cooked_dir.to_str().unwrap())],
        ))];

        let result = context_object.cook_environment("my-project");
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

        let result = context_object.cook_environment("my-project");
        assert!(result.is_err());

        let logs = notifications.borrow();
        assert_eq!(logs.len(), 0);
    }
}
