//! Environment status types - the canonical schema for the `status` field
//! of per-env `meta.json`, AND the payload of the `status_changed`
//! cookbook->daemon wire event (#302).
//!
//! Lives in the SDK (not the daemon) because both the daemon and the
//! cookbooks must agree on it: the daemon writes it to `meta.json`, and
//! cookbooks emit it on their `listen` stdout stream. `enwiro-daemon::meta`
//! re-exports these so existing daemon/CLI call sites are unchanged.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Status {
    #[serde(rename = "uncooked")]
    Uncooked,
    #[serde(rename = "cooked")]
    Cooked {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase: Option<CookedPhase>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<StatusDetail>,
    },
    #[serde(rename = "done")]
    Done {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<DoneOutcome>,
    },
    #[serde(rename = "evergreen")]
    Evergreen,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CookedPhase {
    Active,
    Waiting,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DoneOutcome {
    Completed,
    Abandoned,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusDetail {
    pub source: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info: Option<serde_json::Value>,
}

/// Whether a cookbook is allowed to set this status automatically.
/// Cookbooks may only report *derived facts* - `Done` (work merged/closed)
/// and `Evergreen` (long-lived) - never workflow *intent* (`Cooked`'s
/// active/waiting/ready), which is the user's to set. The daemon rejects
/// auto-writes that fail this guard.
pub fn is_cookbook_settable(status: &Status) -> bool {
    matches!(status, Status::Done { .. } | Status::Evergreen)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookbook_settable_allows_done_and_evergreen_only() {
        assert!(is_cookbook_settable(&Status::Done { outcome: None }));
        assert!(is_cookbook_settable(&Status::Evergreen));
        assert!(!is_cookbook_settable(&Status::Uncooked));
        assert!(!is_cookbook_settable(&Status::Cooked {
            phase: Some(CookedPhase::Active),
            detail: None
        }));
    }

    #[test]
    fn status_roundtrips_through_json() {
        let s = Status::Done {
            outcome: Some(DoneOutcome::Completed),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Status>(&json).unwrap(), s);
    }
}
