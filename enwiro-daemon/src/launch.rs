//! Isolation launch decision (issue #540). Given a resolved environment +
//! command, decide whether to run on the host or inside a prebuilt OCI image,
//! and return the final `program` + `args`. This logic used to live in the
//! `enw` CLI (`commands/wrap.rs`); it now lives in the daemon so the daemon is
//! the single source of truth for the launch decision. The CLI is a thin
//! client that just exec-replaces into whatever this returns.
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

/// Terminal emulators enwiro launches as host *chrome* with a wrapped shell
/// inside (pilot: kitty only — see the launch-template registry plan). Matched
/// by binary basename. A recognized terminal runs on the **host**; only the
/// shell inside it is wrapped (host or container), so the terminal itself never
/// needs display passthrough.
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
/// command unchanged; container path returns `engine run … <image> <command>`.
pub fn resolve_launch(params: &LaunchResolveParams) -> LaunchResolveResult {
    // Terminal-emulator template (issue #540): run the terminal on the HOST with
    // the env's shell inside it. If the env containerizes, the terminal's inner
    // command is the container invocation for the shell (`kitty <engine> run …
    // <image> <shell>`); otherwise the terminal runs on the host and uses
    // `$SHELL`. The terminal is host chrome — only the shell inside is wrapped —
    // so this needs no display passthrough.
    if is_terminal(&params.command) {
        #[cfg(feature = "container-wrap")]
        if !params.env_name.is_empty()
            && let Some(engine) = find_container_engine()
        {
            let image = container_image_tag(&params.env_name);
            if image_exists(engine, &image) {
                let mut args = vec![engine.to_string()];
                // The terminal supplies the pty, so the inner shell is always
                // interactive regardless of the caller's stdin.
                args.extend(build_container_argv(
                    &image,
                    &params.env_path,
                    &params.env_name,
                    TERMINAL_CONTAINER_SHELL,
                    &[],
                    true,
                ));
                return LaunchResolveResult {
                    program: params.command.clone(),
                    args,
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
/// fallback). Built daemon-side so the launch decision — including which env
/// vars a launched process carries — has a single source of truth.
fn launch_env_vars(environment_name: &str) -> Vec<(String, String)> {
    vec![(ENWIRO_ENV_VAR.to_string(), environment_name.to_string())]
}

/// The OCI image tag the daemon looks for to containerize a given environment.
#[cfg(feature = "container-wrap")]
fn container_image_tag(environment_name: &str) -> String {
    format!("{CONTAINER_IMAGE_PREFIX}{environment_name}")
}

/// First available container engine on PATH, in preference order, or None.
#[cfg(feature = "container-wrap")]
fn find_container_engine() -> Option<&'static str> {
    CONTAINER_ENGINES
        .into_iter()
        .find(|engine| binary_on_path(engine))
}

/// True iff a file named `name` exists in any PATH directory.
#[cfg(feature = "container-wrap")]
fn binary_on_path(name: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(name).is_file())
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

/// Assemble the `run …` args (engine excluded — it's the `program`). The env's
/// project dir is bind-mounted at the *same* path it has on the host (paths
/// match → no translation), cwd set there, `ENWIRO_ENV` injected; `-it` when
/// the caller's stdin is a TTY, `-i` otherwise.
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
    argv.push("-v".to_string());
    argv.push(format!("{environment_path}:{environment_path}"));
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
                "-v",
                "/home/u/.enwiro_envs/my-proj/my-proj:/home/u/.enwiro_envs/my-proj/my-proj",
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
