use anyhow::anyhow;

use crate::CommandContext;
use crate::commands::wrap::resolve_launch_via_daemon;
use crate::environments::Environment;

use enwiro_sdk::process::ProcessSpec;

use std::io::{self, IsTerminal, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(200);
const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const FALLBACK_SHELL: &str = "/bin/sh";

#[derive(clap::Args)]
#[command(
    author,
    version,
    about = "Run your shell inside the current environment, waiting while it is being prepared. \
             Intended as a terminal emulator's configured shell: with no environment it \
             degrades to the plain shell."
)]
pub struct ShellArgs {
    /// Seconds to wait for an environment that is still being prepared before
    /// falling back to a plain shell. 0 waits forever.
    #[arg(long, default_value_t = DEFAULT_TIMEOUT_SECS)]
    pub timeout: u64,

    /// Arguments forwarded verbatim to the shell (e.g. `-c <command>`).
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub shell_args: Vec<String>,
}

pub fn shell<W: Write>(context: &mut CommandContext<W>, args: ShellArgs) -> anyhow::Result<()> {
    let program = resolve_shell();
    match resolve_environment(context, args.timeout) {
        Some(environment) => exec_wrapped(&program, &args.shell_args, &environment),
        None => exec_plain(&program, &args.shell_args),
    }
}

/// The environment to wrap the shell in, or `None` for a plain shell.
///
/// Never cooks: activation owns cooking (ADR-0005). A recipe-cache hit with
/// no environment yet means a cook is presumably in flight in the activating
/// process, so wait for the environment to appear, bounded by `timeout_secs`.
fn resolve_environment<W: Write>(
    context: &CommandContext<W>,
    timeout_secs: u64,
) -> Option<Environment> {
    let name = match context.resolve_environment_name(&None) {
        Ok(name) => name,
        Err(e) => {
            tracing::debug!(error = %e, "No environment resolved, starting plain shell");
            return None;
        }
    };
    let flat_name = name.replace('/', "-");
    if let Ok(environment) = Environment::get_one(&context.config.workspaces_directory, &flat_name)
    {
        return Some(environment);
    }
    if !context.find_recipe_in_cache_by_name(&name) {
        tracing::debug!(name = %name, "No environment and no recipe, starting plain shell");
        return None;
    }
    wait_for_environment(
        &context.config.workspaces_directory,
        &name,
        &flat_name,
        timeout_secs,
    )
}

/// Poll until the environment appears (the cook in the activating process
/// finishing), rendering progress on stderr. Returns `None` on timeout.
fn wait_for_environment(
    workspaces_directory: &str,
    name: &str,
    flat_name: &str,
    timeout_secs: u64,
) -> Option<Environment> {
    let started = Instant::now();
    let tty = io::stderr().is_terminal();
    if !tty {
        eprintln!("enwiro: preparing environment '{name}'...");
    }
    let mut frame = 0usize;
    loop {
        if let Ok(environment) = Environment::get_one(workspaces_directory, flat_name) {
            if tty {
                clear_progress_line();
            }
            return Some(environment);
        }
        if timeout_secs > 0 && started.elapsed() >= Duration::from_secs(timeout_secs) {
            if tty {
                clear_progress_line();
            }
            eprintln!(
                "enwiro: environment '{name}' was not ready after {timeout_secs}s; \
                 starting a plain shell (new terminals will use it once it is ready)"
            );
            return None;
        }
        if tty {
            let spinner = SPINNER_FRAMES[frame % SPINNER_FRAMES.len()];
            let elapsed = started.elapsed().as_secs();
            eprint!("\r\x1b[2K{spinner} preparing environment '{name}'... ({elapsed}s)");
            let _ = io::stderr().flush();
            frame += 1;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn clear_progress_line() {
    eprint!("\r\x1b[2K");
    let _ = io::stderr().flush();
}

/// Wrap-parity launch: daemon `launch.resolve`, then exec-replace with cwd +
/// env vars. Unlike `wrap`, a daemon failure degrades to the plain shell
/// silently: as a terminal's configured shell this must never nag on every
/// window.
fn exec_wrapped(
    program: &str,
    shell_args: &[String],
    environment: &Environment,
) -> anyhow::Result<()> {
    let resolved = match resolve_launch_via_daemon(
        &environment.name,
        &environment.path,
        program,
        shell_args,
        io::stdin().is_terminal(),
    ) {
        Ok(resolved) => resolved,
        Err(e) => {
            let message = e.degraded_launch_message(program);
            tracing::warn!(error = %message, "daemon launch.resolve failed; starting plain shell");
            return exec_plain(program, shell_args);
        }
    };

    let mut command = ProcessSpec::new(resolved.program.clone())
        .args(resolved.args)
        .into_command();
    command.current_dir(&environment.path);
    command.envs(resolved.env_vars);
    let err = command.exec();

    Err(anyhow!(err).context(format!("Failed to exec {}", resolved.program)))
}

fn exec_plain(program: &str, shell_args: &[String]) -> anyhow::Result<()> {
    let err = ProcessSpec::new(program.to_string())
        .args(shell_args.to_vec())
        .into_command()
        .exec();
    Err(anyhow!(err).context(format!("Failed to exec {program}")))
}

fn resolve_shell() -> String {
    choose_shell(std::env::var("SHELL").ok(), login_shell_from_passwd())
}

/// `$SHELL` unless it is unset, empty, or points back at enwiro itself (a
/// terminal configured to run `enw shell` may leave `$SHELL` set to it, which
/// would fork-loop); then the passwd login shell; then `/bin/sh`.
fn choose_shell(env_shell: Option<String>, login_shell: Option<String>) -> String {
    if let Some(shell) = env_shell
        && !shell.is_empty()
        && !is_enwiro_binary(&shell)
    {
        return shell;
    }
    login_shell
        .filter(|shell| !shell.is_empty())
        .unwrap_or_else(|| FALLBACK_SHELL.to_string())
}

fn is_enwiro_binary(path: &str) -> bool {
    matches!(
        Path::new(path).file_name().and_then(|name| name.to_str()),
        Some("enw") | Some("enwiro")
    )
}

fn login_shell_from_passwd() -> Option<String> {
    // SAFETY: getpwuid returns a pointer into static libc storage; it is read
    // immediately and not held across any other libc call.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() {
            return None;
        }
        let shell = (*pw).pw_shell;
        if shell.is_null() {
            return None;
        }
        std::ffi::CStr::from_ptr(shell)
            .to_str()
            .ok()
            .map(str::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    use crate::commands::adapter::EnwiroAdapterNone;
    use crate::test_utils::test_utilities::{
        AdapterLog, FakeContext, NotificationLog, context_object,
    };

    #[test]
    fn choose_shell_prefers_env_shell() {
        assert_eq!(
            choose_shell(Some("/bin/zsh".into()), Some("/bin/bash".into())),
            "/bin/zsh"
        );
    }

    #[test]
    fn choose_shell_skips_enwiro_as_shell() {
        assert_eq!(
            choose_shell(Some("/usr/bin/enw".into()), Some("/bin/bash".into())),
            "/bin/bash"
        );
    }

    #[test]
    fn choose_shell_falls_back_to_login_shell() {
        assert_eq!(choose_shell(None, Some("/bin/fish".into())), "/bin/fish");
        assert_eq!(
            choose_shell(Some(String::new()), Some("/bin/fish".into())),
            "/bin/fish"
        );
    }

    #[test]
    fn choose_shell_final_fallback_is_bin_sh() {
        assert_eq!(choose_shell(None, None), FALLBACK_SHELL);
    }

    /// Same layout as `FakeContext::create_mock_environment`, standalone so a
    /// helper thread can create the environment mid-wait.
    fn create_environment_at(workspaces_directory: &str, name: &str) {
        let env_dir = Path::new(workspaces_directory).join(name);
        std::fs::create_dir(&env_dir).unwrap();
        let target_dir = env_dir.join(".target");
        std::fs::create_dir(&target_dir).unwrap();
        std::os::unix::fs::symlink(&target_dir, env_dir.join(name)).unwrap();
    }

    #[rstest]
    fn existing_environment_is_returned_immediately(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut ctx, _, _) = context_object;
        // The fixture adapter reports "foobaz" as the active environment.
        ctx.create_mock_environment("foobaz");

        let environment = resolve_environment(&ctx, 1);
        assert_eq!(environment.unwrap().name, "foobaz");
    }

    #[rstest]
    fn no_recipe_means_plain_shell_without_waiting(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, ctx, _, _) = context_object;
        // No environment and an empty recipe cache: must not wait out the timeout.
        ctx.write_cache_entries(&[]);

        let started = Instant::now();
        assert!(resolve_environment(&ctx, 30).is_none());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[rstest]
    fn no_adapter_means_plain_shell(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut ctx, _, _) = context_object;
        ctx.adapter = Box::new(EnwiroAdapterNone {});

        assert!(resolve_environment(&ctx, 30).is_none());
    }

    #[rstest]
    fn global_env_flag_selects_the_environment(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut ctx, _, _) = context_object;
        ctx.create_mock_environment("other");
        ctx.global_env = Some("other".to_string());

        let environment = resolve_environment(&ctx, 1);
        assert_eq!(environment.unwrap().name, "other");
    }

    #[rstest]
    fn waits_for_an_in_flight_cook_to_finish(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, ctx, _, _) = context_object;
        // A recipe for the active environment exists, so a cook is presumed
        // in flight; the environment appears while we wait.
        ctx.write_cache_entry("git", "foobaz");
        let workspaces_directory = ctx.config.workspaces_directory.clone();
        let cook = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            create_environment_at(&workspaces_directory, "foobaz");
        });

        let environment = resolve_environment(&ctx, 10);
        cook.join().unwrap();
        assert_eq!(environment.unwrap().name, "foobaz");
    }

    #[rstest]
    fn times_out_to_plain_shell_when_the_cook_never_finishes(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, ctx, _, _) = context_object;
        ctx.write_cache_entry("git", "foobaz");

        let started = Instant::now();
        assert!(resolve_environment(&ctx, 1).is_none());
        assert!(started.elapsed() >= Duration::from_secs(1));
    }
}
