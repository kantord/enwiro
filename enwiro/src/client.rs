use anyhow::Context;
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
        let output = Command::new(&self.plugin.executable)
            .arg("list-recipes")
            .output()
            .context("Cookbook failed to list recipes")?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.lines().map(|x| x.to_string()).collect())
    }

    fn cook(&self, recipe: &str) -> anyhow::Result<String> {
        let output = Command::new(&self.plugin.executable)
            .arg("cook")
            .arg(recipe)
            .output()
            .context("Failed to cook recipe")?;

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn name(&self) -> &str {
        &self.plugin.name
    }
}
