//! JSON-RPC 2.0 over UDS for client↔daemon communication.
//!
//! See `docs/adr/0002-daemon-ipc-architecture.md` for the design rationale.
//! Plugin↔host data emission stays on stdout-JSONL; this module covers only
//! the client↔daemon channel.
//!
//! Wire format: newline-delimited JSON, one envelope per line, over a unix
//! domain socket at `$XDG_RUNTIME_DIR/enwiro/rpc.sock`.
//!
//! Both sides import the same `Request` / `Response` types from this module
//! so request shapes can't drift across server and client.
//!
//! See `Client` for the typed client.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// JSON-RPC 2.0 string. Always "2.0".
pub const JSONRPC_VERSION: &str = "2.0";

/// Default socket location relative to `$XDG_RUNTIME_DIR`.
pub const SOCKET_FILENAME: &str = "rpc.sock";

/// Env var name a daemon sets so child processes (cookbooks etc) discover
/// the live socket without having to compute the path themselves.
pub const SOCKET_ENV_VAR: &str = "ENWIRO_RPC_SOCKET";

/// Env var name carrying the cookbook-call chain for cycle detection on
/// `cookbook.invoke` (per ADR-0002 §4).
pub const CALL_CHAIN_ENV_VAR: &str = "ENWIRO_RPC_CALL_CHAIN";

/// Maximum length (bytes) of a single newline-delimited JSON frame. Caps
/// memory use against pathological peer behaviour (one frame that grows
/// forever); larger payloads should be chunked across messages or sent
/// via a different transport. 1 MiB comfortably accommodates the
/// CookbookPayload + cookbook stdout in practice (today's `recipes.cache`
/// for ~3000 recipes is well under 500 KiB).
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Resolve the default socket path. `$XDG_RUNTIME_DIR/enwiro/rpc.sock`.
pub fn default_socket_path() -> anyhow::Result<PathBuf> {
    let base = dirs::runtime_dir()
        .or_else(|| dirs::cache_dir().map(|d| d.join("run")))
        .ok_or_else(|| anyhow::anyhow!("could not determine runtime or cache directory"))?;
    Ok(base.join("enwiro").join(SOCKET_FILENAME))
}

/// One request from a client to the daemon. Tagged so serde routes by the
/// `method` string and deserialises `params` into the matching variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum Request {
    #[serde(rename = "cookbook.invoke")]
    CookbookInvoke(CookbookInvokeParams),
}

/// JSON-RPC envelope wrapping a `Request`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(flatten)]
    pub request: Request,
}

impl RequestEnvelope {
    pub fn new(id: u64, request: Request) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.into(),
            id,
            request,
        }
    }
}

/// One response from the daemon back to the client. `id` is optional
/// because parse errors on the server side have no request ID to echo
/// back; JSON-RPC 2.0 conveys this as `id: null`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseEnvelope {
    pub jsonrpc: String,
    pub id: Option<u64>,
    #[serde(flatten)]
    pub body: ResponseBody,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponseBody {
    Ok { result: serde_json::Value },
    Err { error: RpcError },
}

impl ResponseEnvelope {
    pub fn ok(id: u64, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.into(),
            id: Some(id),
            body: ResponseBody::Ok { result },
        }
    }

    pub fn err(id: u64, error: RpcError) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.into(),
            id: Some(id),
            body: ResponseBody::Err { error },
        }
    }

    /// Response with no `id` — used by the server when the request line
    /// couldn't be parsed and no `id` is available to echo back. Carries
    /// a `PARSE_ERROR` payload so the client can surface it cleanly.
    pub fn parse_error(message: impl Into<String>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.into(),
            id: None,
            body: ResponseBody::Err {
                error: RpcError {
                    code: RpcError::PARSE_ERROR,
                    message: message.into(),
                    data: None,
                },
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl RpcError {
    /// JSON-RPC 2.0 standard: requested method does not exist.
    pub const METHOD_NOT_FOUND: i32 = -32601;
    /// JSON-RPC 2.0 standard: invalid params for the requested method.
    pub const INVALID_PARAMS: i32 = -32602;
    /// JSON-RPC 2.0 standard: internal server error.
    pub const INTERNAL_ERROR: i32 = -32603;
    /// JSON-RPC 2.0 standard: parse error.
    pub const PARSE_ERROR: i32 = -32700;
    /// Implementation-defined: an application-level error from the handler.
    pub const APPLICATION_ERROR: i32 = -32000;
    /// Implementation-defined: cycle detected in `cookbook.invoke`.
    pub const CYCLE_DETECTED: i32 = -32001;
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "rpc error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for RpcError {}

/// Params for `cookbook.invoke`: delegate a cookbook operation through the
/// daemon. The daemon resolves `cookbook` to a plugin binary, spawns it with
/// the existing stdout-JSONL protocol, and returns the parsed result.
///
/// `payload` is the resolved cookbook configuration that gets piped to the
/// cookbook's stdin as a `CookbookPayload` (see ADR-0001). The caller
/// (typically `enw`, which knows the project cwd) is responsible for
/// running the project-layer config walker before invoking; the daemon
/// itself has no concept of "current project".
///
/// `call_chain` carries the names of cookbooks already invoked in the
/// current spawn tree for cycle detection. Clients spawned from within
/// another cookbook should populate it from `$ENWIRO_RPC_CALL_CHAIN`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CookbookInvokeParams {
    pub cookbook: String,
    pub op: String,
    #[serde(default)]
    pub args: serde_json::Value,
    #[serde(default)]
    pub payload: serde_json::Value,
    #[serde(default)]
    pub call_chain: Vec<String>,
}

/// Result shape for `cookbook.invoke`. The cookbook's stdout is returned
/// verbatim as a string; the caller parses domain-specific JSON from it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CookbookInvokeResult {
    pub v: u32,
    pub stdout: String,
}

pub mod client;

pub use client::{Client, ClientError};
