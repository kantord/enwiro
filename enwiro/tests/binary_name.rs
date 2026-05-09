use std::process::{Command, Stdio};

/// The `enwiro` crate must produce a binary named `enw` (issue #310 renames the
/// user-facing CLI from `enwiro` to `enw` while keeping the crate name `enwiro`).
///
/// `env!("CARGO_BIN_EXE_enw")` resolves at compile time to the path of the
/// integration test target's sibling bin named `enw`. If no such bin target
/// exists in `enwiro/Cargo.toml`, this expression fails to compile — that is
/// the failing-test signal until a `[[bin]] name = "enw"` entry is added.
#[test]
fn enw_binary_runs_and_help_refers_to_enw() {
    let enw = env!("CARGO_BIN_EXE_enw");

    let output = Command::new(enw)
        .arg("--help")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to run `enw --help`");

    assert!(
        output.status.success(),
        "`enw --help` should exit successfully, got status {:?}, stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Clap's auto-generated help opens with a `Usage: <bin-name> ...` line.
    // The first non-empty line that starts with "Usage:" must reference `enw`,
    // not `enwiro`. We pin both halves: the positive (mentions `enw`) and the
    // negative (does not mention `enwiro` as the bin name) so a regression
    // that keeps the old name cannot pass.
    let usage_line = stdout
        .lines()
        .map(str::trim_start)
        .find(|line| line.starts_with("Usage:"))
        .unwrap_or_else(|| panic!("`enw --help` stdout has no `Usage:` line, got: {stdout:?}"));

    assert!(
        usage_line.contains("enw"),
        "Usage line must reference the `enw` binary, got: {usage_line:?}"
    );
    assert!(
        !usage_line.contains("enwiro"),
        "Usage line must not still reference `enwiro` as the bin name, got: {usage_line:?}"
    );
}
