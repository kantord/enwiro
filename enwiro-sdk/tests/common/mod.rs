//! Fixture setup shared by the integration tests that run plugin binaries.

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use enwiro_sdk::plugin::{Plugin, PluginKind, PluginName};
use serde_json::Value;
use tempfile::TempDir;

const FIXTURE: &str = env!("CARGO_BIN_EXE_test-fake-plugin");

pub fn install_fixture(name: &str, config: &Value) -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = dir.path().join(name);
    symlink(FIXTURE, &bin).expect("symlink fixture");
    std::fs::write(
        bin.with_extension("config"),
        serde_json::to_vec(config).expect("serialize fixture config"),
    )
    .expect("write fixture config");
    (dir, bin)
}

pub fn cookbook_plugin(bin: &Path) -> Plugin {
    Plugin {
        name: PluginName::new("fake").unwrap(),
        kind: PluginKind::Cookbook,
        executable: bin.to_string_lossy().into_owned(),
    }
}
