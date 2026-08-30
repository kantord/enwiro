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
use commands::browser::{BrowserArgs, browser};
use commands::env_info::{EnvInfoArgs, env_info};
use commands::goal::{GoalArgs, goal};
use commands::kanban::{KanbanArgs, kanban};
use commands::ls::{LsArgs, ls};
use commands::mark::{MarkArgs, mark};
use commands::prep::{PrepArgs, prep};
use commands::rm::{RmArgs, rm};
use commands::run::{RunArgs, run};
use commands::run_gear;
use commands::run_gear::{ENV_FLAG, LONG_YES_FLAG, SHORT_YES_FLAG};
use commands::shell::{ShellArgs, shell};
use commands::wrap::{WrapArgs, wrap};
use context::CommandContext;
use enwiro_daemon::ConfigurationValues;
use std::ffi::OsString;
use std::fs::create_dir;
use std::io::Write;
use std::path::Path;

#[derive(Parser)]
struct Cli {
    #[arg(global = true, long)]
    env: Option<String>,

    #[command(subcommand)]
    command: EnwiroCli,
}

#[derive(clap::Subcommand)]
enum EnwiroCli {
    Activate(ActivateArgs),
    /// Hidden: dumps the CLI reference page for the docs site
    /// (`just docs-cli`); not part of the interactive CLI surface.
    #[command(hide = true)]
    GenerateCliDocs,
    /// Hidden: the browser extension's native messaging host and its
    /// installer, spawned by the browser / run once at setup rather than
    /// being part of the interactive CLI surface.
    #[command(hide = true)]
    Browser(BrowserArgs),
    Goal(GoalArgs),
    Info(EnvInfoArgs),
    Kanban(KanbanArgs),
    Ls(LsArgs),
    Mark(MarkArgs),
    Prep(PrepArgs),
    Rm(RmArgs),
    Run(RunArgs),
    Shell(ShellArgs),
    Wrap(WrapArgs),
}

/// The docs site's CLI reference page (`docs/src/content/docs/reference/cli.md`),
/// generated from the clap definitions so `--help` stays the single source of
/// truth. Regenerate with `just docs-cli`; CI fails if the committed page drifts.
fn generate_cli_docs<W: Write>(writer: &mut W) -> anyhow::Result<()> {
    let options = clap_markdown::MarkdownOptions::new()
        .title("CLI reference".to_string())
        .show_footer(false);
    // The crate is `enwiro` but the binary is `enw` (see tests/binary_name.rs);
    // rename so the reference shows the command users actually type.
    let command = <Cli as clap::CommandFactory>::command()
        .name("enw")
        .bin_name("enw");
    let body = clap_markdown::help_markdown_command_custom(&command, &options);
    // Starlight renders the frontmatter title as the page's H1; drop
    // clap-markdown's own H1 to avoid a duplicate heading. Trim the end too:
    // main() appends the global trailing newline, and the pre-commit
    // end-of-file-fixer (and the CI drift check) require exactly one.
    let body = body
        .strip_prefix("# CLI reference\n")
        .unwrap_or(&body)
        .trim();
    write!(
        writer,
        "---\ntitle: CLI reference\ndescription: Every enw subcommand, generated from the CLI's own help text.\n---\n\n\
         This page is generated from the `enw` CLI definitions with `just docs-cli` - do not edit it by hand.\n\n\
         In addition to the subcommands below, `enw [-y] :<gear> [entry]` dispatches\n\
         to an environment's [gear](/launching-apps/) entries directly.\n\n{body}"
    )?;
    Ok(())
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
    let mut pos = 1;
    if argv.get(pos).and_then(|a| a.to_str()) == Some(ENV_FLAG) {
        pos += 2;
    }
    if argv
        .get(pos)
        .and_then(|a| a.to_str())
        .is_some_and(|s| s == SHORT_YES_FLAG || s == LONG_YES_FLAG)
    {
        pos += 1;
    }
    argv.get(pos)
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

    let cli = Cli::parse();
    let mut writer = std::io::stdout();
    let mut context_object = CommandContext::new(config, &mut writer)?;
    context_object.global_env = cli.env;
    ensure_can_run(&context_object)?;

    let result = match cli.command {
        EnwiroCli::Activate(args) => activate(&mut context_object, args),
        EnwiroCli::GenerateCliDocs => generate_cli_docs(&mut context_object.writer),
        EnwiroCli::Browser(args) => browser(&mut context_object, args),
        EnwiroCli::Goal(args) => goal(&mut context_object, args),
        EnwiroCli::Info(args) => env_info(&mut context_object, args),
        EnwiroCli::Kanban(args) => kanban(&mut context_object, args),
        EnwiroCli::Ls(args) => {
            let scope = args.scope();
            let status_filter = args.status.clone();
            ls(&mut context_object, scope, args.json, status_filter)
        }
        EnwiroCli::Mark(args) => mark(&mut context_object, args),
        EnwiroCli::Prep(args) => prep(&mut context_object, args),
        EnwiroCli::Rm(args) => rm(&mut context_object, args),
        EnwiroCli::Run(args) => run(&mut context_object, args),
        EnwiroCli::Shell(args) => shell(&mut context_object, args),
        EnwiroCli::Wrap(args) => wrap(&mut context_object, args),
    };

    context_object
        .writer
        .write_all("\n".as_bytes())
        .context("Could not write to output")?;

    result
}
