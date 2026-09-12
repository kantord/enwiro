//! Isolation launch decision (issue #540, ADR-0006). Given a resolved environment +
//! command, decide whether to run on the host or inside a microsandbox microVM, and
//! return the final `program` + `args`. The daemon is the single source of truth for
//! this decision; the `enw` CLI is a thin client that exec-replaces into whatever
//! `resolve_launch` returns.
//!
//! The isolation path is behind the `container-wrap` build feature (off by default);
//! without it `resolve_launch` always succeeds with the host command. Backend:
//! microsandbox, driven as a subprocess to its `msb` CLI (ADR-0006) -- podman/krun
//! are gone, along with the OCI-image-presence trigger they used.

use enwiro_sdk::process::ENWIRO_ENV_VAR;
use enwiro_sdk::rpc::{LaunchResolveParams, LaunchResolveResult};
use std::path::Path;

/// The microsandbox CLI. The only backend enwiro drives (ADR-0006) -- no
/// engine choice, no runtime override, unlike the podman/krun era.
#[cfg(feature = "container-wrap")]
const MSB_BIN: &str = "msb";

/// Terminal emulators enwiro runs as host chrome with a wrapped shell inside
/// (pilot: kitty only; see the launch-template registry plan). Matched by binary
/// basename; the terminal itself never needs display passthrough.
const TERMINAL_BINARIES: &[&str] = &["kitty"];

/// Shell run inside an *isolated terminal* (must exist in the image/snapshot).
#[cfg(feature = "container-wrap")]
const TERMINAL_ISOLATED_SHELL: &str = "bash";

/// Basename of a command path (everything after the last `/`).
fn command_basename(command: &str) -> &str {
    command.rsplit('/').next().unwrap_or(command)
}

/// True iff `command` (by basename) is a known terminal emulator.
fn is_terminal(command: &str) -> bool {
    TERMINAL_BINARIES.contains(&command_basename(command))
}

/// Decide how to launch `command` in the environment. Host path returns the
/// command unchanged; the isolated path returns `msb run ... <image> -- <command>`.
///
/// `Err` means the environment's project declared `isolate = true` but no
/// image/snapshot could be resolved (or its isolation config is malformed) --
/// the caller (the daemon's `launch.resolve` RPC handler) turns this into a
/// `launch.resolve` error, same as any other resolve failure. `enw wrap`
/// already treats *every* such error the same way: a loud warning, then a
/// bare, unwrapped host launch (ADR-0006 -- deliberately not a new "refuse to
/// launch" failure mode; the CLI has exactly one degrade path today and this
/// reuses it rather than adding a second).
//
// TODO(#540): the terminal handling below is a hardcoded pilot (kitty only, via
// `TERMINAL_BINARIES`) and the isolated-terminal branch duplicates the generic
// isolated branch. Replace both with a general launch-template registry
// (binary-name -> strategy) so new terminals and per-app rules don't require
// editing this function.
pub fn resolve_launch(
    params: &LaunchResolveParams,
    #[allow(unused_variables)] workspaces_directory: &Path,
) -> Result<LaunchResolveResult, String> {
    // Terminal template (issue #540): the terminal runs on the host; if the env
    // isolates, its inner command is the isolated invocation for the shell
    // (`kitty msb run ... <image> -- <shell>`), otherwise it uses `$SHELL`.
    if is_terminal(&params.command) {
        #[cfg(feature = "container-wrap")]
        if !params.env_name.is_empty()
            && let Some(image) = isolation_image(&params.env_path)?
        {
            let git_identity = host_git_identity(&params.env_path);
            let claude_token = claude_oauth_token();
            let env = IsolatedEnv {
                image: &image,
                environment_path: &params.env_path,
                environment_name: &params.env_name,
                inject_claude_secret: claude_token.is_some(),
                git_identity: git_identity
                    .as_ref()
                    .map(|(name, email)| (name.as_str(), email.as_str())),
                workspaces_directory,
            };
            return Ok(LaunchResolveResult {
                program: params.command.clone(),
                args: build_terminal_isolated_args(&params.args, &env),
                env_vars: claude_secret_env_vars(claude_token.as_deref()),
            });
        }

        // Host terminal: run it directly (it uses `$SHELL`); the client applies
        // cwd (= env path) + `ENWIRO_ENV`.
        return Ok(LaunchResolveResult {
            program: params.command.clone(),
            args: params.args.clone(),
            env_vars: launch_env_vars(&params.env_name),
        });
    }

    #[cfg(feature = "container-wrap")]
    if !params.env_name.is_empty()
        && let Some(image) = isolation_image(&params.env_path)?
    {
        let git_identity = host_git_identity(&params.env_path);
        let claude_token = claude_oauth_token();
        let env = IsolatedEnv {
            image: &image,
            environment_path: &params.env_path,
            environment_name: &params.env_name,
            inject_claude_secret: claude_token.is_some(),
            git_identity: git_identity
                .as_ref()
                .map(|(name, email)| (name.as_str(), email.as_str())),
            workspaces_directory,
        };
        return Ok(LaunchResolveResult {
            program: MSB_BIN.to_string(),
            args: build_isolated_argv(&env, &params.command, &params.args, params.interactive),
            // Everything the *guest* needs is delivered via `-e`/`--secret` inside
            // `build_isolated_argv`; the only thing the `msb` process itself (the
            // exec target here) needs from its own environment is the raw secret
            // value `--secret` reads at start time -- see `claude_secret_env_vars`.
            env_vars: claude_secret_env_vars(claude_token.as_deref()),
        });
    }

    Ok(LaunchResolveResult {
        program: params.command.clone(),
        args: params.args.clone(),
        env_vars: launch_env_vars(&params.env_name),
    })
}

/// Environment variables the daemon injects on a host-launched process: just
/// `ENWIRO_ENV` carrying the resolved environment name (empty on the home-dir
/// fallback).
fn launch_env_vars(environment_name: &str) -> Vec<(String, String)> {
    vec![(ENWIRO_ENV_VAR.to_string(), environment_name.to_string())]
}

/// A project's isolation policy (ADR-0006): `isolate = true` plus an optional
/// image/snapshot reference. Read from the `[isolation]` section of
/// `.enwiro.toml` (project layers, innermost wins), falling back per-key to a
/// personal default at `~/.config/enwiro/isolation.toml` -- no enwiro-shipped
/// default exists at either level. `#[serde(default)]` so a section missing
/// either key (or missing entirely) deserializes to "not isolated".
#[cfg(feature = "container-wrap")]
#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct IsolationPolicy {
    isolate: bool,
    image: Option<String>,
}

/// Resolve whether `environment_path` wants isolation and, if so, the
/// image/snapshot to boot. `Ok(None)` means "run on the host" (`isolate` is
/// false or unset anywhere). `Err` means `isolate = true` but no image
/// resolved from the project or the user's personal default, the isolation
/// config itself is malformed, or `msb` isn't on the daemon's `PATH`.
///
/// NOTE: the `msb` check resolves against the *daemon's* `PATH`, not the
/// calling user's -- a `systemd --user` daemon with a stripped `PATH` may
/// fail to find it even when the user's own shell would (same caveat the old
/// podman-engine lookup had).
#[cfg(feature = "container-wrap")]
fn isolation_image(environment_path: &str) -> Result<Option<String>, String> {
    let value = enwiro_sdk::config::build_cookbook_config(
        Path::new(environment_path),
        "isolation",
        &["isolate", "image"],
    )
    .map_err(|e| format!("could not resolve isolation policy: {e:#}"))?;
    let policy: IsolationPolicy =
        serde_json::from_value(value).map_err(|e| format!("malformed isolation policy: {e}"))?;
    if !policy.isolate {
        return Ok(None);
    }
    let image = policy.image.ok_or_else(|| {
        "project has `isolate = true` but no image/snapshot is configured -- set `image` \
         under [isolation] in .enwiro.toml, or a personal default under [isolation] in \
         ~/.config/enwiro/isolation.toml"
            .to_string()
    })?;
    if which::which(MSB_BIN).is_err() {
        return Err(format!(
            "project has `isolate = true` but the `{MSB_BIN}` CLI is not on PATH"
        ));
    }
    Ok(Some(image))
}

/// The host's effective git identity `(user.name, user.email)` for the
/// environment (issue #725). Resolved with `git -C <env_path> config --get`
/// rather than `--global` so conditional includes (`includeIf "gitdir:..."`)
/// and worktree config yield exactly the identity the user would commit with
/// on the host for this repo. `None` unless both halves resolve non-empty.
#[cfg(feature = "container-wrap")]
fn host_git_identity(environment_path: &str) -> Option<(String, String)> {
    let get = |key: &str| -> Option<String> {
        let output = std::process::Command::new("git")
            .args(["-C", environment_path, "config", "--get", key])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let value = String::from_utf8(output.stdout).ok()?;
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_string())
    };
    Some((get("user.name")?, get("user.email")?))
}

/// The parts of an isolated launch that stay constant regardless of what
/// command actually runs inside it. Bundled so `build_isolated_argv` and
/// `build_terminal_isolated_args` take one narrow, named thing instead of
/// loose positional fields that happen to travel together. Every field is
/// itself `Copy`, so the whole struct is too.
#[cfg(feature = "container-wrap")]
#[derive(Clone, Copy)]
struct IsolatedEnv<'a> {
    image: &'a str,
    environment_path: &'a str,
    environment_name: &'a str,
    inject_claude_secret: bool,
    /// The host's effective git `(user.name, user.email)` for this env, or
    /// `None` when it couldn't be resolved (no seeding then; the sandbox
    /// behaves as if the host had no identity configured either).
    git_identity: Option<(&'a str, &'a str)>,
    workspaces_directory: &'a Path,
}

/// Build the args for an *isolated terminal*: the terminal runs on the host
/// (it is the `program`) with its own `terminal_args` preserved, followed by
/// the `msb` invocation that runs the env's shell inside the image. The
/// terminal supplies the pty, so the inner shell is always interactive.
#[cfg(feature = "container-wrap")]
fn build_terminal_isolated_args(terminal_args: &[String], env: &IsolatedEnv) -> Vec<String> {
    let mut args = terminal_args.to_vec();
    args.push(MSB_BIN.to_string());
    args.extend(build_isolated_argv(env, TERMINAL_ISOLATED_SHELL, &[], true));
    args
}

/// `-v` args binding `path` into the sandbox at the identical path on both
/// sides (no translation).
///
/// Known limitation vs. the old podman path: `msb`'s mount flags (`-v`,
/// `--mount-dir`, ...) are all `SOURCE:DEST[:OPTIONS]` with no escaping, and
/// `msb` itself refuses a host path containing `:`, `,`, or `;` (verified
/// hands-on: "bind host path must not contain ',', ':', or ';'"). Podman's
/// `--mount type=bind,source=,target=` sidestepped this; microsandbox has no
/// equivalent colon-safe syntax. Accepted (ADR-0006): such paths are rare for
/// enwiro environment/project directories in practice, and the failure is a
/// clear `msb` error at launch rather than a silent mis-mount.
#[cfg(feature = "container-wrap")]
fn mount_arg(path: &str) -> [String; 2] {
    ["-v".to_string(), format!("{path}:{path}")]
}

/// The `msb --secret` source-env-var name carrying Claude's real token. Never
/// appears in `msb` argv (which is visible via `ps`/`msb inspect`) -- only in
/// the `msb` process's own environment, which `--secret` reads from at start
/// time and never inlines into the sandbox config.
#[cfg(feature = "container-wrap")]
const CLAUDE_TOKEN_HOST_ENV_VAR: &str = "ENWIRO_CLAUDE_TOKEN";

/// `LaunchResolveResult.env_vars` for the process about to be exec'd (i.e.
/// `msb` itself, not the guest) when Claude's token is configured: just the
/// raw value under [`CLAUDE_TOKEN_HOST_ENV_VAR`], for `--secret` to read.
/// Empty when no token is configured -- the daemon injects nothing.
#[cfg(feature = "container-wrap")]
fn claude_secret_env_vars(claude_token: Option<&str>) -> Vec<(String, String)> {
    claude_token
        .map(|token| vec![(CLAUDE_TOKEN_HOST_ENV_VAR.to_string(), token.to_string())])
        .unwrap_or_default()
}

/// Assemble the `run ...` args (the `msb` binary itself is the `program`, not
/// part of this). The env's project dir is bind-mounted at the *same* path it
/// has on the host, cwd set there, `ENWIRO_ENV` injected; `-t` when the
/// caller's stdin is a TTY, `--no-tty` otherwise.
///
/// Ownership needs no special handling here, unlike podman/krun: verified
/// hands-on that microsandbox's mounts already present a host-owned directory
/// as owned by the guest's own effective uid, in *either* direction -- a file
/// the guest creates (as root, or as an arbitrary non-root `-u`) comes back on
/// the host owned by the real host user, and `git commit` in a bind-mounted
/// repo works with no ownership flags at all. So there is no `--userns`/
/// `--user`-for-ownership dance to port over. `-u` below is set purely for
/// process hardening (non-root; Claude's `--dangerously-skip-permissions`
/// requires it), independent of the (already-solved) ownership question.
#[cfg(feature = "container-wrap")]
fn build_isolated_argv(
    env: &IsolatedEnv,
    command: &str,
    child_args: &[String],
    interactive: bool,
) -> Vec<String> {
    let IsolatedEnv {
        image,
        environment_path,
        environment_name,
        inject_claude_secret,
        git_identity,
        workspaces_directory,
    } = *env;
    let mut argv = vec!["run".to_string()];
    argv.push(if interactive { "-t" } else { "--no-tty" }.to_string());
    argv.extend(mount_arg(environment_path));
    // `environment_path` is often enwiro's own stable per-env symlink
    // (`<workspaces_directory>/<name>/<name>`), not the real underlying path --
    // that indirection is what lets an env keep the same address across
    // re-cooks even if what a cookbook produces underneath changes. But some
    // tools hard-code the *real* absolute path into their own metadata (e.g. a
    // git worktree's main repo references this worktree's own real path in its
    // reverse `.git/worktrees/<name>/gitdir` pointer), which won't resolve
    // inside a sandbox that only sees the symlink path. Mount the real path
    // too, purely additively.
    if let Ok(real_path) = std::fs::canonicalize(environment_path)
        && let Some(real_path) = real_path.to_str()
        && real_path != environment_path
    {
        argv.extend(mount_arg(real_path));
    }
    // Cookbooks may declare that this environment depends on additional host
    // paths beyond its own directory to function -- e.g. a git worktree's
    // `.git` is a pointer into a separate main repo holding the shared object
    // database. The daemon has no idea *why* a path is needed; it just mounts
    // whatever was declared, at the same absolute path on both sides.
    let env_dir = workspaces_directory.join(environment_name);
    for path in enwiro_sdk::external_paths::load_external_paths(&env_dir) {
        argv.extend(mount_arg(&path));
    }
    argv.push("-w".to_string());
    argv.push(environment_path.to_string());
    argv.push("-e".to_string());
    argv.push(format!("ENWIRO_ENV={environment_name}"));
    // Host git identity (issue #725): the host's ~/.gitconfig is not mounted,
    // so a fresh sandbox has no user.name/user.email and every `git commit`
    // fails with "Author identity unknown". Delivered as plain env (non-secret)
    // for the launch prelude to seed *global* git config from -- global scope,
    // not GIT_AUTHOR_*/GIT_COMMITTER_* vars, because those would take highest
    // precedence and silently override a deliberate per-repo identity in the
    // bind-mounted .git/config.
    if let Some((name, email)) = git_identity {
        argv.push("-e".to_string());
        argv.push(format!("ENWIRO_GIT_USER_NAME={name}"));
        argv.push("-e".to_string());
        argv.push(format!("ENWIRO_GIT_USER_EMAIL={email}"));
    }
    // Non-root hardening (issue #682's original motivation, ownership concern
    // now moot -- see this function's doc comment). Linux only: unverified
    // whether `-u` behaves the same under msb's macOS (Apple HVF) backend.
    if cfg!(target_os = "linux") {
        argv.push("-u".to_string());
        argv.push(format!("{}:{}", host_uid(), host_gid()));
    }
    // Claude auth (ADR-0006): the real token lives only in the `msb` process's
    // own environment (see `claude_secret_env_vars`); `--secret` reads it from
    // there and scopes it to api.anthropic.com, terminating the sandbox on any
    // attempt to send it elsewhere. This replaces the old host-side proxy +
    // per-launch capability token entirely -- one generic mechanism instead of
    // a bespoke one for this single credential.
    if inject_claude_secret {
        argv.push("--secret".to_string());
        argv.push(format!("{CLAUDE_TOKEN_HOST_ENV_VAR}@api.anthropic.com"));
        argv.push("--on-secret-violation".to_string());
        argv.push("block-and-terminate".to_string());
    }
    argv.push(image.to_string());
    argv.push("--".to_string());
    // Run the command through a small `sh` prelude that (1) seeds a default
    // `.claude.json` to skip claude's first-run wizard, (2) seeds git identity
    // when unresolvable otherwise, and (3) renames the `--secret`-delivered
    // token to the name claude actually reads, then `exec`s the real command.
    // Doing this at start (rather than baking into the image) keeps BYO
    // images untouched; everything written is non-secret (or, for the
    // renamed token, no more exposed than `--secret` already made it) and
    // lives in the sandbox's ephemeral filesystem.
    argv.push("sh".to_string());
    argv.push("-c".to_string());
    argv.push(ISOLATED_PRELUDE_SCRIPT.to_string());
    argv.push("sh".to_string()); // $0 for the exec'd shell
    argv.push(command.to_string());
    argv.extend(child_args.iter().cloned());
    argv
}

/// The daemon's real uid / gid, run as inside the sandbox purely for
/// non-root hardening (ownership of bind-mounted files is unaffected either
/// way -- see `build_isolated_argv`'s doc comment).
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

/// `sh -c` launch prelude, run before the actual command. Then `exec "$@"`
/// (the real command, supplied after the `sh` `$0`):
///
/// 1. **Claude token rename.** `--secret ENV@HOST` delivers the value inside
///    the guest as `MSB_<ENV>`, not `<ENV>` (verified hands-on) -- claude
///    itself reads `CLAUDE_CODE_OAUTH_TOKEN`, so rename it when present.
/// 2. **Onboarding seed.** Write a default `.claude.json` (only when absent) so a
///    fresh sandbox skips Claude's first-run wizard. Claude has no env/setting
///    for this (issue anthropics/claude-code#4714), so the file is the only lever.
///    It marks `hasCompletedOnboarding` (theme + welcome) and, for the working
///    directory, `hasTrustDialogAccepted` (the "trust this folder" prompt).
///    Claude's config is `$CLAUDE_CONFIG_DIR/.claude.json` when set, else
///    `$HOME/.claude.json`. An image that ships its own `.claude.json` is left
///    untouched.
/// 3. **Git identity seed** (issue #725). When the daemon passed the host's
///    identity (`ENWIRO_GIT_USER_NAME`/`_EMAIL`) and git can't already resolve
///    a `user.email` from any config the image ships (system, global, or the
///    bind-mounted repo's own -- the workdir is the repo), write it to global
///    config so `git commit` works out of the box. Seeding goes through
///    `git config --global` rather than `printf`ing a file so git does the
///    value escaping, and global scope keeps repo-local config authoritative.
///
/// Everything written here is non-secret (or, for the renamed token, no more
/// exposed than `--secret` already made it) and lives in the sandbox's
/// ephemeral filesystem.
#[cfg(feature = "container-wrap")]
const ISOLATED_PRELUDE_SCRIPT: &str = concat!(
    r#"[ -n "$HOME" ] && mkdir -p "$HOME"; "#,
    r#"[ -n "$MSB_"#,
    "ENWIRO_CLAUDE_TOKEN",
    r#"" ] && export CLAUDE_CODE_OAUTH_TOKEN="$MSB_"#,
    "ENWIRO_CLAUDE_TOKEN",
    r#""; "#,
    r#"if [ -n "$CLAUDE_CONFIG_DIR" ]; then f="$CLAUDE_CONFIG_DIR/.claude.json"; else f="$HOME/.claude.json"; fi; "#,
    r#"[ -f "$f" ] || { mkdir -p "$(dirname "$f")" && "#,
    r#"printf '{"hasCompletedOnboarding":true,"theme":"dark-ansi","projects":{"%s":{"hasTrustDialogAccepted":true,"hasCompletedProjectOnboarding":true}}}' "$(pwd)" > "$f"; }; "#,
    r#"if [ -n "$ENWIRO_GIT_USER_NAME" ] && [ -n "$ENWIRO_GIT_USER_EMAIL" ] && command -v git >/dev/null 2>&1 "#,
    r#"&& ! git config --get user.email >/dev/null 2>&1; then "#,
    r#"git config --global user.name "$ENWIRO_GIT_USER_NAME" && git config --global user.email "$ENWIRO_GIT_USER_EMAIL"; fi; "#,
    r#"exec "$@""#,
);

/// A cached Claude Code OAuth token to inject into a *claude*-capable launch,
/// or `None` if none is configured. Sources, first wins: the daemon's
/// `CLAUDE_CODE_OAUTH_TOKEN` env var, else a single line in
/// `$XDG_CONFIG_HOME/enwiro/claude_oauth_token` (defaulting to
/// `~/.config/enwiro/claude_oauth_token`). Mint one with `claude setup-token`.
///
/// One cached token is reused across envs (no per-env token proliferation);
/// see `claude_secret_env_vars` for how it reaches `msb`.
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
        // No isolation policy for this env (and/or feature off) -> host
        // terminal: run it directly so it uses `$SHELL`; cwd + ENWIRO_ENV
        // applied by the client.
        let res = resolve_launch(
            &LaunchResolveParams {
                env_name: "__nope__".to_string(),
                env_path: "/tmp".to_string(),
                command: "kitty".to_string(),
                args: vec![],
                interactive: false,
            },
            Path::new("/nonexistent-workspaces-dir"),
        )
        .unwrap();
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

    /// An `IsolatedEnv` fixture for tests that only care about the command
    /// being run, not the environment identity around it.
    fn test_env(inject_claude_secret: bool) -> IsolatedEnv<'static> {
        IsolatedEnv {
            image: "my-snapshot",
            environment_path: "/p",
            environment_name: "x",
            inject_claude_secret,
            git_identity: None,
            workspaces_directory: Path::new("/nonexistent-workspaces-dir"),
        }
    }

    // A cookbook-declared external path (e.g. a git worktree's main repo,
    // reported by the cookbook -- see `enwiro_sdk::external_paths`) gets
    // mounted alongside the env's own path. The daemon has no idea *why* the
    // path was declared; it just mounts whatever it finds.
    #[test]
    fn isolated_argv_mounts_a_declared_external_path() {
        let main_repo = tempfile::tempdir().unwrap();
        let env_path = tempfile::tempdir().unwrap();
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

        let env = IsolatedEnv {
            image: "my-snapshot",
            environment_path: env_path.path().to_str().unwrap(),
            environment_name: "x",
            inject_claude_secret: false,
            git_identity: None,
            workspaces_directory: workspaces_dir.path(),
        };
        let argv = build_isolated_argv(&env, "bash", &[], true);
        let expected = format!(
            "{}:{}",
            main_repo.path().display(),
            main_repo.path().display()
        );
        assert!(
            argv.windows(2).any(|w| w[0] == "-v" && w[1] == expected),
            "{argv:?}"
        );
    }

    // `environment_path` is often enwiro's own stable per-env symlink, not the
    // env's real underlying path -- e.g. a git worktree's main repo references
    // the worktree's own *real* absolute path in its reverse `.git/worktrees/
    // <name>/gitdir` pointer, which needs to resolve inside the sandbox too.
    #[test]
    fn isolated_argv_additionally_mounts_the_real_path_behind_a_symlinked_env_path() {
        let real_target = tempfile::tempdir().unwrap();
        let symlink_parent = tempfile::tempdir().unwrap();
        let symlinked_env_path = symlink_parent.path().join("env-symlink");
        std::os::unix::fs::symlink(real_target.path(), &symlinked_env_path).unwrap();

        let env = IsolatedEnv {
            environment_path: symlinked_env_path.to_str().unwrap(),
            ..test_env(false)
        };
        let argv = build_isolated_argv(&env, "bash", &[], true);

        let symlink_mount = format!("{0}:{0}", symlinked_env_path.display());
        let real_mount = format!("{0}:{0}", real_target.path().display());
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "-v" && w[1] == symlink_mount),
            "missing the primary symlink mount: {argv:?}"
        );
        assert!(
            argv.windows(2).any(|w| w[0] == "-v" && w[1] == real_mount),
            "missing the additive real-path mount: {argv:?}"
        );
    }

    #[test]
    fn isolated_argv_mounts_a_non_symlinked_env_path_only_once() {
        let real_dir = tempfile::tempdir().unwrap();
        let env = IsolatedEnv {
            environment_path: real_dir.path().to_str().unwrap(),
            ..test_env(false)
        };
        let argv = build_isolated_argv(&env, "bash", &[], true);

        let mount_count = argv.windows(2).filter(|w| w[0] == "-v").count();
        assert_eq!(mount_count, 1, "{argv:?}");
    }

    #[test]
    fn isolated_argv_mounts_env_path_at_same_path_and_sets_env() {
        let env = IsolatedEnv {
            image: "my-snapshot",
            environment_path: "/home/u/.enwiro_envs/my-proj/my-proj",
            environment_name: "my-proj",
            inject_claude_secret: false,
            git_identity: None,
            workspaces_directory: Path::new("/nonexistent-workspaces-dir"),
        };
        let argv = build_isolated_argv(&env, "bash", &["-l".to_string()], true);
        // Before the image: `run` + tty flag + mount + cwd + ENWIRO_ENV (plus
        // `-u` on Linux, checked separately).
        let image_idx = argv.iter().position(|a| a == "my-snapshot").unwrap();
        let head = &argv[..image_idx];
        assert_eq!(&argv[..2], &["run", "-t"]);
        assert!(
            head.windows(2).any(|w| w[0] == "-v"
                && w[1]
                    == "/home/u/.enwiro_envs/my-proj/my-proj:/home/u/.enwiro_envs/my-proj/my-proj"),
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
        // The command is wrapped `-- sh -c <prelude> sh <command> <args>`.
        assert_eq!(&argv[image_idx + 1..image_idx + 4], &["--", "sh", "-c"]);
        assert_eq!(&argv[image_idx + 5..], &["sh", "bash", "-l"]);
    }

    // Non-root hardening (issue #682's original motivation) is preserved even
    // though ownership itself needs no special handling under microsandbox.
    #[test]
    #[cfg(target_os = "linux")]
    fn isolated_argv_runs_as_host_uid_on_linux() {
        let argv = build_isolated_argv(&test_env(false), "bash", &[], true);
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "-u" && w[1] == format!("{}:{}", host_uid(), host_gid())),
            "{argv:?}"
        );
    }

    #[test]
    fn isolated_argv_uses_no_tty_when_not_interactive() {
        let argv = build_isolated_argv(&test_env(false), "echo", &[], false);
        assert!(argv.contains(&"--no-tty".to_string()));
        assert!(!argv.contains(&"-t".to_string()));
    }

    // A default `.claude.json` is seeded only if absent, then the real command
    // is exec'd, so a fresh sandbox skips Claude's onboarding wizard (theme +
    // workspace-trust) without baking anything into the image.
    #[test]
    fn isolated_argv_seeds_onboarding_then_execs_command() {
        let argv = build_isolated_argv(&test_env(false), "claude", &[], true);
        let script = &argv[argv.iter().position(|a| a == "-c").unwrap() + 1];
        assert!(script.contains("hasCompletedOnboarding"), "{script}");
        assert!(script.contains("hasTrustDialogAccepted"), "{script}");
        assert!(script.contains(r#"f="$HOME/.claude.json""#), "{script}");
        assert!(script.contains(r#""$(pwd)""#), "{script}");
        assert!(
            script.contains(".claude.json") && script.contains("[ -f"),
            "seeds only when absent: {script}"
        );
        assert!(script.trim_end().ends_with(r#"exec "$@""#), "{script}");
    }

    // Host git identity (issue #725): the host's ~/.gitconfig is not mounted,
    // so without seeding every `git commit` in a fresh sandbox fails with
    // "Author identity unknown". The daemon passes the identity as env...
    #[test]
    fn isolated_argv_passes_git_identity_env_when_known() {
        let env = IsolatedEnv {
            git_identity: Some(("Jane Dev", "jane@dev.example")),
            ..test_env(false)
        };
        let argv = build_isolated_argv(&env, "bash", &[], true);
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "-e" && w[1] == "ENWIRO_GIT_USER_NAME=Jane Dev"),
            "{argv:?}"
        );
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "-e" && w[1] == "ENWIRO_GIT_USER_EMAIL=jane@dev.example"),
            "{argv:?}"
        );
    }

    #[test]
    fn isolated_argv_omits_git_identity_env_when_unknown() {
        let argv = build_isolated_argv(&test_env(false), "bash", &[], true);
        assert!(
            !argv
                .windows(2)
                .any(|w| w[0] == "-e" && w[1].starts_with("ENWIRO_GIT_USER")),
            "{argv:?}"
        );
    }

    // ...and the prelude seeds *global* config from it, but only when git can't
    // already resolve an identity from config the image (or the mounted repo)
    // ships -- so a deliberate image/repo identity always wins over the host's.
    #[test]
    fn prelude_seeds_git_identity_only_when_unresolvable() {
        assert!(
            ISOLATED_PRELUDE_SCRIPT.contains("! git config --get user.email"),
            "{ISOLATED_PRELUDE_SCRIPT}"
        );
        assert!(
            ISOLATED_PRELUDE_SCRIPT
                .contains(r#"git config --global user.name "$ENWIRO_GIT_USER_NAME""#),
            "{ISOLATED_PRELUDE_SCRIPT}"
        );
        assert!(
            ISOLATED_PRELUDE_SCRIPT
                .contains(r#"git config --global user.email "$ENWIRO_GIT_USER_EMAIL""#),
            "{ISOLATED_PRELUDE_SCRIPT}"
        );
        assert!(
            ISOLATED_PRELUDE_SCRIPT.contains("command -v git"),
            "{ISOLATED_PRELUDE_SCRIPT}"
        );
    }

    // Resolved via `git -C <env_path> config --get` (not `--global`) so the
    // answer is the *effective* identity for that repo -- conditional includes
    // and repo-local overrides included. A local config makes this test
    // deterministic regardless of the machine's own global identity.
    #[test]
    fn host_git_identity_resolves_effective_identity_for_the_repo() {
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(repo.path())
                .args(args)
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} failed");
        };
        git(&["init"]);
        git(&["config", "user.name", "Jane Dev"]);
        git(&["config", "user.email", "jane@dev.example"]);
        assert_eq!(
            host_git_identity(repo.path().to_str().unwrap()),
            Some(("Jane Dev".to_string(), "jane@dev.example".to_string()))
        );
    }

    #[test]
    fn host_git_identity_is_none_when_git_cannot_run_there() {
        assert_eq!(host_git_identity("/nonexistent-enwiro-env-path"), None);
    }

    // With the Claude secret enabled, the sandbox gets `--secret ...@api.anthropic.com`
    // plus `--on-secret-violation block-and-terminate`, never a raw token in argv.
    #[test]
    fn isolated_argv_injects_claude_secret_when_enabled() {
        let argv = build_isolated_argv(&test_env(true), "claude", &[], true);
        assert!(
            argv.windows(2).any(|w| w[0] == "--secret"
                && w[1] == format!("{CLAUDE_TOKEN_HOST_ENV_VAR}@api.anthropic.com")),
            "{argv:?}"
        );
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--on-secret-violation" && w[1] == "block-and-terminate"),
            "{argv:?}"
        );
    }

    #[test]
    fn isolated_argv_no_secret_when_disabled() {
        let argv = build_isolated_argv(&test_env(false), "bash", &[], true);
        assert!(!argv.iter().any(|a| a == "--secret"), "{argv:?}");
        assert!(
            !argv.iter().any(|a| a == "--on-secret-violation"),
            "{argv:?}"
        );
    }

    // The `msb` process itself (about to be exec'd) needs the raw token in its
    // own environment for `--secret` to read; it must never appear in argv.
    #[test]
    fn claude_secret_env_vars_carries_the_raw_token() {
        assert_eq!(
            claude_secret_env_vars(Some("tok-123")),
            vec![(CLAUDE_TOKEN_HOST_ENV_VAR.to_string(), "tok-123".to_string())]
        );
        assert_eq!(claude_secret_env_vars(None), Vec::<(String, String)>::new());
    }

    #[test]
    fn prelude_renames_the_secret_delivered_token_for_claude() {
        // `--secret ENV@HOST` delivers the value inside the guest as
        // `MSB_<ENV>`; claude itself reads `CLAUDE_CODE_OAUTH_TOKEN`.
        assert!(
            ISOLATED_PRELUDE_SCRIPT.contains(&format!("MSB_{CLAUDE_TOKEN_HOST_ENV_VAR}")),
            "{ISOLATED_PRELUDE_SCRIPT}"
        );
        assert!(
            ISOLATED_PRELUDE_SCRIPT.contains("export CLAUDE_CODE_OAUTH_TOKEN="),
            "{ISOLATED_PRELUDE_SCRIPT}"
        );
    }

    // A path containing a colon can't be mounted at all (verified hands-on:
    // `msb` itself refuses it) -- documented as a known limitation rather than
    // silently mis-mounted, so at minimum this doesn't regress into producing
    // an argv that *looks* fine but breaks at `msb`'s own argument parsing in
    // a different way (e.g. splitting mid-path).
    #[test]
    fn mount_arg_keeps_path_intact_for_msb_to_validate() {
        let colon_path = "/home/u/.enwiro_envs/proj:1/proj:1";
        assert_eq!(
            mount_arg(colon_path),
            ["-v".to_string(), format!("{colon_path}:{colon_path}")]
        );
    }

    // A containerized terminal must preserve the terminal's own args
    // (e.g. `kitty --session foo`), not just run the inner sandbox shell.
    #[test]
    fn terminal_isolated_args_preserve_terminal_args() {
        let terminal_args = vec!["--session".to_string(), "foo".to_string()];
        let env = IsolatedEnv {
            image: "my-snapshot",
            environment_path: "/p",
            environment_name: "my-proj",
            inject_claude_secret: false,
            git_identity: None,
            workspaces_directory: Path::new("/nonexistent-workspaces-dir"),
        };
        let args = build_terminal_isolated_args(&terminal_args, &env);
        // The terminal's own args come first (kitty parses them), then the
        // `msb` invocation for the inner shell.
        assert_eq!(&args[0], "--session");
        assert_eq!(&args[1], "foo");
        assert_eq!(&args[2], MSB_BIN);
        assert_eq!(&args[3], "run");
        assert!(args.iter().any(|a| a == "bash"));
    }

    #[test]
    fn host_path_returns_command_unchanged_when_not_isolated() {
        // No `.enwiro.toml` isolation policy for this env -> host path.
        let res = resolve_launch(
            &LaunchResolveParams {
                env_name: "__nope__".to_string(),
                env_path: "/tmp".to_string(),
                command: "echo".to_string(),
                args: vec!["hi".to_string()],
                interactive: false,
            },
            Path::new("/nonexistent-workspaces-dir"),
        )
        .unwrap();
        assert_eq!(res.program, "echo");
        assert_eq!(res.args, vec!["hi".to_string()]);
        assert_eq!(
            res.env_vars,
            vec![("ENWIRO_ENV".to_string(), "__nope__".to_string())]
        );
    }

    // `isolate = true` with no image resolvable anywhere is an error, not a
    // silent host fallback -- the caller turns this into the same
    // `launch.resolve` failure path a down daemon already has.
    #[test]
    fn isolate_true_without_an_image_is_an_error() {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join(".enwiro.toml"),
            "[isolation]\nisolate = true\n",
        )
        .unwrap();
        let err = resolve_launch(
            &LaunchResolveParams {
                env_name: "x".to_string(),
                env_path: project.path().to_str().unwrap().to_string(),
                command: "bash".to_string(),
                args: vec![],
                interactive: false,
            },
            Path::new("/nonexistent-workspaces-dir"),
        )
        .unwrap_err();
        assert!(err.contains("isolate"), "{err}");
        assert!(err.contains("image"), "{err}");
    }

    #[test]
    fn host_env_vars_carry_enwiro_env() {
        assert_eq!(
            launch_env_vars("my-proj"),
            vec![("ENWIRO_ENV".to_string(), "my-proj".to_string())]
        );
    }
}
