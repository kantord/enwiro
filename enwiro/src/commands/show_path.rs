use std::io::{self, Read, Write};

use crate::{environments::Environment, CommandContext};

#[derive(clap::Args)]
#[command(author, version, about)]
pub struct ShowPathArgs {
    pub environment_name: Option<String>,
}

pub fn show_path<R: Read, W: Write>(
    context: &mut CommandContext<R, W>,
    args: ShowPathArgs,
) -> Result<(), io::Error> {
    let environments = Environment::get_all(&context.config.workspaces_directory)?;
    let selected_environment_name = match args.environment_name {
        Some(x) => x,
        None => context.adapter.get_active_environment_name()?,
    };
    let selected_environment = match environments.get(&selected_environment_name) {
        Some(x) => x,
        None => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("Environment {} does not exist", selected_environment_name),
        ))?,
    };

    context
        .writer
        .write(selected_environment.path.as_bytes())
        .unwrap();

    Ok(())
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
                environment_name: Some("foobar".to_string()),
            },
        )
        .unwrap();

        assert_eq!(context_object.get_output().ends_with("foobar"), true);
    }

    #[rstest]
    #[should_panic]
    fn test_show_path_panics_when_env_does_not_exist(mut context_object: FakeContext) {
        context_object.create_mock_environment("existing_env");
        show_path(
            &mut context_object,
            ShowPathArgs {
                environment_name: Some("non_existing_env".to_string()),
            },
        )
        .unwrap();
    }

    #[rstest]
    #[should_panic]
    fn test_show_panic_when_no_env_name_is_specified_and_no_adapter_found(
        mut context_object: FakeContext,
    ) {
        context_object.create_mock_environment("existing_env");
        show_path(
            &mut context_object,
            ShowPathArgs {
                environment_name: None,
            },
        )
        .unwrap();
    }

    #[rstest]
    fn test_takes_env_name_from_adapter_when_needed(mut context_object: FakeContext) {
        context_object.create_mock_environment("foobaz");
        show_path(
            &mut context_object,
            ShowPathArgs {
                environment_name: None,
            },
        )
        .unwrap();

        assert_eq!(context_object.get_output().ends_with("foobaz"), true);
    }
}
