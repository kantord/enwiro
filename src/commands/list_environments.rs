use crate::{environments::Environment, CommandContext};

use std::io::{Read, Write};

#[derive(clap::Args)]
#[command(author, version, about)]
pub struct ListEnvironmentsArgs {}

pub fn list_environments<R: Read, W: Write>(context: &mut CommandContext<R, W>) {
    let environments = Environment::get_all(&context.config.workspaces_directory);

    for environment in environments.values() {
        println!("{}", environment.name);
    }
}
