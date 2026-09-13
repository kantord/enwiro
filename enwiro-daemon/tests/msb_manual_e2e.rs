//! Manual, throwaway end-to-end smoke tests (not part of `cargo test --workspace`
//! since they need a real `msb` install + KVM + network to pull `alpine`, which
//! CI won't have). Run explicitly:
//!
//!   cargo test --features container-wrap --test msb_manual_e2e -- --ignored --nocapture
//!
//! Each test exercises the *real* production code path (`resolve_launch`, then
//! actually spawning what it returns) against a real `msb run`, specifically to
//! catch regressions in bugs found only by testing against real `msb` -- not
//! things a pure-argv unit test (which never invokes `msb`) could ever catch.
//! Every test here corresponds to one such bug, found during the podman -> microsandbox
//! migration (ADR-0006):
//!
//! - `$HOME` fallback: the prelude checked emptiness, but a `-u <uid>` with no
//!   matching `/etc/passwd` entry leaves `$HOME` defaulting to `/` (unwritable),
//!   not empty -- fixed to test writability instead.
//! - Symlinked mount sources: `msb` refuses to mount a symlink at all (ELOOP),
//!   unlike podman -- and enwiro's own per-env layout is always a symlink.
//! - Guest memory: `msb`'s default (~517M) OOM-kills any real dev tool with no
//!   explanation beyond a bare "Killed".

#![cfg(feature = "container-wrap")]

use enwiro_daemon::launch::resolve_launch;
use enwiro_sdk::rpc::LaunchResolveParams;
use std::process::Command;

fn git(dir: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

/// Build the params for a `sh -c <script>` isolated launch of `project`, then
/// resolve + actually run it, returning the captured output. Panics with the
/// resolved argv and full output on any failure, so a broken run is easy to
/// diagnose from `--nocapture` output.
fn run_isolated(
    project: &std::path::Path,
    workspaces_dir: &std::path::Path,
    script: &str,
) -> std::process::Output {
    let params = LaunchResolveParams {
        env_name: "e2e-test".to_string(),
        env_path: project.to_str().unwrap().to_string(),
        command: "sh".to_string(),
        args: vec!["-c".to_string(), script.to_string()],
        interactive: false,
    };
    let resolved = resolve_launch(&params, workspaces_dir)
        .expect("resolve_launch should isolate this env (isolate=true, image=alpine)");
    assert_eq!(resolved.program, "msb");
    assert!(resolved.args.contains(&"run".to_string()));
    eprintln!("resolved argv: {:?}", resolved.args);

    let output = Command::new(&resolved.program)
        .args(&resolved.args)
        .envs(resolved.env_vars)
        .output()
        .expect("spawning the resolved msb invocation");
    eprintln!("stdout:\n{}", String::from_utf8_lossy(&output.stdout));
    eprintln!("stderr:\n{}", String::from_utf8_lossy(&output.stderr));
    output
}

fn write_isolation_config(project: &std::path::Path) {
    std::fs::write(
        project.join(".enwiro.toml"),
        "[isolation]\nisolate = true\nimage = \"alpine\"\n",
    )
    .unwrap();
}

#[test]
#[ignore = "manual: needs a real msb install + KVM + network (pulls alpine)"]
fn real_msb_run_mounts_a_project_correctly_and_seeds_git_identity_env() {
    // A real git repo, so `host_git_identity()` (called by `resolve_launch`,
    // same as production) has something real to resolve, exercising that
    // argv wiring too even though this test's inner command doesn't run git.
    let project = tempfile::tempdir().unwrap();
    git(project.path(), &["init", "-q"]);
    git(project.path(), &["config", "user.name", "Host User"]);
    git(
        project.path(),
        &["config", "user.email", "host@example.com"],
    );
    std::fs::write(project.path().join("f"), "hello\n").unwrap();
    git(project.path(), &["add", "f"]);
    git(project.path(), &["commit", "-qm", "init"]);
    write_isolation_config(project.path());

    let workspaces_dir = tempfile::tempdir().unwrap();

    // Plain `alpine` has no git preinstalled and (correctly, given the
    // non-root hardening) can't `apk add` one at runtime as a non-root uid --
    // BYO images are expected to ship their own tools already baked in, not
    // install them at launch. So this proves the *mechanics* enwiro's own
    // code is responsible for:
    // 1. the repo is really (bind-)mounted, and a file the sandbox writes is
    //    readable back (ownership claim)
    // 2. ENWIRO_ENV really arrives
    // 3. the process really runs as the non-root uid this PR sets
    // 4. the prelude's $HOME fix succeeds (a bare `-u 1000` has no matching
    //    /etc/passwd entry on alpine, so $HOME defaults to `/`; without the
    //    fix, `mkdir -p "$HOME"` itself fails with Permission denied)
    let output = run_isolated(
        project.path(),
        workspaces_dir.path(),
        r#"echo "ENWIRO_ENV=$ENWIRO_ENV" && id -u && [ -w "$HOME" ] && echo HOME_WRITABLE && cat f && echo second >> f && cat f"#,
    );

    assert!(
        output.status.success(),
        "msb run exited non-zero: {:?}",
        output.status
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        [
            "ENWIRO_ENV=e2e-test",
            "1000",
            "HOME_WRITABLE",
            "hello",
            "hello",
            "second"
        ],
        "{stdout}"
    );
    // Non-root hardening: really the daemon's own uid, never root.
    assert_ne!(lines[1], "0", "{stdout}");

    // The write made *inside* the sandbox should be visible on the *host*
    // afterward -- a real bind mount, not a copy.
    let host_file = std::fs::read_to_string(project.path().join("f")).unwrap();
    eprintln!("host-side file content after sandbox exit:\n{host_file}");
    assert_eq!(host_file, "hello\nsecond\n");
}

// Regression test for a real bug: `msb` refuses to mount a symlinked source
// at all ("Too many levels of symbolic links", ELOOP), unlike podman -- and
// enwiro's own per-environment layout is *always* a symlink
// (`<workspaces_directory>/<name>/<name>` -> the cookbook's real output).
// `canonical_environment_path`'s own unit test only proves the Rust-side
// resolution logic is correct; it never invokes real `msb`, so it could not
// have caught the actual mount failure this reproduces end to end.
#[test]
#[ignore = "manual: needs a real msb install + KVM + network (pulls alpine)"]
fn real_msb_run_mounts_a_symlinked_environment_path() {
    let real_target = tempfile::tempdir().unwrap();
    std::fs::write(real_target.path().join("marker"), "real-target\n").unwrap();
    write_isolation_config(real_target.path());

    // Mirrors enwiro's real per-env layout: `<workspaces_dir>/<name>/<name>`
    // is a symlink to wherever the cookbook actually put the project.
    let workspaces_dir = tempfile::tempdir().unwrap();
    let env_dir = workspaces_dir.path().join("e2e-test");
    std::fs::create_dir_all(&env_dir).unwrap();
    let symlinked_env_path = env_dir.join("e2e-test");
    std::os::unix::fs::symlink(real_target.path(), &symlinked_env_path).unwrap();

    let output = run_isolated(&symlinked_env_path, workspaces_dir.path(), "cat marker");

    assert!(
        output.status.success(),
        "msb run exited non-zero (this is the ELOOP regression if so): {:?}",
        output.status
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "real-target\n");
}

// Regression test for a real bug: `msb`'s default guest memory (~517M,
// verified hands-on) is too small for real tools -- surfacing to the user as
// a bare "Killed" with no other explanation for an actual OOM kill.
// `/dev/shm` (tmpfs, RAM-backed) writing 800M needs no package install (bare
// `alpine` has no compiler/python, and can't `apk add` one as non-root --
// see the other test's comment), so a failure here is unambiguously about
// available memory, not an unrelated non-root package-install limitation.
// Verified hands-on: this exact write fails ("No space left on device") at
// `msb`'s bare default, and succeeds once a real `-m` floor is set.
#[test]
#[ignore = "manual: needs a real msb install + KVM + network (pulls alpine)"]
fn real_msb_run_has_enough_memory_for_a_moderate_allocation() {
    let project = tempfile::tempdir().unwrap();
    write_isolation_config(project.path());
    let workspaces_dir = tempfile::tempdir().unwrap();

    let output = run_isolated(
        project.path(),
        workspaces_dir.path(),
        "dd if=/dev/zero of=/dev/shm/bigfile bs=1M count=800 2>&1 && echo allocated-800mb-ok",
    );

    assert!(
        output.status.success(),
        "msb run exited non-zero (this is the too-small-guest-memory regression if so): {:?}",
        output.status
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("allocated-800mb-ok"),
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}
