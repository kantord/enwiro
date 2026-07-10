//! Wire types for the core->adapter activate protocol.
//!
//! Core constructs `ActivatePayload` once per `enw activate <env>` call
//! and pipes the JSON-serialized form to the adapter's stdin. Adapters
//! deserialize it and use the fields they care about (`managed_envs` for
//! workspace-slot scoring, `gear` for auto-open behavior). Adapters that
//! don't care about a given field can ignore it; serde's defaults apply
//! when the field is absent, which keeps older adapters forward-compatible
//! with newer cores as long as additions are non-breaking.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::gear::Gear;
use crate::metadata::{Capability, DeclaredCapabilities};

/// The capabilities an adapter is allowed to declare. Required subcommands
/// (`get-active-workspace-id`, `activate`, `run`, `metadata`) are the
/// kind's base contract and are never declared here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterCapability {
    /// The daemon spawns and supervises the adapter's `listen` subcommand,
    /// which emits workspace-switch events on stdout. Declaring this
    /// commits the adapter to accepting a `--debounce-secs <seconds>` flag
    /// on `listen`: the daemon always passes it, and an adapter that
    /// rejects the flag would exit on a usage error and be crash-looped by
    /// the process pool.
    Listen,
}

impl Capability for AdapterCapability {
    const ALL: &'static [Self] = &[AdapterCapability::Listen];

    fn wire_name(self) -> &'static str {
        match self {
            AdapterCapability::Listen => "listen",
        }
    }
}

/// Stdout of the adapter's `metadata` subcommand - the shared
/// plugin-metadata convention (see [`crate::metadata`]). Adapters that
/// predate the convention probe to the default (no capabilities) and are
/// simply left alone by the daemon.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AdapterMetadata {
    #[serde(skip_serializing_if = "DeclaredCapabilities::is_empty")]
    pub capabilities: DeclaredCapabilities,
}

impl AdapterMetadata {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("AdapterMetadata is always serializable")
    }

    pub fn with_capabilities(capabilities: impl IntoIterator<Item = AdapterCapability>) -> Self {
        Self {
            capabilities: DeclaredCapabilities::declare(capabilities),
        }
    }

    pub fn has(&self, capability: AdapterCapability) -> bool {
        self.capabilities.has(capability)
    }
}

/// Run `<adapter> metadata` and parse its stdout. Best-effort: any failure
/// yields the default metadata (no capabilities).
pub fn fetch_adapter_metadata(executable: &str) -> AdapterMetadata {
    crate::metadata::fetch_metadata(executable)
}

/// Wire format version. Bumped when `ActivatePayload` shape changes in a
/// backward-incompatible way; adapters can match on `payload.version` to
/// branch behavior across multiple core versions.
pub const ACTIVATE_PAYLOAD_VERSION: u32 = 1;

/// One enwiro-managed env, with the score the adapter uses to place it
/// into a workspace slot.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ManagedEnvInfo {
    pub name: String,
    pub slot_score: f64,
}

/// Stdin payload for the adapter's `activate` subcommand.
///
/// `gear` is intentionally an opaque `serde_json::Value` rather than a
/// typed map. Adapters that auto-open URLs walk
/// `gear.<name>.web.<entry>.url` directly; future schema additions (a
/// `cli` category, a `type` field, etc.) don't require a protocol bump
/// because adapters only read fields they recognize. Core constructs the
/// `gear` field from its typed `HashMap<String, Gear>` via
/// [`ActivatePayload::from_owned`].
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct ActivatePayload {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub managed_envs: Vec<ManagedEnvInfo>,
    #[serde(default)]
    pub gear: serde_json::Value,
}

impl ActivatePayload {
    /// Build a payload from core's owned data. Converts the typed gear
    /// map to its JSON representation so the wire format matches what
    /// adapters expect to deserialize.
    pub fn from_owned(managed_envs: Vec<ManagedEnvInfo>, gear: &HashMap<String, Gear>) -> Self {
        Self {
            version: ACTIVATE_PAYLOAD_VERSION,
            managed_envs,
            gear: serde_json::to_value(gear).unwrap_or(serde_json::Value::Null),
        }
    }
}

/// Wire format version for [`RunPayload`]. Bumped when the shape changes
/// in a backward-incompatible way.
pub const RUN_PAYLOAD_VERSION: u32 = 1;

/// Stdin payload for the adapter's `run` subcommand.
///
/// Core constructs this once per `enw run <cmd> [args]` call and pipes
/// the JSON-serialized form to the adapter's stdin. The adapter is
/// responsible for spawning `command` (with `args`) in whatever context
/// is native to it: a new terminal window (i3wm), a new tmux window
/// (tmux), inline `exec` (shell), etc. Adapters MUST inject
/// `ENWIRO_ENV=env_name` and set cwd to `env_path` on the spawned child.
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct RunPayload {
    #[serde(default)]
    pub version: u32,
    pub env_name: String,
    pub env_path: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

impl RunPayload {
    pub fn new(env_name: String, env_path: String, command: String, args: Vec<String>) -> Self {
        Self {
            version: RUN_PAYLOAD_VERSION,
            env_name,
            env_path,
            command,
            args,
        }
    }

    pub fn read_from_stdin() -> anyhow::Result<Self> {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("Could not read run payload from stdin")?;
        serde_json::from_str(&buf).context("Could not parse run payload as JSON")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_roundtrips_and_answers_has() {
        let metadata = AdapterMetadata::with_capabilities([AdapterCapability::Listen]);
        assert_eq!(
            metadata.to_json(),
            r#"{"capabilities":[{"name":"listen"}]}"#
        );
        let parsed: AdapterMetadata = serde_json::from_str(&metadata.to_json()).unwrap();
        assert!(parsed.has(AdapterCapability::Listen));
    }

    #[test]
    fn default_metadata_serializes_to_empty_object_and_declares_nothing() {
        let metadata = AdapterMetadata::default();
        assert_eq!(metadata.to_json(), "{}");
        assert!(!metadata.has(AdapterCapability::Listen));
    }
}
