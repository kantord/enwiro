//! Garnishes: parallel-to-cookbook extensions. Each Garnish looks at an
//! env and, when applicable, contributes a `Gear` payload. Many can
//! apply to one env simultaneously.
//!
//! - Cookbook: per-tool integration, attached at cook time.
//! - Garnish:  per-project-shape integration, auto-attached when its
//!   activation predicate matches.
//!
//! Garnishes ship as separate binaries (`enwiro-garnish-<name>`) and are
//! discovered via [`crate::plugin::get_plugins`] like cookbooks. Each
//! binary implements two subcommands:
//!
//! - `applies-to <project_dir>` — exit 0 if it wants to contribute gear.
//! - `gear <project_dir>`       — stdout = a `GearFileData` JSON document.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::process::Command;

use anyhow::Context;

use crate::gear::GearFileData;
use crate::plugin::Plugin;

/// Activation predicate + gear contribution.
pub trait Garnish: Send + Sync {
    /// Stable kebab-case identifier; appears in diagnostic logs and in
    /// the `gear.d/garnish-X.json` filename.
    fn name(&self) -> &str;

    /// `gear.d/` filename for this Garnish's contribution. Prefix keeps
    /// cookbook and garnish files from colliding.
    fn filename(&self) -> String {
        format!("garnish-{}.json", self.name())
    }

    /// Cheap check: does this Garnish want to contribute gear to the
    /// project at `project_dir`?
    fn applies_to(&self, project_dir: &Path) -> bool;

    /// Produce the gear payload. Called only when `applies_to` returned
    /// true. `Ok(None)` = "applies but nothing to say right now."
    fn gear(&self, project_dir: &Path) -> anyhow::Result<Option<GearFileData>>;
}

/// Subprocess-backed implementation of [`Garnish`]. Wraps a discovered
/// `enwiro-garnish-<name>` binary and dispatches each trait method to a
/// CLI subcommand.
pub struct GarnishClient {
    plugin: Plugin,
}

impl GarnishClient {
    pub fn new(plugin: Plugin) -> Self {
        Self { plugin }
    }
}

impl Garnish for GarnishClient {
    fn name(&self) -> &str {
        &self.plugin.name
    }

    fn applies_to(&self, project_dir: &Path) -> bool {
        Command::new(&self.plugin.executable)
            .arg("applies-to")
            .arg(project_dir)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn gear(&self, project_dir: &Path) -> anyhow::Result<Option<GearFileData>> {
        let output = Command::new(&self.plugin.executable)
            .arg("gear")
            .arg(project_dir)
            .output()
            .with_context(|| format!("spawn `{} gear`", self.plugin.executable))?;
        if !output.status.success() {
            anyhow::bail!(
                "`{} gear` exited with {}: {}",
                self.plugin.executable,
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        if output.stdout.iter().all(u8::is_ascii_whitespace) {
            return Ok(None);
        }
        let data: GearFileData = serde_json::from_slice(&output.stdout)
            .with_context(|| format!("parse `{} gear` stdout", self.plugin.executable))?;
        Ok(Some(data))
    }
}

/// Run a Garnish with panic safety. Errors and panics in `applies_to`
/// or `gear()` are debug-logged and swallowed — a misbehaving Garnish
/// must not block the rest. `None` = doesn't apply / nothing to say /
/// failed.
pub fn run_garnish(garnish: &dyn Garnish, project_dir: &Path) -> Option<GearFileData> {
    let name = garnish.name();

    let applies = catch_unwind(AssertUnwindSafe(|| garnish.applies_to(project_dir)))
        .inspect_err(|_| tracing::debug!(garnish = name, "applies_to panicked; skipping"))
        .ok()?;
    if !applies {
        return None;
    }

    match catch_unwind(AssertUnwindSafe(|| garnish.gear(project_dir))) {
        Ok(Ok(data)) => data,
        Ok(Err(err)) => {
            tracing::debug!(garnish = name, error = %err, "gear() errored; skipping");
            None
        }
        Err(_) => {
            tracing::debug!(garnish = name, "gear() panicked; skipping");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gear::{Gear, SCHEMA_VERSION};
    use crate::plugin::PluginKind;
    use std::collections::HashMap;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn one_gear(name: &str, description: &str) -> GearFileData {
        GearFileData {
            version: SCHEMA_VERSION,
            gear: HashMap::from([(
                name.into(),
                Gear {
                    description: description.into(),
                    ..Default::default()
                },
            )]),
        }
    }

    mod run {
        use super::*;

        /// One configurable fake so each test sets only the fields it cares
        /// about, instead of a proliferation of one-off types.
        struct FakeGarnish {
            applies: bool,
            result: FakeResult,
        }

        enum FakeResult {
            Some,
            None,
            Err,
            PanicInApplies,
            PanicInGear,
        }

        impl Garnish for FakeGarnish {
            fn name(&self) -> &str {
                "fixture"
            }
            fn applies_to(&self, _: &Path) -> bool {
                if matches!(self.result, FakeResult::PanicInApplies) {
                    panic!("test panic");
                }
                self.applies
            }
            fn gear(&self, _: &Path) -> anyhow::Result<Option<GearFileData>> {
                match self.result {
                    FakeResult::Some => {
                        Ok(Some(one_gear("just", "Tasks from the project's justfile")))
                    }
                    FakeResult::None => Ok(None),
                    FakeResult::Err => Err(anyhow::anyhow!("boom")),
                    FakeResult::PanicInGear => panic!("test panic"),
                    FakeResult::PanicInApplies => unreachable!(),
                }
            }
        }

        fn run(applies: bool, result: FakeResult) -> Option<GearFileData> {
            run_garnish(&FakeGarnish { applies, result }, Path::new("/nowhere"))
        }

        #[test]
        fn filename_uses_garnish_prefix() {
            assert_eq!(
                FakeGarnish {
                    applies: false,
                    result: FakeResult::None
                }
                .filename(),
                "garnish-fixture.json"
            );
        }

        #[test]
        fn returns_gear_when_applies_and_gear_is_some() {
            let out = run(true, FakeResult::Some).expect("Some");
            assert_eq!(out.version, SCHEMA_VERSION);
            assert_eq!(
                out.gear["just"].description,
                "Tasks from the project's justfile"
            );
        }

        #[test]
        fn returns_none_when_does_not_apply() {
            assert!(run(false, FakeResult::Some).is_none());
        }

        #[test]
        fn returns_none_when_gear_emits_none() {
            assert!(run(true, FakeResult::None).is_none());
        }

        #[test]
        fn swallows_gear_error() {
            assert!(run(true, FakeResult::Err).is_none());
        }

        #[test]
        fn swallows_panic_in_applies_to() {
            assert!(run(false, FakeResult::PanicInApplies).is_none());
        }

        #[test]
        fn swallows_panic_in_gear() {
            assert!(run(true, FakeResult::PanicInGear).is_none());
        }
    }

    mod client {
        use super::*;

        /// Drops a fake garnish binary (POSIX shell) into a tempdir,
        /// matched against the given subcommand → output script body.
        /// Returns a `GarnishClient` pointed at it.
        fn fake_garnish(name: &str, script: &str) -> (tempfile::TempDir, GarnishClient) {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(format!("enwiro-garnish-{name}"));
            fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            let plugin = Plugin {
                name: name.into(),
                kind: PluginKind::Garnish,
                executable: path.to_string_lossy().into(),
            };
            (dir, GarnishClient::new(plugin))
        }

        #[test]
        fn name_uses_plugin_name() {
            let (_d, c) = fake_garnish("just", "exit 0");
            assert_eq!(c.name(), "just");
        }

        #[test]
        fn applies_to_true_when_binary_exits_zero() {
            let (_d, c) = fake_garnish("just", r#"case "$1" in applies-to) exit 0 ;; esac"#);
            assert!(c.applies_to(Path::new("/tmp")));
        }

        #[test]
        fn applies_to_false_when_binary_exits_nonzero() {
            let (_d, c) = fake_garnish("just", r#"case "$1" in applies-to) exit 1 ;; esac"#);
            assert!(!c.applies_to(Path::new("/tmp")));
        }

        #[test]
        fn gear_parses_stdout_as_gearfiledata() {
            let json = r#"{"version":1,"gear":{"just":{"description":"x"}}}"#;
            let (_d, c) = fake_garnish(
                "just",
                &format!(r#"case "$1" in gear) printf '{json}' ;; esac"#),
            );
            let out = c.gear(Path::new("/tmp")).unwrap().unwrap();
            assert_eq!(out.version, 1);
            assert_eq!(out.gear["just"].description, "x");
        }

        #[test]
        fn gear_returns_none_for_empty_stdout() {
            let (_d, c) = fake_garnish("just", r#"case "$1" in gear) exit 0 ;; esac"#);
            assert!(c.gear(Path::new("/tmp")).unwrap().is_none());
        }

        #[test]
        fn gear_errors_on_nonzero_exit() {
            let (_d, c) = fake_garnish(
                "just",
                r#"case "$1" in gear) echo "broken" >&2; exit 2 ;; esac"#,
            );
            let err = c.gear(Path::new("/tmp")).unwrap_err();
            assert!(err.to_string().contains("exited with"));
        }
    }
}
