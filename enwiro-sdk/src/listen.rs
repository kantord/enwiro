use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::cookbook::Recipe;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum RecipeUpdate {
    Recipes { data: Vec<Recipe> },
}

impl RecipeUpdate {
    pub fn to_jsonl(&self) -> String {
        serde_json::to_string(self).expect("RecipeUpdate is always serializable")
    }
}

/// Long-running listen loop for cookbook binaries. Calls `build` to
/// collect current recipes, emits `RecipeUpdate::Recipes` to stdout
/// (skipping unchanged emissions), then sleeps `interval` before
/// repeating. Cookbooks invoke this from their `listen` subcommand.
///
/// Never returns under normal operation; the daemon terminates the
/// cookbook subprocess via the optative-process-pool's SIGTERM-then-
/// SIGKILL teardown.
pub fn serve<F>(interval: Duration, mut build: F) -> !
where
    F: FnMut() -> Vec<Recipe>,
{
    let mut last: Option<String> = None;
    loop {
        let update = RecipeUpdate::Recipes { data: build() };
        let line = update.to_jsonl();
        if last.as_deref() != Some(line.as_str()) {
            println!("{}", line);
            last = Some(line);
        }
        std::thread::sleep(interval);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recipes_roundtrip_through_json() {
        let update = RecipeUpdate::Recipes {
            data: vec![Recipe::new("foo"), Recipe::with_description("bar", "desc")],
        };
        let line = update.to_jsonl();
        let parsed: RecipeUpdate = serde_json::from_str(&line).unwrap();
        assert_eq!(update, parsed);
    }

    #[test]
    fn recipes_serializes_with_type_tag() {
        let update = RecipeUpdate::Recipes { data: vec![] };
        let line = update.to_jsonl();
        assert!(line.contains(r#""type":"recipes""#));
    }
}
