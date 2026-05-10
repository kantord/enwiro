//! Shared SDK for enwiro plugin authors (cookbooks, adapters, bridges).
//!
//! Owns the cross-component contracts and infrastructure that core and
//! plugins both depend on: logging setup, the `gear` schema and on-disk
//! conventions, and any future plugin-protocol types.

pub mod adapter;
pub mod gear;
pub mod logging;

pub use logging::init_logging;
