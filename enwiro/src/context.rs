use anyhow::{Context, bail};

use crate::{
    client::{CookbookClient, CookbookTrait},
    commands::adapter::{EnwiroAdapterExternal, EnwiroAdapterNone, EnwiroAdapterTrait},
    config::ConfigurationValues,
    environments::Environment,
    plugin::{PluginKind, get_plugins},
};
use std::{collections::HashMap, io::Write, os::unix::fs::symlink, path::Path};

pub struct CommandContext<W: Write> {
    pub config: ConfigurationValues,
    pub writer: W,
    pub adapter: Box<dyn EnwiroAdapterTrait>,
    pub cookbooks: Vec<Box<dyn CookbookTrait>>,
}

impl<W: Write> CommandContext<W> {
    pub fn new(config: ConfigurationValues, writer: W) -> Self {
        let adapter: Box<dyn EnwiroAdapterTrait> = match &config.adapter {
            None => Box::new(EnwiroAdapterNone {}),
            Some(adapter_name) => Box::new(EnwiroAdapterExternal::new(adapter_name)),
        };

        let plugins = get_plugins(PluginKind::Cookbook);
        let cookbooks: Vec<Box<dyn CookbookTrait>> = plugins
            .into_iter()
            .map(|p| Box::new(CookbookClient::new(p)) as Box<dyn CookbookTrait>)
            .collect();

        Self {
            config,
            writer,
            adapter,
            cookbooks,
        }
    }

    fn get_environment(&self, name: &Option<String>) -> anyhow::Result<Environment> {
        let selected_environment_name = match name {
            Some(x) => x.clone(),
            None => self.adapter.get_active_environment_name()?,
        };

        Environment::get_one(
            &self.config.workspaces_directory,
            &selected_environment_name,
        )
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
                return Environment::get_one(&self.config.workspaces_directory, name);
            }
        }

        bail!("No recipe available to cook this environment.")
    }

    pub fn get_or_cook_environment(&self, name: &Option<String>) -> anyhow::Result<Environment> {
        match self.get_environment(name) {
            Ok(env) => Ok(env),
            Err(_) => {
                let recipe_name = match name {
                    Some(n) => n,
                    None => bail!("No environment could be found or cooked."),
                };

                let environment = self
                    .cook_environment(recipe_name)
                    .context("Could not cook environment")?;
                Ok(environment)
            }
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

    use crate::test_utils::test_utilities::{FakeContext, FakeCookbook, context_object};

    #[rstest]
    fn test_cook_environment_creates_symlink_for_matching_recipe(
        context_object: (tempfile::TempDir, FakeContext),
    ) {
        let (temp_dir, mut context_object) = context_object;

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
    fn test_cook_environment_errors_when_no_recipe_matches(
        context_object: (tempfile::TempDir, FakeContext),
    ) {
        let (_temp_dir, mut context_object) = context_object;
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
        context_object: (tempfile::TempDir, FakeContext),
    ) {
        let (_temp_dir, context_object) = context_object;

        let result = context_object.cook_environment("anything");
        assert!(result.is_err());
    }

    #[rstest]
    fn test_get_or_cook_returns_existing_environment(
        context_object: (tempfile::TempDir, FakeContext),
    ) {
        let (_temp_dir, mut context_object) = context_object;
        context_object.create_mock_environment("my-env");

        let env = context_object
            .get_or_cook_environment(&Some("my-env".to_string()))
            .unwrap();
        assert_eq!(env.name, "my-env");
    }

    #[rstest]
    fn test_get_or_cook_falls_back_to_cooking(context_object: (tempfile::TempDir, FakeContext)) {
        let (temp_dir, mut context_object) = context_object;

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
    fn test_get_or_cook_errors_when_no_name_and_adapter_fails(
        context_object: (tempfile::TempDir, FakeContext),
    ) {
        let (_temp_dir, mut context_object) = context_object;
        // Replace adapter with one that returns a non-existent env
        context_object.adapter = Box::new(
            crate::test_utils::test_utilities::EnwiroAdapterMock::new("nonexistent"),
        );

        let result = context_object.get_or_cook_environment(&None);
        assert!(result.is_err());
    }
}
