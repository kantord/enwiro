//! Bridge plugin protocol types (issue #485).
//!
//! A bridge is a standalone `enwiro-bridge-*` binary that integrates enwiro
//! with another application. Bridges declare daemon-relevant abilities via a
//! `metadata` subcommand that prints [`BridgeMetadata`] as JSON on stdout,
//! mirroring the cookbook `metadata` convention.
//!
//! The daemon probes every discovered bridge with `<bridge> metadata` at
//! startup. A bridge that declares the [`LISTEN_CAPABILITY`] gets its
//! `listen` subcommand spawned and supervised by the daemon; anything else
//! (probe failure, timeout, unparseable output, no capability) means the
//! bridge is left alone.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Capability name a bridge declares to have its `listen` subcommand
/// spawned and supervised by the daemon.
pub const LISTEN_CAPABILITY: &str = "listen";

/// How long a bridge gets to answer the `metadata` probe before the caller
/// gives up and treats it as declaring nothing. Guards the daemon against
/// bridges that ignore argv and simply start their long-running behavior.
const METADATA_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Poll cadence while waiting for the probed bridge to exit.
const PROBE_POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BridgeMetadata {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<BridgeCapability>,
}

/// A single declared ability. An object rather than a bare string so future
/// capabilities can carry parameters without a schema break; consumers must
/// ignore capability names they don't recognize.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeCapability {
    pub name: String,
}

impl BridgeMetadata {
    pub fn from_json(s: &str) -> anyhow::Result<Self> {
        serde_json::from_str(s).map_err(|e| anyhow::anyhow!("Failed to parse bridge metadata: {e}"))
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("BridgeMetadata is always serializable")
    }

    pub fn with_capabilities<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            capabilities: names
                .into_iter()
                .map(|name| BridgeCapability { name: name.into() })
                .collect(),
        }
    }

    pub fn has_capability(&self, name: &str) -> bool {
        self.capabilities.iter().any(|c| c.name == name)
    }
}

/// Run `<bridge> metadata` and parse its stdout. Best-effort: any failure
/// (spawn error, non-zero exit, timeout, unparseable output) yields the
/// default metadata (no capabilities), since a bridge that predates or
/// ignores the convention must simply be left alone.
pub fn fetch_bridge_metadata(executable: &str) -> BridgeMetadata {
    fetch_bridge_metadata_with_timeout(executable, METADATA_PROBE_TIMEOUT)
}

pub fn fetch_bridge_metadata_with_timeout(executable: &str, timeout: Duration) -> BridgeMetadata {
    match probe_metadata(executable, timeout) {
        Ok(metadata) => metadata,
        Err(e) => {
            tracing::debug!(%executable, error = %e, "Bridge metadata probe failed, treating as no capabilities");
            BridgeMetadata::default()
        }
    }
}

fn probe_metadata(executable: &str, timeout: Duration) -> anyhow::Result<BridgeMetadata> {
    let mut child = Command::new(executable)
        .arg("metadata")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to spawn bridge metadata command: {e}"))?;

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!("Bridge did not answer the metadata probe within {timeout:?}");
            }
            Ok(None) => std::thread::sleep(PROBE_POLL_INTERVAL),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!("Failed to wait for bridge metadata command: {e}");
            }
        }
    };

    if !status.success() {
        anyhow::bail!("Bridge metadata command exited with {status}");
    }

    let mut stdout = String::new();
    child
        .stdout
        .take()
        .expect("stdout was piped")
        .read_to_string(&mut stdout)
        .map_err(|e| anyhow::anyhow!("Bridge metadata produced unreadable output: {e}"))?;

    BridgeMetadata::from_json(&stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

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
    fn default_metadata_serializes_to_empty_object() {
        assert_eq!(BridgeMetadata::default().to_json(), "{}");
    }

    #[test]
    fn metadata_roundtrips_through_json() {
        let metadata = BridgeMetadata::with_capabilities(["listen"]);
        let parsed = BridgeMetadata::from_json(&metadata.to_json()).unwrap();
        assert_eq!(parsed, metadata);
        assert!(parsed.has_capability(LISTEN_CAPABILITY));
        assert!(!parsed.has_capability("unknown"));
    }

    #[test]
    fn empty_object_parses_to_no_capabilities() {
        let parsed = BridgeMetadata::from_json("{}").unwrap();
        assert_eq!(parsed, BridgeMetadata::default());
    }

    #[test]
    fn fetch_parses_declared_capabilities() {
        let dir = tempfile::tempdir().unwrap();
        let exe = write_script(
            dir.path(),
            "bridge-with-listen",
            r#"echo '{"capabilities":[{"name":"listen"}]}'"#,
        );
        let metadata = fetch_bridge_metadata_with_timeout(&exe, TEST_PROBE_TIMEOUT);
        assert!(metadata.has_capability(LISTEN_CAPABILITY));
    }

    #[test]
    fn fetch_treats_garbage_output_as_no_capabilities() {
        let dir = tempfile::tempdir().unwrap();
        let exe = write_script(dir.path(), "bridge-garbage", "echo 'row one\trofi entry'");
        let metadata = fetch_bridge_metadata_with_timeout(&exe, TEST_PROBE_TIMEOUT);
        assert_eq!(metadata, BridgeMetadata::default());
    }

    #[test]
    fn fetch_treats_nonzero_exit_as_no_capabilities() {
        let dir = tempfile::tempdir().unwrap();
        let exe = write_script(
            dir.path(),
            "bridge-failing",
            r#"echo '{"capabilities":[{"name":"listen"}]}'; exit 1"#,
        );
        let metadata = fetch_bridge_metadata_with_timeout(&exe, TEST_PROBE_TIMEOUT);
        assert_eq!(metadata, BridgeMetadata::default());
    }

    #[test]
    fn fetch_kills_bridge_that_never_answers() {
        let dir = tempfile::tempdir().unwrap();
        let exe = write_script(dir.path(), "bridge-hanging", "sleep 60");
        let started = Instant::now();
        let metadata = fetch_bridge_metadata_with_timeout(&exe, TEST_SHORT_TIMEOUT);
        assert_eq!(metadata, BridgeMetadata::default());
        assert!(started.elapsed() < Duration::from_secs(30));
    }

    #[test]
    fn fetch_treats_missing_executable_as_no_capabilities() {
        let metadata =
            fetch_bridge_metadata_with_timeout("/nonexistent/enwiro-bridge-x", TEST_PROBE_TIMEOUT);
        assert_eq!(metadata, BridgeMetadata::default());
    }
}
