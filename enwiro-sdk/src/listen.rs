use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::cookbook::Recipe;
use crate::status::Status;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum RecipeUpdate {
    Recipes {
        data: Vec<Recipe>,
    },
    /// A cookbook reporting the auto-detected status of one recipe (#302).
    /// `status` is the canonical [`Status`] schema; the daemon maps the
    /// recipe to its env and writes meta.json (guarded by
    /// [`crate::status::is_cookbook_settable`]).
    #[serde(rename = "status_changed")]
    StatusChanged {
        recipe: String,
        status: Status,
    },
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
    serve_updates(interval, move || {
        vec![RecipeUpdate::Recipes { data: build() }]
    })
}

/// Like [`serve`], but the `build` closure returns a full batch of
/// [`RecipeUpdate`]s (recipes AND/OR `status_changed` events) to emit each
/// tick. One JSON line per update; the whole batch is de-duplicated against
/// the previous tick so an unchanged world emits nothing. Cookbooks that
/// report status use this instead of [`serve`] (#302).
pub fn serve_updates<F>(interval: Duration, mut build: F) -> !
where
    F: FnMut() -> Vec<RecipeUpdate>,
{
    let mut last: Option<String> = None;
    loop {
        let lines: Vec<String> = build().iter().map(RecipeUpdate::to_jsonl).collect();
        let batch = lines.join("\n");
        if last.as_deref() != Some(batch.as_str()) {
            for line in &lines {
                println!("{}", line);
            }
            last = Some(batch);
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
