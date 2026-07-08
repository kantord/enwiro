//! Pattern recipes: regex claims cookbooks make over recipe names they can
//! cook on demand without listing them concretely (e.g. a git repo claiming
//! `repo@<any-new-branch>`).
//!
//! Patterns use Rust `regex` syntax — no backreferences or lookaround — and
//! are emitted unanchored by cookbooks; the daemon anchors them at
//! cache-build time so a pattern always matches the whole recipe name.
//! The accompanying description is a `{group}` template rendered with the
//! pattern's named capture groups when a name matches. Validation and
//! rendering both live here, in trusted core: cookbooks never compile
//! patterns or render templates themselves.

use std::collections::{HashMap, HashSet};

use anyhow::Context;

/// Re-exported so cookbooks can embed literal strings (repo names) in their
/// patterns without carrying their own regex dependency.
pub use regex::escape;

/// Maximum length of a recipe description, in chars, in the daemon cache and
/// in any rendered pattern description. Longer text is cut and suffixed
/// with `…`.
pub const MAX_DESCRIPTION_CHARS: usize = 200;

/// Cap `text` at [`MAX_DESCRIPTION_CHARS`] chars, appending `…` when cut.
pub fn truncate_description(text: &str) -> String {
    if text.chars().count() <= MAX_DESCRIPTION_CHARS {
        return text.to_string();
    }
    let mut truncated: String = text.chars().take(MAX_DESCRIPTION_CHARS - 1).collect();
    truncated.push('…');
    truncated
}

/// Wrap a cookbook-emitted pattern so it must match the whole recipe name.
pub fn anchor(pattern: &str) -> String {
    format!("^(?:{})$", pattern)
}

/// Validate a cookbook-emitted pattern entry: the anchored pattern must
/// compile, and every `{key}` in the description template must name one of
/// the pattern's capture groups. Run at daemon cache-build time so invalid
/// entries are dropped before any consumer sees them.
pub fn validate(pattern: &str, description: Option<&str>) -> anyhow::Result<()> {
    let compiled = regex::Regex::new(&anchor(pattern))
        .with_context(|| format!("invalid recipe pattern '{}'", pattern))?;
    let Some(template) = description else {
        return Ok(());
    };
    let parsed = leon::Template::parse(template)
        .with_context(|| format!("invalid description template '{}'", template))?;
    let groups: HashSet<&str> = compiled.capture_names().flatten().collect();
    for key in parsed.keys() {
        anyhow::ensure!(
            groups.contains(*key),
            "description template references '{{{}}}', which is not a capture group of '{}'",
            key,
            pattern,
        );
    }
    Ok(())
}

/// Match `name` against an already-anchored cached pattern. Outer `None`
/// means no match (an uncompilable pattern also counts as a non-match —
/// cache entries were validated at build time, so that only happens with a
/// hand-edited cache). On match, the inner value is the rendered, truncated
/// description, or `None` when the entry has no template.
pub fn match_rendering(
    anchored_pattern: &str,
    template: Option<&str>,
    name: &str,
) -> Option<Option<String>> {
    let compiled = regex::Regex::new(anchored_pattern).ok()?;
    let captures = compiled.captures(name)?;
    let Some(template) = template else {
        return Some(None);
    };
    let Ok(parsed) = leon::Template::parse(template) else {
        return Some(None);
    };
    let values: HashMap<String, String> = compiled
        .capture_names()
        .flatten()
        .filter_map(|group| {
            captures
                .name(group)
                .map(|m| (group.to_string(), m.as_str().to_string()))
        })
        .collect();
    let rendered = parsed.render(&values).ok();
    Some(rendered.map(|text| truncate_description(&text)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_keeps_short_text() {
        assert_eq!(truncate_description("hello"), "hello");
    }

    #[test]
    fn truncate_caps_at_max_chars_with_ellipsis() {
        let long: String = "x".repeat(MAX_DESCRIPTION_CHARS + 50);
        let truncated = truncate_description(&long);
        assert_eq!(truncated.chars().count(), MAX_DESCRIPTION_CHARS);
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let long: String = "é".repeat(MAX_DESCRIPTION_CHARS + 1);
        let truncated = truncate_description(&long);
        assert_eq!(truncated.chars().count(), MAX_DESCRIPTION_CHARS);
    }

    #[test]
    fn validate_accepts_matching_template_keys() {
        validate(
            "my-project@(?P<branch>.+)",
            Some("Create new branch '{branch}' in my-project"),
        )
        .unwrap();
    }

    #[test]
    fn validate_rejects_unknown_template_key() {
        let err = validate("my-project@(?P<branch>.+)", Some("branch {typo}")).unwrap_err();
        assert!(err.to_string().contains("{typo}"), "{err}");
    }

    #[test]
    fn validate_rejects_invalid_regex() {
        assert!(validate("broken(", None).is_err());
    }

    #[test]
    fn validate_accepts_pattern_without_description() {
        validate("anything@(.+)", None).unwrap();
    }

    #[test]
    fn match_rendering_renders_captures() {
        let anchored = anchor("my-project@(?P<branch>.+)");
        let rendered = match_rendering(
            &anchored,
            Some("Create new branch '{branch}' in my-project"),
            "my-project@feat/login",
        );
        assert_eq!(
            rendered,
            Some(Some(
                "Create new branch 'feat/login' in my-project".to_string()
            ))
        );
    }

    #[test]
    fn match_rendering_is_anchored() {
        let anchored = anchor("my-project@(?P<branch>.+)");
        assert_eq!(match_rendering(&anchored, None, "other-my-project@x"), None);
    }

    #[test]
    fn match_rendering_without_template_signals_plain_match() {
        let anchored = anchor("my-project@(.+)");
        assert_eq!(match_rendering(&anchored, None, "my-project@x"), Some(None));
    }

    #[test]
    fn match_rendering_truncates_long_result() {
        let anchored = anchor("p@(?P<branch>.+)");
        let long_branch: String = "b".repeat(MAX_DESCRIPTION_CHARS * 2);
        let rendered = match_rendering(
            &anchored,
            Some("New branch {branch}"),
            &format!("p@{}", long_branch),
        )
        .unwrap()
        .unwrap();
        assert_eq!(rendered.chars().count(), MAX_DESCRIPTION_CHARS);
    }

    #[test]
    fn escaped_literals_do_not_act_as_regex() {
        let anchored = anchor(&format!("{}@(.+)", escape("a.b")));
        assert!(match_rendering(&anchored, None, "axb@branch").is_none());
        assert!(match_rendering(&anchored, None, "a.b@branch").is_some());
    }
}
