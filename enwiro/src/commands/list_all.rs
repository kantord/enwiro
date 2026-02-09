use std::io::{self, Write};

use crate::context::CommandContext;

#[derive(clap::Args)]
#[command(
    author,
    version,
    about = "list all existing environments as well as recipes to create environments"
)]
pub struct ListAllArgs {}

pub fn list_all<W: Write>(context: &mut CommandContext<W>) -> Result<(), io::Error> {
    for environment in context.get_all_environments()?.values() {
        context
            .writer
            .write_all(format!("_: {}\n", environment.name).as_bytes())
            .expect("Could not write to output");
    }

    for cookbook in context.get_cookbooks() {
        for line in cookbook.list_recipes() {
            context
                .writer
                .write_all(format!("{}: {}\n", cookbook.plugin.name, line).as_bytes())
                .expect("Could not write to output");
        }
    }

    Ok(())
}
