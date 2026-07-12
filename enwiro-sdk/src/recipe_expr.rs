//! The recipe expression grammar: the single source of truth for parsing
//! the recipe names users type and metadata stores (#375).
//!
//! Grammar (v1):
//!
//! ```text
//! expression := name ('+' name)*
//! name       := allowed-char+
//! ```
//!
//! A single name cooks one recipe from one cookbook; `a+b` composes the
//! parts into one environment whose project directory holds a symlink per
//! part. Names are restricted to the alphabet below. Every other character
//! is reserved for future grammar (`foo(bar)` wrapper calls, `,` argument
//! lists, `=` named arguments - `=` is also `enw activate`'s alias
//! separator), so no cookbook can claim a character that later becomes an
//! operator. The same alphabet is enforced daemon-side on every cookbook
//! recipe name at cache-build time and on pattern-derived names at match
//! time.
//!
//! Semantic conventions inside names (not grammar, just blessed meaning):
//! `#` separates a container from an item in it (`enwiro#42`,
//! `obsidian#some-note`), `@` pins a ref or variant (`repo@branch`), `/`
//! expresses hierarchy (`owner/repo`).

use ariadne::{Config, Label, Report, ReportKind, Source};
use chumsky::prelude::*;

/// Non-alphanumeric characters allowed in recipe names.
pub const ALLOWED_SPECIAL_CHARS: [char; 6] = ['@', '#', '/', '.', '_', '-'];

/// The reserved cookbook name recorded in a composed environment's
/// meta.json. No real plugin may take it (`plugin::PluginName` rejects it).
pub const COMPOSED_COOKBOOK_NAME: &str = "composed";

/// Characters that are grammar today or explicitly reserved for planned
/// grammar (#715 wrapper calls, #723 parametric recipes), diagnosed with a
/// dedicated message instead of the generic "not allowed".
const RESERVED_GRAMMAR_CHARS: [char; 5] = ['+', '(', ')', ',', '='];

/// Whether `c` may appear in a recipe name. Alphanumeric is Unicode-aware:
/// letters and digits of any script are names, never operators, so banning
/// them would break real-world names (an obsidian note with an accent) for
/// no grammar benefit.
pub fn is_allowed_name_char(c: char) -> bool {
    c.is_alphanumeric() || ALLOWED_SPECIAL_CHARS.contains(&c)
}

/// Whether `name` is a valid recipe name (one grammar atom): non-empty and
/// made of allowed characters only.
pub fn is_valid_recipe_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(is_allowed_name_char)
}

/// A parsed recipe expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecipeExpr {
    /// A plain recipe name, cooked by a single cookbook.
    Name(String),
    /// `a+b(+...)`: a composed environment (#375). Parts keep the user's
    /// order and are each a plain recipe name; always at least two.
    Composition(Vec<String>),
}

/// A recipe expression parse failure, carrying a pre-rendered [`ariadne`]
/// diagnostic that points at the offending character.
#[derive(Debug)]
pub struct ParseError {
    rendered: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.rendered)
    }
}

impl std::error::Error for ParseError {}

/// Parse a recipe expression. A single valid name parses to
/// [`RecipeExpr::Name`]; `a+b` to [`RecipeExpr::Composition`].
pub fn parse(input: &str) -> Result<RecipeExpr, ParseError> {
    match parser().parse(input).into_result() {
        Ok(mut parts) => {
            if parts.len() == 1 {
                Ok(RecipeExpr::Name(parts.pop().expect("checked len")))
            } else {
                Ok(RecipeExpr::Composition(parts))
            }
        }
        Err(errors) => Err(render_error(input, &errors)),
    }
}

fn parser<'src>() -> impl Parser<'src, &'src str, Vec<String>, extra::Err<Rich<'src, char>>> {
    let name = any()
        .filter(|c: &char| is_allowed_name_char(*c))
        .repeated()
        .at_least(1)
        .collect::<String>();
    name.separated_by(just('+'))
        .at_least(1)
        .collect::<Vec<String>>()
        .then_ignore(end())
}

/// Render the first parse error as an ariadne diagnostic. Only the first:
/// with this grammar every later error is a consequence of the same
/// offending character, and one precise caret beats a pile.
fn render_error(input: &str, errors: &[Rich<'_, char>]) -> ParseError {
    let error = errors.first().expect("into_result returned at least one");
    let span = error.span().start..error.span().end.max(error.span().start + 1);
    let (message, label) = match error.found() {
        Some(c) if RESERVED_GRAMMAR_CHARS.contains(c) => (
            format!("'{c}' is reserved recipe grammar"),
            "a recipe name cannot continue here".to_string(),
        ),
        Some(c) => (
            format!("'{c}' is not allowed in recipe names"),
            format!(
                "allowed: letters, digits, and {}",
                ALLOWED_SPECIAL_CHARS.map(|c| format!("'{c}'")).join(" ")
            ),
        ),
        None => (
            "expected a recipe name".to_string(),
            "a recipe name must follow here".to_string(),
        ),
    };

    let mut buffer = Vec::new();
    let report = Report::build(ReportKind::Error, ("recipe", span.clone()))
        .with_config(Config::default().with_color(false))
        .with_message(&message)
        .with_label(Label::new(("recipe", span)).with_message(label))
        .finish();
    let rendered = match report.write(("recipe", Source::from(input)), &mut buffer) {
        Ok(()) => String::from_utf8_lossy(&buffer).into_owned(),
        Err(_) => message,
    };
    ParseError { rendered }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_name_parses_to_name() {
        assert_eq!(
            parse("my-project").unwrap(),
            RecipeExpr::Name("my-project".to_string())
        );
    }

    #[test]
    fn conventional_separators_are_plain_name_chars() {
        for name in ["repo@feat/login", "enwiro#42", "obsidian#c.notes_v2"] {
            assert_eq!(parse(name).unwrap(), RecipeExpr::Name(name.to_string()));
        }
    }

    #[test]
    fn unicode_letters_are_name_chars() {
        assert_eq!(
            parse("obsidian#café-notes").unwrap(),
            RecipeExpr::Name("obsidian#café-notes".to_string())
        );
    }

    #[test]
    fn plus_composes_two_parts() {
        assert_eq!(
            parse("foo+bar").unwrap(),
            RecipeExpr::Composition(vec!["foo".to_string(), "bar".to_string()])
        );
    }

    #[test]
    fn composition_is_n_ary_and_preserves_order() {
        assert_eq!(
            parse("c+a+b").unwrap(),
            RecipeExpr::Composition(vec!["c".to_string(), "a".to_string(), "b".to_string()])
        );
    }

    #[test]
    fn composition_parts_keep_conventional_chars() {
        assert_eq!(
            parse("enwiro#34+enwiro#999").unwrap(),
            RecipeExpr::Composition(vec!["enwiro#34".to_string(), "enwiro#999".to_string()])
        );
    }

    #[test]
    fn empty_input_is_an_error() {
        assert!(parse("").is_err());
    }

    #[test]
    fn trailing_plus_is_an_error() {
        let err = parse("foo+").unwrap_err().to_string();
        assert!(err.contains("expected a recipe name"), "{err}");
    }

    #[test]
    fn leading_plus_is_an_error() {
        assert!(parse("+foo").is_err());
    }

    #[test]
    fn empty_part_is_an_error() {
        assert!(parse("foo++bar").is_err());
    }

    #[test]
    fn reserved_grammar_chars_get_a_dedicated_diagnostic() {
        for input in ["foo(bar)", "foo,bar", "foo=bar"] {
            let err = parse(input).unwrap_err().to_string();
            assert!(err.contains("reserved recipe grammar"), "{input}: {err}");
        }
    }

    #[test]
    fn disallowed_chars_name_the_allowed_alphabet() {
        let err = parse("foo bar").unwrap_err().to_string();
        assert!(err.contains("not allowed in recipe names"), "{err}");
        assert!(err.contains("allowed:"), "{err}");
    }

    #[test]
    fn diagnostic_points_at_the_input() {
        let err = parse("foo?bar").unwrap_err().to_string();
        assert!(
            err.contains("foo?bar"),
            "diagnostic must quote input: {err}"
        );
    }

    #[test]
    fn is_valid_recipe_name_accepts_cookbook_conventions() {
        for name in ["my-project", "repo@feat/x", "owner/repo#42", "a.b_c"] {
            assert!(is_valid_recipe_name(name), "{name}");
        }
    }

    #[test]
    fn is_valid_recipe_name_rejects_grammar_and_junk() {
        for name in ["", "a+b", "a b", "a(b)", "a,b", "a=b", "a\tb", "a\0b"] {
            assert!(!is_valid_recipe_name(name), "{name:?}");
        }
    }
}
