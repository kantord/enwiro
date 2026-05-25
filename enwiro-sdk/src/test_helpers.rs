//! Test fixtures for crates that implement or consume [`crate::client::CookbookTrait`].
//!
//! Gated by the `test-helpers` feature so they are only compiled when consumers
//! enable them in `[dev-dependencies]`.

use std::collections::HashMap;

use crate::client::CookbookTrait;
use crate::cookbook::Recipe;
use crate::plugin::PluginName;

pub struct FakeCookbook {
    pub cookbook_name: PluginName,
    pub recipes: Vec<Recipe>,
    pub cook_results: HashMap<String, String>,
    pub priority: u32,
    pub gear_json: Option<serde_json::Value>,
}

impl FakeCookbook {
    pub fn new(name: &str, recipes: Vec<&str>, cook_results: Vec<(&str, &str)>) -> Self {
        Self {
            cookbook_name: PluginName::new(name).unwrap(),
            recipes: recipes.into_iter().map(Recipe::new).collect(),
            cook_results: cook_results
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            priority: 50,
            gear_json: None,
        }
    }

    pub fn with_gear(mut self, gear: serde_json::Value) -> Self {
        self.gear_json = Some(gear);
        self
    }

    pub fn new_with_descriptions(
        name: &str,
        recipes: Vec<(&str, Option<&str>)>,
        cook_results: Vec<(&str, &str)>,
    ) -> Self {
        Self {
            cookbook_name: PluginName::new(name).unwrap(),
            recipes: recipes
                .into_iter()
                .map(|(n, d)| match d {
                    Some(desc) => Recipe::with_description(n, desc),
                    None => Recipe::new(n),
                })
                .collect(),
            cook_results: cook_results
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            priority: 50,
            gear_json: None,
        }
    }

    pub fn with_priority(mut self, priority: u32) -> Self {
        self.priority = priority;
        self
    }

    pub fn with_sort_orders(mut self, sort_orders: Vec<u32>) -> Self {
        for (recipe, order) in self.recipes.iter_mut().zip(sort_orders) {
            recipe.sort_order = order;
        }
        self
    }
}

impl CookbookTrait for FakeCookbook {
    fn list_recipes(&self) -> anyhow::Result<Vec<Recipe>> {
        Ok(self.recipes.clone())
    }

    fn cook(&self, recipe: &str) -> anyhow::Result<String> {
        self.cook_results
            .get(recipe)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Recipe not found: {}", recipe))
    }

    fn name(&self) -> &str {
        self.cookbook_name.as_str()
    }

    fn priority(&self) -> u32 {
        self.priority
    }

    fn gear(&self, _recipe: &str) -> anyhow::Result<Option<serde_json::Value>> {
        Ok(self.gear_json.clone())
    }
}

pub struct FailingCookbook {
    pub cookbook_name: PluginName,
}

impl CookbookTrait for FailingCookbook {
    fn list_recipes(&self) -> anyhow::Result<Vec<Recipe>> {
        anyhow::bail!("simulated failure")
    }

    fn cook(&self, _recipe: &str) -> anyhow::Result<String> {
        anyhow::bail!("simulated failure")
    }

    fn name(&self) -> &str {
        self.cookbook_name.as_str()
    }
}
