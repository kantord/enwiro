use anyhow::{Context, bail};
use std::fs;
use std::io::Write;
use std::path::Path;

use enwiro_sdk::client::CookbookTrait;
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
    remove_env(
        Path::new(&context.config.workspaces_directory),
        &args.name,
        args.yes,
        std::env::var(ENWIRO_ENV_VAR).ok().as_deref(),
        &context.cookbooks,
        &mut context.writer,
    )
}

/// Best-effort: ask the env's owning cookbook to tear down whatever it
/// materialized for it (e.g. a git worktree), before the env's own
/// directory disappears - `env_path/meta.json` is the only place
/// `cookbook`/`recipe` are recorded, so this must run first. Silent no-op
/// for envs with no recorded cookbook (legacy envs, manually created envs)
/// or whose cookbook isn't loaded.
///
/// `recipe` falls back to the env's own directory name when `meta.json`
/// predates that field (recorded only since a later release - about 30%
/// of envs in an established install lack it): for a non-composed env the
/// name and its recipe are the same string by convention. A stale/wrong
/// guess is harmless here - it just fails to resolve to any real resource
/// and `prune` no-ops, same as "nothing to prune".
fn prune_cookbook_resource<W: Write>(
    env_path: &Path,
    cookbooks: &[Box<dyn CookbookTrait>],
    writer: &mut W,
) {
    let env_meta = enwiro_daemon::meta::load_env_meta(env_path);
    let Some(cookbook_name) = env_meta.cookbook else {
        return;
    };
    let recipe = env_meta.recipe.or_else(|| {
        env_path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
    });
    let Some(recipe) = recipe else {
        return;
    };
    let Some(cookbook) = cookbooks.iter().find(|c| c.name() == cookbook_name) else {
        return;
    };
    match cookbook.prune(&recipe) {
        Ok(Some(message)) => {
            let _ = writeln!(writer, "{message}");
        }
        Ok(None) => {}
        Err(err) => {
            let _ = writeln!(writer, "Could not prune '{recipe}': {err}");
        }
    }
}

pub(crate) fn remove_env<W: Write>(
    workspaces_directory: &Path,
    name: &str,
    yes: bool,
    active_env: Option<&str>,
    cookbooks: &[Box<dyn CookbookTrait>],
    writer: &mut W,
) -> anyhow::Result<()> {
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
        writeln!(writer, "Aborted.").context("Could not write to output")?;
        return Ok(());
    }

    prune_cookbook_resource(&env_path, cookbooks, writer);

    if meta.file_type().is_symlink() {
        fs::remove_file(&env_path).with_context(|| format!("Could not remove env '{name}'"))?;
    } else if meta.is_dir() {
        fs::remove_dir_all(&env_path).with_context(|| format!("Could not remove env '{name}'"))?;
    } else {
        bail!("unexpected file type at {}", env_path.display());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use std::io::Cursor;

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
        let mut out: Cursor<Vec<u8>> = Cursor::new(vec![]);

        let err =
            remove_env(&workspaces, "ghost", true, None, &[], &mut out).expect_err("must error");
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

        let mut out: Cursor<Vec<u8>> = Cursor::new(vec![]);
        remove_env(&workspaces, "foo", true, None, &[], &mut out).expect("must succeed");

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

        let mut out: Cursor<Vec<u8>> = Cursor::new(vec![]);
        remove_env(&workspaces, "foo", true, None, &[], &mut out).expect("must succeed");

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

        let mut out: Cursor<Vec<u8>> = Cursor::new(vec![]);
        remove_env(&workspaces, "legacy", true, None, &[], &mut out).expect("must succeed");

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

        let mut out: Cursor<Vec<u8>> = Cursor::new(vec![]);
        let err =
            remove_env(&workspaces, "foo", false, None, &[], &mut out).expect_err("must refuse");
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

        let mut out: Cursor<Vec<u8>> = Cursor::new(vec![]);
        let err = remove_env(
            &workspaces,
            "active-env",
            true,
            Some("active-env"),
            &[],
            &mut out,
        )
        .expect_err("must refuse");

        assert!(
            err.to_string().contains("active-env"),
            "error must name the env: {err}"
        );
        assert!(env_path.exists(), "env must NOT be deleted");
    }

    #[rstest]
    fn prunes_the_owning_cookbooks_resource_before_removing(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context, _, _) = context_object;
        context.create_mock_environment("foo");
        let workspaces = Path::new(&context.config.workspaces_directory).to_path_buf();
        let env_path = workspaces.join("foo");
        write_meta_with_recipe(&env_path, "git", "repo@branch");

        let cookbooks: Vec<Box<dyn CookbookTrait>> = vec![Box::new(
            FakeCookbook::new("git", vec![], vec![]).with_prune("removed worktree at /tmp/x"),
        )];

        let mut out: Cursor<Vec<u8>> = Cursor::new(vec![]);
        remove_env(&workspaces, "foo", true, None, &cookbooks, &mut out).expect("must succeed");

        assert!(!env_path.exists(), "env dir must still be gone");
        let output = String::from_utf8(out.into_inner()).unwrap();
        assert!(
            output.contains("removed worktree at /tmp/x"),
            "the cookbook's prune outcome must be surfaced, got: {output}"
        );
    }

    /// Echoes exactly which recipe `prune` was called with in its outcome
    /// message, so the fallback test below can assert on it rather than
    /// just on whether some fixed message came back.
    struct RecordingCookbook;

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
        fn prune(&self, recipe: &str) -> anyhow::Result<Option<String>> {
            Ok(Some(format!("removed {recipe}")))
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

        let cookbooks: Vec<Box<dyn CookbookTrait>> = vec![Box::new(RecordingCookbook)];

        let mut out: Cursor<Vec<u8>> = Cursor::new(vec![]);
        remove_env(
            &workspaces,
            "costae@add-design-token-system",
            true,
            None,
            &cookbooks,
            &mut out,
        )
        .expect("must succeed");

        let output = String::from_utf8(out.into_inner()).unwrap();
        assert!(
            output.contains("removed costae@add-design-token-system"),
            "must prune using the env's own directory name as the recipe \
             when meta.json has no recorded recipe, got: {output}"
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
        // no-cookbook-recorded case never reaches it (an error would
        // otherwise surface as a "Could not prune" line, checked below).
        let cookbooks: Vec<Box<dyn CookbookTrait>> = vec![Box::new(FailingCookbook {
            cookbook_name: enwiro_sdk::plugin::PluginName::new("git").unwrap(),
        })];

        let mut out: Cursor<Vec<u8>> = Cursor::new(vec![]);
        remove_env(&workspaces, "foo", true, None, &cookbooks, &mut out).expect("must succeed");

        assert!(!workspaces.join("foo").exists());
        let output = String::from_utf8(out.into_inner()).unwrap();
        assert!(
            !output.contains("Could not prune"),
            "an env with no recorded cookbook/recipe must never invoke prune, got: {output}"
        );
    }
}
