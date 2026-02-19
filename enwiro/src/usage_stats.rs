use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{fs, io};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EnvStats {
    pub last_activated: i64,
    pub activation_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookbook: Option<String>,
}

/// Per-environment usage statistics.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UsageStats {
    pub envs: HashMap<String, EnvStats>,
}

pub fn stats_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("enwiro").join("usage-stats.json"))
}

pub fn now_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Load stats from disk. Returns empty stats on any error (missing file, corrupt JSON, etc.).
pub fn load_stats(path: &Path) -> UsageStats {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Load stats from the default XDG path. Returns empty stats if path unavailable.
pub fn load_stats_default() -> UsageStats {
    match stats_path() {
        Some(path) => load_stats(&path),
        None => UsageStats::default(),
    }
}

/// Save stats atomically (write tmp + rename).
fn save_stats(path: &Path, stats: &UsageStats) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, serde_json::to_string(stats)?)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Record that an environment was activated. Best-effort (errors logged, not propagated).
pub fn record_activation(env_name: &str) {
    let Some(path) = stats_path() else { return };
    record_activation_to(&path, env_name);
}

/// Record activation to a specific path (for testing).
fn record_activation_to(path: &Path, env_name: &str) {
    let mut stats = load_stats(path);
    let entry = stats.envs.entry(env_name.to_string()).or_default();
    entry.last_activated = now_timestamp();
    entry.activation_count += 1;
    if let Err(e) = save_stats(path, &stats) {
        tracing::warn!(error = %e, "Could not save usage stats");
    }
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

/// Save per-environment metadata atomically (write tmp + rename).
fn save_env_meta(env_dir: &Path, meta: &EnvStats) -> io::Result<()> {
    let meta_path = env_dir.join("meta.json");
    let tmp = meta_path.with_extension("tmp");
    fs::write(&tmp, serde_json::to_string(meta)?)?;
    fs::rename(&tmp, &meta_path)?;
    Ok(())
}

/// Record activation in per-env meta.json. Best-effort.
pub fn record_activation_per_env(env_dir: &Path) {
    let mut meta = load_env_meta(env_dir);
    meta.last_activated = now_timestamp();
    meta.activation_count += 1;
    if let Err(e) = save_env_meta(env_dir, &meta) {
        tracing::warn!(error = %e, "Could not save environment metadata");
    }
}

/// Save cookbook and description metadata in per-env meta.json. Best-effort.
pub fn record_cook_metadata_per_env(env_dir: &Path, cookbook: &str, description: Option<&str>) {
    let mut meta = load_env_meta(env_dir);
    meta.cookbook = Some(cookbook.to_string());
    if let Some(d) = description {
        meta.description = Some(d.to_string());
    }
    if let Err(e) = save_env_meta(env_dir, &meta) {
        tracing::warn!(error = %e, "Could not save environment metadata");
    }
}

/// Compute frecency score for an environment (zoxide-style bucket multiplier).
/// Pass the current timestamp (seconds since epoch) for deterministic results.
pub fn frecency_score(stats: &EnvStats, now: i64) -> f64 {
    let age_secs = (now - stats.last_activated).max(0) as f64;
    let multiplier = if age_secs < 3600.0 {
        4.0
    } else if age_secs < 86400.0 {
        2.0
    } else if age_secs < 604800.0 {
        0.5
    } else {
        0.25
    };
    stats.activation_count as f64 * multiplier
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stats.json");

        record_activation_to(&path, "my-project");

        let stats = load_stats(&path);
        assert_eq!(stats.envs.len(), 1);
        let entry = &stats.envs["my-project"];
        assert_eq!(entry.activation_count, 1);
        assert!(entry.last_activated > 0);
    }

    #[test]
    fn test_record_increments_count() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stats.json");

        record_activation_to(&path, "my-project");
        record_activation_to(&path, "my-project");

        let stats = load_stats(&path);
        assert_eq!(stats.envs["my-project"].activation_count, 2);
    }

    #[test]
    fn test_record_multiple_environments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stats.json");

        record_activation_to(&path, "project-a");
        record_activation_to(&path, "project-b");
        record_activation_to(&path, "project-a");

        let stats = load_stats(&path);
        assert_eq!(stats.envs["project-a"].activation_count, 2);
        assert_eq!(stats.envs["project-b"].activation_count, 1);
    }

    #[test]
    fn test_frecency_score_recent_high() {
        let now = 1_000_000;
        let stats = EnvStats {
            last_activated: now,
            activation_count: 10,
            ..Default::default()
        };
        assert!((frecency_score(&stats, now) - 40.0).abs() < 0.01);
    }

    #[test]
    fn test_frecency_score_old_low() {
        let now = 1_000_000;
        let stats = EnvStats {
            last_activated: now - 604801, // >1 week
            activation_count: 10,
            ..Default::default()
        };
        assert!((frecency_score(&stats, now) - 2.5).abs() < 0.01);
    }

    #[test]
    fn test_frecency_score_bucket_boundaries() {
        let now = 1_000_000;
        let count = 10;

        // Just under 1 hour → ×4
        let stats = EnvStats {
            last_activated: now - 3599,
            activation_count: count,
            ..Default::default()
        };
        assert!((frecency_score(&stats, now) - 40.0).abs() < 0.01);

        // Exactly 1 hour → ×2
        let stats = EnvStats {
            last_activated: now - 3600,
            activation_count: count,
            ..Default::default()
        };
        assert!((frecency_score(&stats, now) - 20.0).abs() < 0.01);

        // Just under 1 day → ×2
        let stats = EnvStats {
            last_activated: now - 86399,
            activation_count: count,
            ..Default::default()
        };
        assert!((frecency_score(&stats, now) - 20.0).abs() < 0.01);

        // Exactly 1 day → ×0.5
        let stats = EnvStats {
            last_activated: now - 86400,
            activation_count: count,
            ..Default::default()
        };
        assert!((frecency_score(&stats, now) - 5.0).abs() < 0.01);

        // Just under 1 week → ×0.5
        let stats = EnvStats {
            last_activated: now - 604799,
            activation_count: count,
            ..Default::default()
        };
        assert!((frecency_score(&stats, now) - 5.0).abs() < 0.01);

        // Exactly 1 week → ×0.25
        let stats = EnvStats {
            last_activated: now - 604800,
            activation_count: count,
            ..Default::default()
        };
        assert!((frecency_score(&stats, now) - 2.5).abs() < 0.01);
    }

    #[test]
    fn test_per_env_record_activation() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        record_activation_per_env(&env_dir);

        let meta = load_env_meta(&env_dir);
        assert_eq!(meta.activation_count, 1);
        assert!(meta.last_activated > 0);
    }

    #[test]
    fn test_per_env_record_activation_increments() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        record_activation_per_env(&env_dir);
        record_activation_per_env(&env_dir);

        let meta = load_env_meta(&env_dir);
        assert_eq!(meta.activation_count, 2);
    }

    #[test]
    fn test_per_env_record_cook_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        record_cook_metadata_per_env(&env_dir, "github", Some("Fix auth bug"));

        let meta = load_env_meta(&env_dir);
        assert_eq!(meta.cookbook, Some("github".to_string()));
        assert_eq!(meta.description, Some("Fix auth bug".to_string()));
    }

    #[test]
    fn test_per_env_load_missing_dir_returns_default() {
        let meta = load_env_meta(Path::new("/nonexistent/env/dir"));
        assert_eq!(meta.activation_count, 0);
        assert_eq!(meta.description, None);
    }

    #[test]
    fn test_per_env_metadata_preserves_activation_on_cook() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        record_activation_per_env(&env_dir);
        record_activation_per_env(&env_dir);
        record_cook_metadata_per_env(&env_dir, "git", Some("My project"));

        let meta = load_env_meta(&env_dir);
        assert_eq!(meta.activation_count, 2);
        assert_eq!(meta.cookbook, Some("git".to_string()));
        assert_eq!(meta.description, Some("My project".to_string()));
    }

    #[test]
    fn test_load_missing_file_returns_empty() {
        let stats = load_stats(Path::new("/nonexistent/path/stats.json"));
        assert!(stats.envs.is_empty());
    }

    #[test]
    fn test_load_corrupt_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stats.json");
        fs::write(&path, "not valid json{{{").unwrap();

        let stats = load_stats(&path);
        assert!(stats.envs.is_empty());
    }
}
