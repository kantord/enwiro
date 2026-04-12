use anyhow::Context;
use std::io::Write;
use std::path::Path;

use crate::commands::adapter::ManagedEnvInfo;
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
    if let Err(e) = context.adapter.activate(&args.name, &managed_envs) {
        context
            .notifier
            .notify_error(&format!("Failed to activate workspace: {}", e));
        return Err(e).context("Could not activate workspace");
    }

    // Ensure the environment exists on disk (cook from recipe if needed)
    if let Err(e) = context.get_or_cook_environment(&Some(args.name.clone())) {
        context.notifier.notify_error(&format!(
            "Could not set up environment '{}': {}",
            args.name, e
        ));
        tracing::warn!(error = %e, "Could not set up environment");
    }

    let flat_name = args.name.replace('/', "-");
    let env_dir = Path::new(&context.config.workspaces_directory).join(&flat_name);
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

        fn activate(&self, _name: &str, managed_envs: &[ManagedEnvInfo]) -> anyhow::Result<()> {
            *self.captured.borrow_mut() = managed_envs.to_vec();
            Ok(())
        }
    }

    /// Verify that `build_managed_envs` is wired to `slot_scores` from `usage_stats`.
    ///
    /// This test checks two things:
    /// 1. `crate::usage_stats::slot_scores` is a callable public symbol (compile-time check).
    /// 2. The `slot_score` values passed to the adapter by `activate` / `build_managed_envs`
    ///    are consistent with what `slot_scores` returns for the same input data, establishing
    ///    that the caller is wired to `slot_scores` rather than some other scoring function.
    ///
    /// Two environments are created; one receives a recent activation.  `slot_scores` is called
    /// directly with the same metadata to derive expected values.  The captured adapter args
    /// must match those expected values.
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
            (env_x_info.slot_score - 0.5).abs() < 1e-10,
            "env-x must have slot_score 0.5 (rank 1/2), got {}",
            env_x_info.slot_score
        );

        drop(temp_dir);
    }

    /// `build_managed_envs` must set `slot_score` from percentile rank, not raw frecency.
    ///
    /// Setup: two environments on disk; "active-env" has one recent activation, "idle-env" has
    /// none. After `activate` is called:
    ///   - "active-env" must have slot_score > "idle-env" slot_score (it ranked higher)
    ///   - Both scores must be in the range [0.0, 1.0) — valid percentile fractions
    ///   - "idle-env" must have slot_score == 0.0 (no activations → lowest percentile)
    ///   - "active-env" must have slot_score == 0.5 (1 env strictly below out of 2 total)
    #[rstest]
    fn test_build_managed_envs_uses_percentile_scores(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut ctx, _, _) = context_object;

        // Create two environments on disk
        ctx.create_mock_environment("active-env");
        ctx.create_mock_environment("idle-env");

        // Record a recent activation for "active-env" so its frecency > 0
        let active_env_dir =
            std::path::Path::new(&ctx.config.workspaces_directory).join("active-env");
        crate::usage_stats::record_activation_per_env(&active_env_dir);

        // Install the capturing adapter
        let captured = std::rc::Rc::new(std::cell::RefCell::new(vec![]));
        ctx.adapter = Box::new(CapturingAdapter {
            captured: captured.clone(),
        });

        let result = activate(
            &mut ctx,
            ActivateArgs {
                name: "active-env".to_string(),
            },
        );
        assert!(result.is_ok());

        let infos = captured.borrow();
        assert_eq!(
            infos.len(),
            2,
            "Both environments must appear in managed_envs"
        );

        let active = infos
            .iter()
            .find(|e| e.name == "active-env")
            .expect("active-env must be present");
        let idle = infos
            .iter()
            .find(|e| e.name == "idle-env")
            .expect("idle-env must be present");

        // Percentile scores must be in [0.0, 1.0)
        assert!(
            active.slot_score >= 0.0 && active.slot_score < 1.0,
            "active-env slot_score must be in [0.0, 1.0), got {}",
            active.slot_score
        );
        assert!(
            idle.slot_score >= 0.0 && idle.slot_score < 1.0,
            "idle-env slot_score must be in [0.0, 1.0), got {}",
            idle.slot_score
        );

        // active-env has higher frecency → higher percentile rank
        assert!(
            active.slot_score > idle.slot_score,
            "active-env (has recent activation) must have higher slot_score than idle-env; \
             active={}, idle={}",
            active.slot_score,
            idle.slot_score
        );

        // idle-env has no activations → 0 envs strictly below → rank 0/2 = 0.0
        assert!(
            idle.slot_score.abs() < 1e-10,
            "idle-env with no activations must have slot_score 0.0, got {}",
            idle.slot_score
        );

        // active-env has 1 env strictly below (idle-env) out of 2 total → rank 1/2 = 0.5
        assert!(
            (active.slot_score - 0.5).abs() < 1e-10,
            "active-env must have slot_score 0.5 (rank 1/2), got {}",
            active.slot_score
        );

        drop(temp_dir); // keep TempDir alive until end
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

    #[rstest]
    fn test_activate_notifies_on_adapter_error(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut ctx, _, notifications) = context_object;

        use crate::commands::adapter::EnwiroAdapterNone;
        ctx.adapter = Box::new(EnwiroAdapterNone {});

        let result = activate(
            &mut ctx,
            ActivateArgs {
                name: "my-project".to_string(),
            },
        );

        assert!(result.is_err());

        let logs = notifications.borrow();
        assert_eq!(logs.len(), 1);
        assert!(logs[0].starts_with("ERROR:"));
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
            },
        );

        assert!(result.is_ok());

        let logs = notifications.borrow();
        assert_eq!(logs.len(), 1);
        assert!(logs[0].starts_with("ERROR:"));
        assert!(logs[0].contains("unknown"));
    }

    #[rstest]
    fn test_activate_no_error_notification_on_success(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut ctx, _, notifications) = context_object;

        let cooked_dir = temp_dir.path().join("cooked-target");
        fs::create_dir(&cooked_dir).unwrap();

        ctx.cookbooks = vec![Box::new(FakeCookbook::new(
            "git",
            vec!["my-project"],
            vec![("my-project", cooked_dir.to_str().unwrap())],
        ))];

        let result = activate(
            &mut ctx,
            ActivateArgs {
                name: "my-project".to_string(),
            },
        );

        assert!(result.is_ok());

        let logs = notifications.borrow();
        let error_count = logs.iter().filter(|log| log.starts_with("ERROR:")).count();
        assert_eq!(error_count, 0);
    }
}
