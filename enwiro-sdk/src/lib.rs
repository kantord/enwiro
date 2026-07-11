//! Shared SDK for enwiro plugin authors (cookbooks, adapters, bridges).
//!
//! Owns the cross-component contracts and infrastructure that core and
//! plugins both depend on: logging setup, the `gear` schema and on-disk
//! conventions, and any future plugin-protocol types.

pub mod adapter;
pub mod bridge;
pub mod browser;
pub mod capability;
#[cfg(feature = "cli")]
pub mod cli;
pub mod client;
pub mod config;
pub mod cookbook;
pub mod dropin;
pub mod external_paths;
pub mod fs;
pub mod garnish;
pub mod gear;
#[cfg(feature = "git")]
pub mod git;
pub mod listen;
pub mod logging;
pub mod metadata;
pub mod plugin;
pub mod process;
pub mod recipe_pattern;
pub mod rpc;
pub mod status;
pub mod url_rule;

#[cfg(any(test, feature = "test-helpers"))]
pub mod test_helpers;

pub use cookbook::{CookbookMetadata, CookbookPayload, PatternRecipe, Recipe, RecipeItem};
pub use logging::init_logging;
