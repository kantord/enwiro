use anyhow::{Context, bail};
use std::fs;
use std::io::Write;
use std::path::Path;

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
        &mut context.writer,
    )
}

pub(crate) fn remove_env<W: Write>(
    workspaces_directory: &Path,
    name: &str,
    yes: bool,
    active_env: Option<&str>,
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
        AdapterLog, FakeContext, NotificationLog, context_object,
    };

    #[rstest]
    fn errors_when_env_does_not_exist(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, context, _, _) = context_object;
        let workspaces = Path::new(&context.config.workspaces_directory).to_path_buf();
        let mut out: Cursor<Vec<u8>> = Cursor::new(vec![]);

        let err = remove_env(&workspaces, "ghost", true, None, &mut out).expect_err("must error");
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
        remove_env(&workspaces, "foo", true, None, &mut out).expect("must succeed");

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
        remove_env(&workspaces, "foo", true, None, &mut out).expect("must succeed");

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
        remove_env(&workspaces, "legacy", true, None, &mut out).expect("must succeed");

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
        let err = remove_env(&workspaces, "foo", false, None, &mut out).expect_err("must refuse");
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
            &mut out,
        )
        .expect_err("must refuse");

        assert!(
            err.to_string().contains("active-env"),
            "error must name the env: {err}"
        );
        assert!(env_path.exists(), "env must NOT be deleted");
    }
}
