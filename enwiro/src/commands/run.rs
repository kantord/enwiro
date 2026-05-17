use anyhow::Context;
use enwiro_sdk::adapter::RunPayload;
use std::io::Write;

use crate::CommandContext;

#[derive(clap::Args)]
#[command(
    author,
    version,
    about = "Run a command via the active environment's adapter"
)]
pub struct RunArgs {
    pub command_name: String,
    pub environment_name: Option<String>,

    #[clap(allow_hyphen_values = true, num_args = 0.., last=true)]
    child_args: Option<Vec<String>>,
}

pub fn run<W: Write>(context: &mut CommandContext<W>, args: RunArgs) -> anyhow::Result<()> {
    let environment = context
        .get_or_cook_environment(&args.environment_name)
        .context("Could not resolve an active environment for `enw run`")?;

    let payload = RunPayload::new(
        environment.name.clone(),
        environment.path.clone(),
        args.command_name.clone(),
        args.child_args.unwrap_or_default(),
    );
    context
        .adapter
        .run(&payload)
        .with_context(|| format!("Adapter failed to run `{}`", args.command_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::test_utilities::{
        AdapterLog, FakeContext, NotificationLog, context_object,
    };
    use rstest::rstest;

    #[rstest]
    fn run_dispatches_payload_with_resolved_env_and_args(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut ctx, _, _) = context_object;
        ctx.create_mock_environment("my-env");

        let mock = crate::test_utils::test_utilities::EnwiroAdapterMock::new("my-env");
        let runs = mock.runs.clone();
        ctx.adapter = Box::new(mock);

        let result = run(
            &mut ctx,
            RunArgs {
                command_name: "echo".to_string(),
                environment_name: Some("my-env".to_string()),
                child_args: Some(vec!["hi".to_string()]),
            },
        );

        assert!(result.is_ok(), "run should succeed: {:?}", result);
        let calls = runs.borrow();
        assert_eq!(calls.len(), 1, "adapter.run must be called exactly once");
        assert_eq!(calls[0].command, "echo");
        assert_eq!(calls[0].args, vec!["hi".to_string()]);
        assert_eq!(calls[0].env_name, "my-env");
    }
}
