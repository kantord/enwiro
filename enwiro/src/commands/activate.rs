use anyhow::Context;
use std::io::Write;

use crate::context::CommandContext;

#[derive(clap::Args)]
#[command(
    author,
    version,
    about = "Activate a workspace for a given environment, creating it if needed"
)]
pub struct ActivateArgs {
    pub name: String,
}

pub fn activate<W: Write>(
    context: &mut CommandContext<W>,
    args: ActivateArgs,
) -> anyhow::Result<()> {
    context
        .adapter
        .activate(&args.name)
        .context("Could not activate workspace")?;

    // Ensure the environment exists on disk (cook from recipe if needed)
    if let Err(e) = context.get_or_cook_environment(&Some(args.name)) {
        eprintln!("Warning: could not set up environment: {}", e);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use std::fs;

    use crate::test_utils::test_utilities::{
        AdapterLog, FakeContext, FakeCookbook, context_object,
    };

    #[rstest]
    fn test_activate_calls_adapter_with_correct_name(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog),
    ) {
        let (_temp_dir, mut ctx, activated) = context_object;

        let result = activate(
            &mut ctx,
            ActivateArgs {
                name: "my-project".to_string(),
            },
        );
        assert!(result.is_ok());
        assert_eq!(*activated.borrow(), vec!["my-project".to_string()]);
    }

    #[rstest]
    fn test_activate_cooks_recipe_if_needed(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog),
    ) {
        let (temp_dir, mut ctx, _) = context_object;

        let cooked_dir = temp_dir.path().join("cooked-target");
        fs::create_dir(&cooked_dir).unwrap();

        ctx.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["new-project"],
            vec![("new-project", cooked_dir.to_str().unwrap())],
        ))];

        let result = activate(
            &mut ctx,
            ActivateArgs {
                name: "new-project".to_string(),
            },
        );
        assert!(result.is_ok());

        // Verify environment was cooked (symlink created)
        let link_path = temp_dir.path().join("new-project");
        assert!(link_path.is_symlink());
    }

    #[rstest]
    fn test_activate_succeeds_even_without_recipe(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog),
    ) {
        let (_temp_dir, mut ctx, _) = context_object;

        // No cookbooks, no existing environment — activate should still succeed
        // (the adapter part works, cooking just warns on stderr)
        let result = activate(
            &mut ctx,
            ActivateArgs {
                name: "unknown".to_string(),
            },
        );
        assert!(result.is_ok());
    }
}
