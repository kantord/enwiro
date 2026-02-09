use std::process::Command;

use crate::plugin::Plugin;

#[derive(Debug, PartialEq, Eq, Hash)]
pub struct CookbookClient {
    pub plugin: Plugin,
}

impl CookbookClient {
    pub fn new(plugin: Plugin) -> Self {
        Self { plugin }
    }

    pub fn list_recipes(&self) -> Vec<String> {
        let output = Command::new(&self.plugin.executable)
            .arg("list-recipes")
            .output()
            .expect("Adapter failed to determine active environment name");

        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.lines().map(|x| x.to_string()).collect()
    }

    pub fn cook(&self, recipe: &str) -> String {
        let output = Command::new(&self.plugin.executable)
            .arg("cook")
            .arg(recipe)
            .output()
            .expect("Failed to cook recipe");

        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }
}
