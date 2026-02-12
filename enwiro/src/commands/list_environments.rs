use anyhow::Context;

use crate::{CommandContext, environments::Environment};

use std::io::Write;

#[derive(clap::Args)]
#[command(author, version, about = "List all existing environments")]
pub struct ListEnvironmentsArgs {}

pub fn list_environments<W: Write>(context: &mut CommandContext<W>) -> anyhow::Result<()> {
    let environments = Environment::get_all(&context.config.workspaces_directory)?;

    for environment in environments.values() {
        context
            .writer
            .write_all(format!("{}\n", environment.name).as_bytes())
            .context("Could not write to output")?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use assertables::*;
    use rstest::rstest;

    use crate::test_utils::test_utilities::{AdapterLog, FakeContext, context_object};

    #[rstest]
    fn test_list_environments_2_examples(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog),
    ) {
        let (_temp_dir, mut context_object, _) = context_object;
        context_object.create_mock_environment("foobar");
        context_object.create_mock_environment("baz");

        list_environments(&mut context_object).unwrap();

        let output = context_object.get_output();
        let output_lines: Vec<&str> = output.lines().collect();
        let expected_output = vec!["foobar", "baz"];

        assert_set_eq!(output_lines, expected_output);
    }
}
