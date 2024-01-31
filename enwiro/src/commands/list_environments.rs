use crate::{environments::Environment, CommandContext};

use std::io::{self, Read, Write};

#[derive(clap::Args)]
#[command(author, version, about = "List all existing environments")]
pub struct ListEnvironmentsArgs {}

pub fn list_environments<R: Read, W: Write>(
    context: &mut CommandContext<R, W>,
) -> Result<(), io::Error> {
    let environments = Environment::get_all(&context.config.workspaces_directory)?;

    for environment in environments.values() {
        context
            .writer
            .write_all(format!("{}\n", environment.name).as_bytes())
            .expect("Could not write to output");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use assertables::*;
    use rstest::rstest;

    use crate::test_utils::test_utils::{context_object, FakeContext};

    #[rstest]
    fn test_list_environments_2_examples(mut context_object: FakeContext) {
        context_object.create_mock_environment("foobar");
        context_object.create_mock_environment("baz");

        list_environments(&mut context_object).unwrap();

        let output = context_object.get_output();
        let output_lines: Vec<&str> = output.lines().collect();
        let expected_output = vec!["foobar", "baz"];

        assert_set_eq!(output_lines, expected_output);
    }
}
