//! Shared SDK for enwiro plugin authors (cookbooks, adapters, bridges).
//!
//! Owns the cross-component contracts and infrastructure that core and
//! plugins both depend on: logging setup, the `gear` schema and on-disk
//! conventions, and any future plugin-protocol types.

pub mod adapter;
pub mod bridge;
pub mod capability;
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
pub mod pattern;
pub mod plugin;
pub mod process;
pub mod rpc;
pub mod status;

#[cfg(any(test, feature = "test-helpers"))]
pub mod test_helpers;

pub use cookbook::{CookbookMetadata, CookbookPayload, PatternRecipe, Recipe, RecipeItem};
pub use logging::init_logging;
