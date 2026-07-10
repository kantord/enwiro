//! Garnishes: parallel-to-cookbook extensions. Each Garnish looks at an
//! env and, when applicable, contributes a `Gear` payload. Many can
//! apply to one env simultaneously.
//!
//! - Cookbook: per-tool integration, attached at cook time.
//! - Garnish:  per-project-shape integration, auto-attached when it has
//!   something to contribute.
//!
//! Garnishes ship as separate binaries (`enwiro-garnish-<name>`) and are
//! discovered via [`crate::plugin::get_plugins`] like cookbooks. Each
//! binary implements one subcommand:
//!
//! - `gear <project_dir>` — stdout = a `GearFileData` JSON document, or
//!   nothing at all (empty/whitespace output) when the garnish has no
//!   contribution for this project.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::process::Command;

use anyhow::Context;

use crate::gear::GearFileData;
use crate::plugin::Plugin;

/// A garnish's gear contribution.
pub trait Garnish: Send + Sync {
    /// Stable kebab-case identifier; appears in diagnostic logs and in
    /// the `gear.d/garnish-X.json` filename.
    fn name(&self) -> &str;

    /// `gear.d/` filename for this Garnish's contribution. Prefix keeps
    /// cookbook and garnish files from colliding.
    fn filename(&self) -> String {
        format!("garnish-{}.json", self.name())
    }

    /// Produce the gear payload for the project at `project_dir`.
    /// `Ok(None)` = "nothing to contribute here" — the garnish doesn't
    /// apply to this project, or applies but has nothing to say.
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
        self.plugin.name.as_str()
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

/// Run a Garnish with panic safety. Errors and panics in `gear()` are
/// debug-logged and swallowed — a misbehaving Garnish must not block the
/// rest. `None` = nothing to contribute / failed.
pub fn run_garnish(garnish: &dyn Garnish, project_dir: &Path) -> Option<GearFileData> {
    let name = garnish.name();

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
    use std::collections::HashMap;

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

        struct FakeGarnish {
            result: FakeResult,
        }

        enum FakeResult {
            Some,
            None,
            Err,
            Panic,
        }

        impl Garnish for FakeGarnish {
            fn name(&self) -> &str {
                "fixture"
            }
            fn gear(&self, _: &Path) -> anyhow::Result<Option<GearFileData>> {
                match self.result {
                    FakeResult::Some => {
                        Ok(Some(one_gear("just", "Tasks from the project's justfile")))
                    }
                    FakeResult::None => Ok(None),
                    FakeResult::Err => Err(anyhow::anyhow!("boom")),
                    FakeResult::Panic => panic!("test panic"),
                }
            }
        }

        fn run(result: FakeResult) -> Option<GearFileData> {
            run_garnish(&FakeGarnish { result }, Path::new("/nowhere"))
        }

        #[test]
        fn filename_uses_garnish_prefix() {
            assert_eq!(
                FakeGarnish {
                    result: FakeResult::None
                }
                .filename(),
                "garnish-fixture.json"
            );
        }

        #[test]
        fn returns_gear_when_gear_is_some() {
            let out = run(FakeResult::Some).expect("Some");
            assert_eq!(out.version, SCHEMA_VERSION);
            assert_eq!(
                out.gear["just"].description,
                "Tasks from the project's justfile"
            );
        }

        #[test]
        fn returns_none_when_gear_emits_none() {
            assert!(run(FakeResult::None).is_none());
        }

        #[test]
        fn swallows_gear_error() {
            assert!(run(FakeResult::Err).is_none());
        }

        #[test]
        fn swallows_panic_in_gear() {
            assert!(run(FakeResult::Panic).is_none());
        }
    }
}
