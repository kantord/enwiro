use anyhow::Context;
use std::io::Write;

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

pub fn show_path<W: Write>(
    context: &mut CommandContext<W>,
    args: ShowPathArgs,
) -> anyhow::Result<()> {
    let selected_environment = context
        .get_or_cook_environment(&args.environment_name)
        .context("Could not identify active environment")?;

    context
        .writer
        .write_all(selected_environment.path.as_bytes())
        .context("Could not write to output")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use crate::{
        commands::show_path::{show_path, ShowPathArgs},
        test_utils::test_utilities::{context_object, FakeContext},
    };

    #[rstest]
    fn test_show_path_when_environment_works(context_object: (tempfile::TempDir, FakeContext)) {
        let (_temp_dir, mut context_object) = context_object;
        context_object.create_mock_environment("foobar");
        let result = show_path(
            &mut context_object,
            ShowPathArgs {
                environment_name: Some("foobar".to_string()),
            },
        );

        assert!(result.is_ok());
        assert_eq!(context_object.get_output().ends_with("foobar"), true);
    }

    #[rstest]
    fn test_show_path_errors_when_env_does_not_exist(
        context_object: (tempfile::TempDir, FakeContext),
    ) {
        let (_temp_dir, mut context_object) = context_object;
        context_object.create_mock_environment("existing_env");
        let result = show_path(
            &mut context_object,
            ShowPathArgs {
                environment_name: Some("non_existing_env".to_string()),
            },
        );

        assert!(result.is_err());
    }

    #[rstest]
    fn test_show_path_errors_when_no_env_name_is_specified_and_no_adapter_found(
        context_object: (tempfile::TempDir, FakeContext),
    ) {
        let (_temp_dir, mut context_object) = context_object;
        context_object.create_mock_environment("existing_env");
        let result = show_path(
            &mut context_object,
            ShowPathArgs {
                environment_name: None,
            },
        );

        assert!(result.is_err());
    }

    #[rstest]
    fn test_takes_env_name_from_adapter_when_needed(
        context_object: (tempfile::TempDir, FakeContext),
    ) {
        let (_temp_dir, mut context_object) = context_object;
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
