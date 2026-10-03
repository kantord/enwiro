use anyhow::{Context, bail};
use std::fs;
use std::io::Write;
use std::path::Path;

use enwiro_sdk::client::CookbookTrait;
use enwiro_sdk::cookbook::PruneOutcome;
use enwiro_sdk::process::ENWIRO_ENV_VAR;

use crate::CommandContext;

#[derive(clap::Args)]
#[command(author, version, about = "Remove an environment")]
pub struct RmArgs {
    pub name: String,
    /// Skip the confirmation prompt
    #[arg(short = 'y', long = "yes")]
    pub yes: bool,
}

pub fn rm<W: Write>(context: &mut CommandContext<W>, args: RmArgs) -> anyhow::Result<()> {
    let outcome = remove_env(
        Path::new(&context.config.workspaces_directory),
        &args.name,
        args.yes,
        std::env::var(ENWIRO_ENV_VAR).ok().as_deref(),
        &context.cookbooks,
    )?;
    write_outcome(&mut context.writer, &args.name, &outcome)?;
    context.exit_failure |= outcome.needs_attention();
    Ok(())
}

/// How [`remove_env`] left an environment. Callers must look at it:
/// ignoring the cookbook's verdict is how a kept worktree used to be
/// reported as removed.
#[derive(Debug, PartialEq, Eq)]
#[must_use]
pub(crate) enum RemoveOutcome {
    Removed,
    /// The user declined the confirmation prompt.
    Aborted,
    /// The owning cookbook could not fully clean up after the environment.
    /// The environment is left in place, so removing it again retries.
    Kept {
        reason: String,
    },
    /// The environment is gone, but asking its cookbook to clean up failed
    /// (crash, timeout, no `prune` subcommand), so nothing is known about
    /// what was left behind.
    CleanupFailed {
        error: String,
    },
}

impl RemoveOutcome {
    /// Whether the user has something left to deal with, i.e. whether the
    /// command should exit non-zero.
    pub(crate) fn needs_attention(&self) -> bool {
        match self {
            RemoveOutcome::Removed | RemoveOutcome::Aborted => false,
            RemoveOutcome::Kept { .. } | RemoveOutcome::CleanupFailed { .. } => true,
        }
    }
}

/// The one-line report for `name`, shared by `enw rm` and
/// `enw stale prune`.
pub(crate) fn write_outcome<W: Write>(
    writer: &mut W,
    name: &str,
    outcome: &RemoveOutcome,
) -> anyhow::Result<()> {
    match outcome {
        RemoveOutcome::Removed => writeln!(writer, "✓ {name}"),
        RemoveOutcome::Aborted => writeln!(writer, "Aborted."),
        RemoveOutcome::Kept { reason } => writeln!(writer, "✗ {name}  kept: {reason}"),
        RemoveOutcome::CleanupFailed { error } => {
            writeln!(
                writer,
                "! {name}  removed, but cookbook cleanup failed: {error}"
            )
        }
    }
    .context("Could not write to output")
}

/// Ask the env's owning cookbook to tear down whatever it materialized for
/// it (e.g. a git worktree), before the env's own directory disappears -
/// `env_path/meta.json` is the only place `cookbook`/`recipe` are recorded,
/// so this must run first. `Ok(None)` for envs with no recorded cookbook
/// (legacy envs, manually created envs), whose cookbook isn't loaded, or
/// which have nothing to tear down.
///
/// `recipe` falls back to the env's own directory name when `meta.json`
/// predates that field (recorded only since a later release - about 30%
/// of envs in an established install lack it): for a non-composed env the
/// name and its recipe are the same string by convention. A stale/wrong
/// guess is harmless here - it just fails to resolve to any real resource
/// and `prune` reports nothing to prune.
fn prune_cookbook_resource(
    env_path: &Path,
    cookbooks: &[Box<dyn CookbookTrait>],
) -> anyhow::Result<Option<PruneOutcome>> {
    let env_meta = enwiro_daemon::meta::load_env_meta(env_path);
    let Some(cookbook_name) = env_meta.cookbook else {
        return Ok(None);
    };
    let recipe = env_meta.recipe.or_else(|| {
        env_path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
    });
    let Some(recipe) = recipe else {
        return Ok(None);
    };
    let Some(cookbook) = cookbooks.iter().find(|c| c.name() == cookbook_name) else {
        return Ok(None);
    };
    cookbook.prune(&recipe)
}

pub(crate) fn remove_env(
    workspaces_directory: &Path,
    name: &str,
    yes: bool,
    active_env: Option<&str>,
    cookbooks: &[Box<dyn CookbookTrait>],
) -> anyhow::Result<RemoveOutcome> {
    if active_env == Some(name) {
        bail!(
            "cannot remove the currently active env '{name}'; deactivate first \
             (open a shell outside the env or unset {ENWIRO_ENV_VAR})"
        );
    }

    let env_path = workspaces_directory.join(name);
    let meta = fs::symlink_metadata(&env_path)
        .with_context(|| format!("Environment \"{name}\" does not exist"))?;

    if !yes && !crate::confirm::confirm(&format!("Remove env '{name}'?"))? {
        return Ok(RemoveOutcome::Aborted);
    }

    let cleanup_error = match prune_cookbook_resource(&env_path, cookbooks) {
        Ok(Some(PruneOutcome::Kept { reason })) => return Ok(RemoveOutcome::Kept { reason }),
        Ok(Some(PruneOutcome::Removed) | None) => None,
        // TODO(2027-01-03): the env is deleted even though its prune failed
        // only because cookbooks built before the `PruneOutcome` contract
        // may not have a `prune` subcommand at all, and such an env would
        // otherwise be impossible to remove. Once the legacy fallback in
        // `PruneOutcome::parse_stdout` is gone, refactor this: report the
        // failure for this env, leave it in place, and simply move on so
        // the other environments still get cleaned up properly.
        Err(err) => Some(format!("{err:#}")),
    };

    if meta.file_type().is_symlink() {
        fs::remove_file(&env_path).with_context(|| format!("Could not remove env '{name}'"))?;
    } else if meta.is_dir() {
        fs::remove_dir_all(&env_path).with_context(|| format!("Could not remove env '{name}'"))?;
    } else {
        bail!("unexpected file type at {}", env_path.display());
    }

    Ok(match cleanup_error {
        Some(error) => RemoveOutcome::CleanupFailed { error },
        None => RemoveOutcome::Removed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use std::cell::RefCell;
    use std::io::Cursor;
    use std::rc::Rc;

    use crate::test_utils::test_utilities::{
        AdapterLog, FailingCookbook, FakeContext, FakeCookbook, NotificationLog, context_object,
    };
    use enwiro_daemon::meta::EnvStats;

    fn write_meta_with_recipe(env_dir: &Path, cookbook: &str, recipe: &str) {
        let meta = EnvStats {
            cookbook: Some(cookbook.to_string()),
            recipe: Some(recipe.to_string()),
            ..Default::default()
        };
        fs::write(
            env_dir.join("meta.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();
    }

    #[rstest]
    fn errors_when_env_does_not_exist(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, context, _, _) = context_object;
        let workspaces = Path::new(&context.config.workspaces_directory).to_path_buf();

        let err = remove_env(&workspaces, "ghost", true, None, &[]).expect_err("must error");
        assert!(
            err.to_string().contains("\"ghost\""),
            "error must name the env: {err}"
        );
    }

    #[rstest]
    fn deletes_new_format_env_with_yes(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context, _, _) = context_object;
        context.create_mock_environment("foo");
        let workspaces = Path::new(&context.config.workspaces_directory).to_path_buf();
        let env_path = workspaces.join("foo");
        assert!(env_path.exists());

        let outcome = remove_env(&workspaces, "foo", true, None, &[]).expect("must succeed");

        assert_eq!(outcome, RemoveOutcome::Removed);
        assert!(!env_path.exists(), "env dir must be gone");
    }

    #[rstest]
    fn symlink_target_outside_env_survives_removal(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, context, _, _) = context_object;
        let workspaces = Path::new(&context.config.workspaces_directory).to_path_buf();

        let project_dir = temp_dir.path().join("project-target");
        fs::create_dir(&project_dir).unwrap();
        let sentinel = project_dir.join("KEEP_ME");
        fs::write(&sentinel, b"keep").unwrap();

        let env_dir = workspaces.join("foo");
        fs::create_dir(&env_dir).unwrap();
        let inner_symlink = env_dir.join("foo");
        std::os::unix::fs::symlink(&project_dir, &inner_symlink).unwrap();
        fs::write(env_dir.join("meta.json"), b"{}").unwrap();

        let outcome = remove_env(&workspaces, "foo", true, None, &[]).expect("must succeed");

        assert_eq!(outcome, RemoveOutcome::Removed);
        assert!(!env_dir.exists(), "env dir must be gone");
        assert!(
            project_dir.exists(),
            "project dir (symlink target) must survive"
        );
        assert!(
            sentinel.exists(),
            "sentinel inside project dir must survive"
        );
    }

    #[rstest]
    fn deletes_legacy_bare_symlink_env(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, context, _, _) = context_object;
        let workspaces = Path::new(&context.config.workspaces_directory).to_path_buf();

        let target = temp_dir.path().join("project-target");
        fs::create_dir(&target).unwrap();
        let sentinel = target.join("KEEP_ME");
        fs::write(&sentinel, b"keep").unwrap();

        let env_path = workspaces.join("legacy");
        std::os::unix::fs::symlink(&target, &env_path).unwrap();

        let outcome = remove_env(&workspaces, "legacy", true, None, &[]).expect("must succeed");

        assert_eq!(outcome, RemoveOutcome::Removed);
        assert!(!env_path.exists(), "symlink must be gone");
        assert!(target.exists(), "symlink target must survive");
        assert!(sentinel.exists(), "target contents must survive");
    }

    #[rstest]
    fn non_tty_without_yes_refuses(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context, _, _) = context_object;
        context.create_mock_environment("foo");
        let workspaces = Path::new(&context.config.workspaces_directory).to_path_buf();
        let env_path = workspaces.join("foo");

        let err = remove_env(&workspaces, "foo", false, None, &[]).expect_err("must refuse");
        assert!(err.to_string().contains("-y"), "error must hint -y: {err}");
        assert!(env_path.exists(), "env must NOT be deleted");
    }

    #[rstest]
    fn refuses_active_env_even_with_yes(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context, _, _) = context_object;
        context.create_mock_environment("active-env");
        let workspaces = Path::new(&context.config.workspaces_directory).to_path_buf();
        let env_path = workspaces.join("active-env");

        let err = remove_env(&workspaces, "active-env", true, Some("active-env"), &[])
            .expect_err("must refuse");

        assert!(
            err.to_string().contains("active-env"),
            "error must name the env: {err}"
        );
        assert!(env_path.exists(), "env must NOT be deleted");
    }

    #[rstest]
    fn removes_the_env_after_the_cookbook_reports_its_resource_removed(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context, _, _) = context_object;
        context.create_mock_environment("foo");
        let workspaces = Path::new(&context.config.workspaces_directory).to_path_buf();
        let env_path = workspaces.join("foo");
        write_meta_with_recipe(&env_path, "git", "repo@branch");

        let cookbooks: Vec<Box<dyn CookbookTrait>> = vec![Box::new(
            FakeCookbook::new("git", vec![], vec![]).with_prune(PruneOutcome::Removed),
        )];

        let outcome = remove_env(&workspaces, "foo", true, None, &cookbooks).expect("must succeed");

        assert_eq!(outcome, RemoveOutcome::Removed);
        assert!(!env_path.exists(), "env dir must be gone");
    }

    #[rstest]
    fn keeps_the_env_when_the_cookbook_keeps_its_resource(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context, _, _) = context_object;
        context.create_mock_environment("foo");
        let workspaces = Path::new(&context.config.workspaces_directory).to_path_buf();
        let env_path = workspaces.join("foo");
        write_meta_with_recipe(&env_path, "git", "repo@branch");

        let cookbooks: Vec<Box<dyn CookbookTrait>> = vec![Box::new(
            FakeCookbook::new("git", vec![], vec![]).with_prune(PruneOutcome::Kept {
                reason: "uncommitted changes".to_string(),
            }),
        )];

        let outcome = remove_env(&workspaces, "foo", true, None, &cookbooks).expect("must succeed");

        assert_eq!(
            outcome,
            RemoveOutcome::Kept {
                reason: "uncommitted changes".to_string()
            }
        );
        assert!(env_path.exists(), "env dir must survive so a retry works");
        assert!(
            env_path.join("meta.json").exists(),
            "meta.json records the cookbook and recipe a retry needs"
        );
    }

    #[rstest]
    fn removes_the_env_but_reports_it_when_the_cookbook_prune_fails(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context, _, _) = context_object;
        context.create_mock_environment("foo");
        let workspaces = Path::new(&context.config.workspaces_directory).to_path_buf();
        let env_path = workspaces.join("foo");
        write_meta_with_recipe(&env_path, "git", "repo@branch");

        let cookbooks: Vec<Box<dyn CookbookTrait>> = vec![Box::new(FailingCookbook {
            cookbook_name: enwiro_sdk::plugin::PluginName::new("git").unwrap(),
        })];

        let outcome = remove_env(&workspaces, "foo", true, None, &cookbooks).expect("must succeed");

        assert_eq!(
            outcome,
            RemoveOutcome::CleanupFailed {
                error: "simulated failure".to_string()
            }
        );
        assert!(!env_path.exists(), "env dir must still be gone");
    }

    /// Records which recipe `prune` was called with, so the fallback test
    /// below can assert on it rather than just on whether pruning happened.
    struct RecordingCookbook {
        pruned: Rc<RefCell<Vec<String>>>,
    }

    impl CookbookTrait for RecordingCookbook {
        fn list_recipes(&self) -> anyhow::Result<Vec<enwiro_sdk::cookbook::Recipe>> {
            Ok(vec![])
        }
        fn cook(&self, _recipe: &str) -> anyhow::Result<String> {
            anyhow::bail!("not used in this test")
        }
        fn name(&self) -> &str {
            "git"
        }
        fn prune(&self, recipe: &str) -> anyhow::Result<Option<PruneOutcome>> {
            self.pruned.borrow_mut().push(recipe.to_string());
            Ok(Some(PruneOutcome::Removed))
        }
    }

    #[rstest]
    fn falls_back_to_the_env_name_as_recipe_when_meta_predates_that_field(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context, _, _) = context_object;
        context.create_mock_environment("costae@add-design-token-system");
        let workspaces = Path::new(&context.config.workspaces_directory).to_path_buf();
        let env_path = workspaces.join("costae@add-design-token-system");
        // Legacy meta.json shape: `cookbook` recorded, `recipe` never was.
        let meta = EnvStats {
            cookbook: Some("git".to_string()),
            ..Default::default()
        };
        fs::write(
            env_path.join("meta.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();

        let pruned = Rc::new(RefCell::new(vec![]));
        let cookbooks: Vec<Box<dyn CookbookTrait>> = vec![Box::new(RecordingCookbook {
            pruned: pruned.clone(),
        })];

        let outcome = remove_env(
            &workspaces,
            "costae@add-design-token-system",
            true,
            None,
            &cookbooks,
        )
        .expect("must succeed");

        assert_eq!(outcome, RemoveOutcome::Removed);
        assert_eq!(
            *pruned.borrow(),
            vec!["costae@add-design-token-system".to_string()],
            "must prune using the env's own directory name as the recipe \
             when meta.json has no recorded recipe"
        );
    }

    #[rstest]
    fn skips_pruning_for_an_env_with_no_recorded_cookbook(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context, _, _) = context_object;
        context.create_mock_environment("foo");
        let workspaces = Path::new(&context.config.workspaces_directory).to_path_buf();

        // A cookbook whose `prune` always errors - proves the
        // no-cookbook-recorded case never reaches it (it would otherwise
        // come back as `CleanupFailed`).
        let cookbooks: Vec<Box<dyn CookbookTrait>> = vec![Box::new(FailingCookbook {
            cookbook_name: enwiro_sdk::plugin::PluginName::new("git").unwrap(),
        })];

        let outcome = remove_env(&workspaces, "foo", true, None, &cookbooks).expect("must succeed");

        assert_eq!(
            outcome,
            RemoveOutcome::Removed,
            "an env with no recorded cookbook/recipe must never invoke prune"
        );
        assert!(!workspaces.join("foo").exists());
    }

    fn report(name: &str, outcome: &RemoveOutcome) -> String {
        let mut out: Cursor<Vec<u8>> = Cursor::new(vec![]);
        write_outcome(&mut out, name, outcome).unwrap();
        String::from_utf8(out.into_inner()).unwrap()
    }

    #[test]
    fn reports_each_outcome_on_one_line() {
        assert_eq!(report("a", &RemoveOutcome::Removed), "✓ a\n");
        assert_eq!(report("a", &RemoveOutcome::Aborted), "Aborted.\n");
        assert_eq!(
            report(
                "a",
                &RemoveOutcome::Kept {
                    reason: "dirty".to_string()
                }
            ),
            "✗ a  kept: dirty\n"
        );
        assert_eq!(
            report(
                "a",
                &RemoveOutcome::CleanupFailed {
                    error: "timed out".to_string()
                }
            ),
            "! a  removed, but cookbook cleanup failed: timed out\n"
        );
    }

    #[test]
    fn only_kept_and_failed_cleanup_need_attention() {
        assert!(!RemoveOutcome::Removed.needs_attention());
        assert!(!RemoveOutcome::Aborted.needs_attention());
        assert!(
            RemoveOutcome::Kept {
                reason: String::new()
            }
            .needs_attention()
        );
        assert!(
            RemoveOutcome::CleanupFailed {
                error: String::new()
            }
            .needs_attention()
        );
    }
}
