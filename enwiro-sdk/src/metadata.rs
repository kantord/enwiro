//! Shared plugin-metadata convention (issue #726).
//!
//! Every plugin kind exposes a `metadata` subcommand that prints a JSON
//! object on stdout. The object always carries an optional `capabilities`
//! list of `{name}` entries declaring the plugin's *optional* abilities;
//! kind-specific fields (e.g. a cookbook's `defaultPriority`) sit alongside
//! it. Required abilities are never declared here - they are part of the
//! kind's base contract (for the in-repo Rust plugins, enforced at compile
//! time by the `cli` core enums), so there is nothing to probe.
//!
//! Each kind has its own capability enum ([`crate::cookbook::CookbookCapability`],
//! [`crate::adapter::AdapterCapability`], [`crate::bridge::BridgeCapability`]),
//! so a host cannot ask a plugin about another kind's capability - the
//! question doesn't type-check. Unknown or kind-illegal names are dropped at
//! parse time: old hosts ignore new capabilities instead of erroring.
//!
//! The probe here is the single hardened path for reading any plugin's
//! metadata: bounded by a timeout (a plugin that ignores argv and starts its
//! long-running behavior must not hang the host), killed on overrun, and
//! best-effort - every failure mode yields `T::default()`, since a plugin
//! that predates or ignores the convention must simply be left alone.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// A plugin kind's capability vocabulary. Implemented by one enum per kind;
/// the enum's variants are the full set of capabilities plugins of that kind
/// are allowed to declare.
pub trait Capability: Copy + 'static {
    /// The capability's name on the wire, e.g. `"listen"`.
    fn wire_name(self) -> &'static str;

    /// Reverse of [`Self::wire_name`]. `None` for names this kind does not
    /// recognize - callers drop those for forward compatibility.
    fn from_wire_name(name: &str) -> Option<Self>;
}

/// One declared capability as it appears on the wire. An object rather than
/// a bare string so future capabilities can carry parameters without a
/// schema break.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawCapability {
    pub name: String,
}

/// The `capabilities` field shared by every kind's metadata type. Stores
/// the raw wire entries; typed queries go through a kind's [`Capability`]
/// enum so unknown names are naturally ignored rather than rejected.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeclaredCapabilities(Vec<RawCapability>);

impl DeclaredCapabilities {
    pub fn declare<C: Capability>(capabilities: impl IntoIterator<Item = C>) -> Self {
        Self(
            capabilities
                .into_iter()
                .map(|c| RawCapability {
                    name: c.wire_name().to_string(),
                })
                .collect(),
        )
    }

    pub fn has<C: Capability>(&self, capability: C) -> bool {
        self.0.iter().any(|c| c.name == capability.wire_name())
    }

    /// The declared capabilities this kind recognizes, in declaration
    /// order. Unknown names are skipped.
    pub fn recognized<C: Capability>(&self) -> impl Iterator<Item = C> + '_ {
        self.0.iter().filter_map(|c| C::from_wire_name(&c.name))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// How long a plugin gets to answer the `metadata` probe before the caller
/// gives up and treats it as declaring nothing.
pub const METADATA_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Poll cadence while waiting for the probed plugin to exit.
const PROBE_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How many times to retry a spawn that fails with ETXTBSY, and how long
/// to wait between attempts. A process forked concurrently (e.g. by another
/// thread) can briefly hold a freshly written plugin binary's write fd
/// open, making exec fail with "text file busy"; it clears as soon as that
/// child execs or exits.
const EXEC_BUSY_RETRIES: u32 = 10;
const EXEC_BUSY_RETRY_DELAY: Duration = Duration::from_millis(20);

/// Run `<plugin> metadata` and parse its stdout as `T`. Best-effort: any
/// failure (spawn error, non-zero exit, timeout, unparseable output) yields
/// `T::default()`.
pub fn fetch_metadata<T: DeserializeOwned + Default>(executable: &str) -> T {
    fetch_metadata_with_timeout(executable, METADATA_PROBE_TIMEOUT)
}

pub fn fetch_metadata_with_timeout<T: DeserializeOwned + Default>(
    executable: &str,
    timeout: Duration,
) -> T {
    match probe_metadata(executable, timeout) {
        Ok(metadata) => metadata,
        Err(e) => {
            tracing::debug!(%executable, error = %e, "Metadata probe failed, using defaults");
            T::default()
        }
    }
}

fn spawn_probe(executable: &str) -> std::io::Result<std::process::Child> {
    let mut attempt = 0;
    loop {
        let result = Command::new(executable)
            .arg("metadata")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        match result {
            Err(e)
                if e.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && attempt < EXEC_BUSY_RETRIES =>
            {
                attempt += 1;
                std::thread::sleep(EXEC_BUSY_RETRY_DELAY);
            }
            other => return other,
        }
    }
}

fn probe_metadata<T: DeserializeOwned>(executable: &str, timeout: Duration) -> anyhow::Result<T> {
    let mut child = spawn_probe(executable)
        .map_err(|e| anyhow::anyhow!("Failed to spawn plugin metadata command: {e}"))?;

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!("Plugin did not answer the metadata probe within {timeout:?}");
            }
            Ok(None) => std::thread::sleep(PROBE_POLL_INTERVAL),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!("Failed to wait for plugin metadata command: {e}");
            }
        }
    };

    if !status.success() {
        anyhow::bail!("Plugin metadata command exited with {status}");
    }

    let mut stdout = String::new();
    child
        .stdout
        .take()
        .expect("stdout was piped")
        .read_to_string(&mut stdout)
        .map_err(|e| anyhow::anyhow!("Plugin metadata produced unreadable output: {e}"))?;

    serde_json::from_str(&stdout)
        .map_err(|e| anyhow::anyhow!("Failed to parse plugin metadata: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum TestCapability {
        Listen,
    }

    impl Capability for TestCapability {
        fn wire_name(self) -> &'static str {
            match self {
                TestCapability::Listen => "listen",
            }
        }

        fn from_wire_name(name: &str) -> Option<Self> {
            match name {
                "listen" => Some(TestCapability::Listen),
                _ => None,
            }
        }
    }

    #[derive(Debug, Default, PartialEq, serde::Deserialize)]
    #[serde(rename_all = "camelCase", default)]
    struct TestMetadata {
        capabilities: DeclaredCapabilities,
    }

    /// Generous: only bounds the worst case for scripts that exit on their
    /// own; a loaded test machine can take surprisingly long to spawn a
    /// shell. Tests that exercise the timeout path use a short one instead.
    const TEST_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
    const TEST_SHORT_TIMEOUT: Duration = Duration::from_millis(500);

    fn write_script(dir: &Path, name: &str, body: &str) -> String {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().to_string()
    }

    #[test]
    fn declared_capabilities_answer_has() {
        let caps = DeclaredCapabilities::declare([TestCapability::Listen]);
        assert!(caps.has(TestCapability::Listen));
        assert!(!caps.is_empty());
    }

    #[test]
    fn empty_capabilities_answer_nothing() {
        let caps = DeclaredCapabilities::default();
        assert!(!caps.has(TestCapability::Listen));
        assert!(caps.is_empty());
    }

    #[test]
    fn unknown_wire_names_are_skipped_by_recognized() {
        let caps: DeclaredCapabilities =
            serde_json::from_str(r#"[{"name":"listen"},{"name":"from-the-future"}]"#).unwrap();
        let recognized: Vec<TestCapability> = caps.recognized().collect();
        assert_eq!(recognized, vec![TestCapability::Listen]);
        assert!(caps.has(TestCapability::Listen));
    }

    #[test]
    fn declare_roundtrips_through_json() {
        let caps = DeclaredCapabilities::declare([TestCapability::Listen]);
        let json = serde_json::to_string(&caps).unwrap();
        assert_eq!(json, r#"[{"name":"listen"}]"#);
        let parsed: DeclaredCapabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, caps);
    }

    #[test]
    fn fetch_parses_declared_capabilities() {
        let dir = tempfile::tempdir().unwrap();
        let exe = write_script(
            dir.path(),
            "plugin-with-listen",
            r#"echo '{"capabilities":[{"name":"listen"}]}'"#,
        );
        let metadata: TestMetadata = fetch_metadata_with_timeout(&exe, TEST_PROBE_TIMEOUT);
        assert!(metadata.capabilities.has(TestCapability::Listen));
    }

    #[test]
    fn fetch_treats_garbage_output_as_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let exe = write_script(dir.path(), "plugin-garbage", "echo 'row one\trofi entry'");
        let metadata: TestMetadata = fetch_metadata_with_timeout(&exe, TEST_PROBE_TIMEOUT);
        assert_eq!(metadata, TestMetadata::default());
    }

    #[test]
    fn fetch_treats_nonzero_exit_as_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let exe = write_script(
            dir.path(),
            "plugin-failing",
            r#"echo '{"capabilities":[{"name":"listen"}]}'; exit 1"#,
        );
        let metadata: TestMetadata = fetch_metadata_with_timeout(&exe, TEST_PROBE_TIMEOUT);
        assert_eq!(metadata, TestMetadata::default());
    }

    #[test]
    fn fetch_kills_plugin_that_never_answers() {
        let dir = tempfile::tempdir().unwrap();
        let exe = write_script(dir.path(), "plugin-hanging", "sleep 60");
        let started = Instant::now();
        let metadata: TestMetadata = fetch_metadata_with_timeout(&exe, TEST_SHORT_TIMEOUT);
        assert_eq!(metadata, TestMetadata::default());
        assert!(started.elapsed() < Duration::from_secs(30));
    }

    #[test]
    fn fetch_treats_missing_executable_as_defaults() {
        let metadata: TestMetadata =
            fetch_metadata_with_timeout("/nonexistent/enwiro-plugin-x", TEST_PROBE_TIMEOUT);
        assert_eq!(metadata, TestMetadata::default());
    }
}
