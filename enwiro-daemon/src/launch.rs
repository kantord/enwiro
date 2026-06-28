//! Isolation launch decision (issue #540). Given a resolved environment +
//! command, decide whether to run on the host or inside a prebuilt OCI image,
//! and return the final `program` + `args`. The daemon is the single source of
//! truth for this decision; the `enw` CLI is a thin client that exec-replaces
//! into whatever `resolve_launch` returns.
//!
//! The container path is behind the `container-wrap` build feature (off by
//! default); without it `resolve_launch` always returns the host command.

use enwiro_sdk::process::ENWIRO_ENV_VAR;
use enwiro_sdk::rpc::{LaunchResolveParams, LaunchResolveResult};

/// Prefix for the per-environment OCI image tag. The trigger is purely the
/// *presence* of an image named `enwiro/<env-name>`; building it is out-of-band.
#[cfg(feature = "container-wrap")]
const CONTAINER_IMAGE_PREFIX: &str = "enwiro/";

/// Container engines we know how to drive, in preference order (podman first).
#[cfg(feature = "container-wrap")]
const CONTAINER_ENGINES: [&str; 2] = ["podman", "docker"];

/// Terminal emulators enwiro runs as host chrome with a wrapped shell inside
/// (pilot: kitty only; see the launch-template registry plan). Matched by binary
/// basename; the terminal itself never needs display passthrough.
const TERMINAL_BINARIES: &[&str] = &["kitty"];

/// Shell run inside a *containerized* terminal (must exist in the image).
#[cfg(feature = "container-wrap")]
const TERMINAL_CONTAINER_SHELL: &str = "bash";

/// True iff `command` (by basename) is a known terminal emulator.
fn is_terminal(command: &str) -> bool {
    let basename = command.rsplit('/').next().unwrap_or(command);
    TERMINAL_BINARIES.contains(&basename)
}

/// Decide how to launch `command` in the environment. Host path returns the
/// command unchanged; container path returns `engine run ... <image> <command>`.
//
// TODO(#540): the terminal handling below is a hardcoded pilot (kitty only, via
// `TERMINAL_BINARIES`) and the container-terminal branch duplicates the generic
// container branch. Replace both with a general launch-template registry
// (binary-name -> strategy) so new terminals and per-app rules don't require
// editing this function.
pub fn resolve_launch(params: &LaunchResolveParams) -> LaunchResolveResult {
    // Terminal template (issue #540): the terminal runs on the host; if the env
    // containerizes, its inner command is the container invocation for the shell
    // (`kitty <engine> run ... <image> <shell>`), otherwise it uses `$SHELL`.
    if is_terminal(&params.command) {
        #[cfg(feature = "container-wrap")]
        if !params.env_name.is_empty()
            && let Some(engine) = find_container_engine()
        {
            let image = container_image_tag(&params.env_name);
            if image_exists(engine, &image) {
                return LaunchResolveResult {
                    program: params.command.clone(),
                    args: build_terminal_container_args(
                        &params.args,
                        engine,
                        &image,
                        &params.env_path,
                        &params.env_name,
                    ),
                    env_vars: Vec::new(),
                };
            }
        }

        // Host terminal: run it directly (it uses `$SHELL`); the client applies
        // cwd (= env path) + `ENWIRO_ENV`.
        return LaunchResolveResult {
            program: params.command.clone(),
            args: params.args.clone(),
            env_vars: launch_env_vars(&params.env_name),
        };
    }

    #[cfg(feature = "container-wrap")]
    if !params.env_name.is_empty()
        && let Some(engine) = find_container_engine()
    {
        let image = container_image_tag(&params.env_name);
        if image_exists(engine, &image) {
            return LaunchResolveResult {
                program: engine.to_string(),
                args: build_container_argv(
                    &image,
                    &params.env_path,
                    &params.env_name,
                    &params.command,
                    &params.args,
                    params.interactive,
                ),
                // The container path injects `ENWIRO_ENV` *inside* the
                // container via `-e` (see `build_container_argv`), so the host
                // `engine` process needs no extra vars.
                env_vars: Vec::new(),
            };
        }
    }

    LaunchResolveResult {
        program: params.command.clone(),
        args: params.args.clone(),
        env_vars: launch_env_vars(&params.env_name),
    }
}

/// Environment variables the daemon injects on a host-launched process: just
/// `ENWIRO_ENV` carrying the resolved environment name (empty on the home-dir
/// fallback).
fn launch_env_vars(environment_name: &str) -> Vec<(String, String)> {
    vec![(ENWIRO_ENV_VAR.to_string(), environment_name.to_string())]
}

/// The OCI image tag the daemon looks for to containerize a given environment.
#[cfg(feature = "container-wrap")]
fn container_image_tag(environment_name: &str) -> String {
    format!("{CONTAINER_IMAGE_PREFIX}{environment_name}")
}

/// First available container engine on PATH, in preference order, or None.
///
/// NOTE: this resolves against the *daemon's* `PATH`, not the calling user's. A
/// `systemd --user` daemon with a stripped `PATH` may fail to find an engine the
/// user has, so the env silently runs on the host (likewise `image_exists`
/// probes the daemon's engine context). The robust fix is to thread the caller's
/// `PATH` through `LaunchResolveParams` and probe with `which::which_in`;
/// deferred while the isolation layer is experimental.
#[cfg(feature = "container-wrap")]
fn find_container_engine() -> Option<&'static str> {
    CONTAINER_ENGINES
        .into_iter()
        .find(|engine| which::which(engine).is_ok())
}

/// Build the args for a *containerized terminal*: the terminal runs on the host
/// (it is the `program`) with its own `terminal_args` preserved, followed by the
/// container invocation that runs the env's shell inside the image. The terminal
/// supplies the pty, so the inner shell is always interactive.
#[cfg(feature = "container-wrap")]
fn build_terminal_container_args(
    terminal_args: &[String],
    engine: &str,
    image: &str,
    environment_path: &str,
    environment_name: &str,
) -> Vec<String> {
    let mut args = terminal_args.to_vec();
    args.push(engine.to_string());
    args.extend(build_container_argv(
        image,
        environment_path,
        environment_name,
        TERMINAL_CONTAINER_SHELL,
        &[],
        true,
    ));
    args
}

/// Ask the engine whether `image` exists locally. Podman has `image exists`
/// (exit 0/1); Docker uses `image inspect` (exit 0 iff present). Any spawn
/// error counts as "absent".
#[cfg(feature = "container-wrap")]
fn image_exists(engine: &str, image: &str) -> bool {
    use std::process::{Command, Stdio};
    let probe_args: [&str; 3] = match engine {
        "podman" => ["image", "exists", image],
        _ => ["image", "inspect", image],
    };
    Command::new(engine)
        .args(probe_args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Assemble the `run ...` args (engine excluded; it is the `program`). The env's
/// project dir is bind-mounted at the *same* path it has on the host (paths
/// match, so no translation), cwd set there, `ENWIRO_ENV` injected; `-it` when
/// the caller's stdin is a TTY, `-i` otherwise.
///
/// The bind mount uses `--mount type=bind,source=,target=` rather than
/// `-v src:dst` so a path containing a colon is not mis-parsed as the
/// `-v`-syntax field separator.
#[cfg(feature = "container-wrap")]
fn build_container_argv(
    image: &str,
    environment_path: &str,
    environment_name: &str,
    command: &str,
    child_args: &[String],
    interactive: bool,
) -> Vec<String> {
    let mut argv = vec!["run".to_string(), "--rm".to_string()];
    argv.push(if interactive { "-it" } else { "-i" }.to_string());
    argv.push("--mount".to_string());
    argv.push(format!(
        "type=bind,source={environment_path},target={environment_path}"
    ));
    argv.push("-w".to_string());
    argv.push(environment_path.to_string());
    argv.push("-e".to_string());
    argv.push(format!("ENWIRO_ENV={environment_name}"));
    argv.push(image.to_string());
    argv.push(command.to_string());
    argv.extend(child_args.iter().cloned());
    argv
}

#[cfg(test)]
mod terminal_tests {
    use super::*;

    #[test]
    fn recognizes_kitty_by_basename() {
        assert!(is_terminal("kitty"));
        assert!(is_terminal("/usr/bin/kitty"));
        assert!(!is_terminal("bash"));
        assert!(!is_terminal("vim"));
    }

    #[test]
    fn host_terminal_runs_directly_with_enwiro_env() {
        // No `enwiro/__nope__` image (and/or feature off) → host terminal: run
        // it directly so it uses `$SHELL`; cwd + ENWIRO_ENV applied by the client.
        let res = resolve_launch(&LaunchResolveParams {
            env_name: "__nope__".to_string(),
            env_path: "/tmp".to_string(),
            command: "kitty".to_string(),
            args: vec![],
            interactive: false,
        });
        assert_eq!(res.program, "kitty");
        assert!(res.args.is_empty());
        assert_eq!(
            res.env_vars,
            vec![("ENWIRO_ENV".to_string(), "__nope__".to_string())]
        );
    }
}

#[cfg(all(test, feature = "container-wrap"))]
mod tests {
    use super::*;

    #[test]
    fn image_tag_is_prefixed_env_name() {
        assert_eq!(container_image_tag("my-proj"), "enwiro/my-proj");
    }

    #[test]
    fn container_argv_mounts_env_path_at_same_path_and_sets_env() {
        let argv = build_container_argv(
            "enwiro/my-proj",
            "/home/u/.enwiro_envs/my-proj/my-proj",
            "my-proj",
            "bash",
            &["-l".to_string()],
            true,
        );
        assert_eq!(
            argv,
            vec![
                "run",
                "--rm",
                "-it",
                "--mount",
                "type=bind,source=/home/u/.enwiro_envs/my-proj/my-proj,target=/home/u/.enwiro_envs/my-proj/my-proj",
                "-w",
                "/home/u/.enwiro_envs/my-proj/my-proj",
                "-e",
                "ENWIRO_ENV=my-proj",
                "enwiro/my-proj",
                "bash",
                "-l",
            ]
        );
    }

    #[test]
    fn container_argv_uses_dash_i_when_not_a_tty() {
        let argv = build_container_argv("enwiro/x", "/p", "x", "echo", &[], false);
        assert!(argv.contains(&"-i".to_string()));
        assert!(!argv.contains(&"-it".to_string()));
    }

    // A path containing a colon must not be split by the engine: `--mount` keeps
    // it intact as `source=`/`target=`, where `-v src:dst` would mis-split it.
    #[test]
    fn container_argv_mount_survives_colon_in_path() {
        let colon_path = "/home/u/.enwiro_envs/proj:1/proj:1";
        let argv = build_container_argv("enwiro/x", colon_path, "x", "bash", &[], true);
        // The path appears verbatim inside a single `--mount` value...
        let mount_idx = argv
            .iter()
            .position(|a| a == "--mount")
            .expect("has --mount");
        let mount_val = &argv[mount_idx + 1];
        assert_eq!(
            mount_val,
            &format!("type=bind,source={colon_path},target={colon_path}")
        );
        // ...and never as a bare `src:dst` `-v` value (which the engine would
        // mis-split on the colon).
        assert!(!argv.iter().any(|a| a == "-v"));
        assert!(
            !argv
                .iter()
                .any(|a| a.contains(&format!("{colon_path}:{colon_path}")))
        );
    }

    // A containerized terminal must preserve the terminal's own args
    // (e.g. `kitty --session foo`), not just run the inner container shell.
    #[test]
    fn terminal_container_args_preserve_terminal_args() {
        let terminal_args = vec!["--session".to_string(), "foo".to_string()];
        let args = build_terminal_container_args(
            &terminal_args,
            "docker",
            "enwiro/my-proj",
            "/p",
            "my-proj",
        );
        // The terminal's own args come first (kitty parses them), then the
        // container invocation for the inner shell.
        assert_eq!(&args[0], "--session");
        assert_eq!(&args[1], "foo");
        assert_eq!(&args[2], "docker");
        assert_eq!(&args[3], "run");
        assert!(args.iter().any(|a| a == "bash"));
    }

    #[test]
    fn host_path_returns_command_unchanged_when_no_image() {
        // No `enwiro/__nope__` image exists → host path.
        let res = resolve_launch(&LaunchResolveParams {
            env_name: "__nope__".to_string(),
            env_path: "/tmp".to_string(),
            command: "echo".to_string(),
            args: vec!["hi".to_string()],
            interactive: false,
        });
        assert_eq!(res.program, "echo");
        assert_eq!(res.args, vec!["hi".to_string()]);
        assert_eq!(
            res.env_vars,
            vec![("ENWIRO_ENV".to_string(), "__nope__".to_string())]
        );
    }

    #[test]
    fn host_env_vars_carry_enwiro_env() {
        assert_eq!(
            launch_env_vars("my-proj"),
            vec![("ENWIRO_ENV".to_string(), "my-proj".to_string())]
        );
    }
}
