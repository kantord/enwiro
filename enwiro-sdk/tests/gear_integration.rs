//! Replaces the previous in-source `cookbook_client_from_script` /
//! `fake_garnish` unit tests, which `fs::write`-and-`Command::spawn`'d a
//! fresh shell script on every test and raced under parallel cargo test
//! with ETXTBSY (issue #458). Here we symlink to the pre-built
//! `test-fake-plugin` binary (compiled by cargo before any test thread
//! starts) and drop a sibling `.config` describing what each subcommand
//! should print. No test thread ever opens an executable for writing.

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use enwiro_sdk::client::{CookbookClient, CookbookTrait};
use enwiro_sdk::cookbook::CookbookPayload;
use enwiro_sdk::garnish::{Garnish, GarnishClient};
use enwiro_sdk::plugin::{Plugin, PluginKind};
use serde_json::{Value, json};
use tempfile::TempDir;

const FIXTURE: &str = env!("CARGO_BIN_EXE_test-fake-plugin");

fn install_fixture(name: &str, config: &Value) -> (TempDir, PathBuf) {
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

fn cookbook_plugin(bin: &Path) -> Plugin {
    Plugin {
        name: enwiro_sdk::plugin::PluginName::new("fake").unwrap(),
        kind: PluginKind::Cookbook,
        executable: bin.to_string_lossy().into_owned(),
    }
}

fn garnish_plugin(name: &str, bin: &Path) -> Plugin {
    Plugin {
        name: enwiro_sdk::plugin::PluginName::new(name).unwrap(),
        kind: PluginKind::Garnish,
        executable: bin.to_string_lossy().into_owned(),
    }
}

#[test]
fn cookbook_gear_invokes_subcommand_and_parses_stdout() {
    let json = r#"{"version":1,"gear":{"x":{"description":"y","web":{"p":{"description":"z","url":"https://example.com"}}}}}"#;
    let (_dir, bin) = install_fixture(
        "fake-cookbook",
        &json!({
            "metadata": { "stdout": "{}", "exit": 0 },
            "gear":     { "stdout": json, "exit": 0 },
        }),
    );
    let client = CookbookClient::new_user_level_only(cookbook_plugin(&bin));

    let value = client
        .gear("some-recipe")
        .expect("gear returns Ok")
        .expect("gear returns Some");
    assert_eq!(value["version"], 1);
    assert_eq!(value["gear"]["x"]["description"], "y");
}

#[test]
fn cookbook_gear_returns_none_when_subcommand_exits_nonzero() {
    let (_dir, bin) = install_fixture(
        "fake-cookbook",
        &json!({
            "metadata": { "stdout": "{}", "exit": 0 },
            "gear":     { "stderr": "unsupported", "exit": 1 },
        }),
    );
    let client = CookbookClient::new_user_level_only(cookbook_plugin(&bin));
    assert!(client.gear("some-recipe").unwrap().is_none());
}

#[test]
fn cookbook_cook_pipes_payload_to_stdin() {
    let (_dir, bin) = install_fixture(
        "fake-cookbook",
        &json!({
            "metadata": { "stdout": "{}", "exit": 0 },
            "cook":     { "echo_stdin": true, "exit": 0 },
        }),
    );
    let client = CookbookClient::new_user_level_only(cookbook_plugin(&bin));

    let stdout = client.cook("anything").expect("cook returns stdout");
    let payload: CookbookPayload =
        serde_json::from_str(&stdout).expect("cookbook received a valid CookbookPayload on stdin");
    assert_eq!(payload.version, 1);
}

#[test]
fn cookbook_gear_returns_none_when_spawn_fails() {
    let plugin = Plugin {
        name: enwiro_sdk::plugin::PluginName::new("fake").unwrap(),
        kind: PluginKind::Cookbook,
        executable: "/nonexistent/enwiro-test-binary".into(),
    };
    let client = CookbookClient::new_user_level_only(plugin);

    assert!(client.gear("any").unwrap().is_none());
}

#[test]
fn garnish_name_uses_plugin_name() {
    let (_dir, bin) = install_fixture("enwiro-garnish-just", &json!({}));
    assert_eq!(
        GarnishClient::new(garnish_plugin("just", &bin)).name(),
        "just"
    );
}

#[test]
fn garnish_applies_to_true_when_binary_exits_zero() {
    let (_dir, bin) = install_fixture(
        "enwiro-garnish-just",
        &json!({ "applies-to": { "exit": 0 } }),
    );
    let client = GarnishClient::new(garnish_plugin("just", &bin));
    assert!(client.applies_to(Path::new("/tmp")));
}

#[test]
fn garnish_applies_to_false_when_binary_exits_nonzero() {
    let (_dir, bin) = install_fixture(
        "enwiro-garnish-just",
        &json!({ "applies-to": { "exit": 1 } }),
    );
    let client = GarnishClient::new(garnish_plugin("just", &bin));
    assert!(!client.applies_to(Path::new("/tmp")));
}

#[test]
fn garnish_gear_parses_stdout_as_gearfiledata() {
    let body = r#"{"version":1,"gear":{"just":{"description":"x"}}}"#;
    let (_dir, bin) = install_fixture(
        "enwiro-garnish-just",
        &json!({ "gear": { "stdout": body, "exit": 0 } }),
    );
    let client = GarnishClient::new(garnish_plugin("just", &bin));

    let out = client.gear(Path::new("/tmp")).unwrap().unwrap();
    assert_eq!(out.version, 1);
    assert_eq!(out.gear["just"].description, "x");
}

#[test]
fn garnish_gear_returns_none_for_empty_stdout() {
    let (_dir, bin) = install_fixture("enwiro-garnish-just", &json!({ "gear": { "exit": 0 } }));
    let client = GarnishClient::new(garnish_plugin("just", &bin));
    assert!(client.gear(Path::new("/tmp")).unwrap().is_none());
}

#[test]
fn garnish_gear_errors_on_nonzero_exit() {
    let (_dir, bin) = install_fixture(
        "enwiro-garnish-just",
        &json!({ "gear": { "stderr": "broken", "exit": 2 } }),
    );
    let client = GarnishClient::new(garnish_plugin("just", &bin));

    let err = client.gear(Path::new("/tmp")).unwrap_err();
    assert!(err.to_string().contains("exited with"));
}
