//! Frecency scoring over `UserIntentSignals` buffers - the project's single
//! relevance model. `enw ls` feeds these scores to the launcher (rofi) and the
//! adapter slot assignment; `enwiro-gui` sorts kanban columns with them. Moved
//! here from `enwiro::usage_stats` so every consumer shares one formula.

use std::collections::HashMap;

use crate::meta::EnvStats;

/// Compute exponential-decay frecency over a `(timestamp, weight)` buffer.
/// λ = ln(2) / (48h) gives a 48-hour half-life. Used by both
/// activation- and prep-signal scoring.
fn frecency_of_buffer(buffer: &[(i64, f64)], now: i64) -> f64 {
    let lambda = std::f64::consts::LN_2 / (48.0 * 3600.0);
    buffer
        .iter()
        .map(|&(ts, weight)| {
            let age = (now - ts).max(0) as f64;
            weight * (-lambda * age).exp()
        })
        .sum()
}

/// Compute exponential-decay score for an environment over its activation buffer.
/// Pass the current timestamp (seconds since epoch) for deterministic results.
pub fn frecency_score(stats: &EnvStats, now: i64) -> f64 {
    frecency_of_buffer(&stats.signals.activation_buffer, now)
}

/// Compute a two-component exponential-decay score from a workspace-switch buffer.
/// Fast component: 6h half-life. Slow component: 48h half-life.
/// `score = 0.5 * fast_sum + 0.5 * slow_sum`
/// Each entry `(timestamp, weight)` contributes `weight * exp(-λ * elapsed_seconds)`.
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
    percentiles(all_stats, |stats| frecency_score(stats, now))
}

/// Compute percentile ranks for all environments based on their `prep_buffer`
/// frecency. `prep_buffer` holds at most one event, so this captures "how
/// recently was this env prepped" relative to other envs.
fn prep_percentile_scores(all_stats: &HashMap<String, EnvStats>, now: i64) -> HashMap<String, f64> {
    percentiles(all_stats, |stats| {
        frecency_of_buffer(&stats.signals.prep_buffer, now)
    })
}

/// Compute percentile ranks for all environments based on their switch scores.
/// Mirrors [`activation_percentile_scores`] but uses `switch_score` instead of
/// `frecency_score`.
fn switch_percentile_scores(
    all_stats: &HashMap<String, EnvStats>,
    now: i64,
) -> HashMap<String, f64> {
    percentiles(all_stats, |stats| {
        switch_score(&stats.signals.switch_buffer, now)
    })
}

/// Percentile ranks over `all_stats` under `score_fn`: for each env,
/// (count of envs with strictly lower score) / total. Ties share a rank.
fn percentiles(
    all_stats: &HashMap<String, EnvStats>,
    score_fn: impl Fn(&EnvStats) -> f64,
) -> HashMap<String, f64> {
    let total = all_stats.len();
    if total == 0 {
        return HashMap::new();
    }
    let scores: HashMap<&str, f64> = all_stats
        .iter()
        .map(|(name, stats)| (name.as_str(), score_fn(stats)))
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
/// Blends activation, prep, and switch percentile signals:
/// 0.8 × activation + 0.8 × prep + 0.2 × switch. Prep events feed in
/// with the same weight as activations - pre-warming an env is a
/// reliable signal the user also wants to observe it.
pub fn launcher_score(all_stats: &HashMap<String, EnvStats>, now: i64) -> HashMap<String, f64> {
    blended_scores(all_stats, now, 0.8, 0.8, 0.2)
}

/// Score function for workspace slot assignment (`activate`).
/// Blends activation, prep, and switch percentile signals:
/// 0.2 × activation + 0.2 × prep + 0.8 × switch. Prep events feed in
/// with the same weight as activations.
pub fn slot_scores(all_stats: &HashMap<String, EnvStats>, now: i64) -> HashMap<String, f64> {
    blended_scores(all_stats, now, 0.2, 0.2, 0.8)
}

fn blended_scores(
    all_stats: &HashMap<String, EnvStats>,
    now: i64,
    activation_weight: f64,
    prep_weight: f64,
    switch_weight: f64,
) -> HashMap<String, f64> {
    let activation = activation_percentile_scores(all_stats, now);
    let switch = switch_percentile_scores(all_stats, now);
    let prep = prep_percentile_scores(all_stats, now);
    activation
        .into_iter()
        .map(|(name, act)| {
            let sw = switch.get(&name).copied().unwrap_or(0.0);
            let pr = prep.get(&name).copied().unwrap_or(0.0);
            (
                name,
                activation_weight * act + prep_weight * pr + switch_weight * sw,
            )
        })
        .collect()
}
