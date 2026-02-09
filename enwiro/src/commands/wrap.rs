use crate::CommandContext;

use std::{
    env,
    io::{self, Write},
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
    child_args: Option<Vec<String>>,
}

pub fn wrap<W: Write>(context: &mut CommandContext<W>, args: WrapArgs) -> Result<(), io::Error> {
    let selected_environment = context.get_or_cook_environment(&args.environment_name);
    let environment_path: String = match selected_environment {
        Ok(ref environment) => environment.path.clone(),
        Err(ref error) => match error.kind() {
            std::io::ErrorKind::NotFound => {
                // shoudl be stderr write
                context
                    .writer
                    .write_all(
                        "No matching environment found. Falling back to home directory.\n"
                            .as_bytes(),
                    )
                    .unwrap();

                home::home_dir()
                    .expect("Could not determine user home directory")
                    .into_os_string()
                    .into_string()
                    .unwrap()
            }
            _ => panic!("Could not determine environment path: {}", error),
        },
    };
    env::set_current_dir(environment_path).expect("Failed to change directory");

    let environment_name: String = match selected_environment {
        Ok(ref environment) => environment.name.clone(),
        Err(_) => String::from(""),
    };

    let mut child = Command::new(args.command_name)
        .env("ENWIRO_ENV", environment_name)
        .args(match args.child_args {
            Some(x) => x.into_iter().map(|x| x.to_string()).collect(),
            None => vec![],
        })
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("Failed to execute command");

    let _ = child.wait().expect("Command wasn't running");

    Ok(())
}
