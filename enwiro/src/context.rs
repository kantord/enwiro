use crate::{
    client::CookbookClient,
    commands::adapter::{EnwiroAdapterExternal, EnwiroAdapterNone, EnwiroAdapterTrait},
    config::ConfigurationValues,
    environments::Environment,
    plugin::{get_plugins, PluginKind},
};
use std::{
    collections::{HashMap, HashSet},
    io::Write,
    os::unix::fs::symlink,
    path::Path,
};

pub struct CommandContext<W: Write> {
    pub config: ConfigurationValues,
    pub writer: W,
    pub adapter: Box<dyn EnwiroAdapterTrait>,
}

impl<W: Write> CommandContext<W> {
    pub fn new(config: ConfigurationValues, writer: W) -> Self {
        let adapter: Box<dyn EnwiroAdapterTrait> = match &config.adapter {
            None => Box::new(EnwiroAdapterNone {}),
            Some(adapter_name) => Box::new(EnwiroAdapterExternal::new(adapter_name)),
        };

        Self {
            config,
            writer,
            adapter,
        }
    }

    fn get_environment(&self, name: &Option<String>) -> Result<Environment, std::io::Error> {
        let selected_environment_name = match name {
            Some(x) => x.clone(),
            None => self.adapter.get_active_environment_name().unwrap(),
        };

        Environment::get_one(
            &self.config.workspaces_directory,
            &selected_environment_name,
        )
    }

    pub fn cook_environment(&self, name: &str) -> Result<Environment, std::io::Error> {
        for cookbook in self.get_cookbooks() {
            let recipes = cookbook.list_recipes();
            for recipe in recipes.into_iter() {
                if recipe != name {
                    continue;
                }
                let env_path = cookbook.cook(&recipe);
                let target_path = Path::new(&self.config.workspaces_directory).join(name);
                symlink(Path::new(&env_path), target_path)?;
                return Environment::get_one(&self.config.workspaces_directory, name);
            }
        }

        Err(std::io::Error::other(
            "No recipe available to cook this environment.",
        ))
    }

    pub fn get_or_cook_environment(
        &self,
        name: &Option<String>,
    ) -> Result<Environment, std::io::Error> {
        match self.get_environment(name) {
            Ok(env) => Ok(env),
            Err(_) => {
                if name.is_none() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "No environment could be found or cooked.",
                    ));
                }
                let recipe_name = name.clone().unwrap();

                let environment = self
                    .cook_environment(&recipe_name)
                    .expect("Could not cook environment");
                Ok(environment)
            }
        }
    }

    pub fn get_all_environments(&self) -> Result<HashMap<String, Environment>, std::io::Error> {
        Environment::get_all(&self.config.workspaces_directory)
    }

    pub fn get_cookbooks(&self) -> HashSet<CookbookClient> {
        let plugins = get_plugins(PluginKind::Cookbook);
        let clients = plugins.into_iter().map(CookbookClient::new);

        HashSet::from_iter(clients)
    }
}
