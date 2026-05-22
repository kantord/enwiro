//! Shared JSON-RPC contract between `enwiro-daemon` and its clients
//! (`enw`, cookbooks, future external apps).
//!
//! See `docs/adr/0002-daemon-ipc-architecture.md`. We use jsonrpsee 0.26 as
//! the protocol layer: id correlation, envelope serialisation, error codes,
//! batching, notifications all live in the library. We own only the typed
//! trait below + the UDS transport plumbing in `rpc::client` (consumer side)
//! and `enwiro-daemon::rpc` (server side).
//!
//! Plugin↔host data emission (cookbook stdout-JSONL) is unaffected — this
//! channel is strictly for client↔daemon.

use serde::{Deserialize, Serialize};

/// Env var the daemon publishes so child processes can discover the live
/// socket without computing the path themselves.
pub const SOCKET_ENV_VAR: &str = "ENWIRO_RPC_SOCKET";

/// Env var carrying the cookbook-call chain across nested `cookbook.invoke`
/// spawns; SDK helpers forward it automatically.
pub const CALL_CHAIN_ENV_VAR: &str = "ENWIRO_RPC_CALL_CHAIN";

/// Default socket file name under `$XDG_RUNTIME_DIR/enwiro/`.
pub const SOCKET_FILENAME: &str = "rpc.sock";

/// Implementation-defined JSON-RPC error code returned when a cookbook
/// could not be located, exited non-zero, or produced invalid UTF-8.
pub const APPLICATION_ERROR_CODE: i32 = -32000;

/// Implementation-defined JSON-RPC error code returned when a
/// `cookbook.invoke` would extend a chain that already contains the
/// requested cookbook (ADR-0002 §4 cycle detection).
pub const CYCLE_DETECTED_CODE: i32 = -32001;

/// Resolve the default socket path. `$XDG_RUNTIME_DIR/enwiro/rpc.sock`.
pub fn default_socket_path() -> anyhow::Result<std::path::PathBuf> {
    let base = dirs::runtime_dir()
        .or_else(|| dirs::cache_dir().map(|d| d.join("run")))
        .ok_or_else(|| anyhow::anyhow!("could not determine runtime or cache directory"))?;
    Ok(base.join("enwiro").join(SOCKET_FILENAME))
}

/// Params for `cookbook.invoke`: delegate a cookbook operation through the
/// daemon. The daemon resolves `cookbook` to a plugin binary, spawns it
/// with the existing stdout-JSONL contract, pipes `payload` to its stdin
/// as a `CookbookPayload`, and returns the parsed result.
///
/// `payload` is the resolved cookbook configuration (caller is responsible
/// for running the project-layer config walker; the daemon has no cwd).
///
/// `call_chain` carries the names of cookbooks already invoked in the
/// current spawn tree for cycle detection. The SDK client helper forwards
/// `$ENWIRO_RPC_CALL_CHAIN` automatically.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CookbookInvokeParams {
    pub cookbook: String,
    pub op: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub payload: serde_json::Value,
    #[serde(default)]
    pub call_chain: Vec<String>,
}

/// Result shape for `cookbook.invoke`. The cookbook's stdout is returned
/// verbatim as a string; the caller parses domain-specific JSON from it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CookbookInvokeResult {
    pub stdout: String,
}

/// Single source of truth for the client↔daemon RPC surface.
#[jsonrpsee::proc_macros::rpc(server, client)]
pub trait EnwiroRpc {
    #[method(name = "cookbook.invoke")]
    async fn cookbook_invoke(
        &self,
        params: CookbookInvokeParams,
    ) -> Result<CookbookInvokeResult, jsonrpsee::types::ErrorObjectOwned>;
}

pub mod client;
pub use client::{connect, connect_at};
