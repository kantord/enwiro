//! `prune` through a real cookbook subprocess, using the pre-built
//! `test-fake-plugin` binary (see `support/fake_plugin.rs`).

mod common;

use common::{cookbook_plugin, install_fixture};
use enwiro_sdk::client::{CookbookClient, CookbookTrait};
use serde_json::json;

#[test]
fn failed_prune_reports_the_cookbook_stderr_on_one_line() {
    let (_dir, bin) = install_fixture(
        "fake-cookbook",
        &json!({
            "metadata": { "stdout": "{}", "exit": 0 },
            "prune":    { "stderr": "Error: broken\n\nCaused by:\n    disk\n", "exit": 1 },
        }),
    );
    let client = CookbookClient::new_user_level_only(cookbook_plugin(&bin));

    let message = format!("{:#}", client.prune("some-recipe").unwrap_err());

    assert!(
        message.ends_with("Error: broken Caused by: disk"),
        "{message}"
    );
    assert!(!message.contains('\n'), "{message}");
}
