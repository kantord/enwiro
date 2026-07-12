//! `GoalDetail` - the canonical schema for the `goal` field of per-env
//! `meta.json` and of a cookbook-declared [`crate::cookbook::Recipe`] (#756).
//!
//! Lives in the SDK (not the daemon) for the same reason `status.rs` does:
//! both the daemon (writes `meta.json`) and cookbooks (declare it on a
//! `Recipe`) need to agree on the shape. `kind` is deliberately a free
//! string rather than a closed enum - see `CookbookMetadata`/`StatusDetail`
//! for the same rationale - so a cookbook can introduce a new goal kind
//! without an enwiro-sdk release.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalDetail {
    pub kind: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goal_detail_roundtrips_through_json_with_detail() {
        let g = GoalDetail {
            kind: "github_issue".to_string(),
            label: "Fix auth bug (#42)".to_string(),
            detail: Some(serde_json::json!({"repo": "kantord/enwiro", "number": 42})),
        };
        let json = serde_json::to_string(&g).unwrap();
        assert_eq!(serde_json::from_str::<GoalDetail>(&json).unwrap(), g);
    }

    #[test]
    fn goal_detail_omits_detail_key_when_none() {
        let g = GoalDetail {
            kind: "manual".to_string(),
            label: "Ship the release".to_string(),
            detail: None,
        };
        let json = serde_json::to_string(&g).unwrap();
        assert_eq!(json, r#"{"kind":"manual","label":"Ship the release"}"#);
    }
}
