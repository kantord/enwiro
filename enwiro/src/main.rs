mod client;
mod commands;
mod config;
mod context;
mod environments;
mod notifier;
mod plugin;
mod test_utils;

use anyhow::Context;
use clap::Parser;
use commands::activate::{ActivateArgs, activate};
use commands::list_all::{ListAllArgs, list_all};
use commands::list_environments::{ListEnvironmentsArgs, list_environments};
use commands::show_path::{ShowPathArgs, show_path};
use commands::wrap::{WrapArgs, wrap};
use config::ConfigurationValues;
use context::CommandContext;
use std::fs::create_dir;
use std::io::Write;
use std::path::Path;

#[derive(Parser)]
enum EnwiroCli {
    Activate(ActivateArgs),
    ListEnvironments(ListEnvironmentsArgs),
    ListAll(ListAllArgs),
    ShowPath(ShowPathArgs),
    Wrap(WrapArgs),
}

fn ensure_can_run<W: Write>(config: &CommandContext<W>) -> anyhow::Result<()> {
    let environments_directory = Path::new(&config.config.workspaces_directory);
    if !environments_directory.exists() {
        create_dir(environments_directory).context(
            "Workspace directory does not exist and could not be automatically created.",
        )?;
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let _guard = enwiro_logging::init_logging("enwiro.log");

    let args = EnwiroCli::parse();
    let config: ConfigurationValues =
        confy::load("enwiro", "enwiro").context("Could not load configuration")?;
    let mut writer = std::io::stdout();
    let mut context_object = CommandContext::new(config, &mut writer)?;
    ensure_can_run(&context_object)?;

    let result = match args {
        EnwiroCli::Activate(args) => activate(&mut context_object, args),
        EnwiroCli::ListEnvironments(_) => list_environments(&mut context_object),
        EnwiroCli::ListAll(_) => list_all(&mut context_object),
        EnwiroCli::ShowPath(args) => show_path(&mut context_object, args),
        EnwiroCli::Wrap(args) => wrap(&mut context_object, args),
    };

    context_object
        .writer
        .write_all("\n".as_bytes())
        .context("Could not write to output")?;

    result
}
