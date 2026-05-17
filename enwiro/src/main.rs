mod commands;
mod context;
mod environments;
mod notifier;
mod test_utils;
mod usage_stats;

use anyhow::Context;
use clap::Parser;
use commands::activate::{ActivateArgs, activate};
use commands::list_all::{ListAllArgs, list_all};
use commands::list_environments::{ListEnvironmentsArgs, list_environments};
use commands::run_gear;
use commands::run_gear::{LONG_YES_FLAG, SHORT_YES_FLAG};
use commands::show_path::{ShowPathArgs, show_path};
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

    let config: ConfigurationValues =
        confy::load("enwiro", "enwiro").context("Could not load configuration")?;

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
        EnwiroCli::ListEnvironments(_) => list_environments(&mut context_object),
        EnwiroCli::ListAll(args) => list_all(&mut context_object, args.json),
        EnwiroCli::ShowPath(args) => show_path(&mut context_object, args),
        EnwiroCli::Wrap(args) => wrap(&mut context_object, args),
    };

    context_object
        .writer
        .write_all("\n".as_bytes())
        .context("Could not write to output")?;

    result
}
