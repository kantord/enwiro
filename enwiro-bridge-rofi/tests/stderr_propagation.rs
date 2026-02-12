use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

#[test]
fn test_activate_propagates_child_stderr() {
    // Create a fake "enwiro" that writes a known message to stderr
    let dir = tempfile::tempdir().unwrap();
    let fake_enwiro = dir.path().join("enwiro");
    std::fs::write(
        &fake_enwiro,
        "#!/bin/sh\necho 'cook failed: no recipe' >&2\n",
    )
    .unwrap();
    std::fs::set_permissions(&fake_enwiro, std::fs::Permissions::from_mode(0o755)).unwrap();

    // Run the bridge binary with ROFI_RETV=1 (selection mode)
    // and PATH set so it finds our fake enwiro
    let bridge = env!("CARGO_BIN_EXE_enwiro-bridge-rofi");
    let output = Command::new(bridge)
        .arg("test-selection")
        .env("ROFI_RETV", "1")
        .env("PATH", dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to run bridge binary");

    let stderr = String::from_utf8_lossy(&output.stderr);

    // The fake enwiro writes to stderr. Because the bridge uses
    // Stdio::inherit() for the child's stderr, the child's output
    // flows through the bridge's stderr — which we piped and captured.
    // If stderr were Stdio::null(), this would be empty.
    assert!(
        stderr.contains("cook failed"),
        "Child stderr should propagate through the bridge, got: {stderr:?}"
    );
}

#[test]
fn test_empty_selection_does_not_call_enwiro() {
    // PATH points to an empty dir — no enwiro binary exists.
    // If the bridge incorrectly tries to spawn enwiro, it will fail
    // and exit with an error. If it correctly skips the empty selection,
    // it exits successfully.
    let dir = tempfile::tempdir().unwrap();

    let bridge = env!("CARGO_BIN_EXE_enwiro-bridge-rofi");
    let output = Command::new(bridge)
        .arg("")
        .env("ROFI_RETV", "1")
        .env("PATH", dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to run bridge binary");

    assert!(
        output.status.success(),
        "Bridge should exit successfully for empty selection"
    );
}
