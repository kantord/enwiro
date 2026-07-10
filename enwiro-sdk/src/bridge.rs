//! Bridge plugin protocol types (issue #485).
//!
//! A bridge is a standalone `enwiro-bridge-*` binary that integrates enwiro
//! with another application. Bridges declare daemon-relevant abilities via a
//! `metadata` subcommand that prints [`BridgeMetadata`] as JSON on stdout -
//! the shared plugin-metadata convention (see [`crate::metadata`]).
//!
//! The daemon probes every discovered bridge with `<bridge> metadata` at
//! startup. A bridge that declares [`BridgeCapability::Listen`] gets its
//! `listen` subcommand spawned and supervised by the daemon; anything else
//! (probe failure, timeout, unparseable output, no capability) means the
//! bridge is left alone.

use serde::{Deserialize, Serialize};

use crate::metadata::{Capability, DeclaredCapabilities};

/// The capabilities a bridge is allowed to declare.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeCapability {
    /// The daemon spawns and supervises the bridge's `listen` subcommand.
    Listen,
}

impl Capability for BridgeCapability {
    const ALL: &'static [Self] = &[BridgeCapability::Listen];

    fn wire_name(self) -> &'static str {
        match self {
            BridgeCapability::Listen => "listen",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BridgeMetadata {
    #[serde(skip_serializing_if = "DeclaredCapabilities::is_empty")]
    pub capabilities: DeclaredCapabilities,
}

impl BridgeMetadata {
    pub fn from_json(s: &str) -> anyhow::Result<Self> {
        serde_json::from_str(s).map_err(|e| anyhow::anyhow!("Failed to parse bridge metadata: {e}"))
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("BridgeMetadata is always serializable")
    }

    pub fn with_capabilities(capabilities: impl IntoIterator<Item = BridgeCapability>) -> Self {
        Self {
            capabilities: DeclaredCapabilities::declare(capabilities),
        }
    }

    pub fn has(&self, capability: BridgeCapability) -> bool {
        self.capabilities.has(capability)
    }
}

/// Run `<bridge> metadata` and parse its stdout. Best-effort: any failure
/// (spawn error, non-zero exit, timeout, unparseable output) yields the
/// default metadata (no capabilities), since a bridge that predates or
/// ignores the convention must simply be left alone.
pub fn fetch_bridge_metadata(executable: &str) -> BridgeMetadata {
    crate::metadata::fetch_metadata(executable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_metadata_serializes_to_empty_object() {
        assert_eq!(BridgeMetadata::default().to_json(), "{}");
    }

    #[test]
    fn metadata_roundtrips_through_json() {
        let metadata = BridgeMetadata::with_capabilities([BridgeCapability::Listen]);
        let parsed = BridgeMetadata::from_json(&metadata.to_json()).unwrap();
        assert_eq!(parsed, metadata);
        assert!(parsed.has(BridgeCapability::Listen));
    }

    #[test]
    fn wire_format_is_unchanged_from_issue_485() {
        let metadata = BridgeMetadata::with_capabilities([BridgeCapability::Listen]);
        assert_eq!(
            metadata.to_json(),
            r#"{"capabilities":[{"name":"listen"}]}"#
        );
    }

    #[test]
    fn empty_object_parses_to_no_capabilities() {
        let parsed = BridgeMetadata::from_json("{}").unwrap();
        assert_eq!(parsed, BridgeMetadata::default());
    }

    #[test]
    fn unknown_capability_names_are_tolerated() {
        let parsed =
            BridgeMetadata::from_json(r#"{"capabilities":[{"name":"from-the-future"}]}"#).unwrap();
        assert!(!parsed.has(BridgeCapability::Listen));
    }
}
