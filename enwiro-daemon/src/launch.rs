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
use std::path::Path;

/// Prefix for the per-environment OCI image tag. The trigger is purely the
/// *presence* of an image named `enwiro/<env-name>`; building it is out-of-band.
#[cfg(feature = "container-wrap")]
const CONTAINER_IMAGE_PREFIX: &str = "enwiro/";

/// The only container engine enwiro drives. Podman-only (not Docker) because
/// `--userns=keep-id` (see `build_container_argv`) has no Docker equivalent, and
/// rootless Podman's networking has no host-bindable "bridge gateway" the way
/// Docker's does, so a single supported engine avoids the two runtimes silently
/// behaving differently under the same code path.
#[cfg(feature = "container-wrap")]
const CONTAINER_ENGINE: &str = "podman";

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

/// Decide how to launch `command` in the environment. Host path returns the
/// command unchanged; container path returns `engine run ... <image> <command>`.
//
// TODO(#540): the terminal handling below is a hardcoded pilot (kitty only, via
// `TERMINAL_BINARIES`) and the container-terminal branch duplicates the generic
// container branch. Replace both with a general launch-template registry
// (binary-name -> strategy) so new terminals and per-app rules don't require
// editing this function.
pub fn resolve_launch(
    params: &LaunchResolveParams,
    #[allow(unused_variables)] workspaces_directory: &Path,
) -> LaunchResolveResult {
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
                let env = ContainerEnv {
                    image: &image,
                    environment_path: &params.env_path,
                    environment_name: &params.env_name,
                    inject_proxy_shim: claude_oauth_token().is_some(),
                    workspaces_directory,
                };
                return LaunchResolveResult {
                    program: params.command.clone(),
                    args: build_terminal_container_args(&params.args, engine, &env),
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
            // When a Claude token is configured, install the `claude` shim so any
            // `claude` run *inside* the container (directly or from a shell) routes
            // through the host proxy. Container-scoped, not command-scoped: the
            // shim only affects `claude`, so wiring it for every launch is safe.
            let env = ContainerEnv {
                image: &image,
                environment_path: &params.env_path,
                environment_name: &params.env_name,
                inject_proxy_shim: claude_oauth_token().is_some(),
                workspaces_directory,
            };
            return LaunchResolveResult {
                program: engine.to_string(),
                args: build_container_argv(&env, &params.command, &params.args, params.interactive),
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
/// The environment name is sanitized first: OCI repository names must be
/// lowercase and may only contain `[a-z0-9._-]`, but enwiro environment names
/// commonly don't (e.g. GitHub-issue envs named `<repo>#<n>`), which would
/// otherwise make the image untaggable and silently fall back to the host path.
#[cfg(feature = "container-wrap")]
fn container_image_tag(environment_name: &str) -> String {
    format!(
        "{CONTAINER_IMAGE_PREFIX}{}",
        sanitize_image_tag_component(environment_name)
    )
}

/// Map `name` into a valid OCI repository-name component: lowercased, with any
/// run of characters outside `[a-z0-9]` collapsed to a single `-`, and
/// leading/trailing `-` trimmed (a component must start and end with an
/// alphanumeric). Deliberately collapses `.`/`_` too, not just clearly-illegal
/// characters like `#`: Docker's actual grammar only allows a single `.`, one
/// or two `_`, but any number of `-`, and getting that nuance wrong would
/// still produce an untaggable image. Using only `-` as a separator sidesteps
/// the distinction entirely and is always valid.
///
/// This is a best-effort, lossy mapping, not a collision-free encoding: e.g.
/// `my#env` and `my.env` both sanitize to `my-env`. That trade-off is accepted
/// for the common case this unblocks (issue-based envs named `<repo>#<n>`)
/// over a more complex, reversible scheme.
#[cfg(feature = "container-wrap")]
fn sanitize_image_tag_component(name: &str) -> String {
    let mut sanitized = String::with_capacity(name.len());
    let mut last_was_dash = false;
    for ch in name.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            sanitized.push(lower);
            last_was_dash = false;
        } else if !last_was_dash {
            sanitized.push('-');
            last_was_dash = true;
        }
    }
    sanitized.trim_matches('-').to_string()
}

/// `Some(CONTAINER_ENGINE)` if podman is on PATH, else `None`.
///
/// NOTE: this resolves against the *daemon's* `PATH`, not the calling user's. A
/// `systemd --user` daemon with a stripped `PATH` may fail to find the engine the
/// user has, so the env silently runs on the host (likewise `image_exists`
/// probes the daemon's engine context). The robust fix is to thread the caller's
/// `PATH` through `LaunchResolveParams` and probe with `which::which_in`;
/// deferred while the isolation layer is experimental.
#[cfg(feature = "container-wrap")]
pub(crate) fn find_container_engine() -> Option<&'static str> {
    which::which(CONTAINER_ENGINE)
        .is_ok()
        .then_some(CONTAINER_ENGINE)
}

/// The parts of a containerized launch that stay constant regardless of what
/// command actually runs inside it. Bundled so `build_container_argv` and
/// `build_terminal_container_args` take one narrow, named thing instead of
/// five loose positional fields that happen to travel together. Every field
/// is itself `Copy`, so the whole struct is too.
#[cfg(feature = "container-wrap")]
#[derive(Clone, Copy)]
struct ContainerEnv<'a> {
    image: &'a str,
    environment_path: &'a str,
    environment_name: &'a str,
    inject_proxy_shim: bool,
    workspaces_directory: &'a Path,
}

/// Build the args for a *containerized terminal*: the terminal runs on the host
/// (it is the `program`) with its own `terminal_args` preserved, followed by the
/// container invocation that runs the env's shell inside the image. The terminal
/// supplies the pty, so the inner shell is always interactive.
#[cfg(feature = "container-wrap")]
fn build_terminal_container_args(
    terminal_args: &[String],
    engine: &str,
    env: &ContainerEnv,
) -> Vec<String> {
    let mut args = terminal_args.to_vec();
    args.push(engine.to_string());
    // The inner command is a shell; the claude shim (if injected) lets `claude`
    // run from that shell route through the proxy.
    args.extend(build_container_argv(
        env,
        TERMINAL_CONTAINER_SHELL,
        &[],
        true,
    ));
    args
}

/// Ask the engine whether `image` exists locally (`podman image exists`, exit
/// 0/1). Any spawn error counts as "absent".
#[cfg(feature = "container-wrap")]
fn image_exists(engine: &str, image: &str) -> bool {
    use std::process::{Command, Stdio};
    Command::new(engine)
        .args(["image", "exists", image])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// `--mount` args that bind `path` into the container at the identical path
/// on both sides (no translation). Uses `--mount type=bind,source=,target=`
/// rather than `-v src:dst` so a path containing a colon is not mis-parsed
/// as the `-v`-syntax field separator.
#[cfg(feature = "container-wrap")]
fn bind_mount_args(path: &str) -> [String; 2] {
    [
        "--mount".to_string(),
        format!("type=bind,source={path},target={path}"),
    ]
}

/// Assemble the `run ...` args (engine excluded; it is the `program`). The env's
/// project dir is bind-mounted at the *same* path it has on the host, cwd set
/// there, `ENWIRO_ENV` injected; `-it` when the caller's stdin is a TTY, `-i`
/// otherwise.
#[cfg(feature = "container-wrap")]
fn build_container_argv(
    env: &ContainerEnv,
    command: &str,
    child_args: &[String],
    interactive: bool,
) -> Vec<String> {
    let ContainerEnv {
        image,
        environment_path,
        environment_name,
        inject_proxy_shim,
        workspaces_directory,
    } = *env;
    let mut argv = vec!["run".to_string(), "--rm".to_string()];
    argv.push(if interactive { "-it" } else { "-i" }.to_string());
    argv.extend(bind_mount_args(environment_path));
    // Cookbooks may declare that this environment depends on additional host
    // paths beyond its own directory to function -- e.g. a git worktree's
    // `.git` is a pointer into a separate main repo holding the shared object
    // database. The daemon has no idea *why* a path is needed (that's the
    // cookbook's tool-specific business); it just mounts whatever was
    // declared, at the same absolute path on both sides (required for tools
    // like git that hard-code absolute paths into their own metadata).
    //
    // Declarations are written under `<workspaces_directory>/<environment_name>`
    // (see `write_external_paths_if_present` in the host CLI), not under
    // `environment_path`: an env's actual project location is whatever the
    // cookbook returned (a bind-mounted clone, a worktree elsewhere, anything),
    // while enwiro's own per-env metadata always lives in its managed
    // workspaces directory, one level above the project-pointing symlink.
    let env_dir = workspaces_directory.join(environment_name);
    for path in enwiro_sdk::external_paths::load_external_paths(&env_dir) {
        argv.extend(bind_mount_args(&path));
    }
    argv.push("-w".to_string());
    argv.push(environment_path.to_string());
    argv.push("-e".to_string());
    argv.push(format!("ENWIRO_ENV={environment_name}"));
    // Run as the host user's uid/gid, with `--userns=keep-id` mapping that uid to
    // itself inside the container's user namespace. This fixes git's "dubious
    // ownership" error on the bind-mounted project, stops the container from
    // leaving root-owned files on the host, and drops root (hardening; claude's
    // `--dangerously-skip-permissions` also requires non-root). `keep-id` means
    // the uid resolves against the *image's own* `/etc/passwd`, so an image user
    // matching that uid gets its real home + dotfiles for free, with no need to
    // derive or override `HOME` ourselves (verified: `enwiro/nanoref`'s `vscode`
    // user resolves correctly this way).
    //
    // Linux only: on macOS the container runs in a VM whose file-sharing layer
    // maps ownership, and the daemon's uid is meaningless inside that VM.
    if cfg!(target_os = "linux") {
        argv.push("--userns=keep-id".to_string());
        argv.push("--user".to_string());
        argv.push(format!("{}:{}", host_uid(), host_gid()));
    }
    // Credential injection (issue #540): instead of putting the real token in the
    // container, install a `claude` shim (materialized by the launch prelude into
    // a PATH dir) that points claude at the host-side auth proxy. The daemon's
    // proxy (`proxy.rs`) swaps a per-launch capability token for the real
    // `Authorization` on the host, so the credential never enters the container.
    //
    // The capability is minted fresh per launch (not a fixed sentinel): the proxy
    // rejects any request that doesn't present a token it minted itself, which is
    // the actual access control (the bridge-gateway bind alone doesn't stop other
    // local processes from reaching the proxy).
    //
    // Delivering it as a shim (not container-wide env) means any `claude` run in
    // the container, directly or from a shell, is routed, while other processes'
    // env stays clean. `--add-host` makes `host.containers.internal` resolve to
    // the host so the shim's base URL is reachable.
    if inject_proxy_shim {
        let capability = crate::proxy::mint_capability();
        argv.push("--add-host".to_string());
        argv.push("host.containers.internal:host-gateway".to_string());
        argv.push("-e".to_string());
        argv.push("ENWIRO_SHIMS=claude".to_string());
        argv.push("-e".to_string());
        argv.push(format!(
            "ENWIRO_SHIM_claude={}",
            claude_shim_script(&capability)
        ));
    }
    argv.push(image.to_string());
    // Run the container command through a small `sh` prelude that (1) materializes
    // any enwiro shims into a PATH dir and (2) seeds a default `.claude.json` to
    // skip claude's first-run wizard, then `exec`s the real command. Doing this at
    // start (rather than baking into the image) keeps BYO images untouched; the
    // shims and seed are non-secret and live in the container's ephemeral fs.
    argv.push("sh".to_string());
    argv.push("-c".to_string());
    argv.push(CONTAINER_PRELUDE_SCRIPT.to_string());
    argv.push("sh".to_string()); // $0 for the exec'd shell
    argv.push(command.to_string());
    argv.extend(child_args.iter().cloned());
    argv
}

/// Directory the launch prelude writes enwiro shims into, prepended to `PATH`.
#[cfg(feature = "container-wrap")]
const SHIM_DIR: &str = "/tmp/enwiro-bin";

/// The daemon's real uid / gid, injected as the container's `--user` so file
/// ownership on the bind-mounted project matches.
#[cfg(feature = "container-wrap")]
fn host_uid() -> u32 {
    // SAFETY: `getuid` always succeeds and has no preconditions.
    unsafe { libc::getuid() }
}
#[cfg(feature = "container-wrap")]
fn host_gid() -> u32 {
    // SAFETY: `getgid` always succeeds and has no preconditions.
    unsafe { libc::getgid() }
}

/// The `claude` shim: a tiny script installed on `PATH` inside the container that
/// points claude at the host auth proxy, then execs the *real* claude (found by
/// scanning `PATH`, skipping the shim dir). The proxy base URL and the per-launch
/// capability are baked in; the real token is never here. The capability goes in
/// `CLAUDE_CODE_OAUTH_TOKEN` (not `ANTHROPIC_AUTH_TOKEN`) so the CLI stays in
/// subscription-billing mode rather than switching to API-usage billing. Also
/// disables claude's self-updater: the container is ephemeral and non-root, so it
/// has no write access to the image's npm prefix and the update would just fail.
#[cfg(feature = "container-wrap")]
fn claude_shim_script(capability: &str) -> String {
    format!(
        concat!(
            "#!/bin/sh\n",
            "export ANTHROPIC_BASE_URL=http://host.containers.internal:{port}\n",
            "export CLAUDE_CODE_OAUTH_TOKEN={capability}\n",
            // The container is ephemeral and non-root, so claude can't self-update
            // (no write access to the image's npm prefix); skip the failing attempt.
            "export DISABLE_AUTOUPDATER=1\n",
            "real=''\n",
            "oldifs=\"$IFS\"; IFS=:\n",
            "for dir in $PATH; do\n",
            "  [ \"$dir\" = {shim_dir} ] && continue\n",
            "  if [ -x \"$dir/claude\" ]; then real=\"$dir/claude\"; break; fi\n",
            "done\n",
            "IFS=\"$oldifs\"\n",
            "[ -n \"$real\" ] || {{ echo 'enwiro: real claude not found on PATH' >&2; exit 127; }}\n",
            "exec \"$real\" \"$@\"\n",
        ),
        port = crate::proxy::CLAUDE_PROXY_PORT,
        capability = capability,
        shim_dir = SHIM_DIR,
    )
}

/// `sh -c` launch prelude, run before the container command. Two steps, then
/// `exec "$@"` (the real command, supplied after the `sh` `$0`):
///
/// 1. **Shim materialization.** For each name in `$ENWIRO_SHIMS`, write the shim
///    script from `$ENWIRO_SHIM_<name>` into [`SHIM_DIR`] and prepend that dir to
///    `PATH`. The `eval` references the env var by name (it never inlines its
///    contents), so shim bytes can't inject shell code. This is how `claude` gets
///    routed through the proxy without setting its env container-wide.
/// 2. **Onboarding seed.** Write a default `.claude.json` (only when absent) so a
///    fresh container skips Claude's first-run wizard. Claude has no env/setting
///    for this (issue anthropics/claude-code#4714), so the file is the only lever.
///    It marks `hasCompletedOnboarding` (theme + welcome) and, for the working
///    directory, `hasTrustDialogAccepted` (the "trust this folder" prompt).
///    Claude's config is `$CLAUDE_CONFIG_DIR/.claude.json` when set, else
///    `$HOME/.claude.json` (home root, not a `.claude/` subdir). An image that
///    ships its own `.claude.json` is left untouched.
///
/// Everything written here is non-secret and lives in the container's ephemeral
/// filesystem (gone on `--rm`).
#[cfg(feature = "container-wrap")]
const CONTAINER_PRELUDE_SCRIPT: &str = concat!(
    r#"[ -n "$HOME" ] && mkdir -p "$HOME"; "#,
    r#"if [ -n "$ENWIRO_SHIMS" ]; then d=/tmp/enwiro-bin; mkdir -p "$d"; "#,
    r#"for n in $ENWIRO_SHIMS; do eval "c=\${ENWIRO_SHIM_$n}"; printf '%s' "$c" > "$d/$n" && chmod +x "$d/$n"; done; "#,
    r#"PATH="$d:$PATH"; export PATH; fi; "#,
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
        let res = resolve_launch(
            &LaunchResolveParams {
                env_name: "__nope__".to_string(),
                env_path: "/tmp".to_string(),
                command: "kitty".to_string(),
                args: vec![],
                interactive: false,
            },
            Path::new("/nonexistent-workspaces-dir"),
        );
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

    /// A `ContainerEnv` fixture for tests that only care about the command
    /// being run, not the environment identity around it.
    fn test_env(inject_proxy_shim: bool) -> ContainerEnv<'static> {
        ContainerEnv {
            image: "enwiro/x",
            environment_path: "/p",
            environment_name: "x",
            inject_proxy_shim,
            workspaces_directory: Path::new("/nonexistent-workspaces-dir"),
        }
    }

    #[test]
    fn image_tag_is_prefixed_env_name() {
        assert_eq!(container_image_tag("my-proj"), "enwiro/my-proj");
    }

    // GitHub-issue envs are named `<repo>#<n>`, but `#` is illegal in an OCI
    // repository name; without sanitizing, the image can never be tagged or
    // matched, so the container path silently and permanently falls back to
    // the host for every such env.
    #[test]
    fn image_tag_sanitizes_hash_in_issue_style_env_names() {
        assert_eq!(container_image_tag("headson#513"), "enwiro/headson-513");
    }

    #[test]
    fn sanitize_lowercases_and_collapses_runs_of_invalid_chars() {
        assert_eq!(sanitize_image_tag_component("My Env!!Name"), "my-env-name");
    }

    #[test]
    fn sanitize_trims_leading_and_trailing_separators() {
        assert_eq!(
            sanitize_image_tag_component("#leading-and-trailing#"),
            "leading-and-trailing"
        );
    }

    #[test]
    fn sanitize_is_a_no_op_on_an_already_valid_name() {
        assert_eq!(sanitize_image_tag_component("my-proj"), "my-proj");
    }

    // A cookbook-declared external path (e.g. a git worktree's main repo,
    // reported by the cookbook -- see `enwiro_sdk::external_paths`) gets
    // mounted alongside the env's own path. The daemon has no idea *why* the
    // path was declared; it just mounts whatever it finds.
    #[test]
    fn container_argv_mounts_a_declared_external_path() {
        let main_repo = tempfile::tempdir().unwrap();
        let env_path = tempfile::tempdir().unwrap();
        // Declarations live under `<workspaces_directory>/<environment_name>`
        // (enwiro's own per-env metadata dir, e.g. `~/.enwiro_envs/<name>`),
        // not under the project's own path -- the project might be a
        // bind-mounted clone or worktree anywhere on disk.
        let workspaces_dir = tempfile::tempdir().unwrap();
        let env_dir = workspaces_dir.path().join("x");
        let data = enwiro_sdk::external_paths::ExternalPathsFileData {
            version: enwiro_sdk::external_paths::SCHEMA_VERSION,
            paths: vec![main_repo.path().to_str().unwrap().to_string()],
        };
        std::fs::create_dir_all(enwiro_sdk::external_paths::external_paths_dir(&env_dir)).unwrap();
        std::fs::write(
            enwiro_sdk::external_paths::external_paths_dir(&env_dir)
                .join(enwiro_sdk::external_paths::external_paths_filename("git")),
            serde_json::to_vec(&data).unwrap(),
        )
        .unwrap();

        let env = ContainerEnv {
            image: "enwiro/x",
            environment_path: env_path.path().to_str().unwrap(),
            environment_name: "x",
            inject_proxy_shim: false,
            workspaces_directory: workspaces_dir.path(),
        };
        let argv = build_container_argv(&env, "bash", &[], true);
        let expected = format!(
            "type=bind,source={},target={}",
            main_repo.path().display(),
            main_repo.path().display()
        );
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--mount" && w[1] == expected),
            "{argv:?}"
        );
    }

    #[test]
    fn container_argv_mounts_env_path_at_same_path_and_sets_env() {
        let env = ContainerEnv {
            image: "enwiro/my-proj",
            environment_path: "/home/u/.enwiro_envs/my-proj/my-proj",
            environment_name: "my-proj",
            inject_proxy_shim: false,
            workspaces_directory: Path::new("/nonexistent-workspaces-dir"),
        };
        let argv = build_container_argv(&env, "bash", &["-l".to_string()], true);
        // Before the image: run flags + bind mount + cwd + ENWIRO_ENV (plus
        // `--user`/HOME on Linux, checked separately).
        let image_idx = argv.iter().position(|a| a == "enwiro/my-proj").unwrap();
        let head = &argv[..image_idx];
        assert_eq!(&argv[..3], &["run", "--rm", "-it"]);
        assert!(
            head.windows(2).any(|w| w[0] == "--mount"
                && w[1] == "type=bind,source=/home/u/.enwiro_envs/my-proj/my-proj,target=/home/u/.enwiro_envs/my-proj/my-proj"),
            "{argv:?}"
        );
        assert!(
            head.windows(2)
                .any(|w| w[0] == "-w" && w[1] == "/home/u/.enwiro_envs/my-proj/my-proj"),
            "{argv:?}"
        );
        assert!(
            head.windows(2)
                .any(|w| w[0] == "-e" && w[1] == "ENWIRO_ENV=my-proj"),
            "{argv:?}"
        );
        // The command is wrapped `sh -c <prelude> sh <command> <args>`.
        assert_eq!(&argv[image_idx + 1..image_idx + 3], &["sh", "-c"]);
        assert_eq!(&argv[image_idx + 4..], &["sh", "bash", "-l"]);
    }

    // On Linux the container runs as the host uid/gid under `--userns=keep-id`,
    // so bind-mounted files (owned by that user) are accessed as their owner and
    // the image's own passwd entry resolves `HOME` correctly (no override needed).
    #[test]
    #[cfg(target_os = "linux")]
    fn container_argv_runs_as_host_uid_on_linux() {
        let argv = build_container_argv(&test_env(false), "bash", &[], true);
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--user" && w[1] == format!("{}:{}", host_uid(), host_gid())),
            "{argv:?}"
        );
        assert!(argv.contains(&"--userns=keep-id".to_string()), "{argv:?}");
    }

    // A default `.claude.json` is seeded only if absent, then the real command
    // is exec'd, so a fresh container skips Claude's onboarding wizard (theme +
    // workspace-trust) without baking anything into the image.
    #[test]
    fn container_argv_seeds_onboarding_then_execs_command() {
        let argv = build_container_argv(&test_env(false), "claude", &[], true);
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
        let argv = build_container_argv(&test_env(false), "echo", &[], false);
        assert!(argv.contains(&"-i".to_string()));
        assert!(!argv.contains(&"-it".to_string()));
    }

    // With the proxy shim enabled, the container gets `--add-host` + the shim
    // env (`ENWIRO_SHIMS` + `ENWIRO_SHIM_claude`), NOT container-wide `ANTHROPIC_*`
    // and never a real token. The shim script carries the proxy base URL + a
    // freshly minted per-launch capability (not a fixed sentinel).
    #[test]
    fn container_argv_injects_claude_shim_when_enabled() {
        let argv = build_container_argv(&test_env(true), "claude", &[], true);
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--add-host" && w[1] == "host.containers.internal:host-gateway"),
            "{argv:?}"
        );
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "-e" && w[1] == "ENWIRO_SHIMS=claude"),
            "{argv:?}"
        );
        let shim = argv
            .windows(2)
            .find(|w| w[0] == "-e" && w[1].starts_with("ENWIRO_SHIM_claude="))
            .map(|w| w[1].clone())
            .expect("shim env present");
        assert!(
            shim.contains(&format!(
                "ANTHROPIC_BASE_URL=http://host.containers.internal:{}",
                crate::proxy::CLAUDE_PROXY_PORT
            )),
            "{shim}"
        );
        // The capability itself: a 64-char hex string (32 random bytes), not a
        // fixed/predictable value.
        let capability = shim
            .lines()
            .find_map(|line| line.strip_prefix("export CLAUDE_CODE_OAUTH_TOKEN="))
            .expect("capability line present");
        assert_eq!(capability.len(), 64, "{capability}");
        assert!(
            capability.chars().all(|c| c.is_ascii_hexdigit()),
            "{capability}"
        );
        // The container is ephemeral/non-root, so claude's self-updater can't
        // write anywhere and would just fail; the shim disables it.
        assert!(shim.contains("export DISABLE_AUTOUPDATER=1"), "{shim}");
        // The proxy vars live only in the shim, never as container-wide env.
        assert!(
            !argv
                .windows(2)
                .any(|w| w[0] == "-e" && w[1].starts_with("ANTHROPIC_BASE_URL=")),
            "{argv:?}"
        );
        // No real token anywhere.
        assert!(
            !argv.iter().any(|a| a.contains("ANTHROPIC_AUTH_TOKEN")),
            "{argv:?}"
        );
    }

    // Each launch mints its own capability, not a shared/fixed one: two calls
    // must not produce the same value.
    #[test]
    fn container_argv_mints_a_distinct_capability_per_call() {
        let extract_capability = |argv: &[String]| -> String {
            argv.windows(2)
                .find(|w| w[0] == "-e" && w[1].starts_with("ENWIRO_SHIM_claude="))
                .unwrap()[1]
                .lines()
                .find_map(|line| line.strip_prefix("export CLAUDE_CODE_OAUTH_TOKEN="))
                .unwrap()
                .to_string()
        };
        let first = extract_capability(&build_container_argv(&test_env(true), "claude", &[], true));
        let second =
            extract_capability(&build_container_argv(&test_env(true), "claude", &[], true));
        assert_ne!(first, second);
    }

    // Without the proxy shim (no token configured), no shim env or `--add-host`
    // is added. (The prelude script always mentions `ENWIRO_SHIM` as the reader,
    // so check for the `-e` injection specifically, not the substring.)
    #[test]
    fn container_argv_no_proxy_when_disabled() {
        let argv = build_container_argv(&test_env(false), "bash", &[], true);
        assert!(
            !argv
                .windows(2)
                .any(|w| w[0] == "-e" && w[1].starts_with("ENWIRO_SHIM")),
            "{argv:?}"
        );
        assert!(!argv.iter().any(|a| a == "--add-host"), "{argv:?}");
    }

    // A path containing a colon must not be split by the engine: `--mount` keeps
    // it intact as `source=`/`target=`, where `-v src:dst` would mis-split it.
    #[test]
    fn container_argv_mount_survives_colon_in_path() {
        let colon_path = "/home/u/.enwiro_envs/proj:1/proj:1";
        let env = ContainerEnv {
            image: "enwiro/x",
            environment_path: colon_path,
            environment_name: "x",
            inject_proxy_shim: false,
            workspaces_directory: Path::new("/nonexistent-workspaces-dir"),
        };
        let argv = build_container_argv(&env, "bash", &[], true);
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
        let env = ContainerEnv {
            image: "enwiro/my-proj",
            environment_path: "/p",
            environment_name: "my-proj",
            inject_proxy_shim: false,
            workspaces_directory: Path::new("/nonexistent-workspaces-dir"),
        };
        let args = build_terminal_container_args(&terminal_args, "podman", &env);
        // The terminal's own args come first (kitty parses them), then the
        // container invocation for the inner shell.
        assert_eq!(&args[0], "--session");
        assert_eq!(&args[1], "foo");
        assert_eq!(&args[2], "podman");
        assert_eq!(&args[3], "run");
        assert!(args.iter().any(|a| a == "bash"));
    }

    #[test]
    fn host_path_returns_command_unchanged_when_no_image() {
        // No `enwiro/__nope__` image exists → host path.
        let res = resolve_launch(
            &LaunchResolveParams {
                env_name: "__nope__".to_string(),
                env_path: "/tmp".to_string(),
                command: "echo".to_string(),
                args: vec!["hi".to_string()],
                interactive: false,
            },
            Path::new("/nonexistent-workspaces-dir"),
        );
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
