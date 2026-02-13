use anyhow::{Context, bail};
use std::process::Command;

use crate::plugin::Plugin;

pub trait CookbookTrait {
    fn list_recipes(&self) -> anyhow::Result<Vec<String>>;
    fn cook(&self, recipe: &str) -> anyhow::Result<String>;
    fn name(&self) -> &str;
}

pub struct CookbookClient {
    plugin: Plugin,
}

impl CookbookClient {
    pub fn new(plugin: Plugin) -> Self {
        Self { plugin }
    }
}

impl CookbookTrait for CookbookClient {
    fn list_recipes(&self) -> anyhow::Result<Vec<String>> {
        tracing::debug!(cookbook = %self.plugin.name, "Listing recipes from cookbook");
        let output = Command::new(&self.plugin.executable)
            .arg("list-recipes")
            .output()
            .context("Cookbook failed to list recipes")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(cookbook = %self.plugin.name, %stderr, "Cookbook failed to list recipes");
            bail!(
                "Cookbook '{}' failed to list recipes: {}",
                self.plugin.name,
                stderr
            );
        }

        let stdout =
            String::from_utf8(output.stdout).context("Cookbook produced invalid UTF-8 output")?;
        Ok(stdout.lines().map(|x| x.to_string()).collect())
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
            tracing::warn!(cookbook = %self.plugin.name, recipe = %recipe, %stderr, "Cookbook failed to cook recipe");
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
}
