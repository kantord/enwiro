//! `enwiro-garnish-just` — surfaces every non-private justfile recipe as
//! a `cli` gear entry, grouped under one `"just"` gear. Discovered by
//! `enwiro` via the standard `PluginKind::Garnish` mechanism.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use serde::Deserialize;

use enwiro_sdk::gear::{CliEntry, Gear, GearFileData, SCHEMA_VERSION};

const GEAR_NAME: &str = "just";
const GEAR_DESCRIPTION: &str = "Tasks from the project's justfile";
const JUST_BINARY: &str = "just";

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Exit 0 iff `just` recognizes a justfile under `project_dir`.
    AppliesTo { project_dir: PathBuf },
    /// Emit `GearFileData` JSON describing every non-private recipe.
    Gear { project_dir: PathBuf },
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Cmd::AppliesTo { project_dir } => {
            if applies_to(&project_dir) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Cmd::Gear { project_dir } => match build_gear(&project_dir) {
            Ok(data) => {
                serde_json::to_writer(std::io::stdout(), &data).unwrap();
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{e:#}");
                ExitCode::FAILURE
            }
        },
    }
}

/// `just --summary` exits 0 iff a justfile is discoverable AND `just` is
/// on PATH. Both failure modes collapse to `false`.
fn applies_to(project_dir: &Path) -> bool {
    Command::new(JUST_BINARY)
        .arg("--summary")
        .current_dir(project_dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn build_gear(project_dir: &Path) -> anyhow::Result<GearFileData> {
    let output = Command::new(JUST_BINARY)
        .args(["--dump", "--dump-format", "json"])
        .current_dir(project_dir)
        .output()
        .context("spawn `just --dump --dump-format json`")?;
    if !output.status.success() {
        bail!(
            "`just --dump` exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let dump: JustDump =
        serde_json::from_slice(&output.stdout).context("parse `just --dump` JSON")?;

    let cli: HashMap<String, CliEntry> = dump
        .recipes
        .into_iter()
        .filter(|(_, r)| !r.private)
        .map(|(name, r)| {
            (
                name.clone(),
                CliEntry {
                    description: r.doc,
                    command: vec![JUST_BINARY.into(), name],
                    ..Default::default()
                },
            )
        })
        .collect();

    let gear = HashMap::from([(
        GEAR_NAME.to_owned(),
        Gear {
            description: GEAR_DESCRIPTION.into(),
            cli,
            ..Default::default()
        },
    )]);
    Ok(GearFileData {
        version: SCHEMA_VERSION,
        gear,
    })
}

/// Minimal projection of `just --dump --dump-format json` — everything we
/// don't reference (attributes, parameters, body, modules, aliases, ...)
/// is ignored on purpose.
#[derive(Deserialize)]
struct JustDump {
    recipes: HashMap<String, JustRecipe>,
}

#[derive(Deserialize)]
struct JustRecipe {
    #[serde(default)]
    doc: Option<String>,
    #[serde(default)]
    private: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn env_with_justfile(name: &str, contents: &str) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(name), contents).unwrap();
        dir
    }

    fn gear_for(justfile: &str) -> GearFileData {
        let dir = env_with_justfile("justfile", justfile);
        build_gear(dir.path()).unwrap()
    }

    mod applies {
        use super::*;

        #[test]
        fn true_when_lowercase_justfile_exists() {
            let dir = env_with_justfile("justfile", "build:\n    echo build\n");
            assert!(applies_to(dir.path()));
        }

        #[test]
        fn true_for_capitalized_justfile() {
            let dir = env_with_justfile("Justfile", "build:\n    echo build\n");
            assert!(applies_to(dir.path()));
        }

        #[test]
        fn false_when_no_justfile() {
            let dir = tempfile::tempdir().unwrap();
            assert!(!applies_to(dir.path()));
        }
    }

    mod gear {
        use super::*;

        #[test]
        fn emits_one_gear_named_just_with_canonical_description() {
            let data = gear_for("build:\n    echo build\n");
            assert_eq!(data.version, SCHEMA_VERSION);
            assert_eq!(data.gear.len(), 1);
            assert_eq!(data.gear["just"].description, GEAR_DESCRIPTION);
        }

        #[test]
        fn surfaces_doc_comment_as_description() {
            let data = gear_for("# Build the project\nbuild:\n    echo build\n");
            let build = &data.gear["just"].cli["build"];
            assert_eq!(build.description.as_deref(), Some("Build the project"));
            assert_eq!(build.command, vec!["just", "build"]);
        }

        #[test]
        fn omits_description_when_no_doc_comment() {
            let data = gear_for("build:\n    echo build\n");
            assert!(data.gear["just"].cli["build"].description.is_none());
        }

        #[test]
        fn skips_attribute_private_recipes() {
            let data = gear_for(
                "# Public\nbuild:\n    echo build\n\n[private]\ninternal:\n    echo internal\n",
            );
            let cli = &data.gear["just"].cli;
            assert!(cli.contains_key("build"));
            assert!(!cli.contains_key("internal"));
        }

        #[test]
        fn skips_underscore_prefixed_recipes() {
            let data = gear_for("build:\n    echo build\n_helper:\n    echo helper\n");
            assert!(!data.gear["just"].cli.contains_key("_helper"));
        }

        #[test]
        fn surfaces_confirm_recipes_unchanged() {
            let data = gear_for("[confirm]\ndeploy:\n    echo deploy\n");
            assert!(data.gear["just"].cli.contains_key("deploy"));
        }

        #[test]
        fn handles_multiple_recipes() {
            let data = gear_for(
                "# Build\nbuild:\n    echo b\n\n# Test\ntest:\n    echo t\n\ndeploy:\n    echo d\n",
            );
            let cli = &data.gear["just"].cli;
            assert_eq!(cli.len(), 3);
            for name in ["build", "test", "deploy"] {
                assert!(cli.contains_key(name));
            }
        }

        #[test]
        fn command_does_not_inline_recipe_parameters() {
            let data = gear_for("deploy target:\n    echo {{target}}\n");
            assert_eq!(
                data.gear["just"].cli["deploy"].command,
                vec!["just", "deploy"]
            );
        }
    }
}
