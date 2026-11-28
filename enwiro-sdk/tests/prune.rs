//! `prune` through a real cookbook subprocess, using the pre-built
//! `test-fake-plugin` binary (see `support/fake_plugin.rs`).

mod common;

use std::path::Path;

use common::{cookbook_plugin, install_fixture};
use enwiro_sdk::client::{CookbookClient, CookbookTrait, RpcCookbookClient};
use enwiro_sdk::plugin::{Plugin, PluginKind, PluginName};
use serde_json::{Value, json};

const DECLARES_PRUNE: &str = r#"{"capabilities":[{"name":"prune"}]}"#;

fn cookbook(metadata: Value) -> Value {
    json!({
        "metadata": metadata,
        "prune":    { "stderr": "Error: broken\n\nCaused by:\n    disk\n", "exit": 1 },
    })
}

/// A name that cannot be installed: if the RPC client ever calls out for one
/// of these fixtures, no daemon can run a real cookbook for it.
fn absent_plugin(bin: &Path) -> Plugin {
    Plugin {
        name: PluginName::new("enwiro-test-absent").unwrap(),
        kind: PluginKind::Cookbook,
        executable: bin.to_string_lossy().into_owned(),
    }
}

#[test]
fn failed_prune_reports_the_cookbook_stderr_on_one_line() {
    let (_dir, bin) = install_fixture(
        "fake-cookbook",
        &cookbook(json!({ "stdout": DECLARES_PRUNE })),
    );
    let client = CookbookClient::new_user_level_only(cookbook_plugin(&bin));

    let message = format!("{:#}", client.prune("some-recipe").unwrap_err());

    assert!(
        message.ends_with("Error: broken Caused by: disk"),
        "{message}"
    );
    assert!(!message.contains('\n'), "{message}");
}

#[test]
fn prune_is_not_run_on_a_cookbook_that_does_not_declare_it() {
    let (_dir, bin) = install_fixture("fake-cookbook", &cookbook(json!({ "stdout": "{}" })));

    let direct = CookbookClient::new_user_level_only(cookbook_plugin(&bin));
    let rpc = RpcCookbookClient::new(absent_plugin(&bin));

    assert_eq!(direct.prune("some-recipe").unwrap(), None);
    assert_eq!(rpc.prune("some-recipe").unwrap(), None);
}

#[test]
fn prune_fails_when_metadata_fails_because_support_is_unknown() {
    let (_dir, bin) = install_fixture("fake-cookbook", &cookbook(json!({ "exit": 1 })));

    let direct = CookbookClient::new_user_level_only(cookbook_plugin(&bin));
    let rpc = RpcCookbookClient::new(absent_plugin(&bin));

    for result in [direct.prune("some-recipe"), rpc.prune("some-recipe")] {
        let message = format!("{:#}", result.unwrap_err());
        assert!(message.contains("failed to answer 'metadata'"), "{message}");
    }
}
