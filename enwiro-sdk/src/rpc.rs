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

/// Result shape for `env.current`. The daemon returns whatever it last
/// saw from the adapter's `workspace_switch` event stream; `None` fields
/// mean "no switch event observed since daemon start".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvCurrentResult {
    pub env_name: Option<String>,
    pub timestamp: Option<String>,
}

/// Who set a status, so the daemon can protect explicit user marks from
/// being overwritten by automatic detection (#302). `User` covers manual
/// channels (`enw mark`, kanban); `Auto` covers system marks (on cook/prep)
/// and is overridable by cookbook-reported status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MarkSource {
    #[default]
    User,
    Auto,
}

/// Params for `env.mark`: set the status of an environment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvMarkParams {
    pub env_name: String,
    pub status: String,
    /// Provenance of this mark. Defaults to `User` for wire-compatibility
    /// with older clients that don't send it.
    #[serde(default)]
    pub source: MarkSource,
}

/// Result shape for `env.mark`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvMarkResult {
    pub ok: bool,
}

/// Params for `launch.resolve`: the daemon decides *how* to launch a command
/// in an environment (host vs. containerized), but does **not** resolve/cook
/// the env; the client passes the already-resolved `(env_name, env_path)`.
///
/// `interactive` reports whether the *caller's* stdin is a TTY; the daemon
/// can't observe the caller's terminal, so the client tells it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchResolveParams {
    pub env_name: String,
    pub env_path: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub interactive: bool,
}

/// Result of `launch.resolve`: the final process to spawn (`program` + `args`)
/// plus the environment variables the daemon decided the launched process
/// should carry (`env_vars`, e.g. `ENWIRO_ENV`). The client sets cwd, applies
/// `env_vars`, and exec-replaces; it does not decide any of this itself. Host
/// path = the command itself; container path = `<engine> run ... <image> <command> ...`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchResolveResult {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env_vars: Vec<(String, String)>,
}

/// One environment in an `env.list` result: status plus the frecency-derived
/// relevance scores the daemon computed centrally from its usage signals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvListEntry {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<crate::status::Status>,
    pub launcher_score: f64,
    pub slot_score: f64,
}

/// Result shape for `env.list`: every environment under the daemon's
/// workspaces directory, scored in one place so consumers (GUI board,
/// launcher, slot assignment) receive relevance instead of recomputing it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvListResult {
    pub envs: Vec<EnvListEntry>,
}

/// Single source of truth for the client↔daemon RPC surface.
#[jsonrpsee::proc_macros::rpc(server, client)]
pub trait EnwiroRpc {
    #[method(name = "cookbook.invoke")]
    async fn cookbook_invoke(
        &self,
        params: CookbookInvokeParams,
    ) -> Result<CookbookInvokeResult, jsonrpsee::types::ErrorObjectOwned>;

    #[method(name = "env.current")]
    async fn env_current(&self) -> Result<EnvCurrentResult, jsonrpsee::types::ErrorObjectOwned>;

    #[method(name = "env.mark")]
    async fn env_mark(
        &self,
        params: EnvMarkParams,
    ) -> Result<EnvMarkResult, jsonrpsee::types::ErrorObjectOwned>;

    #[method(name = "launch.resolve")]
    async fn launch_resolve(
        &self,
        params: LaunchResolveParams,
    ) -> Result<LaunchResolveResult, jsonrpsee::types::ErrorObjectOwned>;

    #[method(name = "env.list")]
    async fn env_list(&self) -> Result<EnvListResult, jsonrpsee::types::ErrorObjectOwned>;
}

pub mod client;
pub use client::{connect, connect_at};
