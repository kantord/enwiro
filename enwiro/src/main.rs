mod commands;
mod confirm;
mod context;
mod environments;
mod notifier;
mod test_utils;
mod usage_stats;

use anyhow::Context;
use clap::Parser;
use commands::activate::{ActivateArgs, activate};
use commands::env_info::{EnvInfoArgs, env_info};
use commands::ls::{LsArgs, ls};
use commands::prep::{PrepArgs, prep};
use commands::rm::{RmArgs, rm};
use commands::run::{RunArgs, run};
use commands::run_gear;
use commands::run_gear::{LONG_YES_FLAG, SHORT_YES_FLAG};
use commands::wrap::{WrapArgs, wrap};
use context::CommandContext;
use enwiro_daemon::ConfigurationValues;
use std::ffi::OsString;
use std::fs::create_dir;
use std::io::Write;
use std::path::Path;

#[derive(Parser)]
enum EnwiroCli {
    Activate(ActivateArgs),
    Info(EnvInfoArgs),
    Ls(LsArgs),
    Prep(PrepArgs),
    Rm(RmArgs),
    Run(RunArgs),
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

/// True iff argv looks like `enw [-y] :<gear> …`. Sniffed before clap so
/// the `:` prefix bypasses subcommand parsing; an optional pre-positional
/// `-y`/`--yes` is allowed and consumed by the dispatcher itself.
/// Side effect: `--help` after `:<gear> <entry>` reaches the spawned
/// command (e.g. `enw :just --help` runs `just --help`). Intentional.
fn is_dispatch_invocation(argv: &[OsString]) -> bool {
    let leading_arg = argv.get(1).and_then(|a| a.to_str());
    let gear_pos = if leading_arg == Some(SHORT_YES_FLAG) || leading_arg == Some(LONG_YES_FLAG) {
        2
    } else {
        1
    };
    argv.get(gear_pos)
        .and_then(|a| a.to_str())
        .is_some_and(|s| s.starts_with(':'))
}

fn main() -> anyhow::Result<()> {
    let _guard = enwiro_sdk::init_logging("enwiro.log");

    let cwd = std::env::current_dir().context("Could not determine current directory")?;
    let config_json = enwiro_sdk::config::build_cookbook_config(&cwd, "enwiro", &[])
        .context("Could not load configuration")?;
    let config: ConfigurationValues =
        serde_json::from_value(config_json).context("Could not deserialize configuration")?;

    let argv: Vec<OsString> = std::env::args_os().collect();
    if is_dispatch_invocation(&argv) {
        return run_gear::dispatch(Path::new(&config.workspaces_directory), &argv[1..]);
    }

    let args = EnwiroCli::parse();
    let mut writer = std::io::stdout();
    let mut context_object = CommandContext::new(config, &mut writer)?;
    ensure_can_run(&context_object)?;

    let result = match args {
        EnwiroCli::Activate(args) => activate(&mut context_object, args),
        EnwiroCli::Info(args) => env_info(&mut context_object, args),
        EnwiroCli::Ls(args) => {
            let scope = args.scope();
            ls(&mut context_object, scope, args.json)
        }
        EnwiroCli::Prep(args) => prep(&mut context_object, args),
        EnwiroCli::Rm(args) => rm(&mut context_object, args),
        EnwiroCli::Run(args) => run(&mut context_object, args),
        EnwiroCli::Wrap(args) => wrap(&mut context_object, args),
    };

    context_object
        .writer
        .write_all("\n".as_bytes())
        .context("Could not write to output")?;

    result
}
