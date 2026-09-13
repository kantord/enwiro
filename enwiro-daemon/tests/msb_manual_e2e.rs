//! Manual, throwaway end-to-end smoke test (not part of `cargo test --workspace`
//! since it needs a real `msb` install + KVM + network to pull `alpine`, which
//! CI won't have). Run explicitly:
//!
//!   cargo test --features container-wrap --test msb_manual_e2e -- --ignored --nocapture
//!
//! Exercises the *real* production code path: `resolve_launch` (no daemon, no
//! socket, no lock file -- it's a plain function) against a real, throwaway
//! git repo and a real `msb run` of the public `alpine` image, to verify
//! claims made from bare `msb` CLI experiments actually hold through enwiro's
//! own code, not just in isolation. It's what caught a real bug in review:
//! the original `[ -n "$HOME" ] && mkdir -p "$HOME"` prelude line did nothing
//! for a `-u <uid>` with no matching `/etc/passwd` entry (`$HOME` defaults to
//! `/`, not empty) -- fixed to test writability instead of emptiness.

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

    // The isolation policy this PR added: [isolation] isolate + image in
    // .enwiro.toml, resolved by the real `enwiro_sdk::config` walker.
    std::fs::write(
        project.path().join(".enwiro.toml"),
        "[isolation]\nisolate = true\nimage = \"alpine\"\n",
    )
    .unwrap();

    let workspaces_dir = tempfile::tempdir().unwrap();

    // This test only exercises the isolation-policy/mount/git-identity
    // mechanics, not Claude auth (unit-tested separately) -- point
    // `claude_oauth_token()`'s lookup at an empty temp dir so it can't
    // accidentally pick up the real, private token this dev machine has
    // configured at `~/.config/enwiro/claude_oauth_token`.
    let fake_xdg_config = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded manual test, no concurrent env access.
    unsafe {
        std::env::remove_var("CLAUDE_CODE_OAUTH_TOKEN");
        std::env::set_var("XDG_CONFIG_HOME", fake_xdg_config.path());
    }

    let params = LaunchResolveParams {
        env_name: "e2e-test".to_string(),
        env_path: project.path().to_str().unwrap().to_string(),
        command: "sh".to_string(),
        args: vec![
            "-c".to_string(),
            // Plain `alpine` has no git preinstalled and (correctly, given
            // ADR-0006's non-root hardening) can't `apk add` one at runtime
            // as a non-root uid -- BYO images are expected to ship their own
            // tools already baked in, not install them at launch. So this
            // proves the *mechanics* enwiro's own code is responsible for:
            // 1. the repo is really (bind-)mounted, and a file the sandbox
            //    writes is readable back (ownership claim)
            // 2. ENWIRO_ENV really arrives
            // 3. the process really runs as the non-root uid this PR sets
            // 4. the onboarding-seed step succeeds (exercises the HOME fix:
            //    a bare `-u 1000` has no matching /etc/passwd entry on
            //    alpine, so without it this whole script fails writing
            //    $HOME/.claude.json to an unwritable `/`)
            "echo \"ENWIRO_ENV=$ENWIRO_ENV\" && id -u && cat f && echo second >> f && cat f"
                .to_string(),
        ],
        interactive: false,
    };

    let resolved = resolve_launch(&params, workspaces_dir.path())
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

    assert!(
        output.status.success(),
        "msb run exited non-zero: {:?}",
        output.status
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // `echo ENWIRO_ENV=...`, `id -u`, `cat f` (before append), `cat f` (after).
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        ["ENWIRO_ENV=e2e-test", "1000", "hello", "hello", "second"],
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
