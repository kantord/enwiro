use anyhow::{Context, bail};

use crate::{
    client::{CookbookClient, CookbookTrait},
    commands::adapter::{EnwiroAdapterExternal, EnwiroAdapterNone, EnwiroAdapterTrait},
    config::ConfigurationValues,
    environments::Environment,
    notifier::{DesktopNotifier, Notifier},
    plugin::{PluginKind, get_plugins},
};
use std::{collections::HashMap, io::Write, os::unix::fs::symlink, path::Path};

pub struct CommandContext<W: Write> {
    pub config: ConfigurationValues,
    pub writer: W,
    pub adapter: Box<dyn EnwiroAdapterTrait>,
    pub notifier: Box<dyn Notifier>,
    pub cookbooks: Vec<Box<dyn CookbookTrait>>,
}

impl<W: Write> CommandContext<W> {
    pub fn new(config: ConfigurationValues, writer: W) -> anyhow::Result<Self> {
        let adapter: Box<dyn EnwiroAdapterTrait> = match &config.adapter {
            None => Box::new(EnwiroAdapterNone {}),
            Some(adapter_name) => Box::new(EnwiroAdapterExternal::new(adapter_name)?),
        };

        let plugins = get_plugins(PluginKind::Cookbook);
        let cookbooks: Vec<Box<dyn CookbookTrait>> = plugins
            .into_iter()
            .map(|p| Box::new(CookbookClient::new(p)) as Box<dyn CookbookTrait>)
            .collect();

        let notifier: Box<dyn Notifier> = Box::new(DesktopNotifier);

        Ok(Self {
            config,
            writer,
            adapter,
            notifier,
            cookbooks,
        })
    }

    pub fn cook_environment(&self, name: &str) -> anyhow::Result<Environment> {
        for cookbook in &self.cookbooks {
            let recipes = cookbook.list_recipes()?;
            for recipe in recipes.into_iter() {
                if recipe != name {
                    continue;
                }
                let env_path = cookbook.cook(&recipe)?;
                let target_path = Path::new(&self.config.workspaces_directory).join(name);
                symlink(Path::new(&env_path), target_path)?;
                self.notifier
                    .notify_success(&format!("Created environment: {}", name));
                return Environment::get_one(&self.config.workspaces_directory, name);
            }
        }

        bail!("No recipe available to cook this environment.")
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

    fn get_or_cook_environment_by_name(&self, name: &str) -> anyhow::Result<Environment> {
        match Environment::get_one(&self.config.workspaces_directory, name) {
            Ok(env) => Ok(env),
            Err(_) => self
                .cook_environment(name)
                .context("Could not cook environment"),
        }
    }

    pub fn get_or_cook_environment(&self, name: &Option<String>) -> anyhow::Result<Environment> {
        let resolved = self.resolve_environment_name(name)?;
        self.get_or_cook_environment_by_name(&resolved)
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
        AdapterLog, FakeContext, FakeCookbook, NotificationLog, context_object,
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
    fn test_get_or_cook_cooks_via_adapter_name_when_no_explicit_name(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut context_object, _, _) = context_object;

        let cooked_dir = temp_dir.path().join("cooked-target");
        fs::create_dir(&cooked_dir).unwrap();

        // Adapter returns "foobaz" (the default mock value)
        // Cookbook has a recipe for "foobaz"
        // No explicit name passed (None) — should resolve via adapter then cook
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["foobaz"],
            vec![("foobaz", cooked_dir.to_str().unwrap())],
        ))];

        let env = context_object.get_or_cook_environment(&None).unwrap();
        assert_eq!(env.name, "foobaz");
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
