//! Wire-protocol types emitted by cookbook plugins and consumed by the host.
//!
//! Cookbook binaries communicate with the enwiro host over stdout JSON. This
//! module owns the canonical shapes so both sides (host + plugin) share one
//! definition instead of hand-rolling JSON strings or redefining the structs.

use anyhow::Context;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CookbookMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_priority: Option<u32>,
}

impl CookbookMetadata {
    pub fn from_json(s: &str) -> anyhow::Result<Self> {
        serde_json::from_str(s).context("Failed to parse cookbook metadata")
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("CookbookMetadata is always serializable")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recipe {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub sort_order: u32,
}

impl Recipe {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
            sort_order: 0,
        }
    }

    pub fn with_description(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: Some(description.into()),
            sort_order: 0,
        }
    }

    pub fn to_jsonl(&self) -> String {
        serde_json::to_string(self).expect("Recipe is always serializable")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_from_json_valid() {
        let m = CookbookMetadata::from_json(r#"{"defaultPriority":10}"#).unwrap();
        assert_eq!(m.default_priority, Some(10));
    }

    #[test]
    fn metadata_from_json_empty() {
        let m = CookbookMetadata::from_json("{}").unwrap();
        assert_eq!(m.default_priority, None);
    }

    #[test]
    fn metadata_from_json_unknown_fields_ignored() {
        let m = CookbookMetadata::from_json(r#"{"defaultPriority":20,"future":"x"}"#).unwrap();
        assert_eq!(m.default_priority, Some(20));
    }

    #[test]
    fn metadata_from_json_invalid() {
        assert!(CookbookMetadata::from_json("not json").is_err());
    }

    #[test]
    fn metadata_to_json_omits_none() {
        assert_eq!(CookbookMetadata::default().to_json(), "{}");
    }

    #[test]
    fn metadata_to_json_uses_camel_case() {
        let m = CookbookMetadata {
            default_priority: Some(20),
        };
        assert_eq!(m.to_json(), r#"{"defaultPriority":20}"#);
    }

    #[test]
    fn recipe_to_jsonl_minimal_skips_none_description() {
        assert_eq!(
            Recipe::new("foo").to_jsonl(),
            r#"{"name":"foo","sort_order":0}"#
        );
    }

    #[test]
    fn recipe_to_jsonl_includes_description_when_set() {
        let r = Recipe::with_description("foo", "bar");
        assert_eq!(
            r.to_jsonl(),
            r#"{"name":"foo","description":"bar","sort_order":0}"#
        );
    }
}
