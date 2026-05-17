//! Shared SDK for enwiro plugin authors (cookbooks, adapters, bridges).
//!
//! Owns the cross-component contracts and infrastructure that core and
//! plugins both depend on: logging setup, the `gear` schema and on-disk
//! conventions, and any future plugin-protocol types.

pub mod adapter;
pub mod client;
pub mod cookbook;
pub mod fs;
pub mod garnish;
pub mod gear;
pub mod logging;
pub mod plugin;
pub mod process;

#[cfg(any(test, feature = "test-helpers"))]
pub mod test_helpers;

pub use cookbook::{CookbookMetadata, Recipe};
pub use logging::init_logging;
