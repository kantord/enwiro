//! Per-environment metadata: the `meta.json` file the daemon and CLI both
//! write to. Owns the on-disk schema (`EnvStats`, `UserIntentSignals`),
//! the read/write helpers, and the data-collection functions that produce
//! entries on activation, switch, and recipe-cook events.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{fs, io};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserIntentSignals {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub activation_buffer: Vec<(i64, f64)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub switch_buffer: Vec<(i64, f64)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prep_buffer: Vec<(i64, f64)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EnvStats {
    #[serde(flatten, default)]
    pub signals: UserIntentSignals,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookbook: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub event_log: Vec<EventLogEntry>,
}

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventLogEntry {
    #[serde(rename = "type")]
    pub event_type: EventType,
    pub detail: String,
    pub started: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    StatusChange,
    #[serde(untagged)]
    Other(String),
}

pub fn now_utc() -> DateTime<Utc> {
    Utc::now()
}

pub fn now_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Load per-environment metadata from its meta.json file.
/// Returns default (empty) metadata on any error.
pub fn load_env_meta(env_dir: &Path) -> EnvStats {
    let meta_path = env_dir.join("meta.json");
    fs::read_to_string(&meta_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_env_meta(env_dir: &Path, meta: &EnvStats) -> io::Result<()> {
    let meta_path = env_dir.join("meta.json");
    enwiro_sdk::fs::atomic_write(&meta_path, serde_json::to_string(meta)?.as_bytes())
}

/// Record activation in per-env meta.json. Best-effort.
pub fn record_activation_per_env(env_dir: &Path) {
    if !env_dir.is_dir() {
        return;
    }
    let mut meta = load_env_meta(env_dir);
    meta.signals.activation_buffer.push((now_timestamp(), 1.0));
    meta.signals
        .activation_buffer
        .sort_by_key(|b| std::cmp::Reverse(b.0));
    meta.signals.activation_buffer.truncate(10);
    if let Err(e) = save_env_meta(env_dir, &meta) {
        tracing::warn!(error = %e, "Could not save environment metadata");
    }
}

/// Record a workspace switch event in per-env meta.json. Best-effort.
pub fn record_switch_per_env(env_dir: &Path, timestamp: i64) {
    if !env_dir.is_dir() {
        return;
    }
    let mut meta = load_env_meta(env_dir);

    let last_activation_ts = meta
        .signals
        .activation_buffer
        .iter()
        .map(|&(ts, _)| ts)
        .max();
    let last_switch_ts = meta.signals.switch_buffer.iter().map(|&(ts, _)| ts).max();
    let last_signal_ts = [last_activation_ts, last_switch_ts]
        .into_iter()
        .flatten()
        .max();

    meta.signals.switch_buffer.push((timestamp, 1.0));
    meta.signals
        .switch_buffer
        .sort_by_key(|b| std::cmp::Reverse(b.0));
    meta.signals.switch_buffer.truncate(25);

    const EIGHT_HOURS: i64 = 28800;
    if last_signal_ts.is_none_or(|last| timestamp - last > EIGHT_HOURS) {
        meta.signals.activation_buffer.push((timestamp, 0.4));
        meta.signals
            .activation_buffer
            .sort_by_key(|b| std::cmp::Reverse(b.0));
        meta.signals.activation_buffer.truncate(10);
    }

    if let Err(e) = save_env_meta(env_dir, &meta) {
        tracing::warn!(error = %e, "Could not save switch event");
    }
}

/// Record a `prep`-command event in per-env meta.json. Best-effort.
/// `prep_buffer` holds only the most recent event — repeated scripted
/// preps don't stack and can't drown out the activation signal.
pub fn record_prep_per_env(env_dir: &Path) {
    if !env_dir.is_dir() {
        return;
    }
    let mut meta = load_env_meta(env_dir);
    meta.signals.prep_buffer = vec![(now_timestamp(), 1.0)];
    if let Err(e) = save_env_meta(env_dir, &meta) {
        tracing::warn!(error = %e, "Could not save prep event");
    }
}

/// Save cookbook, recipe, and description metadata in per-env meta.json. Best-effort.
pub fn record_cook_metadata_per_env(
    env_dir: &Path,
    cookbook: &str,
    recipe: &str,
    description: Option<&str>,
) {
    let mut meta = load_env_meta(env_dir);
    meta.cookbook = Some(cookbook.to_string());
    meta.recipe = Some(recipe.to_string());
    if let Some(d) = description {
        meta.description = Some(d.to_string());
    }
    if let Err(e) = save_env_meta(env_dir, &meta) {
        tracing::warn!(error = %e, "Could not save environment metadata");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_per_env_record_activation() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        record_activation_per_env(&env_dir);

        let meta = load_env_meta(&env_dir);
        assert_eq!(meta.signals.activation_buffer.len(), 1);
        assert!(meta.signals.activation_buffer[0].0 > 0);
    }

    #[test]
    fn test_per_env_record_activation_increments() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        record_activation_per_env(&env_dir);
        record_activation_per_env(&env_dir);

        let meta = load_env_meta(&env_dir);
        assert_eq!(meta.signals.activation_buffer.len(), 2);
    }

    #[test]
    fn test_per_env_record_prep_writes_distinct_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        record_prep_per_env(&env_dir);

        let meta = load_env_meta(&env_dir);
        assert!(
            meta.signals.activation_buffer.is_empty(),
            "prep must not write to activation_buffer"
        );
        assert_eq!(meta.signals.prep_buffer.len(), 1);
        assert!(meta.signals.prep_buffer[0].0 > 0);
        assert!(
            (meta.signals.prep_buffer[0].1 - 1.0).abs() < 1e-10,
            "prep event weight must be 1.0, got {}",
            meta.signals.prep_buffer[0].1
        );
    }

    /// Repeated preps must not stack — only the latest event is kept so
    /// scripted callers can't drown out the activation signal.
    #[test]
    fn test_per_env_record_prep_does_not_stack() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        record_prep_per_env(&env_dir);
        record_prep_per_env(&env_dir);
        record_prep_per_env(&env_dir);

        let meta = load_env_meta(&env_dir);
        assert_eq!(
            meta.signals.prep_buffer.len(),
            1,
            "prep_buffer must hold only the latest event"
        );
    }

    #[test]
    fn test_per_env_record_cook_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        record_cook_metadata_per_env(
            &env_dir,
            "github",
            "kantord/enwiro#325",
            Some("Fix auth bug"),
        );

        let meta = load_env_meta(&env_dir);
        assert_eq!(meta.cookbook, Some("github".to_string()));
        assert_eq!(meta.recipe, Some("kantord/enwiro#325".to_string()));
        assert_eq!(meta.description, Some("Fix auth bug".to_string()));
    }

    #[test]
    fn test_per_env_record_cook_metadata_persists_recipe() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        record_cook_metadata_per_env(&env_dir, "github", "owner/repo#42", None);

        let raw = fs::read_to_string(env_dir.join("meta.json")).unwrap();
        assert!(
            raw.contains("\"recipe\":\"owner/repo#42\""),
            "meta.json must include the recipe field: {raw}"
        );

        let meta = load_env_meta(&env_dir);
        assert_eq!(meta.recipe, Some("owner/repo#42".to_string()));
    }

    #[test]
    fn test_env_stats_recipe_defaults_to_none_for_old_envs() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("legacy");
        fs::create_dir(&env_dir).unwrap();
        fs::write(
            env_dir.join("meta.json"),
            r#"{"cookbook":"github","description":"old env"}"#,
        )
        .unwrap();

        let meta = load_env_meta(&env_dir);
        assert_eq!(meta.recipe, None);
        assert_eq!(meta.cookbook, Some("github".to_string()));
    }

    #[test]
    fn test_per_env_load_missing_dir_returns_default() {
        let meta = load_env_meta(Path::new("/nonexistent/env/dir"));
        assert!(meta.signals.activation_buffer.is_empty());
        assert_eq!(meta.description, None);
    }

    #[test]
    fn test_per_env_metadata_preserves_activation_on_cook() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        record_activation_per_env(&env_dir);
        record_activation_per_env(&env_dir);
        record_cook_metadata_per_env(&env_dir, "git", "my/project", Some("My project"));

        let meta = load_env_meta(&env_dir);
        assert_eq!(meta.signals.activation_buffer.len(), 2);
        assert_eq!(meta.cookbook, Some("git".to_string()));
        assert_eq!(meta.recipe, Some("my/project".to_string()));
        assert_eq!(meta.description, Some("My project".to_string()));
    }

    #[test]
    fn test_activation_buffer_field_exists_and_defaults_empty() {
        let signals = UserIntentSignals::default();
        assert!(
            signals.activation_buffer.is_empty(),
            "activation_buffer must be empty by default"
        );
    }

    #[test]
    fn test_env_stats_serializes_activation_buffer_field_name() {
        let signals = UserIntentSignals {
            activation_buffer: vec![(1_700_000_000i64, 1.0f64)],
            ..Default::default()
        };
        let stats = EnvStats {
            signals,
            ..Default::default()
        };
        let json = serde_json::to_string(&stats).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(
            value.get("activation_buffer").is_some(),
            "activation_buffer must appear as a top-level JSON key in EnvStats"
        );
    }

    #[test]
    fn test_record_activation_pushes_entry_to_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        let before = now_timestamp();
        record_activation_per_env(&env_dir);
        let after = now_timestamp();

        let meta = load_env_meta(&env_dir);
        assert_eq!(
            meta.signals.activation_buffer.len(),
            1,
            "buffer must have exactly one entry after one activation"
        );
        let (ts, weight) = meta.signals.activation_buffer[0];
        assert!(
            ts >= before && ts <= after,
            "buffer entry timestamp must be within the recording window"
        );
        assert!(
            (weight - 1.0).abs() < 1e-10,
            "buffer entry weight must be 1.0"
        );
    }

    #[test]
    fn test_record_activation_appends_entries() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        record_activation_per_env(&env_dir);
        record_activation_per_env(&env_dir);
        record_activation_per_env(&env_dir);

        let meta = load_env_meta(&env_dir);
        assert_eq!(
            meta.signals.activation_buffer.len(),
            3,
            "buffer must grow by one entry per activation"
        );
        for &(_, weight) in &meta.signals.activation_buffer {
            assert!((weight - 1.0).abs() < 1e-10, "every weight must be 1.0");
        }
    }

    #[test]
    fn test_activation_buffer_capped_at_ten() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        let old_ts: i64 = 1_000_000;
        let mut meta = load_env_meta(&env_dir);
        for _ in 0..10 {
            meta.signals.activation_buffer.push((old_ts, 1.0));
        }
        save_env_meta(&env_dir, &meta).unwrap();

        record_activation_per_env(&env_dir);

        let meta = load_env_meta(&env_dir);
        assert_eq!(
            meta.signals.activation_buffer.len(),
            10,
            "buffer must stay at capacity 10 after an 11th activation"
        );

        let newest_ts = meta.signals.activation_buffer[0].0;
        assert!(
            newest_ts > old_ts,
            "newest entry should be present after cap; got ts={newest_ts}"
        );
        assert_eq!(
            meta.signals.activation_buffer.last().unwrap().0,
            old_ts,
            "some pre-populated old entries must still be in the buffer after cap"
        );
    }

    #[test]
    fn test_switch_buffer_field_exists_and_defaults_empty() {
        let signals = UserIntentSignals::default();
        assert!(
            signals.switch_buffer.is_empty(),
            "switch_buffer must be empty by default"
        );
    }

    #[test]
    fn test_env_stats_serializes_switch_buffer_field_name() {
        let signals = UserIntentSignals {
            switch_buffer: vec![(1_700_000_000i64, 1.0f64)],
            ..Default::default()
        };
        let stats = EnvStats {
            signals,
            ..Default::default()
        };
        let json = serde_json::to_string(&stats).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(
            value.get("switch_buffer").is_some(),
            "switch_buffer must appear as a top-level JSON key in EnvStats"
        );
    }

    #[test]
    fn test_record_switch_per_env_pushes_entry() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        let ts: i64 = 1_700_000_000;
        record_switch_per_env(&env_dir, ts);

        let meta = load_env_meta(&env_dir);
        assert_eq!(
            meta.signals.switch_buffer.len(),
            1,
            "switch_buffer must have exactly one entry after one call"
        );
        let (stored_ts, weight) = meta.signals.switch_buffer[0];
        assert_eq!(
            stored_ts, ts,
            "stored timestamp must match the provided one"
        );
        assert!(
            (weight - 1.0).abs() < 1e-10,
            "switch_buffer entry weight must be 1.0"
        );
    }

    #[test]
    fn test_record_switch_per_env_appends_entries() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        record_switch_per_env(&env_dir, 1_700_000_001);
        record_switch_per_env(&env_dir, 1_700_000_002);
        record_switch_per_env(&env_dir, 1_700_000_003);

        let meta = load_env_meta(&env_dir);
        assert_eq!(
            meta.signals.switch_buffer.len(),
            3,
            "switch_buffer must grow by one entry per call"
        );
        for &(_, weight) in &meta.signals.switch_buffer {
            assert!((weight - 1.0).abs() < 1e-10, "every weight must be 1.0");
        }
    }

    #[test]
    fn test_record_switch_per_env_buffer_sorted_descending() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        record_switch_per_env(&env_dir, 1_700_000_001);
        record_switch_per_env(&env_dir, 1_700_000_003);
        record_switch_per_env(&env_dir, 1_700_000_002);

        let meta = load_env_meta(&env_dir);
        let timestamps: Vec<i64> = meta
            .signals
            .switch_buffer
            .iter()
            .map(|&(ts, _)| ts)
            .collect();
        let mut sorted = timestamps.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(
            timestamps, sorted,
            "switch_buffer entries must be in descending timestamp order"
        );
    }

    #[test]
    fn test_switch_buffer_capped_at_twenty_five() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        let old_ts: i64 = 1_000_000;
        let mut meta = load_env_meta(&env_dir);
        for _ in 0..25 {
            meta.signals.switch_buffer.push((old_ts, 1.0));
        }
        save_env_meta(&env_dir, &meta).unwrap();

        let new_ts: i64 = 1_700_000_000;
        record_switch_per_env(&env_dir, new_ts);

        let meta = load_env_meta(&env_dir);
        assert_eq!(
            meta.signals.switch_buffer.len(),
            25,
            "switch_buffer must stay at capacity 25 after a 26th entry"
        );

        let newest_ts = meta.signals.switch_buffer[0].0;
        assert_eq!(
            newest_ts, new_ts,
            "newest entry should be present after cap; got ts={newest_ts}"
        );
        assert_eq!(
            meta.signals.switch_buffer.last().unwrap().0,
            old_ts,
            "some pre-populated old entries must still be in the buffer after cap"
        );
    }

    #[test]
    fn test_record_switch_per_env_does_not_touch_activation_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        record_activation_per_env(&env_dir);
        record_switch_per_env(&env_dir, 1_700_000_000);

        let meta = load_env_meta(&env_dir);
        assert_eq!(
            meta.signals.activation_buffer.len(),
            1,
            "activation_buffer must be untouched by record_switch_per_env"
        );
    }

    #[test]
    fn test_old_json_without_switch_buffer_gives_empty_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();
        fs::write(
            env_dir.join("meta.json"),
            r#"{"activation_buffer":[[1700000000,1.0]]}"#,
        )
        .unwrap();

        let meta = load_env_meta(&env_dir);
        assert!(
            meta.signals.switch_buffer.is_empty(),
            "old JSON without switch_buffer key must deserialize to an empty buffer"
        );
    }

    #[test]
    fn test_record_switch_per_env_does_nothing_for_nonexistent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("nonexistent-env");
        assert!(
            !env_dir.exists(),
            "test precondition: env_dir must not exist before calling record_switch_per_env"
        );

        record_switch_per_env(&env_dir, 1_700_000_000);

        assert!(
            !env_dir.exists(),
            "record_switch_per_env must not create the env directory when it does not exist"
        );
    }

    #[test]
    fn test_record_switch_per_env_adds_synthetic_activation_when_no_prior_signals() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        let ts: i64 = 1_700_000_000;
        record_switch_per_env(&env_dir, ts);

        let meta = load_env_meta(&env_dir);
        assert_eq!(
            meta.signals.activation_buffer.len(),
            1,
            "activation_buffer must contain exactly one synthetic entry after the first-ever switch"
        );
        let (entry_ts, entry_weight) = meta.signals.activation_buffer[0];
        assert_eq!(
            entry_ts, ts,
            "synthetic activation timestamp must match the switch timestamp"
        );
        assert!(
            (entry_weight - 0.4).abs() < 1e-10,
            "synthetic activation weight must be 0.4, got {entry_weight}"
        );
    }

    #[test]
    fn test_record_switch_per_env_adds_synthetic_activation_when_last_activation_over_8h_ago() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        let ts: i64 = 1_700_000_000;
        let eight_hours_plus_one: i64 = 8 * 3600 + 1;
        let old_activation_ts = ts - eight_hours_plus_one;

        let mut meta = load_env_meta(&env_dir);
        meta.signals
            .activation_buffer
            .push((old_activation_ts, 1.0));
        save_env_meta(&env_dir, &meta).unwrap();

        record_switch_per_env(&env_dir, ts);

        let meta = load_env_meta(&env_dir);
        assert_eq!(
            meta.signals.activation_buffer.len(),
            2,
            "activation_buffer must gain a synthetic entry when last activation was >8h ago"
        );
        let (newest_ts, newest_weight) = meta.signals.activation_buffer[0];
        assert_eq!(
            newest_ts, ts,
            "synthetic activation timestamp must match the switch timestamp"
        );
        assert!(
            (newest_weight - 0.4).abs() < 1e-10,
            "synthetic activation weight must be 0.4, got {newest_weight}"
        );
    }

    #[test]
    fn test_record_switch_per_env_adds_synthetic_activation_when_last_switch_over_8h_ago() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        let ts: i64 = 1_700_000_000;
        let eight_hours_plus_one: i64 = 8 * 3600 + 1;
        let old_switch_ts = ts - eight_hours_plus_one;

        let mut meta = load_env_meta(&env_dir);
        meta.signals.switch_buffer.push((old_switch_ts, 1.0));
        save_env_meta(&env_dir, &meta).unwrap();

        record_switch_per_env(&env_dir, ts);

        let meta = load_env_meta(&env_dir);
        assert_eq!(
            meta.signals.activation_buffer.len(),
            1,
            "activation_buffer must contain one synthetic entry when last switch was >8h ago"
        );
        let (entry_ts, entry_weight) = meta.signals.activation_buffer[0];
        assert_eq!(
            entry_ts, ts,
            "synthetic activation timestamp must match the switch timestamp"
        );
        assert!(
            (entry_weight - 0.4).abs() < 1e-10,
            "synthetic activation weight must be 0.4, got {entry_weight}"
        );
    }

    #[test]
    fn test_record_switch_per_env_no_synthetic_activation_when_last_signal_within_8h() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        let ts: i64 = 1_700_000_000;
        let one_hour: i64 = 3600;
        let recent_activation_ts = ts - one_hour;

        let mut meta = load_env_meta(&env_dir);
        meta.signals
            .activation_buffer
            .push((recent_activation_ts, 1.0));
        save_env_meta(&env_dir, &meta).unwrap();

        record_switch_per_env(&env_dir, ts);

        let meta = load_env_meta(&env_dir);
        assert_eq!(
            meta.signals.activation_buffer.len(),
            1,
            "activation_buffer must remain unchanged when last signal was within 8h"
        );
        let (entry_ts, entry_weight) = meta.signals.activation_buffer[0];
        assert_eq!(
            entry_ts, recent_activation_ts,
            "the only entry must be the pre-existing activation, not a synthetic one"
        );
        assert!(
            (entry_weight - 1.0).abs() < 1e-10,
            "pre-existing activation weight must still be 1.0, got {entry_weight}"
        );
    }

    #[test]
    fn test_record_switch_per_env_no_synthetic_at_exact_8h_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        let ts: i64 = 1_700_000_000;
        const EIGHT_HOURS: i64 = 28800;
        let boundary_activation_ts = ts - EIGHT_HOURS;

        let mut meta = load_env_meta(&env_dir);
        meta.signals
            .activation_buffer
            .push((boundary_activation_ts, 1.0));
        save_env_meta(&env_dir, &meta).unwrap();

        record_switch_per_env(&env_dir, ts);

        let meta = load_env_meta(&env_dir);
        assert_eq!(
            meta.signals.activation_buffer.len(),
            1,
            "activation_buffer must NOT gain a synthetic entry when gap == 28800 (boundary is exclusive)"
        );
        assert_eq!(
            meta.signals.activation_buffer[0].0, boundary_activation_ts,
            "the only entry must still be the pre-existing one, not a synthetic"
        );
    }

    #[test]
    fn test_record_switch_per_env_synthetic_injected_at_28801s_gap() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        let ts: i64 = 1_700_000_000;
        let just_over_ts = ts - 28801;

        let mut meta = load_env_meta(&env_dir);
        meta.signals.activation_buffer.push((just_over_ts, 1.0));
        save_env_meta(&env_dir, &meta).unwrap();

        record_switch_per_env(&env_dir, ts);

        let meta = load_env_meta(&env_dir);
        assert_eq!(
            meta.signals.activation_buffer.len(),
            2,
            "activation_buffer must gain a synthetic entry when gap == 28801 (just over threshold)"
        );
        let (newest_ts, newest_weight) = meta.signals.activation_buffer[0];
        assert_eq!(
            newest_ts, ts,
            "synthetic activation timestamp must match the switch timestamp"
        );
        assert!(
            (newest_weight - 0.4).abs() < 1e-10,
            "synthetic activation weight must be 0.4, got {newest_weight}"
        );
    }

    #[test]
    fn test_record_switch_per_env_no_synthetic_when_switch_buffer_is_recent_activation_is_old() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        let ts: i64 = 1_700_000_000;
        let old_activation_ts = ts - (8 * 3600 + 1);
        let recent_switch_ts = ts - 3600;

        let mut meta = load_env_meta(&env_dir);
        meta.signals
            .activation_buffer
            .push((old_activation_ts, 1.0));
        meta.signals.switch_buffer.push((recent_switch_ts, 1.0));
        save_env_meta(&env_dir, &meta).unwrap();

        record_switch_per_env(&env_dir, ts);

        let meta = load_env_meta(&env_dir);
        assert_eq!(
            meta.signals.activation_buffer.len(),
            1,
            "activation_buffer must NOT gain a synthetic entry when switch_buffer has a recent entry (1h ago)"
        );
        assert_eq!(
            meta.signals.activation_buffer[0].0, old_activation_ts,
            "the only entry must still be the original old activation, not a synthetic"
        );
    }

    #[test]
    fn test_record_switch_per_env_no_synthetic_when_activation_buffer_is_recent_switch_is_old() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        let ts: i64 = 1_700_000_000;
        let old_switch_ts = ts - (8 * 3600 + 1);
        let recent_activation_ts = ts - 3600;

        let mut meta = load_env_meta(&env_dir);
        meta.signals.switch_buffer.push((old_switch_ts, 1.0));
        meta.signals
            .activation_buffer
            .push((recent_activation_ts, 1.0));
        save_env_meta(&env_dir, &meta).unwrap();

        record_switch_per_env(&env_dir, ts);

        let meta = load_env_meta(&env_dir);
        assert_eq!(
            meta.signals.activation_buffer.len(),
            1,
            "activation_buffer must NOT gain a synthetic entry when activation_buffer has a recent entry (1h ago)"
        );
        assert_eq!(
            meta.signals.activation_buffer[0].0, recent_activation_ts,
            "the only entry must still be the original recent activation, not a synthetic"
        );
    }
}
