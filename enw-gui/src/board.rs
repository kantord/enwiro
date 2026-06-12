//! Builds the kanban board (4 status columns) from env dirs + per-env meta +
//! the recipe cache — the same read path the CLI `enw kanban` uses, minus the
//! daemon RPC. `classify` lives here (BE) so the column taxonomy has one source
//! of truth shared with the CLI; the frontend just renders.

use std::fs;
use std::path::Path;

use enwiro_daemon::meta::{CookedPhase, EnvStats, Status, load_env_meta, now_timestamp};
use enwiro_sdk::client::CachedRecipe;
use serde::Serialize;
use utoipa::ToSchema;

/// One environment (or not-yet-materialised recipe) shown as a board card.
#[derive(Serialize, ToSchema)]
pub struct Card {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// True for recipes that have no environment directory yet.
    pub is_recipe: bool,
    /// Frecency-derived relevance (the `launcher` percentile `enw ls` computes
    /// from usage signals; same ordering semantics as the rofi picker).
    /// Columns sort by it descending; the frontend reuses it to keep a moved
    /// card's position consistent. Recipes have no usage signals and get 0.
    pub score: f64,
}

#[derive(Serialize, ToSchema)]
pub struct BoardColumn {
    /// Stable key — also the `status` accepted by `POST /api/env/mark`.
    pub key: String,
    /// Header label.
    pub title: String,
    pub cards: Vec<Card>,
}

#[derive(Serialize, ToSchema)]
pub struct Board {
    pub columns: Vec<BoardColumn>,
}

/// Column order + (mark key, display title). Index matches `classify`.
const COLUMNS: [(&str, &str); 4] = [
    ("ready", "Ready"),
    ("active", "Active"),
    ("waiting", "Waiting"),
    ("done", "Done"),
];

/// Map an env status to a column index, or `None` to hide it (Evergreen).
/// Mirrors `enwiro/src/commands/kanban.rs::classify`.
fn classify(status: Option<&Status>) -> Option<usize> {
    match status {
        None | Some(Status::Uncooked) | Some(Status::Cooked { phase: None, .. }) => Some(0),
        Some(Status::Cooked {
            phase: Some(CookedPhase::Active),
            ..
        }) => Some(1),
        Some(Status::Cooked {
            phase: Some(CookedPhase::Waiting),
            ..
        }) => Some(2),
        Some(Status::Done { .. }) => Some(3),
        Some(Status::Evergreen) => None,
    }
}

fn list_env_names(workspaces_directory: &str) -> Vec<String> {
    let Ok(entries) = fs::read_dir(workspaces_directory) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect()
}

/// All daemon-cache entries: recipes plus env entries, the latter carrying the
/// frecency-derived `scores` that `enw ls` computed.
fn load_cache_entries() -> Vec<CachedRecipe> {
    let Ok(cache) = enwiro_daemon::DaemonCache::open() else {
        return Vec::new();
    };
    let Ok(Some(content)) = cache.read_recipes() else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str::<CachedRecipe>(l).ok())
        .collect()
}

/// Column order: frecency descending (rofi-consistent), envs before recipes on
/// equal score, then name as the stable tie-break.
fn board_order(a: &Card, b: &Card) -> std::cmp::Ordering {
    b.score
        .partial_cmp(&a.score)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then(a.is_recipe.cmp(&b.is_recipe))
        .then(a.name.cmp(&b.name))
}

/// Assemble the board: environments grouped by status, plus recipes that have
/// no environment yet (shown in "Ready"). Cards sort by frecency, computed
/// from usage signals with the same shared formula the launcher uses.
pub fn build_board(workspaces_directory: &str) -> Board {
    let mut columns: [Vec<Card>; 4] = Default::default();
    let env_names = list_env_names(workspaces_directory);
    let cache_entries = load_cache_entries();

    let metas: std::collections::HashMap<String, EnvStats> = env_names
        .iter()
        .map(|name| {
            (
                name.clone(),
                load_env_meta(&Path::new(workspaces_directory).join(name)),
            )
        })
        .collect();
    let scores = enwiro_daemon::scoring::launcher_score(&metas, now_timestamp());

    for (name, meta) in &metas {
        if let Some(col) = classify(meta.status.as_ref()) {
            columns[col].push(Card {
                name: name.clone(),
                description: meta.description.clone(),
                is_recipe: false,
                score: scores.get(name).copied().unwrap_or(0.0),
            });
        }
    }

    for recipe in cache_entries {
        if !env_names.iter().any(|n| n == &recipe.name) {
            columns[0].push(Card {
                name: recipe.name,
                description: recipe.description,
                is_recipe: true,
                score: 0.0,
            });
        }
    }

    for col in &mut columns {
        col.sort_by(board_order);
    }

    let columns = columns
        .into_iter()
        .enumerate()
        .map(|(i, cards)| BoardColumn {
            key: COLUMNS[i].0.to_string(),
            title: COLUMNS[i].1.to_string(),
            cards,
        })
        .collect();

    Board { columns }
}
