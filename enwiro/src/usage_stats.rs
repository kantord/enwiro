use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{fs, io};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserIntentSignals {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub activation_buffer: Vec<(i64, f64)>,
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
        .sort_by(|a, b| b.0.cmp(&a.0));
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
    let mut meta = load_env_meta(env_dir);
    meta.signals.activation_buffer.push((now_timestamp(), 1.0));
    meta.signals.activation_buffer.sort_by(|a, b| b.0.cmp(&a.0));
    meta.signals.activation_buffer.truncate(10);
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
}
