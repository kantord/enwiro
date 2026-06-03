use anyhow::Context;
use std::io::Write;
use std::path::Path;

use crate::context::{CommandContext, CookConfig};

#[derive(clap::Args)]
#[command(
    author,
    version,
    about = "Cook (if needed) and print the env path; no adapter contact"
)]
pub struct PrepArgs {
    pub name: String,

    /// Skip garnish `run_on: [Cook]` autorun hooks when cooking the env.
    #[arg(long)]
    pub no_hooks: bool,
}

pub fn prep<W: Write>(context: &mut CommandContext<W>, args: PrepArgs) -> anyhow::Result<()> {
    let cfg = CookConfig {
        no_hooks: args.no_hooks,
    };

    let env = match context.get_or_cook_environment(&Some(args.name.clone()), &cfg) {
        Ok(e) => e,
        Err(e) => {
            context.notifier.notify_error(&format!(
                "Could not set up environment '{}': {:#}",
                args.name, e
            ));
            return Err(e);
        }
    };

    let env_dir = Path::new(&context.config.workspaces_directory).join(&env.name);
    if env_dir.is_dir() && !env_dir.is_symlink() {
        crate::usage_stats::record_prep_per_env(&env_dir);
        crate::context::mark_via_daemon(&env.name, "ready", enwiro_sdk::rpc::MarkSource::Auto);
    }

    context
        .writer
        .write_all(env.path.as_bytes())
        .context("Could not write env path")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use std::fs;

    use crate::test_utils::test_utilities::{
        AdapterLog, FakeContext, FakeCookbook, NotificationLog, context_object,
    };

    #[rstest]
    fn test_prep_prints_env_path(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut ctx, _, _) = context_object;
        ctx.create_mock_environment("my-env");

        let result = prep(
            &mut ctx,
            PrepArgs {
                name: "my-env".to_string(),
                no_hooks: false,
            },
        );
        assert!(result.is_ok());
        assert!(
            ctx.get_output().ends_with("my-env"),
            "stdout should end with env name, got {:?}",
            ctx.get_output()
        );
    }

    #[rstest]
    fn test_prep_errors_when_env_does_not_exist_and_no_recipe(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut ctx, _, _) = context_object;

        let result = prep(
            &mut ctx,
            PrepArgs {
                name: "non-existent".to_string(),
                no_hooks: false,
            },
        );
        assert!(result.is_err());
    }

    #[rstest]
    fn test_prep_cooks_recipe_if_needed(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut ctx, _, _) = context_object;

        let cooked_dir = temp_dir.path().join("cooked-target");
        fs::create_dir(&cooked_dir).unwrap();

        ctx.write_cache_entry("git", "new-project");
        ctx.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["new-project"],
            vec![("new-project", cooked_dir.to_str().unwrap())],
        ))];

        let result = prep(
            &mut ctx,
            PrepArgs {
                name: "new-project".to_string(),
                no_hooks: false,
            },
        );
        assert!(result.is_ok());

        let env_dir = temp_dir.path().join("new-project");
        assert!(env_dir.is_dir());
        let inner_link = env_dir.join("new-project");
        assert!(inner_link.is_symlink());
    }

    #[rstest]
    fn test_prep_does_not_call_adapter_activate(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut ctx, activated, _) = context_object;
        ctx.create_mock_environment("my-env");

        let result = prep(
            &mut ctx,
            PrepArgs {
                name: "my-env".to_string(),
                no_hooks: false,
            },
        );
        assert!(result.is_ok());
        assert!(
            activated.borrow().is_empty(),
            "prep must not call adapter.activate; got {:?}",
            activated.borrow()
        );
    }

    #[rstest]
    fn test_prep_records_distinct_prep_signal(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut ctx, _, _) = context_object;
        ctx.create_mock_environment("my-env");

        let result = prep(
            &mut ctx,
            PrepArgs {
                name: "my-env".to_string(),
                no_hooks: false,
            },
        );
        assert!(result.is_ok());

        let env_dir = temp_dir.path().join("my-env");
        let meta = crate::usage_stats::load_env_meta(&env_dir);
        assert!(
            meta.signals.activation_buffer.is_empty(),
            "prep must not write to activation_buffer"
        );
        assert_eq!(
            meta.signals.prep_buffer.len(),
            1,
            "prep must write one event to prep_buffer"
        );
    }

    #[rstest]
    fn test_prep_notifies_on_cook_failure(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut ctx, _, notifications) = context_object;

        let result = prep(
            &mut ctx,
            PrepArgs {
                name: "no-such-recipe".to_string(),
                no_hooks: false,
            },
        );
        assert!(result.is_err());

        let logs = notifications.borrow();
        let errors: Vec<_> = logs.iter().filter(|l| l.starts_with("ERROR:")).collect();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("no-such-recipe"));
    }
}
