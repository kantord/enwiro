use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::{fs, io};

pub use enwiro_daemon::meta::{
    EnvStats, load_env_meta, now_timestamp, record_activation_per_env,
    record_cook_metadata_per_env, record_prep_per_env,
};
pub use enwiro_daemon::scoring::{launcher_score, slot_scores};

/// Per-environment usage statistics.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UsageStats {
    pub envs: HashMap<String, EnvStats>,
}

pub fn stats_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("enwiro").join("usage-stats.json"))
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

fn save_stats(path: &Path, stats: &UsageStats) -> io::Result<()> {
    enwiro_sdk::fs::atomic_write(path, serde_json::to_string(stats)?.as_bytes())
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

#[cfg(test)]
mod tests {
    use super::*;
    use enwiro_daemon::meta::UserIntentSignals;
    use enwiro_daemon::scoring::{activation_percentile_scores, frecency_score, switch_score};

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
                    prep_buffer: vec![],
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
                    prep_buffer: vec![],
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

    /// `slot_scores` must weight the `prep_buffer` percentile equivalently to the
    /// activation percentile (both 0.2 in the blend). In a 2-env map where
    /// "prep-only" has a recent prep event and "activation-only" has a recent
    /// activation, both must receive the same slot_score.
    #[test]
    fn test_slot_scores_weights_prep_same_as_activation() {
        let now: i64 = 1_700_000_000;
        let mut all_stats: HashMap<String, EnvStats> = HashMap::new();

        all_stats.insert(
            "activation-only".to_string(),
            EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![(now, 1.0)],
                    switch_buffer: vec![],
                    prep_buffer: vec![],
                },
                ..Default::default()
            },
        );
        all_stats.insert(
            "prep-only".to_string(),
            EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![],
                    switch_buffer: vec![],
                    prep_buffer: vec![(now, 1.0)],
                },
                ..Default::default()
            },
        );

        let result = slot_scores(&all_stats, now);

        assert!(
            (result["activation-only"] - result["prep-only"]).abs() < 1e-10,
            "prep_buffer and activation_buffer must contribute equally to slot_scores; \
             activation-only={}, prep-only={}",
            result["activation-only"],
            result["prep-only"]
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
                    prep_buffer: vec![],
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
                    prep_buffer: vec![],
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

    /// `launcher_score` must weight the `prep_buffer` percentile equivalently to the
    /// activation percentile (both 0.8 in the blend). Prep is treated as a reliable
    /// "user is interested in this env" signal for launcher ordering.
    #[test]
    fn test_launcher_score_weights_prep_same_as_activation() {
        let now: i64 = 1_700_000_000;
        let mut all_stats: HashMap<String, EnvStats> = HashMap::new();

        all_stats.insert(
            "activation-only".to_string(),
            EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![(now, 1.0)],
                    switch_buffer: vec![],
                    prep_buffer: vec![],
                },
                ..Default::default()
            },
        );
        all_stats.insert(
            "prep-only".to_string(),
            EnvStats {
                signals: UserIntentSignals {
                    activation_buffer: vec![],
                    switch_buffer: vec![],
                    prep_buffer: vec![(now, 1.0)],
                },
                ..Default::default()
            },
        );

        let result = launcher_score(&all_stats, now);

        assert!(
            (result["activation-only"] - result["prep-only"]).abs() < 1e-10,
            "prep_buffer and activation_buffer must contribute equally to launcher_score; \
             activation-only={}, prep-only={}",
            result["activation-only"],
            result["prep-only"]
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
                    prep_buffer: vec![],
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
                    prep_buffer: vec![],
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
                    prep_buffer: vec![],
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
                    prep_buffer: vec![],
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
