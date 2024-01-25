use crate::CommandContext;

use std::{
    env,
    io::{self, Read, Write},
    process::Command,
};
#[derive(clap::Args)]
#[command(author, version, about)]
pub struct WrapArgs {
    pub command_name: String,
    pub environment_name: Option<String>,

    #[clap(allow_hyphen_values = true, num_args = 0.., last=true)]
    child_args: Option<String>,
}

pub fn wrap<R: Read, W: Write>(
    context: &mut CommandContext<R, W>,
    args: WrapArgs,
) -> Result<(), io::Error> {
    let selected_environment = context.get_environment(args.environment_name);
    env::set_current_dir(selected_environment.path).expect("Failed to change directory");

    let mut child = Command::new(args.command_name)
        .args(match args.child_args {
            Some(x) => [x.to_string()],
            None => ["".to_string()],
        })
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("Failed to execute command");

    let _ = child.wait().expect("Command wasn't running");

    Ok(())
}
