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
use tracing_subscriber::{Layer, layer::SubscriberExt, util::SubscriberInitExt};

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
    // Tracing subscriber: rolling daily log file + stderr (filtered by RUST_LOG)
    let log_dir = std::env::var("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            home::home_dir()
                .expect("Could not determine home directory")
                .join(".local")
                .join("state")
        })
        .join("enwiro");
    if let Err(e) = std::fs::create_dir_all(&log_dir) {
        eprintln!(
            "Warning: could not create log directory {:?}: {}",
            log_dir, e
        );
    }

    let file_appender = tracing_appender::rolling::daily(&log_dir, "enwiro.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false)
                .with_filter(tracing_subscriber::filter::LevelFilter::DEBUG),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
                ),
        )
        .init();

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
