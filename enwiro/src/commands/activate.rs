use anyhow::Context;
use std::io::Write;
use std::path::Path;

use crate::context::{CommandContext, CookConfig};
use enwiro_sdk::adapter::ManagedEnvInfo;

#[derive(clap::Args)]
#[command(
    author,
    version,
    about = "Activate a workspace for a given environment, creating it if needed"
)]
pub struct ActivateArgs {
    pub name: String,

    /// Skip garnish `run_on: [Cook]` autorun hooks when cooking the env.
    #[arg(long)]
    pub no_hooks: bool,
}

fn build_managed_envs<W: Write>(context: &CommandContext<W>) -> Vec<ManagedEnvInfo> {
    let envs = match context.get_all_environments() {
        Ok(e) => e,
        Err(_) => return vec![],
    };
    let now = crate::usage_stats::now_timestamp();
    let all_stats: std::collections::HashMap<String, crate::usage_stats::EnvStats> = envs
        .values()
        .map(|env| {
            let env_dir = Path::new(&context.config.workspaces_directory).join(&env.name);
            let meta = crate::usage_stats::load_env_meta(&env_dir);
            (env.name.clone(), meta)
        })
        .collect();
    let percentile_scores = crate::usage_stats::slot_scores(&all_stats, now);
    envs.values()
        .map(|env| ManagedEnvInfo {
            name: env.name.clone(),
            slot_score: *percentile_scores.get(&env.name).unwrap_or(&0.0),
        })
        .collect()
}

pub fn activate<W: Write>(
    context: &mut CommandContext<W>,
    args: ActivateArgs,
) -> anyhow::Result<()> {
    let managed_envs = build_managed_envs(context);
    let flat_name = args.name.replace('/', "-");
    let env_dir = Path::new(&context.config.workspaces_directory).join(&flat_name);

    let cook_cfg = CookConfig {
        no_hooks: args.no_hooks,
    };

    let no_gear = std::collections::HashMap::new();
    if let Err(e) = context
        .adapter
        .activate(&args.name, &managed_envs, &no_gear)
    {
        context
            .notifier
            .notify_error(&format!("Failed to activate workspace: {:#}", e));
        return Err(e).context("Could not activate workspace");
    }

    if let Err(e) = context.get_or_cook_environment(&Some(args.name.clone()), &cook_cfg) {
        context.notifier.notify_error(&format!(
            "Could not set up environment '{}': {:#}",
            args.name, e
        ));
        tracing::warn!(error = %e, "Could not set up environment");
    }

    let gear = match enwiro_sdk::gear::LoadedGear::from_env_dir(&env_dir) {
        Ok(g) => g.into_map(),
        Err(e) => {
            context
                .notifier
                .notify_error(&format!("Could not read gear for '{}': {:#}", args.name, e));
            tracing::warn!(error = %e, "Could not read gear, continuing without it");
            std::collections::HashMap::new()
        }
    };
    if !gear.is_empty()
        && let Err(e) = context.adapter.activate(&args.name, &managed_envs, &gear)
    {
        context
            .notifier
            .notify_error(&format!("Failed to apply gear: {:#}", e));
        tracing::warn!(error = %e, "Could not apply gear, workspace exists without gear");
    }

    if env_dir.is_dir() && !env_dir.is_symlink() {
        crate::usage_stats::record_activation_per_env(&env_dir);
    } else {
        crate::usage_stats::record_activation(&flat_name);
    }

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

    /// A capturing adapter that records the full `ManagedEnvInfo` slice passed on activate.
    struct CapturingAdapter {
        captured: std::rc::Rc<std::cell::RefCell<Vec<ManagedEnvInfo>>>,
    }

    impl crate::commands::adapter::EnwiroAdapterTrait for CapturingAdapter {
        fn get_active_environment_name(&self) -> anyhow::Result<String> {
            Ok("some-env".to_string())
        }

        fn activate(
            &self,
            _name: &str,
            managed_envs: &[ManagedEnvInfo],
            _gear: &std::collections::HashMap<String, enwiro_sdk::gear::Gear>,
        ) -> anyhow::Result<()> {
            *self.captured.borrow_mut() = managed_envs.to_vec();
            Ok(())
        }

        fn run(&self, _payload: &enwiro_sdk::adapter::RunPayload) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// `build_managed_envs` must derive each `slot_score` from
    /// `usage_stats::slot_scores`. Computes expected scores via a direct
    /// `slot_scores` call and compares against what the adapter received.
    #[rstest]
    fn test_build_managed_envs_uses_slot_scores(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        use std::collections::HashMap;

        let (temp_dir, mut ctx, _, _) = context_object;
        ctx.create_mock_environment("env-x");
        ctx.create_mock_environment("env-y");

        // Give "env-x" a recent activation.
        let env_x_dir = std::path::Path::new(&ctx.config.workspaces_directory).join("env-x");
        crate::usage_stats::record_activation_per_env(&env_x_dir);

        // Build expected scores using slot_scores directly (compile-time symbol check).
        let mut meta_map: HashMap<String, crate::usage_stats::EnvStats> = HashMap::new();
        let now = crate::usage_stats::now_timestamp();
        meta_map.insert(
            "env-x".to_string(),
            crate::usage_stats::load_env_meta(&env_x_dir),
        );
        let env_y_dir = std::path::Path::new(&ctx.config.workspaces_directory).join("env-y");
        meta_map.insert(
            "env-y".to_string(),
            crate::usage_stats::load_env_meta(&env_y_dir),
        );

        let expected_scores = crate::usage_stats::slot_scores(&meta_map, now);
        assert!(
            expected_scores["env-x"] > expected_scores["env-y"],
            "slot_scores must rank env-x higher than env-y"
        );

        // Install capturing adapter.
        let captured = std::rc::Rc::new(std::cell::RefCell::new(vec![]));
        ctx.adapter = Box::new(CapturingAdapter {
            captured: captured.clone(),
        });

        let result = activate(
            &mut ctx,
            ActivateArgs {
                name: "env-x".to_string(),
                no_hooks: false,
            },
        );
        assert!(result.is_ok());

        let infos = captured.borrow();
        let env_x_info = infos
            .iter()
            .find(|e| e.name == "env-x")
            .expect("env-x must appear in managed_envs");
        let env_y_info = infos
            .iter()
            .find(|e| e.name == "env-y")
            .expect("env-y must appear in managed_envs");

        // The slot_score passed to the adapter must match what slot_scores returns.
        assert!(
            env_x_info.slot_score > env_y_info.slot_score,
            "activate must wire build_managed_envs to slot_scores: env-x slot_score \
             must exceed env-y slot_score; env-x={}, env-y={}",
            env_x_info.slot_score,
            env_y_info.slot_score
        );
        assert!(
            env_y_info.slot_score.abs() < 1e-10,
            "env-y with no activations must have slot_score 0.0, got {}",
            env_y_info.slot_score
        );
        assert!(
            (env_x_info.slot_score - 0.1).abs() < 1e-10,
            "env-x must have slot_score 0.1 (0.2×activation_rank_0.5 + 0.8×switch_rank_0.0), got {}",
            env_x_info.slot_score
        );

        drop(temp_dir);
    }

    #[rstest]
    fn test_activate_calls_adapter_with_correct_name(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut ctx, activated, _) = context_object;

        let result = activate(
            &mut ctx,
            ActivateArgs {
                name: "my-project".to_string(),
                no_hooks: false,
            },
        );
        assert!(result.is_ok());
        assert_eq!(*activated.borrow(), vec!["my-project".to_string()]);
    }

    #[rstest]
    fn test_activate_cooks_recipe_if_needed(
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

        let result = activate(
            &mut ctx,
            ActivateArgs {
                name: "new-project".to_string(),
                no_hooks: false,
            },
        );
        assert!(result.is_ok());

        // Verify environment was cooked (directory with inner symlink)
        let env_dir = temp_dir.path().join("new-project");
        assert!(env_dir.is_dir());
        let inner_link = env_dir.join("new-project");
        assert!(inner_link.is_symlink());
    }

    #[rstest]
    fn test_activate_succeeds_even_without_recipe(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut ctx, _, _) = context_object;

        // No cookbooks, no existing environment - activate should still succeed
        // (the adapter part works, cooking just warns on stderr)
        let result = activate(
            &mut ctx,
            ActivateArgs {
                name: "unknown".to_string(),
                no_hooks: false,
            },
        );
        assert!(result.is_ok());
    }

    #[rstest]
    fn test_activate_notifies_on_adapter_error(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut ctx, _, notifications) = context_object;

        // The env already exists so cook is a no-op; only the adapter failure
        // should generate an error notification.
        ctx.create_mock_environment("my-project");

        use crate::commands::adapter::EnwiroAdapterNone;
        ctx.adapter = Box::new(EnwiroAdapterNone {});

        let result = activate(
            &mut ctx,
            ActivateArgs {
                name: "my-project".to_string(),
                no_hooks: false,
            },
        );

        assert!(result.is_err());

        let logs = notifications.borrow();
        assert_eq!(logs.len(), 1);
        assert!(logs[0].starts_with("ERROR:"));
    }

    /// An adapter whose `activate()` returns a multi-level anyhow error chain so the
    /// leaf detail (`"leaf i3 IPC error: broken pipe"`) is distinct from the outer
    /// wrapper. Used to verify that error notifications surface the full chain.
    struct ChainedErrorAdapter;

    impl crate::commands::adapter::EnwiroAdapterTrait for ChainedErrorAdapter {
        fn get_active_environment_name(&self) -> anyhow::Result<String> {
            Ok("some-env".to_string())
        }

        fn activate(
            &self,
            _name: &str,
            _managed_envs: &[enwiro_sdk::adapter::ManagedEnvInfo],
            _gear: &std::collections::HashMap<String, enwiro_sdk::gear::Gear>,
        ) -> anyhow::Result<()> {
            let leaf = anyhow::anyhow!("leaf i3 IPC error: broken pipe");
            Err(leaf).map_err(|e| e.context("outer: Could not switch to workspace"))
        }

        fn run(&self, _payload: &enwiro_sdk::adapter::RunPayload) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// On adapter failure, the user-facing notification must include the leaf
    /// error from a multi-level anyhow chain - not just the outermost wrapper.
    /// Pins the `{:#}` formatting at the `notify_error` site.
    #[rstest]
    fn test_adapter_error_notification_includes_leaf_detail(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut ctx, _, notifications) = context_object;

        // The env already exists so cook is a no-op; only the adapter failure
        // should generate an error notification.
        ctx.create_mock_environment("my-project");

        ctx.adapter = Box::new(ChainedErrorAdapter);

        let result = activate(
            &mut ctx,
            ActivateArgs {
                name: "my-project".to_string(),
                no_hooks: false,
            },
        );

        // adapter failure propagates as an error result
        assert!(result.is_err());

        let logs = notifications.borrow();
        let error_notifications: Vec<_> = logs.iter().filter(|l| l.starts_with("ERROR:")).collect();
        assert_eq!(
            error_notifications.len(),
            1,
            "expected exactly one error notification"
        );

        let msg = &error_notifications[0];
        assert!(
            msg.contains("leaf i3 IPC error: broken pipe"),
            "notification must include the leaf error detail from the full error chain, \
             but got: {msg:?}"
        );
    }

    #[rstest]
    fn test_activate_notifies_on_cooking_failure(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut ctx, _, notifications) = context_object;

        // Adapter succeeds but no cookbooks and no existing environment
        let result = activate(
            &mut ctx,
            ActivateArgs {
                name: "unknown".to_string(),
                no_hooks: false,
            },
        );

        assert!(result.is_ok());

        let logs = notifications.borrow();
        assert_eq!(logs.len(), 2);
        assert!(logs[0].starts_with("INFO:"));
        assert!(logs[1].starts_with("ERROR:"));
        assert!(logs[1].contains("unknown"));
    }

    /// A cookbook whose `cook()` returns a multi-level anyhow error chain so the
    /// leaf detail (`"leaf git2 error: reference is locked"`) is distinct from the
    /// outer wrapper. Used to verify that error notifications surface the full chain.
    struct ChainedErrorCookbook {
        cookbook_name: enwiro_sdk::plugin::PluginName,
        recipe_name: String,
    }

    impl enwiro_sdk::client::CookbookTrait for ChainedErrorCookbook {
        fn list_recipes(&self) -> anyhow::Result<Vec<enwiro_sdk::cookbook::Recipe>> {
            Ok(vec![enwiro_sdk::cookbook::Recipe::new(&self.recipe_name)])
        }

        fn cook(&self, _recipe: &str) -> anyhow::Result<String> {
            let leaf = anyhow::anyhow!("leaf git2 error: reference is locked");
            Err(leaf).map_err(|e| e.context("outer: Could not create worktree"))
        }

        fn name(&self) -> &str {
            self.cookbook_name.as_str()
        }
    }

    /// On cooking failure, the user-facing notification must include the leaf
    /// error from a multi-level anyhow chain - not just the outermost wrapper.
    /// Pins the `{:#}` formatting at the cooking-error notification site.
    #[rstest]
    fn test_cook_error_notification_includes_leaf_detail(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut ctx, _, notifications) = context_object;

        ctx.write_cache_entry("git", "my-project");
        ctx.cookbooks = vec![Box::new(ChainedErrorCookbook {
            cookbook_name: enwiro_sdk::plugin::PluginName::new("git").unwrap(),
            recipe_name: "my-project".to_string(),
        })];

        let result = activate(
            &mut ctx,
            ActivateArgs {
                name: "my-project".to_string(),
                no_hooks: false,
            },
        );

        // activate succeeds at the adapter level; only cooking fails
        assert!(result.is_ok());

        let logs = notifications.borrow();
        let error_notifications: Vec<_> = logs.iter().filter(|l| l.starts_with("ERROR:")).collect();
        assert_eq!(
            error_notifications.len(),
            1,
            "expected exactly one error notification"
        );

        let msg = &error_notifications[0];
        assert!(
            msg.contains("leaf git2 error: reference is locked"),
            "notification must include the leaf error detail from the full error chain, \
             but got: {msg:?}"
        );
    }

    #[rstest]
    fn test_activate_no_error_notification_on_success(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut ctx, _, notifications) = context_object;

        let cooked_dir = temp_dir.path().join("cooked-target");
        fs::create_dir(&cooked_dir).unwrap();

        ctx.write_cache_entry("git", "my-project");
        ctx.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["my-project"],
            vec![("my-project", cooked_dir.to_str().unwrap())],
        ))];

        let result = activate(
            &mut ctx,
            ActivateArgs {
                name: "my-project".to_string(),
                no_hooks: false,
            },
        );

        assert!(result.is_ok());

        let logs = notifications.borrow();
        let error_count = logs.iter().filter(|log| log.starts_with("ERROR:")).count();
        assert_eq!(error_count, 0);
    }
}
