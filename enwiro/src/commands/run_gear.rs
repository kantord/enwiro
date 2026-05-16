//! Dispatcher for `enw :<gear> <entry> [args...]` — the leading `:` is
//! sniffed pre-clap in `main` so this module sees argv directly.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};

use enwiro_sdk::gear::{CliEntry, Gear, LoadedGear};

const ACTIVE_ENV_VAR: &str = "ENWIRO_ENV";
const NONE_PLACEHOLDER: &str = "<none>";

/// Parsed `enw :<gear> <entry> [args...]` invocation (no program name).
#[derive(Debug, PartialEq, Eq)]
pub struct DispatchTarget {
    pub gear_name: String,
    pub entry_name: String,
    pub passthrough: Vec<OsString>,
}

pub fn parse_dispatch_args(args: &[OsString]) -> anyhow::Result<DispatchTarget> {
    let first = args
        .first()
        .ok_or_else(|| anyhow!("missing :<gear> argument"))?;
    let first_str = first
        .to_str()
        .ok_or_else(|| anyhow!("gear name must be valid UTF-8"))?;
    let gear_name = first_str
        .strip_prefix(':')
        .ok_or_else(|| anyhow!("gear name must start with `:`, got `{first_str}`"))?;
    if gear_name.is_empty() {
        bail!("gear name is empty (got just `:`)");
    }
    let entry = args
        .get(1)
        .ok_or_else(|| anyhow!("missing <entry> argument after `:{gear_name}`"))?;
    let entry_name = entry
        .to_str()
        .ok_or_else(|| anyhow!("entry name must be valid UTF-8"))?
        .to_owned();
    Ok(DispatchTarget {
        gear_name: gear_name.to_owned(),
        entry_name,
        passthrough: args.iter().skip(2).cloned().collect(),
    })
}

/// `workspaces_directory/<name>/<name>` is the inner symlink to the
/// cooked project root; the dispatcher execs there so `just <recipe>`
/// finds the justfile.
pub fn env_project_dir(workspaces_directory: &Path, env_name: &str) -> PathBuf {
    workspaces_directory.join(env_name).join(env_name)
}

pub fn active_env_name() -> anyhow::Result<String> {
    std::env::var(ACTIVE_ENV_VAR).with_context(|| {
        format!(
            "${ACTIVE_ENV_VAR} is unset; run `enw activate <name>` first or invoke from inside a wrapped env"
        )
    })
}

/// Comma-separated alphabetical listing for "available: …" messages.
fn format_available<'a>(items: impl Iterator<Item = &'a str>) -> String {
    let mut v: Vec<&str> = items.collect();
    v.sort();
    if v.is_empty() {
        NONE_PLACEHOLDER.to_owned()
    } else {
        v.join(", ")
    }
}

pub fn resolve_entry<'a>(
    gear_map: &'a HashMap<String, Gear>,
    gear_name: &str,
    entry_name: &str,
) -> anyhow::Result<&'a CliEntry> {
    let gear = gear_map.get(gear_name).ok_or_else(|| {
        anyhow!(
            "no gear named `:{gear_name}` in this env (available: {})",
            format_available(gear_map.keys().map(String::as_str))
        )
    })?;
    gear.cli.get(entry_name).ok_or_else(|| {
        anyhow!(
            "gear `:{gear_name}` has no cli entry `{entry_name}` (available: {})",
            format_available(gear.cli.keys().map(String::as_str))
        )
    })
}

pub fn build_argv(entry: &CliEntry, passthrough: &[OsString]) -> Vec<OsString> {
    entry
        .command
        .iter()
        .map(OsString::from)
        .chain(passthrough.iter().cloned())
        .collect()
}

/// Parse → resolve env + gear → exec. Replaces the current process on
/// child failure (exit with child's status); other errors bubble.
pub fn dispatch(workspaces_directory: &Path, args: &[OsString]) -> anyhow::Result<()> {
    let target = parse_dispatch_args(args)?;
    let env_name = active_env_name()?;
    let env_dir = workspaces_directory.join(&env_name);
    let project_dir = env_project_dir(workspaces_directory, &env_name);

    let gear_map = LoadedGear::from_env_dir(&env_dir)
        .with_context(|| format!("could not load gear for env `{env_name}`"))?
        .into_map();
    let entry = resolve_entry(&gear_map, &target.gear_name, &target.entry_name)?;
    let argv = build_argv(entry, &target.passthrough);
    let (program, rest) = argv
        .split_first()
        .ok_or_else(|| anyhow!("cli entry `{}` has empty command", target.entry_name))?;

    let status = std::process::Command::new(program)
        .args(rest)
        .current_dir(&project_dir)
        .status()
        .with_context(|| {
            format!(
                "failed to spawn `{}` in {}",
                program.to_string_lossy(),
                project_dir.display()
            )
        })?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn osvec(parts: &[&str]) -> Vec<OsString> {
        parts.iter().map(OsString::from).collect()
    }

    fn cli_entry(command: &[&str]) -> CliEntry {
        CliEntry {
            description: None,
            command: command.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn gear_with_cli(entries: &[(&str, CliEntry)]) -> HashMap<String, Gear> {
        let cli = entries
            .iter()
            .map(|(n, e)| ((*n).to_owned(), e.clone()))
            .collect();
        HashMap::from([(
            "just".to_owned(),
            Gear {
                description: "Tasks from the project's justfile".into(),
                cli,
                ..Default::default()
            },
        )])
    }

    mod parse {
        use super::*;

        #[test]
        fn extracts_gear_entry_and_passthrough() {
            let t =
                parse_dispatch_args(&osvec(&[":just", "build", "--release", "-j", "4"])).unwrap();
            assert_eq!(t.gear_name, "just");
            assert_eq!(t.entry_name, "build");
            assert_eq!(t.passthrough, osvec(&["--release", "-j", "4"]));
        }

        #[test]
        fn passthrough_empty_when_no_extra_args() {
            let t = parse_dispatch_args(&osvec(&[":just", "build"])).unwrap();
            assert!(t.passthrough.is_empty());
        }

        #[rstest]
        #[case::missing_leading_colon(&["just", "build"], "must start with `:`")]
        #[case::bare_colon(&[":", "build"], "empty")]
        #[case::missing_entry(&[":just"], "missing <entry>")]
        #[case::empty_argv(&[], "missing :<gear>")]
        fn rejects_with_helpful_message(#[case] args: &[&str], #[case] needle: &str) {
            let err = parse_dispatch_args(&osvec(args)).expect_err("must reject");
            assert!(err.to_string().contains(needle), "got: {err}");
        }
    }

    mod resolve {
        use super::*;

        #[test]
        fn returns_entry_when_both_layers_present() {
            let map = gear_with_cli(&[("build", cli_entry(&["just", "build"]))]);
            assert_eq!(
                resolve_entry(&map, "just", "build").unwrap().command,
                vec!["just", "build"]
            );
        }

        #[test]
        fn errors_with_available_gears_when_gear_missing() {
            let map = gear_with_cli(&[("build", cli_entry(&["just", "build"]))]);
            let msg = resolve_entry(&map, "missing", "build")
                .unwrap_err()
                .to_string();
            assert!(msg.contains("no gear named `:missing`"), "{msg}");
            assert!(msg.contains("available: just"), "{msg}");
        }

        #[test]
        fn errors_with_sorted_available_entries_when_entry_missing() {
            let map = gear_with_cli(&[
                ("build", cli_entry(&["just", "build"])),
                ("test", cli_entry(&["just", "test"])),
            ]);
            let msg = resolve_entry(&map, "just", "nope").unwrap_err().to_string();
            assert!(msg.contains("no cli entry `nope`"), "{msg}");
            assert!(msg.contains("available: build, test"), "{msg}");
        }

        #[test]
        fn errors_with_none_placeholder_when_gear_has_no_cli() {
            let map = HashMap::from([(
                "web-only".to_owned(),
                Gear {
                    description: "x".into(),
                    ..Default::default()
                },
            )]);
            let msg = resolve_entry(&map, "web-only", "any")
                .unwrap_err()
                .to_string();
            assert!(msg.contains("available: <none>"), "{msg}");
        }
    }

    mod argv {
        use super::*;

        #[test]
        fn concatenates_command_with_passthrough() {
            let argv = build_argv(
                &cli_entry(&["just", "build"]),
                &osvec(&["--release", "-j", "4"]),
            );
            assert_eq!(argv, osvec(&["just", "build", "--release", "-j", "4"]));
        }

        #[test]
        fn passthrough_empty_yields_command_only() {
            assert_eq!(
                build_argv(&cli_entry(&["just", "build"]), &[]),
                osvec(&["just", "build"])
            );
        }
    }

    mod env {
        use super::*;

        #[test]
        fn project_dir_appends_inner_symlink_name() {
            assert_eq!(
                env_project_dir(Path::new("/tmp/ws"), "my-env"),
                PathBuf::from("/tmp/ws/my-env/my-env")
            );
        }

        #[test]
        fn active_env_name_errors_when_unset() {
            let prior = std::env::var(ACTIVE_ENV_VAR).ok();
            // SAFETY: serial within this file; no parallel readers of ENWIRO_ENV.
            unsafe { std::env::remove_var(ACTIVE_ENV_VAR) };
            let err = active_env_name().unwrap_err();
            assert!(err.to_string().contains("ENWIRO_ENV"));
            if let Some(v) = prior {
                unsafe { std::env::set_var(ACTIVE_ENV_VAR, v) };
            }
        }
    }
}
