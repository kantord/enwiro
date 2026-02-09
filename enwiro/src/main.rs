mod client;
mod commands;
mod config;
mod context;
mod environments;
mod plugin;
mod test_utils;

use clap::Parser;

use commands::list_all::{list_all, ListAllArgs};
use commands::list_environments::{list_environments, ListEnvironmentsArgs};
use commands::show_path::{show_path, ShowPathArgs};
use commands::wrap::{wrap, WrapArgs};
use config::ConfigurationValues;
use context::CommandContext;
use std::fs::create_dir;
use std::io::Write;
use std::path::Path;

#[derive(Parser)]
enum EnwiroCli {
    ListEnvironments(ListEnvironmentsArgs),
    ListAll(ListAllArgs),
    ShowPath(ShowPathArgs),
    Wrap(WrapArgs),
}

fn ensure_can_run<W: Write>(config: &CommandContext<W>) {
    let environments_directory = Path::new(&config.config.workspaces_directory);
    if !environments_directory.exists() {
        create_dir(environments_directory)
            .expect("Workspace directory does not exist and could not be automatically created.");
    }
}

fn main() -> Result<(), std::io::Error> {
    let args = EnwiroCli::parse();
    let config: ConfigurationValues = match confy::load("enwiro", "enwiro") {
        Ok(x) => x,
        Err(x) => {
            panic!("Could not load configuration: {:?}", x);
        }
    };
    let mut writer = std::io::stdout();
    let mut context_object = CommandContext::new(config, &mut writer);
    ensure_can_run(&context_object);

    let result = match args {
        EnwiroCli::ListEnvironments(_) => list_environments(&mut context_object),
        EnwiroCli::ListAll(_) => list_all(&mut context_object),
        EnwiroCli::ShowPath(args) => show_path(&mut context_object, args),
        EnwiroCli::Wrap(args) => wrap(&mut context_object, args),
    };

    context_object.writer.write_all("\n".as_bytes()).unwrap();

    result
}
