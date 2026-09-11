use std::io::Write;
use std::path::Path;

use anyhow::Context;

use crate::CommandContext;
use crate::usage_stats::{DescriptionSource, load_env_meta, save_env_meta};

#[derive(clap::Args)]
#[command(about = "Refresh environment metadata resolved once at cook time")]
pub struct MetaArgs {
    #[command(subcommand)]
    pub command: MetaCommand,
}

#[derive(clap::Subcommand)]
pub enum MetaCommand {
    /// Re-resolve descriptions still stuck on their cook-time pattern-recipe
    /// template (ADR-0006), e.g. "Work on PR or issue #42 in myrepo".
    Refresh(RefreshArgs),
}

#[derive(clap::Args)]
pub struct RefreshArgs {
    /// Environment to refresh (defaults to the current environment)
    pub env_name: Option<String>,
    /// Refresh every environment instead of just one
    #[arg(long, conflicts_with = "env_name")]
    pub all: bool,
    /// Report what would change without writing anything
    #[arg(long)]
    pub dry_run: bool,
}

pub fn meta<W: Write>(context: &mut CommandContext<W>, args: MetaArgs) -> anyhow::Result<()> {
    match args.command {
        MetaCommand::Refresh(refresh_args) => refresh(context, refresh_args),
    }
}

fn refresh<W: Write>(context: &mut CommandContext<W>, args: RefreshArgs) -> anyhow::Result<()> {
    let env_names: Vec<String> = if args.all {
        let mut names: Vec<String> = context.get_all_environments()?.into_keys().collect();
        names.sort();
        names
    } else {
        vec![
            context
                .resolve_environment_name(&args.env_name)?
                .replace('/', "-"),
        ]
    };

    for env_name in env_names {
        refresh_one(context, &env_name, args.dry_run)?;
    }
    Ok(())
}

fn refresh_one<W: Write>(
    context: &mut CommandContext<W>,
    env_name: &str,
    dry_run: bool,
) -> anyhow::Result<()> {
    let env_dir = Path::new(&context.config.workspaces_directory).join(env_name);
    let mut meta = load_env_meta(&env_dir);

    let Some(description) = meta.description.as_deref() else {
        return Ok(());
    };
    if !is_placeholder_description(description) {
        return Ok(());
    }
    let (Some(cookbook_name), Some(recipe)) = (meta.cookbook.as_deref(), meta.recipe.as_deref())
    else {
        return Ok(());
    };
    let Some(cookbook) = context.cookbooks.iter().find(|c| c.name() == cookbook_name) else {
        writeln!(
            context.writer,
            "{env_name}: cookbook '{cookbook_name}' not installed, skipping"
        )?;
        return Ok(());
    };

    let resolved = cookbook.describe(recipe)?;

    if dry_run {
        match &resolved {
            Some(new_description) => writeln!(
                context.writer,
                "{env_name}: would resolve to \"{new_description}\""
            )?,
            None => writeln!(context.writer, "{env_name}: would clear (still unresolved)")?,
        }
        return Ok(());
    }

    // A direct load/save, not `record_cook_metadata_per_env`: that helper's
    // "`None` means leave whatever is there" contract is right for a
    // transient failure at re-cook time, but would silently keep this exact
    // placeholder frozen forever for every env whose owning cookbook (e.g.
    // git) has no `describe()` opinion at all (ADR-0006 §7).
    match resolved {
        Some(new_description) => {
            writeln!(
                context.writer,
                "{env_name}: resolved to \"{new_description}\""
            )?;
            meta.description = Some(new_description);
            meta.description_source = Some(DescriptionSource::Auto);
        }
        None => {
            writeln!(context.writer, "{env_name}: clearing (still unresolved)")?;
            meta.description = None;
            meta.description_source = None;
        }
    }
    save_env_meta(&env_dir, &meta).context("Could not save environment metadata")?;
    Ok(())
}

/// Matches `enwiro-cookbook-github`'s pattern-recipe template exactly
/// (`item_pattern_recipes`, "Work on PR or issue #{number} in {repo}") -
/// never a real, `describe()`-resolved title, which always starts with
/// `[PR]` or `[issue]` instead.
fn is_github_pattern_placeholder(description: &str) -> bool {
    description.starts_with("Work on PR or issue #") && description.contains(" in ")
}

/// Matches `enwiro-cookbook-git`'s pattern-recipe template exactly
/// (`branch_pattern_recipes`, "Create new branch '{branch}' in {repo}").
fn is_git_pattern_placeholder(description: &str) -> bool {
    description.starts_with("Create new branch '") && description.contains("' in ")
}

fn is_placeholder_description(description: &str) -> bool {
    is_github_pattern_placeholder(description) || is_git_pattern_placeholder(description)
}

#[cfg(test)]
mod tests {
    use super::*;
    use enwiro_sdk::test_helpers::FakeCookbook;
    use rstest::rstest;

    use crate::test_utils::test_utilities::{
        AdapterLog, FakeContext, NotificationLog, context_object,
    };

    #[test]
    fn test_is_placeholder_description_matches_github_template() {
        assert!(is_placeholder_description(
            "Work on PR or issue #42 in myrepo"
        ));
    }

    #[test]
    fn test_is_placeholder_description_matches_git_template() {
        assert!(is_placeholder_description(
            "Create new branch 'my-feature' in myrepo"
        ));
    }

    #[test]
    fn test_is_placeholder_description_rejects_real_titles() {
        assert!(!is_placeholder_description("[PR] Fix auth bug"));
        assert!(!is_placeholder_description("[issue] Fix auth bug"));
        assert!(!is_placeholder_description(
            "A manually-set description mentioning PR or issue #42 elsewhere"
        ));
    }

    fn env_dir(context: &FakeContext, env_name: &str) -> std::path::PathBuf {
        Path::new(&context.config.workspaces_directory).join(env_name)
    }

    #[rstest]
    fn refresh_resolves_a_placeholder_description(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context, _, _) = context_object;
        context.create_mock_environment("my-env");
        context.cookbooks = vec![Box::new(
            FakeCookbook::new("github", vec![], vec![]).with_describe("[issue] Fix auth bug"),
        )];
        let dir = env_dir(&context, "my-env");
        let mut existing = load_env_meta(&dir);
        existing.description = Some("Work on PR or issue #42 in myrepo".to_string());
        existing.cookbook = Some("github".to_string());
        existing.recipe = Some("myrepo#42".to_string());
        save_env_meta(&dir, &existing).unwrap();

        refresh(
            &mut context,
            RefreshArgs {
                env_name: Some("my-env".to_string()),
                all: false,
                dry_run: false,
            },
        )
        .unwrap();

        let meta = load_env_meta(&dir);
        assert_eq!(meta.description.as_deref(), Some("[issue] Fix auth bug"));
        assert_eq!(meta.description_source, Some(DescriptionSource::Auto));
    }

    #[rstest]
    fn refresh_clears_a_placeholder_when_describe_returns_none(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context, _, _) = context_object;
        context.create_mock_environment("my-env");
        context.cookbooks = vec![Box::new(FakeCookbook::new("git", vec![], vec![]))];
        let dir = env_dir(&context, "my-env");
        let mut existing = load_env_meta(&dir);
        existing.description = Some("Create new branch 'my-feature' in myrepo".to_string());
        existing.cookbook = Some("git".to_string());
        existing.recipe = Some("myrepo@my-feature".to_string());
        save_env_meta(&dir, &existing).unwrap();

        refresh(
            &mut context,
            RefreshArgs {
                env_name: Some("my-env".to_string()),
                all: false,
                dry_run: false,
            },
        )
        .unwrap();

        let meta = load_env_meta(&dir);
        assert_eq!(meta.description, None);
        assert_eq!(meta.description_source, None);
    }

    #[rstest]
    fn refresh_leaves_non_placeholder_descriptions_untouched(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context, _, _) = context_object;
        context.create_mock_environment("my-env");
        context.cookbooks = vec![Box::new(
            FakeCookbook::new("github", vec![], vec![]).with_describe("should not be called"),
        )];
        let dir = env_dir(&context, "my-env");
        let mut existing = load_env_meta(&dir);
        existing.description = Some("[issue] A real title".to_string());
        existing.cookbook = Some("github".to_string());
        existing.recipe = Some("myrepo#42".to_string());
        save_env_meta(&dir, &existing).unwrap();

        refresh(
            &mut context,
            RefreshArgs {
                env_name: Some("my-env".to_string()),
                all: false,
                dry_run: false,
            },
        )
        .unwrap();

        let meta = load_env_meta(&dir);
        assert_eq!(meta.description.as_deref(), Some("[issue] A real title"));
    }

    #[rstest]
    fn refresh_dry_run_does_not_write(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut context, _, _) = context_object;
        context.create_mock_environment("my-env");
        context.cookbooks = vec![Box::new(
            FakeCookbook::new("github", vec![], vec![]).with_describe("[issue] Fix auth bug"),
        )];
        let dir = env_dir(&context, "my-env");
        let mut existing = load_env_meta(&dir);
        existing.description = Some("Work on PR or issue #42 in myrepo".to_string());
        existing.cookbook = Some("github".to_string());
        existing.recipe = Some("myrepo#42".to_string());
        save_env_meta(&dir, &existing).unwrap();

        refresh(
            &mut context,
            RefreshArgs {
                env_name: Some("my-env".to_string()),
                all: false,
                dry_run: true,
            },
        )
        .unwrap();

        let meta = load_env_meta(&dir);
        assert_eq!(
            meta.description.as_deref(),
            Some("Work on PR or issue #42 in myrepo")
        );
    }
}
