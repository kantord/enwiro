// TODO: drop `#![allow(dead_code)]` once `rebalance::compile` lands in
// step (5) — it produces `I3Op` values consumed by `render`.
#![allow(dead_code)]

use super::types::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum I3Op {
    Rename { from: Handle, to: Handle },
    Focus { ws: Handle },
}

pub fn render(op: &I3Op) -> String {
    match op {
        I3Op::Rename { from, to } => format!(
            r#"rename workspace "{}" to "{}""#,
            escape(&from.0),
            escape(&to.0),
        ),
        I3Op::Focus { ws } => format!(r#"workspace "{}""#, escape(&ws.0)),
    }
}

fn escape(s: &str) -> String {
    s.replace('\\', r"\\").replace('"', r#"\""#)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_rename_produces_i3_msg_syntax() {
        let op = I3Op::Rename {
            from: Handle("5: old-project".into()),
            to: Handle("10: old-project".into()),
        };
        assert_eq!(
            render(&op),
            r#"rename workspace "5: old-project" to "10: old-project""#
        );
    }

    #[test]
    fn render_rename_escapes_quotes() {
        let op = I3Op::Rename {
            from: Handle(r#"5: has"quote"#.into()),
            to: Handle("10: safe".into()),
        };
        assert_eq!(
            render(&op),
            r#"rename workspace "5: has\"quote" to "10: safe""#
        );
    }

    #[test]
    fn render_focus_wraps_name_in_quotes() {
        let op = I3Op::Focus {
            ws: Handle("3: enwiro".into()),
        };
        assert_eq!(render(&op), r#"workspace "3: enwiro""#);
    }

    #[test]
    fn render_focus_with_semicolon_is_quoted() {
        let op = I3Op::Focus {
            ws: Handle("3: weird; name".into()),
        };
        assert_eq!(render(&op), r#"workspace "3: weird; name""#);
    }

    #[test]
    fn render_focus_with_quote_is_safe() {
        let op = I3Op::Focus {
            ws: Handle(r#"3: has"quote"#.into()),
        };
        assert_eq!(render(&op), r#"workspace "3: has\"quote""#);
    }

    #[test]
    fn render_focus_backslash_quote_does_not_inject() {
        let op = I3Op::Focus {
            ws: Handle(r#"3: \"injected"#.into()),
        };
        assert_eq!(render(&op), r#"workspace "3: \\\"injected""#);
    }
}
