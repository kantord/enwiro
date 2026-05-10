//! Wire types for the core->adapter activate protocol.
//!
//! Core constructs `ActivatePayload` once per `enw activate <env>` call
//! and pipes the JSON-serialized form to the adapter's stdin. Adapters
//! deserialize it and use the fields they care about (`managed_envs` for
//! workspace-slot scoring, `gear` for auto-open behavior). Adapters that
//! don't care about a given field can ignore it; serde's defaults apply
//! when the field is absent, which keeps older adapters forward-compatible
//! with newer cores as long as additions are non-breaking.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::gear::Gear;

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
