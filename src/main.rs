mod commands;
mod environments;

use clap::Parser;

use commands::show_path::{show_path, ShowPathArgs};
use environments::Environment;
use serde_derive::{Deserialize, Serialize};
use std::env;
use std::fs::create_dir;
use std::io::{Read, Write};
use std::path::Path;

#[derive(Debug, Serialize, Deserialize)]
struct ConfigurationValues {
    workspaces_directory: String,
}

impl ::std::default::Default for ConfigurationValues {
    fn default() -> Self {
        let home_dir = env::home_dir().expect("User home directory not found");
        let default_workspaces_directory = home_dir.join(".enwiro_envs");

        Self {
            workspaces_directory: default_workspaces_directory.to_str().unwrap().to_string(),
        }
    }
}

#[derive(Parser)]
enum EnwiroCli {
    ListEnvironments(ListEnvironmentsArgs),
    ShowPath(ShowPathArgs),
}

#[derive(clap::Args)]
#[command(author, version, about)]
struct ListEnvironmentsArgs {}

struct CommandContext<R: Read, W: Write> {
    config: ConfigurationValues,
    reader: R,
    writer: W,
}

fn ensure_can_run<R: Read, W: Write>(config: &CommandContext<R, W>) {
    let environments_directory = Path::new(&config.config.workspaces_directory);
    if !environments_directory.exists() {
        create_dir(environments_directory)
            .expect("Workspace directory does not exist and could not be automatically created.");
    }
}

fn list_environments<R: Read, W: Write>(context: &mut CommandContext<R, W>) {
    let environments = Environment::get_all(&context.config.workspaces_directory);

    for environment in environments.values() {
        println!("{}", environment.name);
    }
}

fn main() {
    let args = EnwiroCli::parse();
    let config: ConfigurationValues =
        confy::load("enwiro", None).expect("Configuration file must be present");

    let mut context_object = CommandContext {
        config,
        reader: &mut std::io::stdin(),
        writer: &mut std::io::stderr(),
    };

    ensure_can_run(&context_object);

    match args {
        EnwiroCli::ListEnvironments(_) => list_environments(&mut context_object),
        EnwiroCli::ShowPath(args) => show_path(&mut context_object, args),
    }

    context_object.writer.write("\n".as_bytes()).unwrap();
}

#[cfg(test)]
mod tests {
    use std::{env::temp_dir, io::Cursor, vec};

    use rand::Rng;
    use rstest::{fixture, rstest};

    use crate::commands::show_path::{show_path, ShowPathArgs};

    use super::*;

    type FakeIO = Cursor<Vec<u8>>;
    type FakeContext = CommandContext<Cursor<Vec<u8>>, Cursor<Vec<u8>>>;

    impl FakeContext {
        fn get_output(&mut self) -> String {
            let mut output = String::new();
            self.writer.set_position(0);

            self.writer
                .read_to_string(&mut output)
                .expect("Could not read output");

            return output;
        }

        fn create_mock_environment(&mut self, environment_name: &str) {
            let environment_directory =
                Path::new(&self.config.workspaces_directory).join(environment_name);
            create_dir(environment_directory).expect("Could not create directory");
        }
    }

    #[fixture]
    fn in_memory_buffer() -> FakeIO {
        Cursor::new(vec![])
    }

    #[fixture]
    fn context_object() -> FakeContext {
        let temporary_directory_path = temp_dir().join(
            rand::thread_rng()
                .gen_range(100000000..999999999)
                .to_string(),
        );
        create_dir(&temporary_directory_path).expect("Could not create temporary directory");
        let reader = in_memory_buffer();
        let writer = in_memory_buffer();
        let mut config = ConfigurationValues::default();
        config.workspaces_directory = temporary_directory_path.to_str().unwrap().to_string();

        return CommandContext {
            config,
            reader,
            writer,
        };
    }

    #[rstest]
    fn test_show_path_when_environment_works(mut context_object: FakeContext) {
        context_object.create_mock_environment("foobar");
        show_path(
            &mut context_object,
            ShowPathArgs {
                environment_name: "foobar".to_string(),
            },
        );

        assert_eq!(context_object.get_output().ends_with("foobar"), true);
    }

    #[rstest]
    #[should_panic]
    fn test_show_path_panics_when_env_does_not_exist(mut context_object: FakeContext) {
        context_object.create_mock_environment("existing_env");
        show_path(
            &mut context_object,
            ShowPathArgs {
                environment_name: "non_existing_env".to_string(),
            },
        );
    }
}
