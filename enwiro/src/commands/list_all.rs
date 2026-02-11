use anyhow::Context;
use std::io::Write;

use crate::context::CommandContext;

#[derive(clap::Args)]
#[command(
    author,
    version,
    about = "list all existing environments as well as recipes to create environments"
)]
pub struct ListAllArgs {}

pub fn list_all<W: Write>(context: &mut CommandContext<W>) -> anyhow::Result<()> {
    for environment in context.get_all_environments()?.values() {
        context
            .writer
            .write_all(format!("_: {}\n", environment.name).as_bytes())
            .context("Could not write to output")?;
    }

    for cookbook in &context.cookbooks {
        for line in cookbook.list_recipes()? {
            context
                .writer
                .write_all(format!("{}: {}\n", cookbook.name(), line).as_bytes())
                .context("Could not write to output")?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    use crate::test_utils::test_utilities::{FakeContext, FakeCookbook, context_object};

    #[rstest]
    fn test_list_all_shows_environments_and_recipes(
        context_object: (tempfile::TempDir, FakeContext),
    ) {
        let (_temp_dir, mut context_object) = context_object;
        context_object.create_mock_environment("my-env");
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["repo-a", "repo-b"],
            vec![],
        ))];

        list_all(&mut context_object).unwrap();

        let output = context_object.get_output();
        assert!(output.contains("_: my-env"));
        assert!(output.contains("git: repo-a"));
        assert!(output.contains("git: repo-b"));
    }

    #[rstest]
    fn test_list_all_with_no_cookbooks(context_object: (tempfile::TempDir, FakeContext)) {
        let (_temp_dir, mut context_object) = context_object;
        context_object.create_mock_environment("env-a");
        context_object.create_mock_environment("env-b");

        list_all(&mut context_object).unwrap();

        let output = context_object.get_output();
        assert!(output.contains("_: env-a"));
        assert!(output.contains("_: env-b"));
        assert!(!output.contains("git:"));
    }

    #[rstest]
    fn test_list_all_with_no_environments_but_has_recipes(
        context_object: (tempfile::TempDir, FakeContext),
    ) {
        let (_temp_dir, mut context_object) = context_object;
        context_object.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["some-repo"],
            vec![],
        ))];

        list_all(&mut context_object).unwrap();

        let output = context_object.get_output();
        assert!(output.contains("git: some-repo"));
        assert!(!output.contains("_:"));
    }
}
