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
