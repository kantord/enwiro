use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{fs, io};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserIntentSignals {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub activation_buffer: Vec<(i64, f64)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub switch_buffer: Vec<(i64, f64)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EnvStats {
    #[serde(flatten, default)]
    pub signals: UserIntentSignals,
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

/// Write bytes to `path` atomically via a `.tmp` staging file.
/// Parent directories are created automatically.
///
/// The staging file is named by replacing `path`'s last extension with `.tmp`
/// (e.g. `meta.json` → `meta.tmp`, `recipes.cache` → `recipes.tmp`).
/// Callers that need a different tmp path must not use this function.
pub fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Serialise stats to JSON and delegate to [`atomic_write`].
fn save_stats(path: &Path, stats: &UsageStats) -> io::Result<()> {
    atomic_write(path, serde_json::to_string(stats)?.as_bytes())
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
    entry.signals.activation_buffer.push((now_timestamp(), 1.0));
    entry
        .signals
        .activation_buffer
        .sort_by_key(|b| std::cmp::Reverse(b.0));
    entry.signals.activation_buffer.truncate(10);
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

/// Serialise per-environment metadata to JSON and delegate to [`atomic_write`].
fn save_env_meta(env_dir: &Path, meta: &EnvStats) -> io::Result<()> {
    let meta_path = env_dir.join("meta.json");
    atomic_write(&meta_path, serde_json::to_string(meta)?.as_bytes())
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

    // Compute the most recent signal timestamp across both buffers, before the current push.
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

    // If the gap since the last signal exceeds 8 hours, inject a synthetic activation.
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

/// Compute exponential-decay score for an environment.
/// λ = ln(2) / (48h) gives a 48-hour half-life.
/// Pass the current timestamp (seconds since epoch) for deterministic results.
pub fn frecency_score(stats: &EnvStats, now: i64) -> f64 {
    let lambda = std::f64::consts::LN_2 / (48.0 * 3600.0);
    stats
        .signals
        .activation_buffer
        .iter()
        .map(|&(ts, weight)| {
            let age = (now - ts).max(0) as f64;
            weight * (-lambda * age).exp()
        })
        .sum()
}

/// Compute a two-component exponential-decay score from a workspace-switch buffer.
/// Fast component: 6h half-life. Slow component: 48h half-life.
/// `score = 0.5 * fast_sum + 0.5 * slow_sum`
/// Each entry `(timestamp, weight)` contributes `weight * exp(-λ * elapsed_seconds)`.
#[allow(dead_code)]
pub fn switch_score(buffer: &[(i64, f64)], now: i64) -> f64 {
    let lambda_fast = std::f64::consts::LN_2 / (6.0 * 3600.0);
    let lambda_slow = std::f64::consts::LN_2 / (48.0 * 3600.0);
    let fast_sum: f64 = buffer
        .iter()
        .map(|&(ts, weight)| {
            let age = (now - ts).max(0) as f64;
            weight * (-lambda_fast * age).exp()
        })
        .sum();
    let slow_sum: f64 = buffer
        .iter()
        .map(|&(ts, weight)| {
            let age = (now - ts).max(0) as f64;
            weight * (-lambda_slow * age).exp()
        })
        .sum();
    0.5 * fast_sum + 0.5 * slow_sum
}

/// Compute percentile ranks for all environments based on their frecency scores.
/// For each env, the percentile is: (count of envs with strictly lower score) / total_envs.
/// Tied envs receive the same rank. Empty input returns empty output.
pub fn activation_percentile_scores(
    all_stats: &HashMap<String, EnvStats>,
    now: i64,
) -> HashMap<String, f64> {
    let total = all_stats.len();
    if total == 0 {
        return HashMap::new();
    }
    let scores: HashMap<&str, f64> = all_stats
        .iter()
        .map(|(name, stats)| (name.as_str(), frecency_score(stats, now)))
        .collect();
    scores
        .iter()
        .map(|(&name, &score)| {
            let count_below = scores.values().filter(|&&s| s < score).count();
            (name.to_string(), count_below as f64 / total as f64)
        })
        .collect()
}

/// Compute percentile ranks for all environments based on their switch scores.
/// Mirrors [`activation_percentile_scores`] but uses `switch_score` instead of `frecency_score`.
fn switch_percentile_scores(
    all_stats: &HashMap<String, EnvStats>,
    now: i64,
) -> HashMap<String, f64> {
    let total = all_stats.len();
    if total == 0 {
        return HashMap::new();
    }
    let scores: HashMap<&str, f64> = all_stats
        .iter()
        .map(|(name, stats)| {
            (
                name.as_str(),
                switch_score(&stats.signals.switch_buffer, now),
            )
        })
        .collect();
    scores
        .iter()
        .map(|(&name, &score)| {
            let count_below = scores.values().filter(|&&s| s < score).count();
            (name.to_string(), count_below as f64 / total as f64)
        })
        .collect()
}

/// Score function for the launcher UI (`list-all`).
/// Blends activation and switch percentile signals: 0.8 × activation + 0.2 × switch.
pub fn launcher_score(all_stats: &HashMap<String, EnvStats>, now: i64) -> HashMap<String, f64> {
    let activation = activation_percentile_scores(all_stats, now);
    let switch = switch_percentile_scores(all_stats, now);
    activation
        .into_iter()
        .map(|(name, act)| {
            let sw = switch.get(&name).copied().unwrap_or(0.0);
            (name, 0.8 * act + 0.2 * sw)
        })
        .collect()
}

/// Score function for workspace slot assignment (`activate`).
/// Blends activation and switch percentile signals: 0.2 × activation + 0.8 × switch.
pub fn slot_scores(all_stats: &HashMap<String, EnvStats>, now: i64) -> HashMap<String, f64> {
    let activation = activation_percentile_scores(all_stats, now);
    let switch = switch_percentile_scores(all_stats, now);
    activation
        .into_iter()
        .map(|(name, act)| {
            let sw = switch.get(&name).copied().unwrap_or(0.0);
            (name, 0.2 * act + 0.8 * sw)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── atomic_write abstraction tests ──────────────────────────────────────

    /// The function must exist and must write the given bytes to the target path.
    #[test]
    fn test_atomic_write_creates_file_with_correct_contents() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("output.json");
        let data = b"{\"hello\": \"world\"}";

        atomic_write(&target, data).expect("atomic_write should succeed");

        let written = fs::read(&target).expect("target file should exist after atomic_write");
        assert_eq!(written, data);
    }

    /// The temporary file must NOT remain on disk after a successful write.
    #[test]
    fn test_atomic_write_leaves_no_tmp_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("output.json");
        let data = b"some content";

        atomic_write(&target, data).expect("atomic_write should succeed");

        let tmp = target.with_extension("tmp");
        assert!(
            !tmp.exists(),
            "the .tmp staging file should be gone after atomic_write succeeds"
        );
    }

    /// Writing the same path twice must overwrite the previous content.
    #[test]
    fn test_atomic_write_overwrites_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("output.json");

        atomic_write(&target, b"first").unwrap();
        atomic_write(&target, b"second").unwrap();

        let written = fs::read(&target).unwrap();
        assert_eq!(written, b"second");
    }

    /// Parent directories must be created automatically (mirrors save_stats behaviour).
    #[test]
    fn test_atomic_write_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a").join("b").join("c").join("output.json");

        atomic_write(&target, b"data").expect("atomic_write should create missing parent dirs");

        assert!(target.exists());
    }

    /// The written bytes must be exactly what was passed in — no extra bytes, no truncation.
    #[test]
    fn test_atomic_write_preserves_exact_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("output.bin");
        let data: Vec<u8> = (0u8..=255).collect();

        atomic_write(&target, &data).unwrap();

        let written = fs::read(&target).unwrap();
        assert_eq!(written, data);
    }

    #[test]
    fn test_record_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stats.json");

        record_activation_to(&path, "my-project");

        let stats = load_stats(&path);
        assert_eq!(stats.envs.len(), 1);
        let entry = &stats.envs["my-project"];
        assert_eq!(entry.signals.activation_buffer.len(), 1);
        assert!(entry.signals.activation_buffer[0].0 > 0);
    }

    #[test]
    fn test_record_increments_count() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stats.json");

        record_activation_to(&path, "my-project");
        record_activation_to(&path, "my-project");

        let stats = load_stats(&path);
        assert_eq!(stats.envs["my-project"].signals.activation_buffer.len(), 2);
    }

    #[test]
    fn test_record_multiple_environments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stats.json");

        record_activation_to(&path, "project-a");
        record_activation_to(&path, "project-b");
        record_activation_to(&path, "project-a");

        let stats = load_stats(&path);
        assert_eq!(stats.envs["project-a"].signals.activation_buffer.len(), 2);
        assert_eq!(stats.envs["project-b"].signals.activation_buffer.len(), 1);
    }

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
        record_cook_metadata_per_env(&env_dir, "git", Some("My project"));

        let meta = load_env_meta(&env_dir);
        assert_eq!(meta.signals.activation_buffer.len(), 2);
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

    // ── activation_buffer ring-buffer tests ────────────────────────────────

    /// `UserIntentSignals` must have an `activation_buffer` field of type `Vec<(i64, f64)>`.
    /// A freshly default-constructed value must have an empty buffer.
    #[test]
    fn test_activation_buffer_field_exists_and_defaults_empty() {
        let signals = UserIntentSignals::default();
        assert!(
            signals.activation_buffer.is_empty(),
            "activation_buffer must be empty by default"
        );
    }

    /// `activation_buffer` must be serialised under that exact JSON key name,
    /// flattened into the top-level `EnvStats` object.
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

    /// Recording an activation pushes a `(timestamp, 1.0)` entry into `activation_buffer`.
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

    /// Each subsequent activation must append another `(timestamp, 1.0)` entry.
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

    /// The buffer must be capped at N=10: when an 11th activation is recorded,
    /// the oldest entry is dropped so the buffer stays at length 10.
    #[test]
    fn test_activation_buffer_capped_at_ten() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        // Pre-populate the buffer with 10 entries using a fixed old timestamp.
        let old_ts: i64 = 1_000_000;
        let mut meta = load_env_meta(&env_dir);
        for _ in 0..10 {
            meta.signals.activation_buffer.push((old_ts, 1.0));
        }
        save_env_meta(&env_dir, &meta).unwrap();

        // Record one more activation — should trim to 10.
        record_activation_per_env(&env_dir);

        let meta = load_env_meta(&env_dir);
        assert_eq!(
            meta.signals.activation_buffer.len(),
            10,
            "buffer must stay at capacity 10 after an 11th activation"
        );

        // The newest entry (just recorded) must be present at buffer[0] after descending sort.
        let newest_ts = meta.signals.activation_buffer[0].0;
        assert!(
            newest_ts > old_ts,
            "newest entry should be present after cap; got ts={newest_ts}"
        );
        // The oldest surviving entries must still carry the old timestamp.
        assert_eq!(
            meta.signals.activation_buffer.last().unwrap().0,
            old_ts,
            "some pre-populated old entries must still be in the buffer after cap"
        );
    }

    /// `frecency_score` must return 0.0 for an empty buffer.
    #[test]
    fn test_frecency_score_empty_buffer_is_zero() {
        let stats = EnvStats::default();
        let now = now_timestamp();
        let score = frecency_score(&stats, now);
        assert!(
            score.abs() < 1e-10,
            "frecency_score for an empty buffer must be 0.0, got {score}"
        );
    }

    /// A single activation recorded at `now` must yield a score ≈ 1.0
    /// (decay factor e^0 = 1.0, weight 1.0).
    #[test]
    fn test_frecency_score_single_entry_at_now_is_one() {
        let now: i64 = 1_700_000_000;
        let stats = EnvStats {
            signals: UserIntentSignals {
                activation_buffer: vec![(now, 1.0)],
                ..Default::default()
            },
            ..Default::default()
        };
        let score = frecency_score(&stats, now);
        assert!(
            (score - 1.0).abs() < 1e-6,
            "score for a single entry at now must be ≈ 1.0, got {score}"
        );
    }

    /// A single activation recorded exactly 48 hours ago must yield a score ≈ 0.5
    /// (48-hour half-life: e^(-λ*48h) = 0.5 by definition).
    #[test]
    fn test_frecency_score_single_entry_48h_ago_is_half() {
        let now: i64 = 1_700_000_000;
        let forty_eight_hours: i64 = 48 * 3600;
        let stats = EnvStats {
            signals: UserIntentSignals {
                activation_buffer: vec![(now - forty_eight_hours, 1.0)],
                ..Default::default()
            },
            ..Default::default()
        };
        let score = frecency_score(&stats, now);
        assert!(
            (score - 0.5).abs() < 1e-6,
            "score for a single entry 48h ago must be ≈ 0.5, got {score}"
        );
    }

    /// Scores from multiple entries must be summed: two entries each with weight 1.0
    /// at `now` must give score ≈ 2.0.
    #[test]
    fn test_frecency_score_sums_multiple_entries() {
        let now: i64 = 1_700_000_000;
        let stats = EnvStats {
            signals: UserIntentSignals {
                activation_buffer: vec![(now, 1.0), (now, 1.0)],
                ..Default::default()
            },
            ..Default::default()
        };
        let score = frecency_score(&stats, now);
        assert!(
            (score - 2.0).abs() < 1e-6,
            "score for two entries at now must be ≈ 2.0, got {score}"
        );
    }

    /// Old on-disk JSON that only has `activation_count` and `last_activated` keys
    /// must deserialize without error, and `activation_buffer` must be empty.
    #[test]
    fn test_old_json_with_activation_count_only_gives_empty_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();
        fs::write(
            env_dir.join("meta.json"),
            r#"{"last_activated":1700000000,"activation_count":99}"#,
        )
        .unwrap();

        let meta = load_env_meta(&env_dir);
        assert!(
            meta.signals.activation_buffer.is_empty(),
            "old JSON without activation_buffer key must deserialize to an empty buffer"
        );
    }

    /// Variant: centralized usage-stats.json with old keys must also give empty buffer.
    #[test]
    fn test_old_centralized_json_gives_empty_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stats.json");
        fs::write(
            &path,
            r#"{"envs":{"my-project":{"last_activated":1700000000,"activation_count":42}}}"#,
        )
        .unwrap();

        let stats = load_stats(&path);
        assert!(
            stats.envs["my-project"]
                .signals
                .activation_buffer
                .is_empty(),
            "old centralized JSON without activation_buffer must deserialize to an empty buffer"
        );
    }

    // ── activation_percentile_scores tests ─────────────────────────────────

    /// An empty input map must produce an empty output map.
    #[test]
    fn test_activation_percentile_scores_empty_input() {
        let all_stats: HashMap<String, EnvStats> = HashMap::new();
        let now: i64 = 1_700_000_000;
        let result = activation_percentile_scores(&all_stats, now);
        assert!(
            result.is_empty(),
            "empty input must produce empty output, got {result:?}"
        );
    }

    /// When all environments have empty activation buffers their frecency score is
    /// 0.0. With no env scoring strictly lower than another, every env must receive
    /// percentile 0.0.
    #[test]
    fn test_activation_percentile_scores_all_zeros() {
        let now: i64 = 1_700_000_000;
        let mut all_stats: HashMap<String, EnvStats> = HashMap::new();
        all_stats.insert("alpha".to_string(), EnvStats::default());
        all_stats.insert("beta".to_string(), EnvStats::default());

        let result: HashMap<String, f64> = activation_percentile_scores(&all_stats, now);

        assert_eq!(result.len(), 2);
        for (name, pct) in result.iter() {
            let pct: f64 = *pct;
            assert!(
                pct.abs() < 1e-10,
                "env '{name}' has no activations so percentile must be 0.0, got {pct}"
            );
        }
    }

    /// Three environments with distinct scores must each receive a percentile equal
    /// to (count of envs with strictly lower score) / total_envs.
    ///
    /// Setup:
    ///   - "low"  : empty buffer → score 0.0  → 0 envs below → rank 0/3 = 0.0
    ///   - "mid"  : 1 activation 48h ago      → score ≈ 0.5 → 1 env below → rank 1/3
    ///   - "high" : 1 activation at now       → score ≈ 1.0 → 2 envs below → rank 2/3
    #[test]
    fn test_activation_percentile_scores_three_varied_scores() {
        let now: i64 = 1_700_000_000;
        let forty_eight_hours: i64 = 48 * 3600;

        let mut all_stats: HashMap<String, EnvStats> = HashMap::new();
        all_stats.insert("low".to_string(), EnvStats::default());
        all_stats.insert(
            "mid".to_string(),
            EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![(now - forty_eight_hours, 1.0)],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        all_stats.insert(
            "high".to_string(),
            EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![(now, 1.0)],
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        let result = activation_percentile_scores(&all_stats, now);

        assert_eq!(result.len(), 3);
        let total = 3.0_f64;
        assert!(
            result["low"].abs() < 1e-10,
            "low must have percentile 0/3 = 0.0, got {}",
            result["low"]
        );
        assert!(
            (result["mid"] - 1.0 / total).abs() < 1e-10,
            "mid must have percentile 1/3, got {}",
            result["mid"]
        );
        assert!(
            (result["high"] - 2.0 / total).abs() < 1e-10,
            "high must have percentile 2/3, got {}",
            result["high"]
        );
    }

    /// Environments with the same frecency score must receive the same percentile rank.
    ///
    /// Setup (3 envs, total = 3):
    ///   - "zero"   : empty buffer → score 0.0          → rank 0/3 = 0.0
    ///   - "tied-a" : 1 activation at now → score ≈ 1.0 → 1 env below → rank 1/3
    ///   - "tied-b" : 1 activation at now → score ≈ 1.0 → 1 env below → rank 1/3
    ///
    /// "tied-a" and "tied-b" share the same score, so neither is strictly below
    /// the other. Both count only "zero" as strictly lower → rank 1/3 each.
    #[test]
    fn test_activation_percentile_scores_ties_get_same_rank() {
        let now: i64 = 1_700_000_000;

        let mut all_stats: HashMap<String, EnvStats> = HashMap::new();
        all_stats.insert("zero".to_string(), EnvStats::default());
        all_stats.insert(
            "tied-a".to_string(),
            EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![(now, 1.0)],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        all_stats.insert(
            "tied-b".to_string(),
            EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![(now, 1.0)],
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        let result = activation_percentile_scores(&all_stats, now);

        assert_eq!(result.len(), 3);
        assert!(
            result["zero"].abs() < 1e-10,
            "zero must have percentile 0/3 = 0.0, got {}",
            result["zero"]
        );
        assert!(
            (result["tied-a"] - 1.0 / 3.0).abs() < 1e-10,
            "tied-a must have percentile 1/3, got {}",
            result["tied-a"]
        );
        assert!(
            (result["tied-b"] - 1.0 / 3.0).abs() < 1e-10,
            "tied-b must have percentile 1/3 (same as tied-a), got {}",
            result["tied-b"]
        );
        assert!(
            (result["tied-a"] - result["tied-b"]).abs() < 1e-10,
            "tied envs must have identical percentile ranks"
        );
    }

    // ── launcher_score / slot_scores wrapper tests ─────────────────────────

    /// `launcher_score` must exist as a public function with the same signature as
    /// `activation_percentile_scores` and must return an empty map for empty input.
    #[test]
    fn test_launcher_score_empty_input_returns_empty() {
        let all_stats: HashMap<String, EnvStats> = HashMap::new();
        let now: i64 = 1_700_000_000;
        let result = launcher_score(&all_stats, now);
        assert!(
            result.is_empty(),
            "launcher_score with empty input must return an empty map, got {result:?}"
        );
    }

    /// `launcher_score` preserves strict ordering: a higher-frecency env must get a
    /// higher score than a lower-frecency env.
    #[test]
    fn test_launcher_score_ordering_high_beats_low() {
        let now: i64 = 1_700_000_000;
        let mut all_stats: HashMap<String, EnvStats> = HashMap::new();
        all_stats.insert("never-used".to_string(), EnvStats::default());
        all_stats.insert(
            "recently-used".to_string(),
            EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![(now, 1.0)],
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        let result = launcher_score(&all_stats, now);

        assert!(
            result["recently-used"] > result["never-used"],
            "recently-used must have a higher launcher_score than never-used; \
             recently-used={}, never-used={}",
            result["recently-used"],
            result["never-used"]
        );
    }

    /// `slot_scores` must exist as a public function with the same signature as
    /// `activation_percentile_scores` and must return an empty map for empty input.
    #[test]
    fn test_slot_scores_empty_input_returns_empty() {
        let all_stats: HashMap<String, EnvStats> = HashMap::new();
        let now: i64 = 1_700_000_000;
        let result = slot_scores(&all_stats, now);
        assert!(
            result.is_empty(),
            "slot_scores with empty input must return an empty map, got {result:?}"
        );
    }

    /// `slot_scores` preserves strict ordering: a higher-frecency env must get a
    /// higher score than a lower-frecency env.
    #[test]
    fn test_slot_scores_ordering_high_beats_low() {
        let now: i64 = 1_700_000_000;
        let mut all_stats: HashMap<String, EnvStats> = HashMap::new();
        all_stats.insert("never-used".to_string(), EnvStats::default());
        all_stats.insert(
            "recently-used".to_string(),
            EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![(now, 1.0)],
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        let result = slot_scores(&all_stats, now);

        assert!(
            result["recently-used"] > result["never-used"],
            "recently-used must have a higher slot_score than never-used; \
             recently-used={}, never-used={}",
            result["recently-used"],
            result["never-used"]
        );
    }

    // ── slot_scores / launcher_score blended-signal tests ─────────────────

    /// `slot_scores` must weight the switch percentile 4× more heavily than the
    /// activation percentile (0.8 vs 0.2). In a 2-env map where "switch-only"
    /// has recent switches but zero activations, and "activation-only" has recent
    /// activations but zero switches, "switch-only" must win slot_scores.
    #[test]
    fn test_slot_scores_weights_switch_4x_more_than_activation() {
        let now: i64 = 1_700_000_000;
        let mut all_stats: HashMap<String, EnvStats> = HashMap::new();

        // "activation-only": high activation percentile, zero switch events
        all_stats.insert(
            "activation-only".to_string(),
            EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![(now, 1.0)],
                    switch_buffer: vec![],
                },
                ..Default::default()
            },
        );
        // "switch-only": zero activation events, high switch percentile
        all_stats.insert(
            "switch-only".to_string(),
            EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![],
                    switch_buffer: vec![(now, 1.0)],
                },
                ..Default::default()
            },
        );

        let result = slot_scores(&all_stats, now);

        assert!(
            result["switch-only"] > result["activation-only"],
            "switch-only must outscore activation-only in slot_scores \
             (switch weight=0.8 > activation weight=0.2); \
             switch-only={}, activation-only={}",
            result["switch-only"],
            result["activation-only"]
        );
    }

    /// `launcher_score` must weight the activation percentile 4× more heavily than the
    /// switch percentile (0.8 vs 0.2). In a 2-env map where "activation-only" has
    /// recent activations but zero switches, and "switch-only" has recent switches but
    /// zero activations, "activation-only" must win launcher_score.
    #[test]
    fn test_launcher_score_weights_activation_4x_more_than_switch() {
        let now: i64 = 1_700_000_000;
        let mut all_stats: HashMap<String, EnvStats> = HashMap::new();

        // "activation-only": high activation percentile, zero switch events
        all_stats.insert(
            "activation-only".to_string(),
            EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![(now, 1.0)],
                    switch_buffer: vec![],
                },
                ..Default::default()
            },
        );
        // "switch-only": zero activation events, high switch percentile
        all_stats.insert(
            "switch-only".to_string(),
            EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![],
                    switch_buffer: vec![(now, 1.0)],
                },
                ..Default::default()
            },
        );

        let result = launcher_score(&all_stats, now);

        assert!(
            result["activation-only"] > result["switch-only"],
            "activation-only must outscore switch-only in launcher_score \
             (activation weight=0.8 > switch weight=0.2); \
             activation-only={}, switch-only={}",
            result["activation-only"],
            result["switch-only"]
        );
    }

    /// An env with many activations but no switch events must score higher under
    /// `launcher_score` than under `slot_scores` — because launcher_score weights
    /// activation at 0.8 while slot_scores weights it at only 0.2.
    #[test]
    fn test_activation_only_env_scores_higher_in_launcher_than_slot() {
        let now: i64 = 1_700_000_000;
        let mut all_stats: HashMap<String, EnvStats> = HashMap::new();

        all_stats.insert(
            "activation-only".to_string(),
            EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![(now, 1.0)],
                    switch_buffer: vec![],
                },
                ..Default::default()
            },
        );
        all_stats.insert(
            "switch-only".to_string(),
            EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![],
                    switch_buffer: vec![(now, 1.0)],
                },
                ..Default::default()
            },
        );

        let slot = slot_scores(&all_stats, now);
        let launcher = launcher_score(&all_stats, now);

        assert!(
            launcher["activation-only"] > slot["activation-only"],
            "activation-only must score higher under launcher_score than slot_scores; \
             launcher={}, slot={}",
            launcher["activation-only"],
            slot["activation-only"]
        );
    }

    /// An env with many switch events but no activations must score higher under
    /// `slot_scores` than under `launcher_score` — because slot_scores weights
    /// switch at 0.8 while launcher_score weights it at only 0.2.
    #[test]
    fn test_switch_only_env_scores_higher_in_slot_than_launcher() {
        let now: i64 = 1_700_000_000;
        let mut all_stats: HashMap<String, EnvStats> = HashMap::new();

        all_stats.insert(
            "activation-only".to_string(),
            EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![(now, 1.0)],
                    switch_buffer: vec![],
                },
                ..Default::default()
            },
        );
        all_stats.insert(
            "switch-only".to_string(),
            EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![],
                    switch_buffer: vec![(now, 1.0)],
                },
                ..Default::default()
            },
        );

        let slot = slot_scores(&all_stats, now);
        let launcher = launcher_score(&all_stats, now);

        assert!(
            slot["switch-only"] > launcher["switch-only"],
            "switch-only must score higher under slot_scores than launcher_score; \
             slot={}, launcher={}",
            slot["switch-only"],
            launcher["switch-only"]
        );
    }

    // ── switch_buffer / record_switch_per_env tests ────────────────────────

    /// `UserIntentSignals` must have a `switch_buffer` field of type `Vec<(i64, f64)>`.
    /// A freshly default-constructed value must have an empty buffer.
    #[test]
    fn test_switch_buffer_field_exists_and_defaults_empty() {
        let signals = UserIntentSignals::default();
        assert!(
            signals.switch_buffer.is_empty(),
            "switch_buffer must be empty by default"
        );
    }

    /// `switch_buffer` must be serialised under that exact JSON key name,
    /// flattened into the top-level `EnvStats` object.
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

    /// `record_switch_per_env` must push a `(timestamp, 1.0)` entry into `switch_buffer`.
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

    /// Each subsequent call to `record_switch_per_env` must append another entry.
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

    /// The buffer must be kept in descending timestamp order.
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

    /// The buffer must be capped at N=25: when a 26th entry is recorded,
    /// the oldest entry is dropped so the buffer stays at length 25.
    #[test]
    fn test_switch_buffer_capped_at_twenty_five() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        // Pre-populate the buffer with 25 entries using a fixed old timestamp.
        let old_ts: i64 = 1_000_000;
        let mut meta = load_env_meta(&env_dir);
        for _ in 0..25 {
            meta.signals.switch_buffer.push((old_ts, 1.0));
        }
        save_env_meta(&env_dir, &meta).unwrap();

        // Record one more switch — should trim to 25.
        let new_ts: i64 = 1_700_000_000;
        record_switch_per_env(&env_dir, new_ts);

        let meta = load_env_meta(&env_dir);
        assert_eq!(
            meta.signals.switch_buffer.len(),
            25,
            "switch_buffer must stay at capacity 25 after a 26th entry"
        );

        // The newest entry must be present at switch_buffer[0] after descending sort.
        let newest_ts = meta.signals.switch_buffer[0].0;
        assert_eq!(
            newest_ts, new_ts,
            "newest entry should be present after cap; got ts={newest_ts}"
        );
        // The oldest surviving entries must still carry the old timestamp.
        assert_eq!(
            meta.signals.switch_buffer.last().unwrap().0,
            old_ts,
            "some pre-populated old entries must still be in the buffer after cap"
        );
    }

    /// `record_switch_per_env` must not affect `activation_buffer`.
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

    /// Old on-disk JSON without `switch_buffer` must deserialize to an empty switch_buffer.
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

    /// `record_switch_per_env` must be a no-op when the environment directory does not exist.
    /// It must NOT create the directory or write any file — switch events for unknown
    /// environments are silently discarded.
    #[test]
    fn test_record_switch_per_env_does_nothing_for_nonexistent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("nonexistent-env");
        // Precondition: the directory must not exist before the call.
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

    // ── synthetic activation on switch tests ──────────────────────────────

    /// When both `activation_buffer` and `switch_buffer` are empty (no prior signals),
    /// a call to `record_switch_per_env` must inject a synthetic activation entry
    /// `(timestamp, 0.4)` into `activation_buffer`, because inactivity is unbounded
    /// (trivially exceeds the 8-hour threshold).
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

    /// When the env's last activation is more than 8 hours before `timestamp`,
    /// a call to `record_switch_per_env` must inject a synthetic activation entry
    /// `(timestamp, 0.4)` into `activation_buffer`.
    #[test]
    fn test_record_switch_per_env_adds_synthetic_activation_when_last_activation_over_8h_ago() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        let ts: i64 = 1_700_000_000;
        let eight_hours_plus_one: i64 = 8 * 3600 + 1;
        let old_activation_ts = ts - eight_hours_plus_one;

        // Pre-populate activation_buffer with one old activation (>8h before ts).
        let mut meta = load_env_meta(&env_dir);
        meta.signals
            .activation_buffer
            .push((old_activation_ts, 1.0));
        save_env_meta(&env_dir, &meta).unwrap();

        record_switch_per_env(&env_dir, ts);

        let meta = load_env_meta(&env_dir);
        // activation_buffer must now have 2 entries: the old one + the synthetic one.
        assert_eq!(
            meta.signals.activation_buffer.len(),
            2,
            "activation_buffer must gain a synthetic entry when last activation was >8h ago"
        );
        // The newest entry (at index 0 after descending sort) must be the synthetic one.
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

    /// When the env's last switch is more than 8 hours before `timestamp` and
    /// `activation_buffer` is empty, `record_switch_per_env` must inject a synthetic
    /// activation `(timestamp, 0.4)` because the switch buffer's most-recent entry
    /// determines inactivity.
    #[test]
    fn test_record_switch_per_env_adds_synthetic_activation_when_last_switch_over_8h_ago() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        let ts: i64 = 1_700_000_000;
        let eight_hours_plus_one: i64 = 8 * 3600 + 1;
        let old_switch_ts = ts - eight_hours_plus_one;

        // Pre-populate only switch_buffer with one old entry (>8h before ts);
        // activation_buffer remains empty.
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

    /// When the env's most recent signal (activation or switch) is within 8 hours of
    /// `timestamp`, `record_switch_per_env` must NOT inject a synthetic activation entry.
    #[test]
    fn test_record_switch_per_env_no_synthetic_activation_when_last_signal_within_8h() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        let ts: i64 = 1_700_000_000;
        let one_hour: i64 = 3600;
        let recent_activation_ts = ts - one_hour; // 1h before switch → within 8h

        // Pre-populate activation_buffer with a recent activation (within 8h).
        let mut meta = load_env_meta(&env_dir);
        meta.signals
            .activation_buffer
            .push((recent_activation_ts, 1.0));
        save_env_meta(&env_dir, &meta).unwrap();

        record_switch_per_env(&env_dir, ts);

        let meta = load_env_meta(&env_dir);
        // activation_buffer must still have exactly 1 entry — the original; no synthetic added.
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

    // ── synthetic-activation boundary / mixed-buffer tests ────────────────

    /// T1a: when the gap is exactly 8 hours (28800s), the threshold is NOT crossed
    /// (`>` is strict), so no synthetic activation must be injected.
    #[test]
    fn test_record_switch_per_env_no_synthetic_at_exact_8h_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        let ts: i64 = 1_700_000_000;
        const EIGHT_HOURS: i64 = 28800;
        let boundary_activation_ts = ts - EIGHT_HOURS; // gap == 28800, condition is false

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

    /// T1b: when the gap is exactly 28801s (one second past 8 hours), the threshold IS crossed
    /// (`timestamp - last > 28800` is true), so a synthetic activation must be injected.
    #[test]
    fn test_record_switch_per_env_synthetic_injected_at_28801s_gap() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        let ts: i64 = 1_700_000_000;
        let just_over_ts = ts - 28801; // gap == 28801, condition is true

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

    /// T2a: `activation_buffer` has an old entry (>8h) but `switch_buffer` has a recent
    /// entry (<8h). The max of both is recent, so no synthetic activation must be injected.
    /// This verifies that `last_switch_ts` participates in the max computation.
    #[test]
    fn test_record_switch_per_env_no_synthetic_when_switch_buffer_is_recent_activation_is_old() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        let ts: i64 = 1_700_000_000;
        let old_activation_ts = ts - (8 * 3600 + 1); // >8h ago
        let recent_switch_ts = ts - 3600; // 1h ago (within 8h)

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

    /// T2b: `switch_buffer` has an old entry (>8h) but `activation_buffer` has a recent
    /// entry (<8h). The max of both is recent, so no synthetic activation must be injected.
    /// This verifies that `last_activation_ts` participates in the max computation.
    #[test]
    fn test_record_switch_per_env_no_synthetic_when_activation_buffer_is_recent_switch_is_old() {
        let dir = tempfile::tempdir().unwrap();
        let env_dir = dir.path().join("my-project");
        fs::create_dir(&env_dir).unwrap();

        let ts: i64 = 1_700_000_000;
        let old_switch_ts = ts - (8 * 3600 + 1); // >8h ago
        let recent_activation_ts = ts - 3600; // 1h ago (within 8h)

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

    // ── switch_score tests ─────────────────────────────────────────────────

    /// `switch_score` must return 0.0 for an empty buffer.
    #[test]
    fn test_switch_score_empty_buffer_is_zero() {
        let now: i64 = 1_700_000_000;
        let score = switch_score(&[], now);
        assert!(
            score.abs() < 1e-10,
            "switch_score for an empty buffer must be 0.0, got {score}"
        );
    }

    /// A single entry with weight 1.0 recorded at `now` (elapsed = 0) must yield
    /// exactly 1.0: each decay factor is e^0 = 1.0, so fast_sum = 1.0, slow_sum = 1.0,
    /// and 0.5 * 1.0 + 0.5 * 1.0 = 1.0.
    #[test]
    fn test_switch_score_single_entry_at_now_is_one() {
        let now: i64 = 1_700_000_000;
        let score = switch_score(&[(now, 1.0)], now);
        assert!(
            (score - 1.0).abs() < 1e-6,
            "switch_score for a single entry at now must be ≈ 1.0, got {score}"
        );
    }

    /// A single entry recorded exactly 6 hours ago:
    /// - fast component (6h half-life): e^(-ln(2)/6h * 6h) = 0.5 → fast_sum = 0.5
    /// - slow component (48h half-life): e^(-ln(2)/48h * 6h) = 2^(-1/8) ≈ 0.9170
    /// Final score = 0.5 * 0.5 + 0.5 * 2^(-1/8)
    #[test]
    fn test_switch_score_single_entry_6h_ago_blends_correctly() {
        let now: i64 = 1_700_000_000;
        let six_hours: i64 = 6 * 3600;
        let buffer = [(now - six_hours, 1.0)];

        let score = switch_score(&buffer, now);

        let fast_sum = 0.5_f64; // e^(-ln2) = 0.5 (exactly one fast half-life)
        let slow_lambda = std::f64::consts::LN_2 / (48.0 * 3600.0);
        let slow_sum = (-slow_lambda * six_hours as f64).exp();
        let expected = 0.5 * fast_sum + 0.5 * slow_sum;
        assert!(
            (score - expected).abs() < 1e-6,
            "switch_score for single entry 6h ago must be ≈ {expected:.6}, got {score:.6}"
        );
    }

    /// A single entry recorded exactly 48 hours ago:
    /// - slow component (48h half-life): decays to 0.5
    /// - fast component (6h half-life): decays to 2^(-8) = 1/256
    /// Final score = 0.5 * (1/256) + 0.5 * 0.5
    #[test]
    fn test_switch_score_single_entry_48h_ago_blends_correctly() {
        let now: i64 = 1_700_000_000;
        let forty_eight_hours: i64 = 48 * 3600;
        let buffer = [(now - forty_eight_hours, 1.0)];

        let score = switch_score(&buffer, now);

        let fast_lambda = std::f64::consts::LN_2 / (6.0 * 3600.0);
        let fast_sum = (-fast_lambda * forty_eight_hours as f64).exp(); // 2^(-8) = 1/256
        let slow_sum = 0.5_f64; // exactly one 48h half-life
        let expected = 0.5 * fast_sum + 0.5 * slow_sum;
        assert!(
            (score - expected).abs() < 1e-6,
            "switch_score for single entry 48h ago must be ≈ {expected:.8}, got {score:.8}"
        );
    }

    /// T1 — Weight != 1.0: a single entry at `now` with weight=2.0 must return
    /// exactly double the score of the same entry with weight=1.0.
    /// If `weight *` were dropped the two scores would be identical.
    #[test]
    fn test_switch_score_weight_doubles_score() {
        let now: i64 = 1_700_000_000;
        let score_w1 = switch_score(&[(now, 1.0)], now);
        let score_w2 = switch_score(&[(now, 2.0)], now);
        assert!(
            (score_w2 - 2.0 * score_w1).abs() < 1e-10,
            "weight=2.0 must yield exactly twice weight=1.0: got {score_w1} vs {score_w2}"
        );
    }

    /// T2 — Multi-entry summation: two entries both at `now` with weight=1.0 each
    /// must return 2.0 (elapsed=0 → each contributes its weight directly).
    /// A regression that took only `.first()` or `.last()` would return 1.0.
    #[test]
    fn test_switch_score_two_entries_at_now_sum_to_two() {
        let now: i64 = 1_700_000_000;
        let buffer = [(now, 1.0), (now, 1.0)];
        let score = switch_score(&buffer, now);
        assert!(
            (score - 2.0).abs() < 1e-10,
            "two entries at now with weight=1.0 each must sum to 2.0, got {score}"
        );
    }

    /// T3 — Future-timestamp clamping: an entry one hour in the future must yield
    /// the same score as an entry at `now` (elapsed clamped to 0, not negative).
    /// A regression removing `.max(0)` would inflate the score via exp(+λ*3600).
    #[test]
    fn test_switch_score_future_timestamp_clamped_to_now() {
        let now: i64 = 1_700_000_000;
        let score_now = switch_score(&[(now, 1.0)], now);
        let score_future = switch_score(&[(now + 3600, 1.0)], now);
        assert!(
            (score_future - score_now).abs() < 1e-10,
            "future timestamp must clamp to elapsed=0; expected ≈{score_now}, got {score_future}"
        );
    }
}
