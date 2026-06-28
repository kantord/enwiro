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

    // Ask the daemon *how* to launch: host vs. containerized (issue #540). The
    // daemon owns the launch decision; this CLI just exec-replaces into whatever
    // it returns (program, args, env vars incl. ENWIRO_ENV) with cwd = env path.
    // Env resolution/cooking stays here (the daemon can't cook yet — #522).
    let resolved = match resolve_launch_via_daemon(
        &environment_name,
        &environment_path,
        &args.command_name,
        &child_args,
        io::stdin().is_terminal(),
    ) {
        Ok(resolved) => resolved,
        Err(e) => {
            // The daemon is the source of truth for how to launch. If it isn't
            // running we don't half-wrap: report loudly (stderr + desktop
            // notification) and exec the command *bare* — no cwd, no
            // ENWIRO_ENV, no isolation.
            tracing::warn!(error = %e, "daemon unavailable; launching unwrapped");
            eprintln!(
                "enwiro: daemon not running ({e}); launching `{}` unwrapped (no environment, no isolation)",
                args.command_name
            );
            context.notifier.notify_error(&format!(
                "enwiro daemon not running — launched `{}` unwrapped (no environment, no isolation)",
                args.command_name
            ));
            let err = ProcessSpec::new(args.command_name.clone())
                .args(child_args)
                .into_command()
                .exec();
            return Err(anyhow!(err).context(format!("Failed to exec {}", args.command_name)));
        }
    };

    let program = resolved.program;
    // The daemon decided the program, args, and env vars (incl. ENWIRO_ENV);
    // we only set the working directory (= env path) and exec-replace into it.
    let mut command = ProcessSpec::new(program.clone())
        .args(resolved.args)
        .into_command();
    command.current_dir(&environment_path);
    command.envs(resolved.env_vars);
    let err = command.exec();

    Err(anyhow!(err).context(format!("Failed to exec {program}")))
}

/// Block on the daemon's `launch.resolve` RPC. The empty env name (home-dir
/// fallback) is passed through too; the daemon simply returns the host command.
fn resolve_launch_via_daemon(
    environment_name: &str,
    environment_path: &str,
    command: &str,
    args: &[String],
    interactive: bool,
) -> anyhow::Result<LaunchResolveResult> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("could not start async runtime")?;

    rt.block_on(async {
        let client = connect()
            .await
            .context("could not connect to enwiro-daemon")?;
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
        .map_err(|e| anyhow!("daemon launch.resolve error: {e}"))
    })
}
