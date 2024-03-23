use crate::CommandContext;

use std::{
    env,
    io::{self, Read, Write},
    process::Command,
};
#[derive(clap::Args)]
#[command(
    author,
    version,
    about = "Run an application/command inside an environment"
)]
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
    let selected_environment = context.get_or_cook_environment(&args.environment_name);
    let environment_path: String = match selected_environment {
        Ok(environment) => environment.path,
        Err(error) => match error.kind() {
            std::io::ErrorKind::NotFound => {
                // shoudl be stderr write
                context
                    .writer
                    .write_all(
                        "No matching environment found. Falling back to home directory.\n"
                            .as_bytes(),
                    )
                    .unwrap();

                env::home_dir()
                    .expect("Could not determine user home directory")
                    .into_os_string()
                    .into_string()
                    .unwrap()
            }
            _ => panic!("Could not determine environment path: {}", error),
        },
    };
    env::set_current_dir(environment_path).expect("Failed to change directory");

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
