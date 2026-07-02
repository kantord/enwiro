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

/// Basename of a command path (everything after the last `/`).
fn command_basename(command: &str) -> &str {
    command.rsplit('/').next().unwrap_or(command)
}

/// True iff `command` (by basename) is a known terminal emulator.
fn is_terminal(command: &str) -> bool {
    TERMINAL_BINARIES.contains(&command_basename(command))
}

/// True iff `command` (by basename) is the Claude Code CLI, which is the only
/// launch routed through the host-side auth proxy.
#[cfg(feature = "container-wrap")]
fn is_claude(command: &str) -> bool {
    command_basename(command) == "claude"
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
            // A claude launch is routed through the host-side auth proxy (the
            // token stays on the host, never in the container) — but only when a
            // token is actually configured; otherwise claude just logs in.
            let route_claude_via_proxy =
                is_claude(&params.command) && claude_oauth_token().is_some();
            return LaunchResolveResult {
                program: engine.to_string(),
                args: build_container_argv(
                    &image,
                    &params.env_path,
                    &params.env_name,
                    &params.command,
                    &params.args,
                    params.interactive,
                    route_claude_via_proxy,
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
    // The inner command is a shell, not claude, so no proxy wiring is injected.
    args.extend(build_container_argv(
        image,
        environment_path,
        environment_name,
        TERMINAL_CONTAINER_SHELL,
        &[],
        true,
        false,
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
    route_claude_via_proxy: bool,
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
    // Credential injection (issue #540): route claude at the host-side auth proxy
    // instead of putting the real token in the container. The container gets only
    // the proxy's base URL + a non-secret sentinel token; the daemon's proxy
    // (`proxy.rs`) swaps the sentinel for the real `Authorization` on the host, so
    // the credential never enters the container and can't be exfiltrated from it.
    // `--add-host` makes `host.docker.internal` resolve to the host on Linux.
    if route_claude_via_proxy {
        argv.push("--add-host".to_string());
        argv.push("host.docker.internal:host-gateway".to_string());
        argv.push("-e".to_string());
        argv.push(format!(
            "ANTHROPIC_BASE_URL=http://host.docker.internal:{}",
            crate::proxy::CLAUDE_PROXY_PORT
        ));
        argv.push("-e".to_string());
        argv.push(format!(
            "ANTHROPIC_AUTH_TOKEN={}",
            crate::proxy::CLAUDE_PROXY_SENTINEL_TOKEN
        ));
    }
    argv.push(image.to_string());
    // Seed a default `.claude.json` (only if absent) so an interactive claude
    // skips the first-run onboarding wizard, then `exec` the real command.
    // Claude has no env var / settings field for `hasCompletedOnboarding`
    // (issue anthropics/claude-code#4714), so a file is the only lever; seeding
    // it at start rather than baking it into the image keeps BYO images
    // untouched. The seed is non-secret and the container mutates its own
    // ephemeral copy (discarded on `--rm`); an image that ships its own
    // `.claude.json` is left alone.
    argv.push("sh".to_string());
    argv.push("-c".to_string());
    argv.push(CLAUDE_ONBOARDING_SEED_SCRIPT.to_string());
    argv.push("sh".to_string()); // $0 for the exec'd shell
    argv.push(command.to_string());
    argv.extend(child_args.iter().cloned());
    argv
}

/// `sh -c` script that seeds a default `.claude.json` (only when absent) then
/// `exec`s the caller's command (`"$@"`, supplied after the `sh` `$0`), so a
/// fresh container skips Claude Code's first-run wizard without baking anything
/// into the image. Claude has no env var / settings field for this (issue
/// anthropics/claude-code#4714), so the file is the only lever.
///
/// It marks BOTH gates: `hasCompletedOnboarding` (theme + welcome) and, for the
/// container's working directory (`$(pwd)`, which enwiro sets to the env path via
/// `-w`), `hasTrustDialogAccepted` (the "trust this folder" prompt). Claude's
/// config path is `$CLAUDE_CONFIG_DIR/.claude.json` when set, else
/// `$HOME/.claude.json` (home root, *not* a `.claude/` subdir) - verified against
/// the CLI. The seed is non-secret and ephemeral; an image shipping its own
/// `.claude.json` is left untouched.
#[cfg(feature = "container-wrap")]
const CLAUDE_ONBOARDING_SEED_SCRIPT: &str = concat!(
    r#"if [ -n "$CLAUDE_CONFIG_DIR" ]; then f="$CLAUDE_CONFIG_DIR/.claude.json"; else f="$HOME/.claude.json"; fi; "#,
    r#"[ -f "$f" ] || { mkdir -p "$(dirname "$f")" && "#,
    r#"printf '{"hasCompletedOnboarding":true,"theme":"dark-ansi","projects":{"%s":{"hasTrustDialogAccepted":true,"hasCompletedProjectOnboarding":true}}}' "$(pwd)" > "$f"; }; "#,
    r#"exec "$@""#,
);

/// A cached Claude Code OAuth token to inject into a *claude* launch, or `None`
/// if none is configured. Sources, first wins: the daemon's
/// `CLAUDE_CODE_OAUTH_TOKEN` env var, else a single line in
/// `$XDG_CONFIG_HOME/enwiro/claude_oauth_token` (defaulting to
/// `~/.config/enwiro/claude_oauth_token`). Mint one with `claude setup-token`.
///
/// One cached token is reused across envs (no per-env token proliferation); it
/// is used by the host-side proxy (`proxy.rs`) for claude launches, never placed
/// in the container.
#[cfg(feature = "container-wrap")]
pub(crate) fn claude_oauth_token() -> Option<String> {
    if let Some(token) = std::env::var("CLAUDE_CODE_OAUTH_TOKEN")
        .ok()
        .filter(|token| !token.is_empty())
    {
        return Some(token);
    }
    let path = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| home::home_dir().map(|home| home.join(".config")))?
        .join("enwiro")
        .join("claude_oauth_token");
    let token = std::fs::read_to_string(path).ok()?;
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_string())
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
            false,
        );
        // Head up to the image: run flags + bind mount + cwd + ENWIRO_ENV.
        let image_idx = argv.iter().position(|a| a == "enwiro/my-proj").unwrap();
        assert_eq!(
            &argv[..=image_idx],
            &[
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
            ]
        );
        // The command is wrapped `sh -c <seed> sh <command> <args>`.
        assert_eq!(&argv[image_idx + 1..image_idx + 3], &["sh", "-c"]);
        assert_eq!(&argv[image_idx + 4..], &["sh", "bash", "-l"]);
    }

    // A default `.claude.json` is seeded only if absent, then the real command
    // is exec'd, so a fresh container skips Claude's onboarding wizard (theme +
    // workspace-trust) without baking anything into the image.
    #[test]
    fn container_argv_seeds_onboarding_then_execs_command() {
        let argv = build_container_argv("enwiro/x", "/p", "x", "claude", &[], true, false);
        let script = &argv[argv.iter().position(|a| a == "-c").unwrap() + 1];
        assert!(script.contains("hasCompletedOnboarding"), "{script}");
        // both onboarding gates: theme/welcome AND per-workspace trust.
        assert!(script.contains("hasTrustDialogAccepted"), "{script}");
        // targets the home-root config path by default, keyed to the workdir.
        assert!(script.contains(r#"f="$HOME/.claude.json""#), "{script}");
        assert!(script.contains(r#""$(pwd)""#), "{script}");
        assert!(
            script.contains(".claude.json") && script.contains("[ -f"),
            "seeds only when absent: {script}"
        );
        assert!(script.trim_end().ends_with(r#"exec "$@""#), "{script}");
    }

    #[test]
    fn container_argv_uses_dash_i_when_not_a_tty() {
        let argv = build_container_argv("enwiro/x", "/p", "x", "echo", &[], false, false);
        assert!(argv.contains(&"-i".to_string()));
        assert!(!argv.contains(&"-it".to_string()));
    }

    #[test]
    fn is_claude_matches_by_basename() {
        assert!(is_claude("claude"));
        assert!(is_claude("/usr/local/bin/claude"));
        assert!(!is_claude("bash"));
        assert!(!is_claude("claude-code"));
    }

    // Routing claude via the proxy points the container at the proxy's base URL +
    // a sentinel token, and NEVER puts the real OAuth token in the container.
    #[test]
    fn container_argv_wires_claude_proxy_when_enabled() {
        let argv = build_container_argv("enwiro/x", "/p", "x", "claude", &[], true, true);
        assert!(
            argv.windows(2).any(|w| w[0] == "-e"
                && w[1]
                    == format!(
                        "ANTHROPIC_BASE_URL=http://host.docker.internal:{}",
                        crate::proxy::CLAUDE_PROXY_PORT
                    )),
            "{argv:?}"
        );
        assert!(
            argv.windows(2).any(|w| w[0] == "-e"
                && w[1]
                    == format!(
                        "ANTHROPIC_AUTH_TOKEN={}",
                        crate::proxy::CLAUDE_PROXY_SENTINEL_TOKEN
                    )),
            "{argv:?}"
        );
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--add-host" && w[1] == "host.docker.internal:host-gateway"),
            "{argv:?}"
        );
        // The real token must never reach the container.
        assert!(
            !argv.iter().any(|a| a.contains("CLAUDE_CODE_OAUTH_TOKEN")),
            "{argv:?}"
        );
    }

    // Without proxy routing (non-claude, or no token configured), no ANTHROPIC_*
    // wiring is added.
    #[test]
    fn container_argv_no_proxy_when_disabled() {
        let argv = build_container_argv("enwiro/x", "/p", "x", "bash", &[], true, false);
        assert!(!argv.iter().any(|a| a.contains("ANTHROPIC_")), "{argv:?}");
        assert!(!argv.iter().any(|a| a == "--add-host"), "{argv:?}");
    }

    // A path containing a colon must not be split by the engine: `--mount` keeps
    // it intact as `source=`/`target=`, where `-v src:dst` would mis-split it.
    #[test]
    fn container_argv_mount_survives_colon_in_path() {
        let colon_path = "/home/u/.enwiro_envs/proj:1/proj:1";
        let argv = build_container_argv("enwiro/x", colon_path, "x", "bash", &[], true, false);
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
