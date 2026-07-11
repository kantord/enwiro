//! URL rules: mappings from web page URLs to recipe names, declared by
//! cookbooks on their pattern recipes (see [`crate::cookbook::PatternRecipe`]).
//!
//! A rule pairs a URLPattern constructor string (the web standard used by
//! browsers, so the same pattern compiles identically in the extension's JS
//! and here) with a `{group}` recipe-name template rendered from the URL
//! pattern's named capture groups - the same template syntax as pattern
//! recipe descriptions (see [`crate::recipe_pattern`]).
//!
//! A client-side router (the enwiro browser extension) matches page URLs
//! against these rules and derives the recipe name to activate. Validation
//! and derivation both live here, in trusted core: cookbooks never compile
//! URL patterns or render templates themselves.

use std::collections::HashMap;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use urlpattern::{UrlPattern, UrlPatternInit, UrlPatternMatchInput};

/// A URL-to-recipe mapping on a pattern recipe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UrlRule {
    /// URLPattern constructor string, e.g.
    /// `https://github.com/:owner/repo/:kind(pull|issues)/:number([0-9]+)`.
    /// Components not present in the string match as wildcards, so query
    /// strings and fragments never prevent a match.
    pub pattern: String,
    /// `{group}` template over the URL pattern's named capture groups,
    /// rendered to a recipe name when a URL matches. The rendered name must
    /// itself match the owning pattern recipe's name claim; consumers check
    /// that before acting on it.
    pub recipe: String,
}

fn parse_pattern(pattern: &str) -> anyhow::Result<UrlPattern> {
    let init = UrlPatternInit::parse_constructor_string::<regex::Regex>(pattern, None)
        .with_context(|| format!("invalid URL pattern '{}'", pattern))?;
    <UrlPattern>::parse(init, Default::default())
        .with_context(|| format!("invalid URL pattern '{}'", pattern))
}

fn group_names(pattern: &UrlPattern) -> impl Iterator<Item = &str> {
    [
        &pattern.protocol,
        &pattern.username,
        &pattern.password,
        &pattern.hostname,
        &pattern.port,
        &pattern.pathname,
        &pattern.search,
        &pattern.hash,
    ]
    .into_iter()
    .flat_map(|component| component.group_name_list.iter().map(String::as_str))
}

/// Validate a cookbook-emitted URL rule: the URL pattern must parse, and
/// every `{key}` in the recipe template must name one of its capture groups.
/// Run at daemon cache-build time so invalid rules are dropped before any
/// consumer sees them.
pub fn validate(rule: &UrlRule) -> anyhow::Result<()> {
    let pattern = parse_pattern(&rule.pattern)?;
    let template = leon::Template::parse(&rule.recipe)
        .with_context(|| format!("invalid recipe template '{}'", rule.recipe))?;
    let groups: std::collections::HashSet<&str> = group_names(&pattern).collect();
    for key in template.keys() {
        anyhow::ensure!(
            groups.contains(*key),
            "recipe template references '{{{}}}', which is not a capture group of '{}'",
            key,
            rule.pattern,
        );
    }
    Ok(())
}

/// Derive the recipe name for `url`, or `None` when the URL does not match
/// the rule. An unparseable rule also counts as a non-match: rules were
/// validated at cache-build time and can only be broken by hand.
pub fn derive_recipe_name(rule: &UrlRule, url: &str) -> Option<String> {
    let pattern = parse_pattern(&rule.pattern).ok()?;
    let url = url::Url::parse(url).ok()?;
    let result = pattern.exec(UrlPatternMatchInput::Url(url)).ok()??;
    let values: HashMap<String, String> = [
        result.protocol,
        result.username,
        result.password,
        result.hostname,
        result.port,
        result.pathname,
        result.search,
        result.hash,
    ]
    .into_iter()
    .flat_map(|component| component.groups)
    .filter_map(|(key, value)| value.map(|value| (key, value)))
    .collect();
    let template = leon::Template::parse(&rule.recipe).ok()?;
    template.render(&values).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn github_rule() -> UrlRule {
        UrlRule {
            pattern: "https://github.com/:owner/enwiro/:kind(pull|issues)/:number([0-9]+)"
                .to_string(),
            recipe: "enwiro#{number}".to_string(),
        }
    }

    #[test]
    fn validate_accepts_wellformed_rule() {
        validate(&github_rule()).unwrap();
    }

    #[test]
    fn validate_rejects_unparseable_pattern() {
        let rule = UrlRule {
            pattern: "https://github.com/:kind(pull".to_string(),
            recipe: "x".to_string(),
        };
        assert!(validate(&rule).is_err());
    }

    #[test]
    fn validate_rejects_template_key_without_group() {
        let rule = UrlRule {
            pattern: "https://github.com/:owner/enwiro/pull/:number([0-9]+)".to_string(),
            recipe: "enwiro#{typo}".to_string(),
        };
        let error = validate(&rule).unwrap_err().to_string();
        assert!(error.contains("{typo}"), "unexpected error: {error}");
    }

    #[test]
    fn validate_rejects_invalid_template() {
        let rule = UrlRule {
            pattern: "https://github.com/:owner".to_string(),
            recipe: "{unclosed".to_string(),
        };
        assert!(validate(&rule).is_err());
    }

    #[test]
    fn derives_name_from_pr_url() {
        let name = derive_recipe_name(&github_rule(), "https://github.com/kantord/enwiro/pull/42");
        assert_eq!(name.as_deref(), Some("enwiro#42"));
    }

    #[test]
    fn derives_name_from_issue_url() {
        let name = derive_recipe_name(
            &github_rule(),
            "https://github.com/kantord/enwiro/issues/615",
        );
        assert_eq!(name.as_deref(), Some("enwiro#615"));
    }

    #[test]
    fn query_string_and_fragment_do_not_prevent_match() {
        let name = derive_recipe_name(
            &github_rule(),
            "https://github.com/kantord/enwiro/pull/42?diff=split#discussion_r1",
        );
        assert_eq!(name.as_deref(), Some("enwiro#42"));
    }

    #[test]
    fn unrelated_url_does_not_match() {
        assert_eq!(
            derive_recipe_name(&github_rule(), "https://github.com/kantord/enwiro/wiki"),
            None
        );
        assert_eq!(
            derive_recipe_name(&github_rule(), "https://example.com/kantord/enwiro/pull/42"),
            None
        );
        assert_eq!(derive_recipe_name(&github_rule(), "not a url"), None);
    }

    #[test]
    fn non_numeric_number_segment_does_not_match() {
        assert_eq!(
            derive_recipe_name(&github_rule(), "https://github.com/kantord/enwiro/pull/abc"),
            None
        );
    }
}
