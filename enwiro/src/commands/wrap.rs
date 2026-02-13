use anyhow::{Context, anyhow};

use crate::CommandContext;

use std::{env, io::Write, process::Command};
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
    let selected_environment = match context.get_or_cook_environment(&args.environment_name) {
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
    env::set_current_dir(environment_path).context("Failed to change directory")?;

    let environment_name: String = match &selected_environment {
        Some(environment) => environment.name.clone(),
        None => String::from(""),
    };

    let mut child = Command::new(args.command_name)
        .env("ENWIRO_ENV", environment_name)
        .args(match args.child_args {
            Some(x) => x.into_iter().map(|x| x.to_string()).collect(),
            None => vec![],
        })
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .context("Failed to execute command")?;

    let status = child.wait().context("Command wasn't running")?;
    tracing::debug!(exit_code = ?status.code(), "Child process exited");

    Ok(())
}
