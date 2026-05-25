use super::i3_op::I3Op;
use super::types::*;
use std::collections::HashMap;

#[derive(Clone, Debug, Default)]
pub struct I3Model {
    pub ws: HashMap<Handle, bool>,
    pub focused: Option<Handle>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum I3Error {
    OldNotFound { from: Handle },
    ReservedPrefix { to: Handle },
    DuplicateNum { num: i32 },
    DuplicateName { name: Handle },
}

impl I3Model {
    pub fn insert(&mut self, handle: Handle, has_content: bool) {
        self.ws.insert(handle, has_content);
    }

    pub fn focus(&mut self, handle: Handle) {
        self.focused = Some(handle);
    }

    fn find_by_name_ci(&self, name: &str) -> Option<Handle> {
        let lower = name.to_lowercase();
        self.ws
            .keys()
            .find(|h| h.0.to_lowercase() == lower)
            .cloned()
    }

    pub fn apply(&mut self, op: &I3Op) -> Result<(), I3Error> {
        match op {
            I3Op::Rename { from, to } => {
                if to.0.starts_with("__") {
                    return Err(I3Error::ReservedPrefix { to: to.clone() });
                }
                if let Some(existing) = self.find_by_name_ci(&to.0) {
                    if existing != *from {
                        return Err(I3Error::DuplicateName { name: to.clone() });
                    }
                }
                let has = self
                    .ws
                    .remove(from)
                    .ok_or_else(|| I3Error::OldNotFound { from: from.clone() })?;
                self.ws.insert(to.clone(), has);
                if self.focused.as_ref() == Some(from) {
                    self.focused = Some(to.clone());
                }
                self.check_nums()
            }
            I3Op::Focus { ws } => {
                if ws.0.starts_with("__") {
                    return Err(I3Error::ReservedPrefix { to: ws.clone() });
                }
                if self.focused.as_ref() == Some(ws) {
                    return Ok(());
                }
                if let Some(existing) = self.find_by_name_ci(&ws.0) {
                    if let Some(prev) = self.focused.take()
                        && self.ws.get(&prev) == Some(&false)
                    {
                        self.ws.remove(&prev);
                    }
                    self.focused = Some(existing);
                    // No workspace created or renamed, so nums can't change.
                    return Ok(());
                }
                if let Some(prev) = self.focused.take()
                    && self.ws.get(&prev) == Some(&false)
                {
                    self.ws.remove(&prev);
                }
                self.ws.entry(ws.clone()).or_insert(false);
                self.focused = Some(ws.clone());
                self.check_nums()
            }
        }
    }

    fn check_nums(&self) -> Result<(), I3Error> {
        let mut seen: HashMap<i32, ()> = HashMap::new();
        for handle in self.ws.keys() {
            if let Some(n) = num_of(handle)
                && seen.insert(n, ()).is_some()
            {
                return Err(I3Error::DuplicateNum { num: n });
            }
        }
        Ok(())
    }
}

fn num_of(h: &Handle) -> Option<i32> {
    let s = h.0.split_once(':').map(|(s, _)| s).unwrap_or(&h.0);
    parse_ws_num(s)
}

fn parse_ws_num(s: &str) -> Option<i32> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let val: i64 = s.parse().ok()?;
    if val < 0 || val > i32::MAX as i64 {
        return None;
    }
    Some(val as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_succeeds_when_old_exists() {
        let mut m = I3Model::default();
        m.insert(Handle("5: a".into()), true);
        let op = I3Op::Rename {
            from: Handle("5: a".into()),
            to: Handle("6: a".into()),
        };
        assert_eq!(m.apply(&op), Ok(()));
        assert!(m.ws.contains_key(&Handle("6: a".into())));
        assert!(!m.ws.contains_key(&Handle("5: a".into())));
    }

    #[test]
    fn rename_fails_when_old_missing() {
        let mut m = I3Model::default();
        let op = I3Op::Rename {
            from: Handle("5: a".into()),
            to: Handle("6: a".into()),
        };
        assert_eq!(
            m.apply(&op),
            Err(I3Error::OldNotFound {
                from: Handle("5: a".into())
            })
        );
    }

    #[test]
    fn rename_to_underscore_prefix_fails() {
        let mut m = I3Model::default();
        m.insert(Handle("5: a".into()), true);
        let op = I3Op::Rename {
            from: Handle("5: a".into()),
            to: Handle("__internal".into()),
        };
        assert_eq!(
            m.apply(&op),
            Err(I3Error::ReservedPrefix {
                to: Handle("__internal".into())
            })
        );
    }

    #[test]
    fn duplicate_num_after_rename_fails() {
        let mut m = I3Model::default();
        m.insert(Handle("5: a".into()), true);
        m.insert(Handle("6: b".into()), true);
        let op = I3Op::Rename {
            from: Handle("5: a".into()),
            to: Handle("6: a".into()),
        };
        assert_eq!(m.apply(&op), Err(I3Error::DuplicateNum { num: 6 }));
    }

    #[test]
    fn focus_creates_missing_workspace_as_empty() {
        let mut m = I3Model::default();
        let op = I3Op::Focus {
            ws: Handle("3: new".into()),
        };
        assert_eq!(m.apply(&op), Ok(()));
        assert_eq!(m.ws.get(&Handle("3: new".into())), Some(&false));
        assert_eq!(m.focused, Some(Handle("3: new".into())));
    }

    #[test]
    fn focus_reaps_previously_focused_empty_workspace() {
        let mut m = I3Model::default();
        // workspace 7 is empty AND focused
        m.insert(Handle("7: empty".into()), false);
        m.focus(Handle("7: empty".into()));
        // focus another workspace
        let op = I3Op::Focus {
            ws: Handle("3: other".into()),
        };
        m.apply(&op).unwrap();
        // empty one is gone
        assert!(!m.ws.contains_key(&Handle("7: empty".into())));
        assert_eq!(m.focused, Some(Handle("3: other".into())));
    }

    #[test]
    fn focus_keeps_previously_focused_non_empty_workspace() {
        let mut m = I3Model::default();
        m.insert(Handle("7: has-stuff".into()), true);
        m.focus(Handle("7: has-stuff".into()));
        let op = I3Op::Focus {
            ws: Handle("3: other".into()),
        };
        m.apply(&op).unwrap();
        assert!(m.ws.contains_key(&Handle("7: has-stuff".into())));
    }

    #[test]
    fn focus_noop_when_already_focused() {
        let mut m = I3Model::default();
        m.insert(Handle("5: a".into()), false);
        m.focus(Handle("5: a".into()));
        let op = I3Op::Focus {
            ws: Handle("5: a".into()),
        };
        m.apply(&op).unwrap();
        assert!(m.ws.contains_key(&Handle("5: a".into())));
        assert_eq!(m.focused, Some(Handle("5: a".into())));
    }

    #[test]
    fn focus_case_insensitive_finds_existing() {
        let mut m = I3Model::default();
        m.insert(Handle("5: MyEnv".into()), true);
        let op = I3Op::Focus {
            ws: Handle("5: myenv".into()),
        };
        m.apply(&op).unwrap();
        assert!(m.ws.contains_key(&Handle("5: MyEnv".into())));
        assert_eq!(m.focused, Some(Handle("5: MyEnv".into())));
        assert!(!m.ws.contains_key(&Handle("5: myenv".into())));
    }

    #[test]
    fn rename_rejects_duplicate_name_case_insensitive() {
        let mut m = I3Model::default();
        m.insert(Handle("5: a".into()), true);
        m.insert(Handle("6: B".into()), true);
        let op = I3Op::Rename {
            from: Handle("5: a".into()),
            to: Handle("6: b".into()),
        };
        assert_eq!(
            m.apply(&op),
            Err(I3Error::DuplicateName {
                name: Handle("6: b".into())
            })
        );
    }

    #[test]
    fn rename_allows_case_change_of_same_workspace() {
        let mut m = I3Model::default();
        m.insert(Handle("5: myenv".into()), true);
        let op = I3Op::Rename {
            from: Handle("5: myenv".into()),
            to: Handle("5: MyEnv".into()),
        };
        assert_eq!(m.apply(&op), Ok(()));
        assert!(m.ws.contains_key(&Handle("5: MyEnv".into())));
    }

    #[test]
    fn num_parsing_accepts_leading_zeros() {
        assert_eq!(parse_ws_num("007"), Some(7));
        assert_eq!(parse_ws_num("0"), Some(0));
    }

    #[test]
    fn num_parsing_rejects_negative_and_overflow() {
        assert_eq!(parse_ws_num("-1"), None);
        assert_eq!(parse_ws_num("2147483648"), None);
        assert_eq!(parse_ws_num(""), None);
    }
}
