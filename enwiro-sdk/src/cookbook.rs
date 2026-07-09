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
    /// Field names the cookbook accepts from project-level `.enwiro.toml`
    /// files. Trusted core silently drops any project-layer keys not on
    /// this list. Missing or empty ⇒ no project overrides accepted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub project_overridable: Vec<String>,
}

impl CookbookMetadata {
    pub fn from_json(s: &str) -> anyhow::Result<Self> {
        serde_json::from_str(s).context("Failed to parse cookbook metadata")
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("CookbookMetadata is always serializable")
    }
}

/// Wire format version for [`CookbookPayload`]. Bumped when the shape
/// changes in a backward-incompatible way.
pub const COOKBOOK_PAYLOAD_VERSION: u32 = 1;

/// Stdin payload for cookbook subcommands (`list-recipes`, `cook`, `gear`).
///
/// Trusted core (the `enw` CLI + daemon) resolves the cookbook's typed
/// config from the user-level TOML plus ancestor `.enwiro.toml` project
/// files, filters the project layer through the cookbook's
/// `project_overridable` allowlist, and serializes the result into
/// `config`. Cookbooks deserialize the payload from stdin and never parse
/// TOML themselves.
///
/// `config` is intentionally an opaque `serde_json::Value` so the wire
/// format doesn't bind to any cookbook's schema. Cookbooks call
/// `serde_json::from_value(payload.config)` to recover their typed
/// `#[derive(Deserialize, Default)]` struct.
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct CookbookPayload {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub config: serde_json::Value,
}

impl CookbookPayload {
    pub fn new(config: serde_json::Value) -> Self {
        Self {
            version: COOKBOOK_PAYLOAD_VERSION,
            config,
        }
    }

    /// Read the payload from stdin. Empty stdin yields a payload whose
    /// `config` is an empty JSON object — this lets cookbooks with
    /// `#[serde(default)]` structs deserialize to defaults rather than
    /// erroring with `invalid type: null` when invoked directly for
    /// debugging without the SDK piping a real payload.
    pub fn read_from_stdin() -> anyhow::Result<Self> {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("Could not read cookbook payload from stdin")?;
        if buf.trim().is_empty() {
            return Ok(Self {
                version: COOKBOOK_PAYLOAD_VERSION,
                config: serde_json::Value::Object(Default::default()),
            });
        }
        serde_json::from_str(&buf).context("Could not parse cookbook payload as JSON")
    }

    /// Read one newline-delimited payload line from stdin. Used by
    /// long-running subcommands (`listen`) whose stdin is kept open by
    /// the daemon for subsequent events. Empty input is treated like
    /// [`Self::read_from_stdin`].
    pub fn read_first_line_from_stdin() -> anyhow::Result<Self> {
        use std::io::BufRead;
        let stdin = std::io::stdin();
        let mut handle = stdin.lock();
        let mut buf = String::new();
        handle
            .read_line(&mut buf)
            .context("Could not read cookbook payload line from stdin")?;
        if buf.trim().is_empty() {
            return Ok(Self {
                version: COOKBOOK_PAYLOAD_VERSION,
                config: serde_json::Value::Object(Default::default()),
            });
        }
        serde_json::from_str(buf.trim()).context("Could not parse cookbook payload as JSON")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recipe {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub sort_order: u32,
    /// Other recipe/environment names this recipe is equivalent to: cooking any
    /// one of them produces an environment that makes the others redundant.
    /// Names live in the single flat environment namespace, so a bare name is
    /// enough (no cookbook prefix). Used by `enw ls` to hide a recipe once an
    /// equivalent one has been cooked — e.g. the github cookbook's `repo#42`
    /// declares the git cookbook's `repo@pr-42`. Optional; omitted on the wire
    /// when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub equivalent_to: Vec<String>,
}

impl Recipe {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
            sort_order: 0,
            equivalent_to: Vec::new(),
        }
    }

    pub fn with_description(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: Some(description.into()),
            sort_order: 0,
            equivalent_to: Vec::new(),
        }
    }

    pub fn to_jsonl(&self) -> String {
        serde_json::to_string(self).expect("Recipe is always serializable")
    }
}

/// A pattern recipe: a regex claim over recipe names the cookbook can cook
/// on demand even though they are not listed concretely - e.g. the git
/// cookbook claiming `repo@<any-branch>` so a not-yet-existing branch can be
/// cooked (#246). See [`crate::recipe_pattern`] for the pattern/template contract:
/// Rust `regex` syntax, emitted unanchored, `{group}` description template
/// rendered from the pattern's named capture groups.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatternRecipe {
    pub pattern: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// One item in a cookbook's recipe listing: a concrete recipe or a pattern
/// claim. Untagged on the wire - pattern entries carry `pattern` instead of
/// `name`, so consumers that only know concrete recipes skip pattern lines
/// as unparseable instead of misreading them. `Concrete` must stay first:
/// untagged tries variants in order, and `name` winning over `pattern`
/// means a stray extra `pattern` field on a concrete recipe cannot flip it
/// into a regex claim (pattern entries still parse - they have no `name`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RecipeItem {
    Concrete(Recipe),
    Pattern(PatternRecipe),
}

impl From<Recipe> for RecipeItem {
    fn from(recipe: Recipe) -> Self {
        RecipeItem::Concrete(recipe)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recipe_item_with_name_and_stray_pattern_field_stays_concrete() {
        let item: RecipeItem =
            serde_json::from_str(r#"{"name":"docs","pattern":"docs-.*","sort_order":3}"#).unwrap();
        let RecipeItem::Concrete(recipe) = item else {
            panic!("a named entry must not deserialize as a pattern claim");
        };
        assert_eq!(recipe.name, "docs");
    }

    #[test]
    fn recipe_item_without_name_parses_as_pattern() {
        let item: RecipeItem =
            serde_json::from_str(r#"{"pattern":"repo@(?P<branch>.+)"}"#).unwrap();
        assert!(matches!(item, RecipeItem::Pattern(_)));
    }

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
            project_overridable: vec![],
        };
        assert_eq!(m.to_json(), r#"{"defaultPriority":20}"#);
    }

    #[test]
    fn metadata_to_json_includes_project_overridable_when_nonempty() {
        let m = CookbookMetadata {
            default_priority: None,
            project_overridable: vec!["repo_globs".to_string()],
        };
        assert_eq!(m.to_json(), r#"{"projectOverridable":["repo_globs"]}"#);
    }

    #[test]
    fn metadata_from_json_parses_project_overridable() {
        let m = CookbookMetadata::from_json(r#"{"projectOverridable":["repo_globs"]}"#).unwrap();
        assert_eq!(m.project_overridable, vec!["repo_globs".to_string()]);
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
