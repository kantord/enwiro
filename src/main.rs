mod commands;
mod config;
mod context;
mod environments;
mod test_utils;

use clap::Parser;

use commands::list_environments::{list_environments, ListEnvironmentsArgs};
use commands::show_path::{show_path, ShowPathArgs};
use config::ConfigurationValues;
use context::CommandContext;
use std::fs::create_dir;
use std::io::{Read, Write};
use std::path::Path;

#[derive(Parser)]
enum EnwiroCli {
    ListEnvironments(ListEnvironmentsArgs),
    ShowPath(ShowPathArgs),
}

fn ensure_can_run<R: Read, W: Write>(config: &CommandContext<R, W>) {
    let environments_directory = Path::new(&config.config.workspaces_directory);
    if !environments_directory.exists() {
        create_dir(environments_directory)
            .expect("Workspace directory does not exist and could not be automatically created.");
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
    use rstest::rstest;

    use crate::{
        commands::show_path::{show_path, ShowPathArgs},
        test_utils::test_utils::{context_object, FakeContext},
    };

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
