use std::io::{Read, Write};

use crate::{environments::Environment, CommandContext};

#[derive(clap::Args)]
#[command(author, version, about)]
pub struct ShowPathArgs {
    pub environment_name: String,
}

pub fn show_path<R: Read, W: Write>(context: &mut CommandContext<R, W>, args: ShowPathArgs) {
    let environments = Environment::get_all(&context.config.workspaces_directory);
    let selected_environment = environments
        .get(&args.environment_name)
        .expect("Environment not found");

    context
        .writer
        .write(selected_environment.path.as_bytes())
        .unwrap();
}
