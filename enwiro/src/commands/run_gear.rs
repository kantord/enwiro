//! Dispatcher for `enw :<gear> <entry> [args...]` — the leading `:` is
//! sniffed pre-clap in `main` so this module sees argv directly.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};

use enwiro_sdk::gear::{CliEntry, Gear, LoadedGear};
use enwiro_sdk::process::ENWIRO_ENV_VAR;

const NONE_PLACEHOLDER: &str = "<none>";

/// Short form of the pre-positional confirmation-bypass flag. The flag
/// is parsed in two places (the pre-clap argv sniffer in `main.rs` and
/// the dispatcher's `strip_yes_flag`) which must stay in lockstep, so
/// the spelling lives here.
pub const SHORT_YES_FLAG: &str = "-y";
pub const LONG_YES_FLAG: &str = "--yes";

/// Parsed `enw [-y] :<gear> [<entry> [args...]]` invocation (no program
/// name). `entry_name` is `None` when the user invoked `enw :<gear>` with
/// no further args — that's a request to list available entries. `yes`
/// reflects an optional pre-positional `-y`/`--yes` flag (`enw -y :gear
/// entry`), which lets the dispatcher run entries marked
/// `require_confirmation: true`.
#[derive(Debug, PartialEq, Eq)]
pub struct DispatchTarget {
    pub gear_name: String,
    pub entry_name: Option<String>,
    pub passthrough: Vec<OsString>,
    pub yes: bool,
    pub env_override: Option<String>,
}

pub const ENV_FLAG: &str = "--env";

fn strip_env_flag(args: &[OsString]) -> (Option<String>, &[OsString]) {
    let is_env = args
        .first()
        .and_then(|a| a.to_str())
        .is_some_and(|s| s == ENV_FLAG);
    if is_env && let Some(val) = args.get(1).and_then(|a| a.to_str()) {
        return (Some(val.to_owned()), &args[2..]);
    }
    (None, args)
}

/// Strip a single leading `-y`/`--yes` flag from the front of `args`.
/// Pre-positional is the only accepted shape — placing the flag after
/// `:<gear>` would let it be matched as a passthrough arg by command-
/// prefix ACLs (e.g. claude-code's `Bash(enw :*)`), defeating the gating.
fn strip_yes_flag(args: &[OsString]) -> (bool, &[OsString]) {
    let is_yes = args
        .first()
        .and_then(|a| a.to_str())
        .is_some_and(|s| s == SHORT_YES_FLAG || s == LONG_YES_FLAG);
    if is_yes {
        (true, &args[1..])
    } else {
        (false, args)
    }
}

pub fn parse_dispatch_args(args: &[OsString]) -> anyhow::Result<DispatchTarget> {
    let (env_override, rest) = strip_env_flag(args);
    let (yes, rest) = strip_yes_flag(rest);
    let first = rest
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
    let entry_name = rest
        .get(1)
        .map(|e| {
            e.to_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("entry name must be valid UTF-8"))
        })
        .transpose()?;
    Ok(DispatchTarget {
        gear_name: gear_name.to_owned(),
        entry_name,
        passthrough: rest.iter().skip(2).cloned().collect(),
        yes,
        env_override,
    })
}

/// `workspaces_directory/<name>/<name>` is the inner symlink to the
/// cooked project root; the dispatcher execs there so `just <recipe>`
/// finds the justfile.
pub fn env_project_dir(workspaces_directory: &Path, env_name: &str) -> PathBuf {
    workspaces_directory.join(env_name).join(env_name)
}

pub fn active_env_name() -> anyhow::Result<String> {
    std::env::var(ENWIRO_ENV_VAR).with_context(|| {
        format!(
            "${ENWIRO_ENV_VAR} is unset; run `enw activate <name>` first or invoke from inside a wrapped env"
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

/// Reject a `require_confirmation` entry when `-y` was not passed and the
/// user declines (or can't) the interactive prompt. On a tty, an unsafe
/// entry without `-y` triggers a y/N prompt; off-tty (CI, scripts) the
/// helper refuses outright. Either refusal returns the same error shape,
/// which embeds a ready-to-run `enw -y :<gear> <entry>`.
pub fn ensure_confirmed(
    gear_name: &str,
    entry_name: &str,
    entry: &CliEntry,
    yes: bool,
) -> anyhow::Result<()> {
    if !entry.require_confirmation || yes {
        return Ok(());
    }
    let prompt = format!("Run :{gear_name} {entry_name}?");
    if crate::confirm::confirm(&prompt).unwrap_or(false) {
        return Ok(());
    }
    bail!(
        "gear entry `:{gear_name} {entry_name}` requires confirmation; pass `-y` (e.g. `enw -y :{gear_name} {entry_name}`) to run it"
    );
}

pub fn build_argv(entry: &CliEntry, passthrough: &[OsString]) -> Vec<OsString> {
    entry
        .command
        .iter()
        .map(OsString::from)
        .chain(passthrough.iter().cloned())
        .collect()
}

/// Look up a gear by name; same not-found error shape as
/// [`resolve_entry`] for consistency.
fn resolve_gear<'a>(
    gear_map: &'a HashMap<String, Gear>,
    gear_name: &str,
) -> anyhow::Result<&'a Gear> {
    gear_map.get(gear_name).ok_or_else(|| {
        anyhow!(
            "no gear named `:{gear_name}` in this env (available: {})",
            format_available(gear_map.keys().map(String::as_str))
        )
    })
}

/// Human-readable list of every cli entry on a gear; used by
/// `enw :<gear>` (no entry) to tell the user what's available. Two-space
/// indent + name; descriptions follow on the same line when present.
pub fn format_entry_list(gear_name: &str, gear: &Gear) -> String {
    use std::fmt::Write;
    let mut out = format!(":{gear_name} — {}\n", gear.description);
    if gear.cli.is_empty() {
        out.push_str("  (no cli entries)\n");
        return out;
    }
    let mut names: Vec<&str> = gear.cli.keys().map(String::as_str).collect();
    names.sort();
    for name in names {
        let entry = &gear.cli[name];
        match entry.description.as_deref() {
            Some(desc) => writeln!(out, "  {name} — {desc}").unwrap(),
            None => writeln!(out, "  {name}").unwrap(),
        }
    }
    out
}

/// Parse → resolve env + gear → exec. With no entry given, lists the
/// gear's cli entries and exits 0. Replaces the current process on
/// child failure (exit with child's status); other errors bubble.
pub fn dispatch(workspaces_directory: &Path, args: &[OsString]) -> anyhow::Result<()> {
    let target = parse_dispatch_args(args)?;
    let env_name = match target.env_override {
        Some(ref name) => name.clone(),
        None => active_env_name()?,
    };
    let env_dir = workspaces_directory.join(&env_name);
    let project_dir = env_project_dir(workspaces_directory, &env_name);

    let gear_map = LoadedGear::from_env_dir(&env_dir)
        .with_context(|| format!("could not load gear for env `{env_name}`"))?
        .into_map();

    let Some(entry_name) = target.entry_name.as_deref() else {
        let gear = resolve_gear(&gear_map, &target.gear_name)?;
        print!("{}", format_entry_list(&target.gear_name, gear));
        return Ok(());
    };

    let entry = resolve_entry(&gear_map, &target.gear_name, entry_name)?;
    ensure_confirmed(&target.gear_name, entry_name, entry, target.yes)?;
    let argv = build_argv(entry, &target.passthrough);
    let (program, rest) = argv
        .split_first()
        .ok_or_else(|| anyhow!("cli entry `{entry_name}` has empty command"))?;

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
            require_confirmation: false,
            ..Default::default()
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
            assert_eq!(t.entry_name.as_deref(), Some("build"));
            assert_eq!(t.passthrough, osvec(&["--release", "-j", "4"]));
        }

        #[test]
        fn passthrough_empty_when_no_extra_args() {
            let t = parse_dispatch_args(&osvec(&[":just", "build"])).unwrap();
            assert!(t.passthrough.is_empty());
        }

        #[test]
        fn entry_is_none_when_only_gear_given() {
            // `enw :just` (no entry) — dispatch turns this into a listing.
            let t = parse_dispatch_args(&osvec(&[":just"])).unwrap();
            assert_eq!(t.gear_name, "just");
            assert!(t.entry_name.is_none());
            assert!(t.passthrough.is_empty());
        }

        #[rstest]
        #[case::missing_leading_colon(&["just", "build"], "must start with `:`")]
        #[case::bare_colon(&[":", "build"], "empty")]
        #[case::empty_argv(&[], "missing :<gear>")]
        fn rejects_with_helpful_message(#[case] args: &[&str], #[case] needle: &str) {
            let err = parse_dispatch_args(&osvec(args)).expect_err("must reject");
            assert!(err.to_string().contains(needle), "got: {err}");
        }

        #[test]
        fn yes_defaults_to_false_when_flag_absent() {
            let t = parse_dispatch_args(&osvec(&[":just", "build"])).unwrap();
            assert!(!t.yes);
        }

        #[rstest]
        #[case::short(&["-y", ":just", "deploy"])]
        #[case::long(&["--yes", ":just", "deploy"])]
        fn pre_positional_yes_flag_is_consumed(#[case] args: &[&str]) {
            let t = parse_dispatch_args(&osvec(args)).unwrap();
            assert!(t.yes);
            assert_eq!(t.gear_name, "just");
            assert_eq!(t.entry_name.as_deref(), Some("deploy"));
            assert!(t.passthrough.is_empty());
        }

        #[test]
        fn yes_flag_after_gear_is_treated_as_passthrough() {
            let t = parse_dispatch_args(&osvec(&[":just", "deploy", "--yes"])).unwrap();
            assert!(!t.yes);
            assert_eq!(t.passthrough, osvec(&["--yes"]));
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

    mod listing {
        use super::*;

        fn just_gear(entries: &[(&str, Option<&str>)]) -> Gear {
            let cli = entries
                .iter()
                .map(|(name, desc)| {
                    (
                        (*name).to_owned(),
                        CliEntry {
                            description: desc.map(str::to_owned),
                            command: vec!["just".into(), (*name).into()],
                            require_confirmation: false,
                            ..Default::default()
                        },
                    )
                })
                .collect();
            Gear {
                description: "Tasks from the project's justfile".into(),
                cli,
                ..Default::default()
            }
        }

        #[test]
        fn header_uses_gear_name_and_description() {
            let listing = format_entry_list("just", &just_gear(&[]));
            assert!(
                listing.starts_with(":just — Tasks from the project's justfile"),
                "{listing}"
            );
        }

        #[test]
        fn lists_entries_alphabetically_with_descriptions() {
            let listing = format_entry_list(
                "just",
                &just_gear(&[
                    ("test", Some("Run tests")),
                    ("build", Some("Build the project")),
                    ("deploy", None),
                ]),
            );
            let lines: Vec<&str> = listing.lines().collect();
            assert_eq!(lines[1], "  build — Build the project");
            assert_eq!(lines[2], "  deploy");
            assert_eq!(lines[3], "  test — Run tests");
        }

        #[test]
        fn empty_cli_map_renders_placeholder() {
            let listing = format_entry_list("just", &just_gear(&[]));
            assert!(listing.contains("(no cli entries)"), "{listing}");
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

    mod confirmation {
        use super::*;

        fn entry_requiring_confirmation() -> CliEntry {
            CliEntry {
                description: None,
                command: vec!["just".into(), "deploy".into()],
                require_confirmation: true,
                ..Default::default()
            }
        }

        #[test]
        fn safe_entry_runs_without_yes() {
            ensure_confirmed("just", "build", &cli_entry(&["just", "build"]), false).unwrap();
        }

        #[test]
        fn unsafe_entry_with_yes_runs() {
            ensure_confirmed("just", "deploy", &entry_requiring_confirmation(), true).unwrap();
        }

        #[test]
        fn unsafe_entry_without_yes_errors_with_retry_hint() {
            let err = ensure_confirmed("just", "deploy", &entry_requiring_confirmation(), false)
                .expect_err("must refuse");
            let msg = err.to_string();
            assert!(msg.contains(":just deploy"), "{msg}");
            assert!(msg.contains("enw -y :just deploy"), "{msg}");
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
            let prior = std::env::var(ENWIRO_ENV_VAR).ok();
            // SAFETY: serial within this file; no parallel readers of ENWIRO_ENV.
            unsafe { std::env::remove_var(ENWIRO_ENV_VAR) };
            let err = active_env_name().unwrap_err();
            assert!(err.to_string().contains("ENWIRO_ENV"));
            if let Some(v) = prior {
                unsafe { std::env::set_var(ENWIRO_ENV_VAR, v) };
            }
        }
    }
}
