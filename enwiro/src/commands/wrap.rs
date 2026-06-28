use anyhow::{Context, anyhow};

use crate::CommandContext;
use crate::context::CookConfig;

use enwiro_sdk::process::ProcessSpec;
use enwiro_sdk::rpc::{EnwiroRpcClient, LaunchResolveParams, LaunchResolveResult, connect};

use std::io::{self, IsTerminal, Write};
use std::os::unix::process::CommandExt;

#[derive(clap::Args)]
#[command(
    author,
    version,
    about = "Run an application/command inside an environment"
)]
pub struct WrapArgs {
    pub command_name: String,
    pub environment_name: Option<String>,

    #[clap(allow_hyphen_values = true, num_args = 0.., last=true)]
    child_args: Option<Vec<String>>,
}

pub fn wrap<W: Write>(context: &mut CommandContext<W>, args: WrapArgs) -> anyhow::Result<()> {
    let selected_environment =
        match context.get_or_cook_environment(&args.environment_name, &CookConfig::default()) {
            Ok(env) => Some(env),
            Err(e) => {
                tracing::warn!(error = %e, "Could not resolve environment");
                None
            }
        };

    let environment_path: String = match &selected_environment {
        Some(environment) => environment.path.clone(),
        None => {
            tracing::warn!("No matching environment found, falling back to home directory");

            home::home_dir()
                .context("Could not determine user home directory")?
                .into_os_string()
                .into_string()
                .map_err(|_| anyhow!("Could not convert home directory path to string"))?
        }
    };

    let environment_name: String = selected_environment
        .as_ref()
        .map(|environment| environment.name.clone())
        .unwrap_or_default();
    let child_args: Vec<String> = args.child_args.unwrap_or_default();

    // Ask the daemon *how* to launch (host vs. containerized, #540): it owns the
    // launch decision. Env resolution/cooking stays CLI-side (daemon can't cook
    // yet, #522).
    let resolved = match resolve_launch_via_daemon(
        &environment_name,
        &environment_path,
        &args.command_name,
        &child_args,
        io::stdin().is_terminal(),
    ) {
        Ok(resolved) => resolved,
        Err(e) => {
            // The daemon is the source of truth for how to launch. If we can't
            // get an answer we don't half-wrap: report loudly (stderr + desktop
            // notification) and exec the command *bare* (no cwd, no ENWIRO_ENV,
            // no isolation). The message distinguishes "daemon not running" from
            // a daemon-side resolve error so a real error isn't mislabelled.
            let message = e.degraded_launch_message(&args.command_name);
            tracing::warn!(error = %message, "daemon launch.resolve failed; launching unwrapped");
            eprintln!("{message}");
            context.notifier.notify_error(&message);
            let err = ProcessSpec::new(args.command_name.clone())
                .args(child_args)
                .into_command()
                .exec();
            return Err(anyhow!(err).context(format!("Failed to exec {}", args.command_name)));
        }
    };

    let program = resolved.program;
    // Apply what the daemon returned (program, args, env vars) and exec-replace;
    // cwd = env path.
    let mut command = ProcessSpec::new(program.clone())
        .args(resolved.args)
        .into_command();
    command.current_dir(&environment_path);
    command.envs(resolved.env_vars);
    let err = command.exec();

    Err(anyhow!(err).context(format!("Failed to exec {program}")))
}

/// Why a daemon `launch.resolve` attempt failed. Kept distinct so the degraded
/// (bare) launch is reported accurately instead of always blaming a down daemon.
enum DaemonLaunchError {
    /// Couldn't reach the daemon (runtime build or connect failed): daemon down.
    Unreachable(String),
    /// The daemon answered but `launch.resolve` itself returned an error.
    Resolve(String),
}

impl DaemonLaunchError {
    /// One-line, user-facing explanation of the degraded bare launch.
    fn degraded_launch_message(&self, command: &str) -> String {
        let cause = match self {
            Self::Unreachable(e) => format!("daemon not running ({e})"),
            Self::Resolve(e) => format!("daemon error ({e})"),
        };
        format!("enwiro: {cause}; launching `{command}` unwrapped (no environment, no isolation)")
    }
}

/// Block on the daemon's `launch.resolve` RPC. The empty env name (home-dir
/// fallback) is passed through too; the daemon simply returns the host command.
fn resolve_launch_via_daemon(
    environment_name: &str,
    environment_path: &str,
    command: &str,
    args: &[String],
    interactive: bool,
) -> Result<LaunchResolveResult, DaemonLaunchError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| {
            DaemonLaunchError::Unreachable(format!("could not start async runtime: {e}"))
        })?;

    rt.block_on(async {
        let client = connect()
            .await
            .map_err(|e| DaemonLaunchError::Unreachable(e.to_string()))?;
        EnwiroRpcClient::launch_resolve(
            &client,
            LaunchResolveParams {
                env_name: environment_name.to_string(),
                env_path: environment_path.to_string(),
                command: command.to_string(),
                args: args.to_vec(),
                interactive,
            },
        )
        .await
        .map_err(|e| DaemonLaunchError::Resolve(e.to_string()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // A daemon-side resolve error must not be mislabelled as "daemon not running".
    #[test]
    fn unreachable_is_reported_as_daemon_not_running() {
        let msg = DaemonLaunchError::Unreachable("connection refused".to_string())
            .degraded_launch_message("nvim");
        assert!(msg.contains("daemon not running"), "{msg}");
        assert!(msg.contains("connection refused"), "{msg}");
        assert!(msg.contains("nvim"), "{msg}");
        assert!(!msg.contains("daemon error"), "{msg}");
    }

    #[test]
    fn resolve_error_is_not_reported_as_daemon_not_running() {
        let msg =
            DaemonLaunchError::Resolve("bad request".to_string()).degraded_launch_message("nvim");
        assert!(msg.contains("daemon error"), "{msg}");
        assert!(msg.contains("bad request"), "{msg}");
        assert!(
            !msg.contains("daemon not running"),
            "resolve errors must not be mislabelled as a down daemon: {msg}"
        );
    }
}
