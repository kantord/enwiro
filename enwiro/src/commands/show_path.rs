use std::io::{self, Read, Write};

use crate::CommandContext;

#[derive(clap::Args)]
#[command(
    author,
    version,
    about = "Show the file system path of a given environment"
)]
pub struct ShowPathArgs {
    pub environment_name: Option<String>,
}

pub fn show_path<R: Read, W: Write>(
    context: &mut CommandContext<R, W>,
    args: ShowPathArgs,
) -> Result<(), io::Error> {
    let selected_environment = context.get_environment(args.environment_name);

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
